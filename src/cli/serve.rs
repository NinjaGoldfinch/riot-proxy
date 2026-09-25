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

pub async fn serve(config: Config) -> anyhow::Result<()> {
    telemetry::init_tracing(&config)?;
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
        client: RiotClient::new(&config)?,
        limiter: Arc::clone(&limiter),
        cache: Arc::clone(&cache),
        archive: Arc::new(SqliteArchive::new(db.clone(), config.archive_timelines)),
        scope: KeyScope::from_key(&config.riot_api_key),
        policy,
        interactive_budget: Duration::from_millis(config.client_wait_budget_ms),
        swr: config.stale_while_revalidate,
    });

    let listener = tokio::net::TcpListener::bind((config.host.as_str(), config.port))
        .await
        .with_context(|| format!("binding {}:{}", config.host, config.port))?;
    let addr = listener.local_addr()?;
    tracing::info!(%addr, "listening");

    let shutdown = app::shutdown_signal()?;
    let auth = Arc::new(Auth::new(&config, db.clone()));
    let state = AppState {
        config: config.into(),
        db: db.clone(),
        limiter: Arc::clone(&limiter),
        fetcher,
        auth,
        quotas: Arc::new(Quotas::new()),
        limiter_restored: restored,
    };
    let served = app::serve(listener, app::router(state, metrics), shutdown).await;

    // After draining, whether or not serve failed: final limiter checkpoint
    // (design/05) and flush pending L2 writes (design/04).
    checkpoints.abort();
    persist::checkpoint_now(&limiter, &db).await;
    cache.shutdown().await;
    tracing::info!("limiter checkpoint and L2 flushed; stopped");
    served?;
    Ok(())
}

fn print_bootstrap_key(key: &str) {
    eprintln!();
    eprintln!("  bootstrap admin key (shown once): {key}");
    eprintln!("  Scopes read,admin. Store it now; it cannot be recovered.");
    eprintln!("  Mint more keys with: riot-proxy key create --name <name>");
    eprintln!();
}
