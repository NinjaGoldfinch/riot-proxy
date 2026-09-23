//! `riot-proxy serve` (docs/design/03 §Process model, 07 §First run).

use anyhow::Context;

use crate::app::{self, AppState};
use crate::config::Config;
use crate::consumers;
use crate::db::Db;
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

    let listener = tokio::net::TcpListener::bind((config.host.as_str(), config.port))
        .await
        .with_context(|| format!("binding {}:{}", config.host, config.port))?;
    let addr = listener.local_addr()?;
    tracing::info!(%addr, "listening");

    let shutdown = app::shutdown_signal()?;
    let state = AppState {
        config: config.into(),
        db,
    };
    app::serve(listener, app::router(state, metrics), shutdown).await?;
    tracing::info!("stopped");
    Ok(())
}

fn print_bootstrap_key(key: &str) {
    eprintln!();
    eprintln!("  bootstrap admin key (shown once): {key}");
    eprintln!("  Scopes read,admin. Store it now; it cannot be recovered.");
    eprintln!("  Mint more keys with: riot-proxy key create --name <name>");
    eprintln!();
}
