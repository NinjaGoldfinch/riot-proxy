//! `riot-proxy packs …` (design/12 §Replica and backups, OBJ-04): rebuild the
//! map of where every packed body is from the bucket. Needs
//! `ARCHIVE_OBJECT_URL`; point it at the R2 replica to recover from that.

use anyhow::Context;
use clap::Subcommand;
use ulid::Ulid;

use crate::archive::objects::Objects;
use crate::archive::recovery;
use crate::clock::Clock;
use crate::config::Config;

#[derive(Debug, Subcommand)]
pub enum PacksCommand {
    /// Rebuild body_packs and body_locations from the packs' own indexes.
    Reindex {
        /// Only the packs written after this pack id (a ULID).
        #[arg(long)]
        since: Option<Ulid>,
    },
    /// Load the newest readable index snapshot, then reindex the packs written after it. Stop the service first.
    RestoreIndex,
    /// Write today's index snapshot now, as the daily archive:index-snapshot does.
    Snapshot,
}

pub async fn run(config: &Config, cmd: PacksCommand) -> anyhow::Result<()> {
    let objects = Objects::from_config(config).context("ARCHIVE_OBJECT_URL")?;
    let db = super::open_db(config).await?;
    match cmd {
        PacksCommand::Reindex { since } => {
            let r = recovery::reindex(&db, &objects, since).await?;
            println!("reindexed {} packs, {} bodies", r.packs, r.bodies);
            failed(&r.failed)
        }
        PacksCommand::RestoreIndex => {
            let r = recovery::restore_index(&db, &objects).await?;
            for (date, why) in &r.skipped {
                println!("skipped the snapshot of {date}: {why}");
            }
            match &r.snapshot {
                Some(date) => println!(
                    "restored the snapshot of {date}: {} packs, {} locations",
                    r.packs, r.locations
                ),
                None => println!("no readable snapshot; reindexed every pack"),
            }
            println!(
                "reindexed {} newer packs, {} bodies",
                r.reindexed.packs, r.reindexed.bodies
            );
            failed(&r.reindexed.failed)
        }
        PacksCommand::Snapshot => {
            let s = recovery::write_snapshot(&db, &objects, Clock::now().unix_ms).await?;
            println!(
                "{} and {}: {} packs, {} locations, last pack {}",
                recovery::packs_key(&s.date),
                recovery::locations_key(&s.date),
                s.packs,
                s.locations,
                s.last_pack.as_deref().unwrap_or("none")
            );
            Ok(())
        }
    }
}

/// Packs that could not be read fail the command, after the rest are done.
fn failed(failed: &[(String, String)]) -> anyhow::Result<()> {
    for (key, why) in failed {
        eprintln!("not reindexed: {key}: {why}");
    }
    if failed.is_empty() {
        Ok(())
    } else {
        anyhow::bail!(
            "{} packs could not be read; the others were recorded",
            failed.len()
        )
    }
}
