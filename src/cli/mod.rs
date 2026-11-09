//! Command-line interface (docs/design/07 §First run). `main.rs` only parses and dispatches.

pub mod backup;
pub mod healthcheck;
pub mod key;
pub mod migrate_v1;
pub mod packs;
#[cfg(feature = "dev-cli")]
pub mod record;
#[cfg(feature = "dev-cli")]
pub mod riot;
pub mod serve;

use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};

use crate::config::{Config, ConfigArgs, Database};
use crate::db::Db;

#[derive(Debug, Parser)]
#[command(
    name = "riot-proxy",
    version,
    about = "Single-binary proxy for the Riot Games API"
)]
#[command(propagate_version = true, arg_required_else_help = true)]
pub struct Cli {
    #[command(flatten)]
    pub config: ConfigArgs,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the proxy: migrate, print the bootstrap admin key on first run, serve HTTP.
    Serve,
    /// Apply pending database migrations and exit (serve also does this on boot).
    Migrate,
    /// Manage consumer API keys.
    #[command(subcommand)]
    Key(key::KeyCommand),
    /// GET /healthz on the local server; exit 0 if it answers 200. For container healthchecks.
    Healthcheck {
        /// Seconds to wait for an answer.
        #[arg(long, default_value_t = 3)]
        timeout: u64,
    },
    /// Print the OpenAPI document (JSON) to stdout.
    Spec,
    /// Import v1's archive and players from a Postgres dump (consumers are not migrated).
    MigrateV1(migrate_v1::Args),
    /// Write a consistent copy of the database to OUT (`VACUUM INTO`), safe while serving.
    Backup {
        /// Where to write the copy; must not exist.
        out: std::path::PathBuf,
    },
    /// Recover the map of packed bodies from the bucket (ARCHIVE_OBJECT_URL).
    #[command(subcommand)]
    Packs(packs::PacksCommand),
    /// Raw Riot API calls for development (built with --features dev-cli).
    #[cfg(feature = "dev-cli")]
    #[command(subcommand)]
    Riot(riot::RiotCommand),
}

/// Parse `std::env::args` and run. Errors are printed here; the exit code says
/// whether the command succeeded.
pub fn run() -> ExitCode {
    let cli = Cli::parse();
    if let Command::Spec = cli.command {
        match crate::routes::docs::spec().to_pretty_json() {
            Ok(json) => println!("{json}"),
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        }
        return ExitCode::SUCCESS;
    }
    let config = match Config::load(cli.config) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_threads())
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: starting the tokio runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    let result = runtime.block_on(async move {
        match cli.command {
            Command::Serve => serve::serve(config).await,
            Command::Migrate => migrate(&config).await,
            Command::Key(cmd) => key::run(&config, cmd).await,
            Command::Healthcheck { timeout } => healthcheck::run(&config, timeout).await,
            Command::Spec => Ok(()),
            Command::Backup { out } => backup::run(&config, &out).await,
            Command::MigrateV1(args) => migrate_v1::run(&config, &args).await,
            Command::Packs(cmd) => packs::run(&config, cmd).await,
            #[cfg(feature = "dev-cli")]
            Command::Riot(cmd) => riot::run(&config, cmd).await,
        }
    });
    shut_down(runtime, BLOCKING_GRACE);
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// How long blocking work a returned command abandoned may still run before
/// the process exits (ADR-209): a boot catch-up query a shutdown cut short
/// goes on in its `spawn_blocking` thread, which dropping the runtime would
/// otherwise wait for, for minutes on a large archive. Everything that must
/// finish (the drain, the job grace, the final checkpoint and L2 flush) was
/// awaited before the command returned.
const BLOCKING_GRACE: Duration = Duration::from_secs(10);
// With the 10 s drain and the 10 s job grace, well inside systemd's 90 s.
const _: () = assert!(BLOCKING_GRACE.as_secs() <= 15);

/// Drop the runtime, giving blocking tasks still running at most `grace`.
fn shut_down(runtime: tokio::runtime::Runtime, grace: Duration) {
    runtime.shutdown_timeout(grace);
}

/// design/03: the workload is I/O; four workers are plenty.
fn worker_threads() -> usize {
    std::thread::available_parallelism().map_or(4, |n| n.get().min(4))
}

pub(crate) fn sqlite_path(config: &Config) -> anyhow::Result<&std::path::Path> {
    match &config.database {
        Database::Sqlite(path) => Ok(path),
        Database::Postgres(_) => anyhow::bail!("only sqlite:// databases are supported in this build"),
    }
}

/// Open the database with a small reader pool, which is all one-shot commands need.
pub(crate) async fn open_db(config: &Config) -> anyhow::Result<Db> {
    use anyhow::Context;
    let path = sqlite_path(config)?.to_path_buf();
    Db::open_async(path.clone(), 1)
        .await
        .with_context(|| format!("opening {}", path.display()))
}

async fn migrate(config: &Config) -> anyhow::Result<()> {
    use anyhow::Context;
    let path = sqlite_path(config)?.to_path_buf();
    let report = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut conn =
            rusqlite::Connection::open(&path).with_context(|| format!("opening {}", path.display()))?;
        Ok((crate::db::migrate(&mut conn)?, path))
    })
    .await??;
    let (report, path) = report;
    let applied: Vec<String> = report
        .applied_migrations()
        .iter()
        .map(ToString::to_string)
        .collect();
    if applied.is_empty() {
        println!("{}: up to date", path.display());
    } else {
        println!("{}: applied {}", path.display(), applied.join(", "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// ADR-209: a blocking task the command abandoned (a boot check a
    /// shutdown cut short) does not hold the process open past the grace.
    #[test]
    fn shutting_down_does_not_wait_for_abandoned_blocking_work() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .build()
            .unwrap();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        runtime.block_on(async {
            // Abandoned, as `select!` abandons a catch-up query.
            drop(tokio::task::spawn_blocking(move || {
                let _ = started_tx.send(());
                std::thread::sleep(Duration::from_secs(30));
            }));
        });
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let at = std::time::Instant::now();
        shut_down(runtime, Duration::from_millis(100));
        assert!(at.elapsed() < Duration::from_secs(5), "{:?}", at.elapsed());
    }
}
