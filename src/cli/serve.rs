//! `riot-proxy serve` (docs/design/03 §Process model, 07 §First run).
//!
//! The boot order (ADR-209): open the database, restore the limiter, warm L1,
//! build the state, bind and **accept**, and only then the boot catch-up
//! (`recover`, lanes, the checks that queue what an older process left
//! undone) followed by the job workers and the ticks. A shutdown signal from
//! "listening" on stops the catch-up where it is and drains.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::Context;
use futures_util::FutureExt;
use tokio::sync::Notify;

use crate::app::{self, AppState};
use crate::archive::SqliteArchive;
use crate::archive::objects::Objects;
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
    /// Test-only: hold the boot catch-up open (ADR-209).
    #[doc(hidden)]
    pub catch_up_hold: Option<CatchUpHold>,
    /// Test-only: shut down when this is notified instead of on SIGTERM or
    /// SIGINT, so an in-process test neither installs nor receives process
    /// signals (another test in the binary sends itself SIGTERM).
    #[doc(hidden)]
    pub shutdown: Option<Arc<Notify>>,
    /// Test-only: one more job kind and its handler, made from the database
    /// (AVL-04's shutdown test runs a job that holds the writer).
    #[doc(hidden)]
    pub extra_handler: Option<ExtraHandler>,
}

/// Test-only (ADR-209): makes the boot catch-up observably slow, as a
/// minutes-long check on a large archive is, without a large archive.
#[doc(hidden)]
#[derive(Debug, Clone, Default)]
pub struct CatchUpHold {
    /// Set when the catch-up reaches the hold: interrupted jobs are re-queued
    /// and lanes assigned, the checks have not run.
    pub reached: Arc<AtomicBool>,
    /// The catch-up waits here until notified.
    pub release: Arc<Notify>,
    /// Set as the catch-up finishes, before the job workers start.
    pub done: Arc<AtomicBool>,
}

/// Test-only: a job kind `serve` does not have, with a handler built from its
/// [`Db`] (AVL-04, ADR-211).
#[doc(hidden)]
#[derive(Clone)]
pub struct ExtraHandler {
    pub kind: &'static str,
    pub make: Arc<dyn Fn(Db) -> Arc<dyn crate::jobs::scheduler::Handler> + Send + Sync>,
}

impl std::fmt::Debug for ExtraHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtraHandler")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

pub async fn serve(config: Config) -> anyhow::Result<()> {
    serve_with(config, ServeOptions::default()).await
}

pub async fn serve_with(config: Config, options: ServeOptions) -> anyhow::Result<()> {
    if !options.skip_tracing_init {
        telemetry::init_tracing(&config)?;
    }
    // `/status`'s build identity, once at startup (STS-01, design/14).
    tracing::info!(
        version = crate::build_info::status_version(),
        commit = crate::build_info::short_commit(),
        built_at = %crate::build_info::built_at_rfc3339(),
        "riot-proxy starting"
    );
    let started = std::time::Instant::now();
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
    let limiter = Arc::new(Limiter::new(config.bulk_usage_ceiling).with_idle_ceiling(
        config.bulk_idle_ceiling,
        Duration::from_secs(config.bulk_idle_after_s.into()),
    ));
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
    if config.dev_ui_ignored {
        tracing::warn!("DEV_UI=true ignored: /dev is never served when ENV=production");
    }
    // The bucket packed bodies go to (OBJ-01), attached to the database whose
    // locations point into it (OBJ-02). Building the client makes no request;
    // a bad endpoint stops the boot here rather than at the first read.
    if config.archive_object_url.is_some() {
        let objects = Objects::from_config(&config).context("ARCHIVE_OBJECT_URL")?;
        tracing::info!(store = objects.describe(), "object store configured");
        db.attach_objects(objects);
    }
    for key in policy.ineffective_overrides() {
        tracing::warn!(key = %key, "CACHE_TTL_OVERRIDES key matches no cacheable endpoint; ignored");
    }
    // The archive queues what a stored match or timeline lacks (TL-01), and
    // the offload once a full pack of bodies is waiting (OBJ-03). A read-only
    // bucket gets neither the offload nor the snapshot, so nothing is packed,
    // freed or written to it (STG-02).
    let queue = crate::jobs::Queue::new(db.clone()).with_kind_limits(config.job_kind_limits.clone());
    let pending = db.objects().filter(|o| !o.read_only()).map(|_| {
        Arc::new(crate::jobs::offload::Pending::new(
            queue.clone(),
            config.archive_pack_target_bytes,
        ))
    });
    let fetcher = Fetcher::new(FetcherParts {
        client: match options.riot_base_url.as_ref().or(config.riot_base_url.as_ref()) {
            Some(url) => RiotClient::with_base_url(&config, url)?,
            None => RiotClient::new(&config)?,
        },
        limiter: Arc::clone(&limiter),
        cache: Arc::clone(&cache),
        archive: Arc::new({
            let archive = SqliteArchive::new(db.clone(), KeyScope::for_config(&config))
                .queue_missing_halves(queue.clone(), config.archive_timelines)
                .late_stamp_days(config.tier_late_stamp_days);
            match &pending {
                Some(p) => archive.count_unpacked(Arc::clone(p)),
                None => archive,
            }
        }),
        scope: KeyScope::for_config(&config),
        policy,
        interactive_budget: Duration::from_millis(config.client_wait_budget_ms),
        job_budget: Duration::from_millis(config.job_yield_budget_ms),
        swr: config.stale_while_revalidate,
    });

    // Before "listening": a SIGTERM from then on must be a clean shutdown, not
    // the default action. Shared: the server's drain and the boot catch-up
    // both stop on it (ADR-209).
    let shutdown = match options.shutdown.clone() {
        Some(stop) => async move {
            stop.notified().await;
            tracing::info!("shutdown requested");
        }
        .boxed(),
        None => app::shutdown_signal()?.boxed(),
    }
    .shared();

    // Jobs (design/06): handlers over one queue, built here; the workers and
    // ticks start after the boot catch-up has re-queued interrupted work.
    let hub = crate::ws::Hub::new();
    let mirror = Arc::new(crate::r#static::Mirror::new(
        config.ddragon_dir.clone(),
        crate::jobs::ddragon::Cdn::new(
            &config,
            options
                .ddragon_urls
                .clone()
                .or_else(|| {
                    config
                        .ddragon_base_url
                        .as_deref()
                        .map(crate::jobs::ddragon::CdnUrls::mock)
                })
                .unwrap_or_default(),
        )?,
    ));
    let ddragon = Arc::new(crate::jobs::ddragon::DdragonSync {
        mirror: Arc::clone(&mirror),
        hub: hub.clone(),
    });
    let scope = KeyScope::for_config(&config).as_str().to_string();
    if config.key_scope.is_some() {
        tracing::warn!(
            key_scope = %scope,
            key_own_scope = %KeyScope::from_key(&config.riot_api_key),
            "KEY_SCOPE pins the key scope; players, facts and cache keys are read and written under it"
        );
    }
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
        collect_overlap_s: config.ladder_collect_overlap_s,
        archive_timelines: config.archive_timelines,
        rank_lookup_limit: config.rank_lookup_limit,
        rank_lookup_recheck_s: config.rank_lookup_recheck_s,
        late_stamp_days: config.tier_late_stamp_days,
        delta_on: config.aggregate_delta_s > 0,
        crawl_match_limit: config.ladder_crawl_match_limit,
    });
    let names = Arc::new(crate::jobs::names::NamesBackfill {
        db: db.clone(),
        key_scope: scope.clone(),
    });
    let maintenance = crate::jobs::maintenance::Maintenance::new(
        db.clone(),
        config.data_dir.join("backups"),
        config.backup_keep as usize,
    );
    let maintenance = Arc::new(match &pending {
        Some(_) => maintenance.then_bucket_jobs(queue.clone()),
        None => maintenance,
    });
    // One per process: the routes serve from it, the rebuild clears and warms it.
    let analytics_cache = Arc::new(crate::routes::analytics::cache::Analytics::new(
        crate::routes::analytics::Reads::new(db.clone(), Arc::clone(&mirror), &scope, &config.ladder_queues),
        config.analytics_cache_entries,
        config.ladder_queues.clone(),
    ));
    let analytics = Arc::new(crate::jobs::analytics::AnalyticsContext {
        cache: Arc::clone(&analytics_cache),
        queue: queue.clone(),
        hub: hub.clone(),
        key_scope: scope.clone(),
        patch_limit: config.aggregate_patch_limit,
        reextract_batch: config.facts_reextract_batch,
        mirror: Arc::clone(&mirror),
        ladders: crate::jobs::ticks::ladders(&config),
        delta: crate::jobs::analytics::DeltaSettings {
            on: config.aggregate_delta_s > 0,
            target_ms: config.aggregate_delta_target_ms,
            cap: config.aggregate_delta_batch as usize,
        },
        delta_sizes: Default::default(),
    });
    let timelines = Arc::new(crate::jobs::timelines::TimelinesContext {
        fetcher: fetcher.clone(),
        queue: queue.clone(),
        patches: config.timeline_backfill_patches,
    });
    let activity = crate::jobs::activity::Activity::new();
    let mut handlers = crate::jobs::handlers(
        &poll,
        &archiving,
        &ddragon,
        &ladder,
        &names,
        &analytics,
        &maintenance,
        &timelines,
    );
    // Only with a bucket: without one the kind does not exist (design/12).
    if let (Some(objects), Some(pending)) = (db.objects(), &pending) {
        let offload = Arc::new(crate::jobs::offload::OffloadContext {
            db: db.clone(),
            objects: objects.clone(),
            pending: Arc::clone(pending),
            local_days: config.archive_body_local_days,
            target_bytes: config.archive_pack_target_bytes,
            pace: Duration::from_millis(config.archive_offload_pace_ms),
        });
        handlers = handlers
            .with(
                crate::jobs::kinds::ARCHIVE_OFFLOAD,
                crate::jobs::offload::OffloadHandler(offload),
            )
            .with(
                crate::jobs::kinds::ARCHIVE_INDEX_SNAPSHOT,
                crate::jobs::index_snapshot::IndexSnapshotHandler {
                    db: db.clone(),
                    objects: objects.clone(),
                },
            );
    }
    if let Some(extra) = &options.extra_handler {
        handlers = handlers.with_shared(extra.kind, (extra.make)(db.clone()));
    }
    let scheduler = crate::jobs::Scheduler::with_queue(queue.clone(), handlers)
        .with_limiter(Arc::clone(&limiter))
        .with_activity(activity.clone());
    // The dashboard's documents: the `metrics` topic ticks only while someone
    // holds it; the sampler (started with the workers, below) reads the
    // table-scanning parts in the background and records the history
    // regardless (v1, ADM-02). It reads the quotas and the cache too (ADM-04),
    // so they are built first.
    let config = Arc::new(config);
    let quotas = Arc::new(Quotas::new());
    let stats = Arc::new(crate::stats::Stats {
        hub: hub.clone(),
        db: db.clone(),
        key_scope: scope.clone(),
        limiter: Arc::clone(&limiter),
        metrics: metrics.clone(),
        mirror: Arc::clone(&mirror),
        started,
        samples: crate::stats::Samples::default(),
        fetcher: fetcher.clone(),
        quotas: Arc::clone(&quotas),
        config: Arc::clone(&config),
    });
    let metrics_topic = crate::ws::metrics::spawn(
        Arc::clone(&stats),
        Duration::from_secs(u64::from(config.metrics_interval_s)),
    );

    let auth = Arc::new(Auth::new(&config, db.clone()));
    let state = AppState {
        config: Arc::clone(&config),
        db: db.clone(),
        limiter: Arc::clone(&limiter),
        fetcher,
        auth,
        quotas,
        limiter_restored: restored,
        refresh: Arc::new(crate::routes::players::RefreshWindows::new()),
        jobs: queue.clone(),
        activity,
        hub: hub.clone(),
        ddragon: mirror,
        stats: Arc::clone(&stats),
        maintenance,
        analytics: analytics_cache,
        status: Arc::new(crate::status::Status::new(started)),
    };
    let checker = crate::status::checks::Checker::new(&state)?;
    let router = app::router(state, metrics);

    // Bind and accept at once (ADR-209): "listening" means accepting. The
    // writer and the limiter are ready, so `/readyz` is too. The catch-up
    // runs while requests are served, and stops where it is when the
    // shutdown signal arrives or the server ends.
    let listener = tokio::net::TcpListener::bind((config.host.as_str(), config.port))
        .await
        .with_context(|| format!("binding {}:{}", config.host, config.port))?;
    let addr = listener.local_addr()?;
    tracing::info!(%addr, "listening");
    let mut server = tokio::spawn(app::serve(listener, router, shutdown.clone()));
    // `/status`'s checks: a round now, then every STATUS_CHECK_INTERVAL_S (STS-02b).
    let checks = checker.spawn();
    let mut ended = None;
    let caught_up = tokio::select! {
        () = shutdown.clone() => false,
        res = &mut server => {
            ended = Some(res);
            false
        }
        () = boot_catch_up(
            &scheduler,
            &queue,
            &config,
            &scope,
            pending.is_some(),
            options.catch_up_hold.as_ref(),
        ) => true,
    };
    // Started only after a complete catch-up, so `recover` always comes first
    // (design/06); after a shutdown during it, nothing is left to stop.
    let jobs = if caught_up {
        tracing::info!(
            workers = config.job_concurrency,
            kind_limits = ?config.job_kind_limits,
            "starting job workers"
        );
        let workers = scheduler.start(usize::try_from(config.job_concurrency).unwrap_or(8));
        let ticks = crate::jobs::ticks::Ticks::start_with_backups(
            &scheduler,
            &scope,
            crate::jobs::ticks::running_schedule(&config),
            crate::jobs::ticks::ladders(&config),
            // With backups on, a maintenance run that wrote none is not the
            // tick's last run (AVL-13).
            (config.backup_keep > 0).then(|| config.data_dir.join("backups")),
        );
        let history = crate::stats::spawn_sampler(
            Arc::clone(&stats),
            crate::stats::FAST_SAMPLE,
            Duration::from_secs(u64::from(config.metrics_history_interval_s)),
        );
        Some((workers, ticks, history))
    } else {
        log_catch_up_stopped(ended.is_some(), shutdown.clone());
        None
    };
    let served = match ended {
        Some(res) => res,
        None => server.await,
    };
    let served: anyhow::Result<()> = match served {
        Ok(res) => res.context("serving"),
        Err(e) => Err(e).context("the server task"),
    };

    // After draining, whether or not serve failed: stop ticking, let running
    // jobs finish (the rest resume on the next boot), close sockets, then the
    // final limiter checkpoint (design/05), the writes still running behind
    // their requests (AVL-02) and pending L2 writes (design/04).
    let (workers, history) = match jobs {
        Some((workers, ticks, history)) => {
            ticks.shutdown().await;
            (Some(workers), Some(history))
        }
        None => (None, None),
    };
    checks.abort();
    metrics_topic.abort();
    if let Some(history) = history {
        history.abort();
    }
    if let Some(workers) = workers {
        workers.shutdown(JOB_GRACE).await;
    }
    hub.shutdown();
    checkpoints.abort();
    persist::checkpoint_now(&limiter, &db).await;
    db.flush_detached().await;
    cache.shutdown().await;
    tracing::info!("limiter checkpoint and L2 flushed; stopped");
    served
}

/// The boot catch-up (design/06, ADR-209): rows a previous process left
/// running re-queued, queued jobs given their lane, then the checks that
/// queue what an older version or process left undone. It runs while the
/// server accepts; the caller starts the workers once it returns, and drops it
/// (at whichever query it is on) on shutdown. Each step logs its own failure
/// and the next runs regardless, as before the reorder.
async fn boot_catch_up(
    scheduler: &crate::jobs::Scheduler,
    queue: &crate::jobs::Queue,
    config: &Config,
    scope: &str,
    offload: bool,
    hold: Option<&CatchUpHold>,
) {
    let began = Instant::now();
    match scheduler.recover().await {
        Ok(r) => {
            if r.requeued > 0 {
                tracing::info!(
                    jobs = r.requeued,
                    "re-queued jobs a previous process left running"
                );
            }
            if r.failed > 0 {
                tracing::warn!(
                    jobs = r.failed,
                    "failed jobs a previous process left running on their last attempt"
                );
            }
        }
        Err(e) => tracing::warn!(error = %e, "could not re-queue interrupted jobs"),
    }
    match scheduler.assign_lanes().await {
        Ok(0) => {}
        Ok(n) => tracing::info!(jobs = n, "gave queued jobs their rate-limit lane"),
        Err(e) => tracing::warn!(error = %e, "could not give queued jobs their rate-limit lane"),
    }
    if let Some(hold) = hold {
        hold.reached.store(true, Ordering::SeqCst);
        hold.release.notified().await;
    }
    match crate::jobs::archive::queue_missing_halves(queue, config.archive_timelines).await {
        Ok(0) => {}
        Ok(n) => tracing::info!(matches = n, "queued the missing halves of archived matches"),
        Err(e) => tracing::warn!(error = %e, "could not queue the missing halves of archived matches"),
    }
    // Facts an older version derived are swept once, in the background.
    match crate::jobs::analytics::reextract_if_stale(queue).await {
        Ok(true) => tracing::info!("queued facts:reextract for matches an older facts version derived"),
        Ok(false) => {}
        Err(e) => tracing::warn!(error = %e, "could not check for stale match facts"),
    }
    // Ranked matches archived before their tiers were stamped (V0016) are
    // stamped once, in the background (THR-02).
    match crate::jobs::analytics::backfill_tiers_if_needed(queue).await {
        Ok(true) => tracing::info!("queued tiers:backfill for matches archived without tier stamps"),
        Ok(false) => {}
        Err(e) => tracing::warn!(error = %e, "could not check for matches without tier stamps"),
    }
    // Analytics rows from before their tables had a tier and side (V0017) are
    // replaced by one rebuild over every patch of their ladder (ADR-129).
    match crate::jobs::analytics::split_if_needed(queue, scope).await {
        Ok(0) => {}
        Ok(n) => tracing::info!(
            ladders = n,
            "queued analytics rebuilds to split rows by tier and side"
        ),
        Err(e) => tracing::warn!(error = %e, "could not check for analytics rows to split by tier and side"),
    }
    // A ladder no full rebuild has stamped since V0029 is rebuilt over every
    // patch once, one at a time, before its deltas start (THR-03, ADR-182).
    if config.aggregate_delta_s > 0 {
        match crate::jobs::analytics::stamp_if_needed(queue, scope, &crate::jobs::ticks::ladders(config))
            .await
        {
            Ok(0) => {}
            Ok(n) => tracing::info!(
                ladders = n,
                "queued full analytics rebuilds to stamp counted matches"
            ),
            Err(e) => tracing::warn!(error = %e, "could not check which ladders are stamped"),
        }
    }
    // A boot run catches a full pack the lost counter would have queued.
    if offload {
        match crate::jobs::offload::enqueue(queue).await {
            Ok(_) => tracing::info!("queued archive:offload"),
            Err(e) => tracing::warn!(error = %e, "could not queue archive:offload"),
        }
    }
    if let Some(hold) = hold {
        hold.done.store(true, Ordering::SeqCst);
    }
    tracing::info!(
        ms = u64::try_from(began.elapsed().as_millis()).unwrap_or(u64::MAX),
        "boot catch-up done"
    );
}

/// How long running jobs get to finish at shutdown.
const JOB_GRACE: Duration = Duration::from_secs(10);

// The drain, the job grace and the writer interrupt stay under 60 s, so a
// stop ends well inside systemd's 90 s `TimeoutStopSec` (AVL-04, ADR-211;
// design/07 §Option C).
const _: () = assert!(
    app::SHUTDOWN_GRACE.as_secs() + JOB_GRACE.as_secs() + crate::jobs::scheduler::INTERRUPT_WAIT.as_secs()
        < 60
);

fn print_bootstrap_key(key: &str) {
    eprintln!();
    eprintln!("  bootstrap admin key (shown once): {key}");
    eprintln!("  Scopes read,admin. Store it now; it cannot be recovered.");
    eprintln!("  Mint more keys with: riot-proxy key create --name <name>");
    eprintln!();
}

/// The catch-up did not finish. `reason=shutdown signal` is the usual case,
/// also when the server task drained on the signal before the `select!` saw
/// it; `reason=server ended` means the server task ended with no signal
/// fired, and the shutdown sequence that follows is the error path.
fn log_catch_up_stopped<F>(server_ended: bool, shutdown: F)
where
    F: std::future::Future<Output = ()> + Unpin,
{
    let reason = if server_ended && shutdown.now_or_never().is_none() {
        "server ended"
    } else {
        "shutdown signal"
    };
    tracing::info!(reason, "boot catch-up stopped; job workers not started");
}
