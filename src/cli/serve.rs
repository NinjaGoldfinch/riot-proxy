//! `riot-proxy serve` (docs/design/03 §Process model, 07 §First run).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Context;

use crate::app::{self, AppState};
use crate::archive::SqliteArchive;
use crate::cache::ResponseCache;
use crate::cache::keys::KeyScope;
use crate::cache::l1::L1;
use crate::cache::l2::{self, L2Writer};
use crate::config::Config;
use crate::consumers;
use crate::db::Db;
use crate::fetcher::{Fetcher, FetcherParts};
use crate::http::auth::Auth;
use crate::http::quota::Quotas;
use crate::riot::client::RiotClient;
use crate::riot::endpoints::TtlPolicy;
use crate::riot::limiter::Limiter;
use crate::riot::limiter::persist;
use crate::telemetry;

/// What an in-process caller (the P6 exit check) changes about `serve`.
#[derive(Debug, Clone, Default)]
pub struct ServeOptions {
    /// Send every Riot request here (a mock) instead of Riot. `None` in production.
    pub riot_base_url: Option<String>,
    /// Leave the global tracing subscriber alone (it can be installed once).
    pub skip_tracing_init: bool,
    /// Where Data Dragon is fetched from. `None`: Riot's hosts.
    pub ddragon_urls: Option<crate::jobs::ddragon::CdnUrls>,
}

pub async fn serve(config: Config) -> anyhow::Result<()> {
    serve_with(config, ServeOptions::default()).await
}

pub async fn serve_with(config: Config, options: ServeOptions) -> anyhow::Result<()> {
    if !options.skip_tracing_init {
        telemetry::init_tracing(&config)?;
    }
    let metrics = telemetry::metrics_handle()?;
    telemetry::spawn_upkeep(metrics.clone());

    let path = super::sqlite_path(&config)?.to_path_buf();
    let db = Db::open_async(path.clone(), Db::default_readers())
        .await
        .with_context(|| format!("opening {}", path.display()))?;
    tracing::info!(path = %path.display(), "database ready");

    let import = config
        .bootstrap_admin_key
        .as_ref()
        .map(|k| k.expose().to_string());
    let imported = import.is_some();
    if let Some(created) = consumers::bootstrap_admin(&db, import).await? {
        tracing::info!(id = %created.consumer.id, "bootstrap admin consumer created");
        if !imported {
            // stderr, not the log stream: JSON logs are often shipped elsewhere.
            print_bootstrap_key(created.key.expose());
        }
    }

    // Restore the limiter before accepting traffic (design/03 §Process model).
    let limiter = Arc::new(Limiter::new(config.bulk_usage_ceiling));
    let restored = Arc::new(AtomicBool::new(false));
    match persist::restore_from(&limiter, &db).await {
        Ok(rows) => tracing::info!(rows, "limiter checkpoint restored"),
        // Nothing usable: start from bootstrap limits; Riot's headers correct it.
        Err(e) => tracing::warn!(error = %e, "limiter checkpoint could not be read; starting fresh"),
    }
    restored.store(true, Ordering::Release);
    let checkpoints = persist::spawn_checkpoints(Arc::clone(&limiter), db.clone());

    // Cache: L1 warmed from L2, expired L2 rows swept (design/04).
    let l1 = L1::from_config(&config);
    match l2::warm(&db, &l1).await {
        Ok(n) => tracing::info!(entries = n, "L1 warmed from L2"),
        Err(e) => tracing::warn!(error = %e, "L2 warm failed; starting with a cold cache"),
    }
    if let Err(e) = l2::sweep(&db).await {
        tracing::warn!(error = %e, "L2 sweep failed");
    }
    let cache = Arc::new(ResponseCache::new(l1, Some(L2Writer::spawn(db.clone()))));
    let policy = TtlPolicy::from_config(&config);
    for key in policy.ineffective_overrides() {
        tracing::warn!(key = %key, "CACHE_TTL_OVERRIDES key matches no cacheable endpoint; ignored");
    }
    let fetcher = Fetcher::new(FetcherParts {
        client: match &options.riot_base_url {
            Some(url) => RiotClient::with_base_url(&config, url)?,
            None => RiotClient::new(&config)?,
        },
        limiter: Arc::clone(&limiter),
        cache: Arc::clone(&cache),
        archive: Arc::new(SqliteArchive::new(
            db.clone(),
            KeyScope::from_key(&config.riot_api_key),
            config.archive_timelines,
        )),
        scope: KeyScope::from_key(&config.riot_api_key),
        policy,
        interactive_budget: Duration::from_millis(config.client_wait_budget_ms),
        swr: config.stale_while_revalidate,
    });

    // Before "listening": a SIGTERM from then on must be a clean shutdown, not
    // the default action.
    let shutdown = app::shutdown_signal()?;
    let listener = tokio::net::TcpListener::bind((config.host.as_str(), config.port))
        .await
        .with_context(|| format!("binding {}:{}", config.host, config.port))?;
    let addr = listener.local_addr()?;
    tracing::info!(%addr, "listening");

    // Jobs (design/06): handlers over one queue, interrupted work re-queued,
    // then workers and the ticks. `maintenance` ticks once its handler exists
    // (P7-05).
    let hub = crate::ws::Hub::new();
    let mirror = Arc::new(crate::r#static::Mirror::new(
        config.ddragon_dir.clone(),
        crate::jobs::ddragon::Cdn::new(&config, options.ddragon_urls.clone().unwrap_or_default())?,
    ));
    let ddragon = Arc::new(crate::jobs::ddragon::DdragonSync {
        mirror: Arc::clone(&mirror),
        hub: hub.clone(),
    });
    let queue = crate::jobs::Queue::new(db.clone());
    let scope = KeyScope::from_key(&config.riot_api_key).as_str().to_string();
    let poll = Arc::new(crate::jobs::poll::PollContext {
        fetcher: fetcher.clone(),
        queue: queue.clone(),
        hub: hub.clone(),
        key_scope: scope.clone(),
        catchup_limit: config.track_catchup_limit,
        backfill_limit: config.lookup_backfill_limit,
        archive_timelines: config.archive_timelines,
    });
    let archiving = Arc::new(crate::jobs::archive::ArchiveContext {
        fetcher: fetcher.clone(),
        queue: queue.clone(),
        hub: hub.clone(),
        key_scope: scope.clone(),
        archive_timelines: config.archive_timelines,
        lookup_backfill_limit: config.lookup_backfill_limit,
    });
    let ladder = Arc::new(crate::jobs::ladder::LadderContext {
        fetcher: fetcher.clone(),
        queue: queue.clone(),
        hub: hub.clone(),
        key_scope: scope.clone(),
        tier_floor: config.ladder_tier_floor.clone(),
        backfill_limit: config.ladder_backfill_limit,
        lookup_backfill_limit: config.lookup_backfill_limit,
        archive_timelines: config.archive_timelines,
    });
    let names = Arc::new(crate::jobs::names::NamesBackfill {
        db: db.clone(),
        key_scope: scope.clone(),
    });
    let scheduler = crate::jobs::Scheduler::with_queue(
        queue.clone(),
        crate::jobs::handlers(&poll, &archiving, &ddragon, &ladder, &names),
    );
    match scheduler.recover().await {
        Ok(0) => {}
        Ok(n) => tracing::info!(jobs = n, "re-queued jobs a previous process left running"),
        Err(e) => tracing::warn!(error = %e, "could not re-queue interrupted jobs"),
    }
    let workers = scheduler.start(usize::try_from(config.job_concurrency).unwrap_or(8));
    let ticks = crate::jobs::ticks::Ticks::start_with(
        &scheduler,
        &scope,
        crate::jobs::ticks::running_schedule(&config),
        crate::jobs::ticks::ladders(&config),
    );
    // The `metrics` topic ticks only while someone holds it (v1).
    let metrics_topic = crate::ws::metrics::spawn(
        hub.clone(),
        db.clone(),
        scope.clone(),
        Duration::from_secs(u64::from(config.metrics_interval_s)),
    );

    let auth = Arc::new(Auth::new(&config, db.clone()));
    let state = AppState {
        config: config.into(),
        db: db.clone(),
        limiter: Arc::clone(&limiter),
        fetcher,
        auth,
        quotas: Arc::new(Quotas::new()),
        limiter_restored: restored,
        refresh: Arc::new(crate::routes::players::RefreshWindows::new()),
        jobs: queue,
        hub: hub.clone(),
        ddragon: mirror,
    };
    let served = app::serve(listener, app::router(state, metrics), shutdown).await;

    // After draining, whether or not serve failed: stop ticking, let running
    // jobs finish (the rest resume on the next boot), close sockets, then the
    // final limiter checkpoint (design/05) and pending L2 writes (design/04).
    ticks.shutdown().await;
    metrics_topic.abort();
    workers.shutdown(JOB_GRACE).await;
    hub.shutdown();
    checkpoints.abort();
    persist::checkpoint_now(&limiter, &db).await;
    cache.shutdown().await;
    tracing::info!("limiter checkpoint and L2 flushed; stopped");
    served?;
    Ok(())
}

/// How long running jobs get to finish at shutdown.
const JOB_GRACE: Duration = Duration::from_secs(10);

fn print_bootstrap_key(key: &str) {
    eprintln!();
    eprintln!("  bootstrap admin key (shown once): {key}");
    eprintln!("  Scopes read,admin. Store it now; it cannot be recovered.");
    eprintln!("  Mint more keys with: riot-proxy key create --name <name>");
    eprintln!();
}
