//! `riot-proxy serve` (docs/design/03 §Process model, 07 §First run).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Context;

use crate::app::{self, AppState};
use crate::config::Config;
use crate::consumers;
use crate::db::Db;
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

    let listener = tokio::net::TcpListener::bind((config.host.as_str(), config.port))
        .await
        .with_context(|| format!("binding {}:{}", config.host, config.port))?;
    let addr = listener.local_addr()?;
    tracing::info!(%addr, "listening");

    let shutdown = app::shutdown_signal()?;
    let state = AppState {
        config: config.into(),
        db: db.clone(),
        limiter: Arc::clone(&limiter),
        limiter_restored: restored,
    };
    let served = app::serve(listener, app::router(state, metrics), shutdown).await;

    // Final checkpoint after draining (design/05), whether or not serve failed.
    checkpoints.abort();
    persist::checkpoint_now(&limiter, &db).await;
    tracing::info!("limiter checkpoint written; stopped");
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
