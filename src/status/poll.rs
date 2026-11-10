//! The status poller (SITE-13, design/14 §Status poller, ADR-219). An
//! in-process task, not a job, reads `lol-status-v4` `platform-data`
//! (`status.platformData`) on each of `STATUS_POLL_PLATFORMS` (every platform
//! by default) at boot and every `STATUS_POLL_INTERVAL_S` (300) after.
//!
//! Each read goes through the fetcher at bulk priority, past the cache read
//! and writing through ([`READ`]), so L1 holds the body and
//! `GET /v1/lol/status/{platform}` answers `HIT` (`status`'s TTL is 300 s,
//! hard 600 s). A platform the limiter has no room for within a second is
//! skipped until the next round, as `/status`'s probe is.
//!
//! Each platform's last answer is stored in `platform_status` ([`stored`]
//! reads it): the last 200's body and `fetched_at`, and the last failure's
//! `failed_at` and error. A failure keeps the body. A 200 that lists an
//! incident is written, redacted, to `STATUS_RECORDINGS_DIR` when it is set,
//! one file per platform and incident id, once ([`Recordings`]).
//!
//! `/status`'s `riot_api` probe is separate and unchanged (design/14 §Checks).

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::future::BoxFuture;
use rusqlite::params;
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;

use crate::app::AppState;
use crate::db::{Db, DbError};
use crate::fetcher::{FetchError, FetchOptions, FetchResult, Fetcher, XCache};
use crate::metrics::STATUS_POLLS_TOTAL;
use crate::riot::client::{RequestError, RiotRequest};
use crate::riot::endpoints::{Endpoint, Target};
use crate::riot::limiter::Priority;
use crate::riot::routing::Platform;

/// The endpoint the poller reads.
pub const METHOD: &str = "status.platformData";
/// No limiter room within this skips a platform until the next round.
pub const ROOM: Duration = Duration::from_secs(1);
/// What a recording has in place of the API key. It never contains `RGAPI-`,
/// which CI greps for.
pub const REDACTED: &str = "[REDACTED]";

/// How the poller fetches: bulk, past the cache read and writing through
/// (`X-Cache: BYPASS`), at most [`ROOM`] for the limiter, and never served
/// the cached copy in place of Riot's answer.
pub const READ: FetchOptions = FetchOptions {
    priority: Priority::Bulk,
    bypass: true,
    budget: Some(ROOM),
    fresh_only: true,
    ..FetchOptions::INTERACTIVE
};

/// What one read of one platform came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Read {
    /// Riot answered 200 with this body.
    Body(Bytes),
    /// The limiter had no room within [`ROOM`]: nothing was asked.
    Skipped,
    /// Anything else, and why.
    Failed(String),
}

/// What a round did with one platform: `status_polls_total`'s `result`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Polled {
    /// A 200 stored.
    Ok,
    /// A failure stored; the last body stays.
    Failed,
    /// Not read this round; nothing stored.
    Skipped,
}

impl Polled {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }
}

/// One platform's read, for the poller. [`Fetcher`] in production; tests
/// script it.
pub trait Reader: Send + Sync + 'static {
    fn read(&self, req: RiotRequest) -> BoxFuture<'_, Read>;
}

impl Reader for Fetcher {
    fn read(&self, req: RiotRequest) -> BoxFuture<'_, Read> {
        Box::pin(async move { read_of(self.fetch(req, READ).await) })
    }
}

/// A fetch with [`READ`] as a [`Read`]. Our own limiter's refusal
/// (`limited_until`) is a skip. A `STALE` answer, which a [`READ`] fetch
/// gets only by joining another caller's call to Riot that failed, is a
/// failure: Riot did not answer.
pub fn read_of(out: Result<FetchResult, FetchError>) -> Read {
    match out {
        Ok(r) if r.x_cache == XCache::Stale => {
            Read::Failed("Riot did not answer; only the cached copy was served".into())
        }
        Ok(r) => Read::Body(r.body),
        Err(e) if e.limited_until.is_some() => Read::Skipped,
        Err(e) => Read::Failed(format!("{}: {}", e.api.code.as_str(), e.api.message)),
    }
}

/// Where bodies with an incident are written, and the key to redact from them.
#[derive(Debug, Clone)]
pub struct Recordings {
    pub dir: PathBuf,
    pub key: String,
}

impl Recordings {
    /// `<dir>/<platform>-incident-<id>.json`.
    pub fn path(&self, platform: Platform, id: i64) -> PathBuf {
        self.dir.join(format!("{}-incident-{id}.json", platform.as_str()))
    }

    /// Write `body` once for each incident id it lists that has no file yet.
    /// Failures are logged: a recording is never worth a round.
    async fn record(&self, platform: Platform, text: &str, body: &serde_json::Value) {
        let ids = incident_ids(body);
        if ids.is_empty() {
            return;
        }
        let Some(redacted) = redact(text, &self.key) else {
            tracing::warn!(
                platform = platform.as_str(),
                "a status body with an incident holds an RGAPI- string; not recorded"
            );
            return;
        };
        for id in ids {
            let path = self.path(platform, id);
            if let Err(e) = write_once(&path, redacted.as_bytes()).await {
                tracing::warn!(path = %path.display(), error = %e, "status recording not written");
            }
        }
    }
}

/// The `id` of each `incidents[]` entry, an integer in the portal's
/// `StatusDto` (ADR-217); an entry without one is left out.
pub fn incident_ids(body: &serde_json::Value) -> BTreeSet<i64> {
    body.get("incidents")
        .and_then(serde_json::Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|i| i.get("id").and_then(serde_json::Value::as_i64))
                .collect()
        })
        .unwrap_or_default()
}

/// `text` with every copy of `key` replaced by [`REDACTED`]; `None` when an
/// `RGAPI-` string is left all the same, so nothing key-shaped is written.
pub fn redact(text: &str, key: &str) -> Option<String> {
    let out = if key.is_empty() {
        text.to_string()
    } else {
        text.replace(key, REDACTED)
    };
    (!out.contains("RGAPI-")).then_some(out)
}

/// Write `bytes` to `path` unless it exists, through a temporary file so a
/// file that exists is whole.
async fn write_once(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    if tokio::fs::try_exists(path).await? {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    tokio::fs::write(&tmp, bytes).await?;
    tokio::fs::rename(&tmp, path).await
}

/// One platform's stored row (`platform_status`), as SITE-14 reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformStatus {
    pub platform: Platform,
    /// The last 200's body, JSON as Riot sent it.
    pub body: Option<String>,
    /// Unix ms of the last 200.
    pub fetched_at_ms: Option<i64>,
    /// Unix ms of the last failure; it failed after the body was read when
    /// it is later than `fetched_at_ms`.
    pub failed_at_ms: Option<i64>,
    /// Why the last failure failed.
    pub error: Option<String>,
}

/// Every stored row, in [`Platform::ALL`] order. A platform polled before
/// and no longer listed keeps its row; a row naming no platform is left out.
pub async fn stored(db: &Db) -> Result<Vec<PlatformStatus>, DbError> {
    let rows = db
        .read(|c| {
            let mut stmt =
                c.prepare("SELECT platform, body, fetched_at, failed_at, error FROM platform_status")?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((r.get::<_, String>(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, DbError>(rows)
        })
        .await?;
    let mut out: Vec<PlatformStatus> = rows
        .into_iter()
        .filter_map(|(platform, body, fetched_at_ms, failed_at_ms, error)| {
            Some(PlatformStatus {
                platform: platform.parse().ok()?,
                body,
                fetched_at_ms,
                failed_at_ms,
                error,
            })
        })
        .collect();
    out.sort_by_key(|s| Platform::ALL.iter().position(|p| *p == s.platform));
    Ok(out)
}

/// What a round stores for one platform.
enum Row {
    Body { text: String, at_ms: i64 },
    Failed { error: String, at_ms: i64 },
}

/// Store a round's rows in one write: a 200 replaces the body and
/// `fetched_at`, a failure `failed_at` and `error`; each keeps the other's.
fn store(c: &mut rusqlite::Connection, rows: &[(Platform, Row)]) -> Result<(), DbError> {
    let tx = c.transaction()?;
    {
        let mut body = tx.prepare(
            "INSERT INTO platform_status (platform, body, fetched_at) VALUES (?1, ?2, ?3)
             ON CONFLICT (platform) DO UPDATE SET body = excluded.body, fetched_at = excluded.fetched_at",
        )?;
        let mut failed = tx.prepare(
            "INSERT INTO platform_status (platform, failed_at, error) VALUES (?1, ?2, ?3)
             ON CONFLICT (platform) DO UPDATE SET failed_at = excluded.failed_at, error = excluded.error",
        )?;
        for (platform, row) in rows {
            match row {
                Row::Body { text, at_ms } => body.execute(params![platform.as_str(), text, at_ms])?,
                Row::Failed { error, at_ms } => failed.execute(params![platform.as_str(), at_ms, error])?,
            };
        }
    }
    tx.commit()?;
    Ok(())
}

/// Why the poller could not be built: its request does not resolve, which is
/// a bug in the endpoint table, never a runtime state.
#[derive(Debug, thiserror::Error)]
pub enum PollerError {
    #[error("the status poller's endpoint {METHOD} is not in the endpoint table")]
    NoEndpoint,
    #[error("the status poller's request on {platform} does not build: {source}")]
    Request {
        platform: Platform,
        #[source]
        source: RequestError,
    },
}

/// The poller: what it reads, with what, and where it stores it.
pub struct Poller {
    reader: Arc<dyn Reader>,
    db: Db,
    interval: Duration,
    /// Each platform's request, built once.
    platforms: Vec<(Platform, RiotRequest)>,
    recordings: Option<Recordings>,
}

impl std::fmt::Debug for Poller {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let platforms: Vec<&str> = self.platforms.iter().map(|(p, _)| p.as_str()).collect();
        f.debug_struct("Poller")
            .field("interval", &self.interval)
            .field("platforms", &platforms)
            .field("recordings", &self.recordings.as_ref().map(|r| &r.dir))
            .finish_non_exhaustive()
    }
}

impl Poller {
    /// The poller `serve` runs, from the config; `None` when
    /// `STATUS_POLL_PLATFORMS` is `none`.
    pub fn new(state: &AppState) -> Result<Option<Self>, PollerError> {
        let config = &state.config;
        if config.status_poll_platforms.is_empty() {
            return Ok(None);
        }
        let recordings = config.status_recordings_dir.clone().map(|dir| Recordings {
            dir,
            key: config.riot_api_key.expose().to_string(),
        });
        Self::with_reader(
            Arc::new(state.fetcher.clone()),
            state.db.clone(),
            &config.status_poll_platforms,
            Duration::from_secs(config.status_poll_interval_s.into()),
            recordings,
        )
        .map(Some)
    }

    /// A poller reading `platforms` through `reader`.
    pub fn with_reader(
        reader: Arc<dyn Reader>,
        db: Db,
        platforms: &[Platform],
        interval: Duration,
        recordings: Option<Recordings>,
    ) -> Result<Self, PollerError> {
        let endpoint = Endpoint::by_id(METHOD).ok_or(PollerError::NoEndpoint)?;
        let platforms = platforms
            .iter()
            .map(|&platform| {
                RiotRequest::new(endpoint, Target::Platform(platform), &[])
                    .map(|req| (platform, req))
                    .map_err(|source| PollerError::Request { platform, source })
            })
            .collect::<Result<_, _>>()?;
        Ok(Self {
            reader,
            db,
            interval,
            platforms,
            recordings,
        })
    }

    /// Run a round now and then every interval, until aborted. A round that
    /// runs long delays the next instead of bunching them up.
    pub fn spawn(self) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut every = tokio::time::interval(self.interval);
            every.set_missed_tick_behavior(MissedTickBehavior::Delay);
            loop {
                every.tick().await;
                self.round().await;
            }
        })
    }

    /// One round: every platform read at once, then their rows stored in
    /// one write. Returns what it did with each, in the configured order.
    pub async fn round(&self) -> Vec<(Platform, Polled)> {
        let reads = futures_util::future::join_all(self.platforms.iter().map(|(platform, req)| async {
            let read = self.reader.read(req.clone()).await;
            (*platform, read, crate::clock::Clock::now().unix_ms)
        }))
        .await;
        let mut rows = Vec::with_capacity(reads.len());
        let mut out = Vec::with_capacity(reads.len());
        for (platform, read, at_ms) in reads {
            let (polled, row) = match read {
                Read::Body(body) => match parse(&body) {
                    Ok((text, value)) => {
                        if let Some(r) = &self.recordings {
                            r.record(platform, &text, &value).await;
                        }
                        (Polled::Ok, Some(Row::Body { text, at_ms }))
                    }
                    Err(error) => (Polled::Failed, Some(Row::Failed { error, at_ms })),
                },
                Read::Failed(error) => (Polled::Failed, Some(Row::Failed { error, at_ms })),
                Read::Skipped => (Polled::Skipped, None),
            };
            match &row {
                Some(Row::Failed { error, .. }) => {
                    tracing::debug!(platform = platform.as_str(), %error, "status poll failed");
                }
                None => tracing::debug!(
                    platform = platform.as_str(),
                    "status poll skipped, no room in the rate limiter"
                ),
                Some(Row::Body { .. }) => {}
            }
            metrics::counter!(
                STATUS_POLLS_TOTAL,
                "platform" => platform.as_str(),
                "result" => polled.as_str()
            )
            .increment(1);
            if let Some(row) = row {
                rows.push((platform, row));
            }
            out.push((platform, polled));
        }
        if !rows.is_empty()
            && let Err(e) = self.db.write(move |c| store(c, &rows)).await
        {
            tracing::warn!(error = %e, "status poll results not stored");
        }
        out
    }
}

/// A 200's body as stored text and as JSON, or why it cannot be stored.
fn parse(body: &Bytes) -> Result<(String, serde_json::Value), String> {
    let text = std::str::from_utf8(body).map_err(|_| "Riot's 200 is not UTF-8".to_string())?;
    let value = serde_json::from_str(text).map_err(|e| format!("Riot's 200 is not JSON: {e}"))?;
    Ok((text.to_string(), value))
}

pub mod summary;

#[cfg(test)]
mod tests;
