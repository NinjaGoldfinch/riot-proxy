//! `riot-proxy backup OUT` (design/07 §Backups & restore): the daily
//! maintenance backup's statement, on demand, e.g. before a major upgrade.

use std::path::Path;

use anyhow::Context;

use crate::config::Config;

pub async fn run(config: &Config, out: &Path) -> anyhow::Result<()> {
    let db = super::sqlite_path(config)?;
    if !db.exists() {
        anyhow::bail!("{} does not exist", db.display());
    }
    crate::jobs::maintenance::vacuum_into(db, out)
        .await
        .with_context(|| format!("backing up {} to {}", db.display(), out.display()))?;
    let size = tokio::fs::metadata(out).await.map(|m| m.len()).unwrap_or(0);
    println!("{}: {size} bytes", out.display());
    Ok(())
}
