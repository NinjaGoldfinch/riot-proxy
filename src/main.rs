use std::process::ExitCode;

use anyhow::Context;
use clap::Parser;
use riot_proxy::app::{self, AppState};
use riot_proxy::config::{Config, ConfigArgs, Database};
use riot_proxy::db::Db;
use riot_proxy::telemetry;

/// Serve the proxy. Subcommands (`serve`, `migrate`, `key …`) arrive in P0-06.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    #[command(flatten)]
    config: ConfigArgs,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let config = match Config::load(cli.config) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    // design/03: the workload is I/O; four workers are plenty.
    let workers = std::thread::available_parallelism().map_or(4, |n| n.get().min(4));
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(workers)
        .enable_all()
        .build();
    let result = runtime
        .context("starting the tokio runtime")
        .and_then(|rt| rt.block_on(serve(config)));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("{e:#}");
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn serve(config: Config) -> anyhow::Result<()> {
    telemetry::init_tracing(&config)?;
    let metrics = telemetry::metrics_handle()?;
    telemetry::spawn_upkeep(metrics.clone());

    let Database::Sqlite(path) = &config.database else {
        anyhow::bail!("only sqlite:// databases are supported in this build");
    };
    let db = Db::open_async(path.clone(), Db::default_readers())
        .await
        .with_context(|| format!("opening {}", path.display()))?;
    tracing::info!(path = %path.display(), "database ready");

    let listener = tokio::net::TcpListener::bind((config.host.as_str(), config.port))
        .await
        .with_context(|| format!("binding {}:{}", config.host, config.port))?;
    tracing::info!(addr = %listener.local_addr()?, "listening");

    let shutdown = app::shutdown_signal()?;
    let state = AppState {
        config: config.into(),
        db,
    };
    app::serve(listener, app::router(state, metrics), shutdown).await?;
    tracing::info!("stopped");
    Ok(())
}
