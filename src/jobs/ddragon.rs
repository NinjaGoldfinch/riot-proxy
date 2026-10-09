//! `ddragon:sync` (design/06 §Job catalogue; v1 `syncDdragon`): read
//! `versions.json`, and when its newest patch is not mirrored yet, download that
//! patch's data files into `DDRAGON_DIR/<version>/` and publish `patch.new`.
//! Riot's queue table is refreshed on every run, patch or not: Riot adds queue
//! ids when a game mode ships, which is not a patch event (v1 #115).
//!
//! Data Dragon is not rate limited and never goes through the limiter.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::events::{self, Event};
use crate::jobs::scheduler::{Handler, Job, JobError};
use crate::r#static::{DATA_FILES, META_DIR, Mirror, VERSIONS_FILE};
use crate::ws::Hub;

/// Data Dragon's host (v1 `DDRAGON_BASE`).
pub const DDRAGON_BASE: &str = "https://ddragon.leagueoflegends.com";
/// Riot's queue table: another host, no version (v1 `QUEUES_URL`).
pub const QUEUES_URL: &str = "https://static.developer.riotgames.com/docs/lol/queues.json";

/// v1's `headersTimeout` / `bodyTimeout`.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const TIMEOUT: Duration = Duration::from_secs(60);
/// Data Dragon's icons are a few KB; anything this big is not one.
const MAX_IMAGE: usize = 1024 * 1024;
const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

#[derive(Debug, thiserror::Error)]
pub enum DdragonError {
    #[error("Data Dragon request failed for {url}: {source}")]
    Http { url: String, source: reqwest::Error },
    #[error("Data Dragon responded {status} for {url}")]
    Status { url: String, status: u16 },
    #[error("Data Dragon returned invalid JSON for {url}")]
    Json { url: String },
    #[error("Data Dragon returned something other than a PNG for {url}")]
    Image { url: String },
    #[error("Data Dragon returned an empty version list")]
    NoVersions,
    #[error("writing the mirror: {0}")]
    Io(#[from] std::io::Error),
    #[error("building the Data Dragon client: {0}")]
    Build(reqwest::Error),
}

/// Where Data Dragon lives: Riot's hosts in production, a mock in tests.
#[derive(Debug, Clone)]
pub struct CdnUrls {
    /// Serves `/api/versions.json` and `/cdn/<version>/data/<locale>/<file>.json`.
    pub ddragon: String,
    pub queues: String,
}

impl Default for CdnUrls {
    fn default() -> Self {
        Self {
            ddragon: DDRAGON_BASE.into(),
            queues: QUEUES_URL.into(),
        }
    }
}

impl CdnUrls {
    /// Everything on one mock server, at Riot's paths.
    pub fn mock(base: &str) -> Self {
        let base = base.trim_end_matches('/');
        Self {
            ddragon: base.into(),
            queues: format!("{base}/docs/lol/queues.json"),
        }
    }

    pub fn versions(&self) -> String {
        format!("{}/api/versions.json", self.ddragon)
    }

    pub fn data(&self, version: &str, locale: &str, file: &str) -> String {
        format!("{}/cdn/{version}/data/{locale}/{file}.json", self.ddragon)
    }

    /// `/cdn/<version>/img/<kind>/<file>`: Data Dragon's image path, `file`
    /// being a data file's `image.full` (`Aatrox.png`, `1001.png`).
    pub fn image(&self, version: &str, kind: &str, file: &str) -> String {
        format!("{}/cdn/{version}/img/{kind}/{file}", self.ddragon)
    }

    /// `/cdn/img/<icon>`: a rune icon, unversioned, `icon` being a
    /// runesReforged.json `icon` (`perk-images/Styles/…`).
    pub fn rune_image(&self, icon: &str) -> String {
        format!("{}/cdn/img/{icon}", self.ddragon)
    }
}

/// The Data Dragon HTTP client: plain GETs, no key, no limiter.
pub struct Cdn {
    http: reqwest::Client,
    urls: CdnUrls,
    locale: String,
}

impl Cdn {
    pub fn new(config: &Config, urls: CdnUrls) -> Result<Self, DdragonError> {
        let http = reqwest::Client::builder()
            .user_agent(&config.riot_user_agent)
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(TIMEOUT)
            .tls_certs_only(crate::riot::client::webpki_roots())
            .build()
            .map_err(DdragonError::Build)?;
        Ok(Self {
            http,
            urls,
            locale: config.ddragon_locale.clone(),
        })
    }

    pub fn urls(&self) -> &CdnUrls {
        &self.urls
    }

    /// A JSON document's bytes, checked to be JSON but otherwise untouched.
    async fn get(&self, url: &str) -> Result<Vec<u8>, DdragonError> {
        let http = |source| DdragonError::Http {
            url: url.into(),
            source,
        };
        let res = self
            .http
            .get(url)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(http)?;
        let status = res.status();
        if !status.is_success() {
            return Err(DdragonError::Status {
                url: url.into(),
                status: status.as_u16(),
            });
        }
        let bytes = res.bytes().await.map_err(http)?;
        if serde_json::from_slice::<&serde_json::value::RawValue>(&bytes).is_err() {
            return Err(DdragonError::Json { url: url.into() });
        }
        Ok(bytes.to_vec())
    }

    /// An image's bytes, checked to be a PNG and no bigger than `MAX_IMAGE`.
    pub async fn image(&self, url: &str) -> Result<Vec<u8>, DdragonError> {
        let http = |source| DdragonError::Http {
            url: url.into(),
            source,
        };
        let res = self.http.get(url).send().await.map_err(http)?;
        let status = res.status();
        if !status.is_success() {
            return Err(DdragonError::Status {
                url: url.into(),
                status: status.as_u16(),
            });
        }
        let bytes = res.bytes().await.map_err(http)?;
        if bytes.len() > MAX_IMAGE || !bytes.starts_with(PNG_MAGIC) {
            return Err(DdragonError::Image { url: url.into() });
        }
        Ok(bytes.to_vec())
    }

    /// One patch's data file (`item`, `runesReforged`, …) in the configured
    /// locale, checked to be JSON.
    pub async fn data(&self, version: &str, file: &str) -> Result<Vec<u8>, DdragonError> {
        self.get(&self.urls.data(version, &self.locale, file)).await
    }

    /// Riot's patch list, newest first, as bytes and parsed.
    pub async fn versions(&self) -> Result<(Vec<u8>, Vec<String>), DdragonError> {
        let url = self.urls.versions();
        let bytes = self.get(&url).await?;
        let list = serde_json::from_slice(&bytes).map_err(|_| DdragonError::Json { url })?;
        Ok((bytes, list))
    }
}

/// What one run did (v1 `SyncResult`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncResult {
    pub version: String,
    /// A patch was downloaded this run.
    pub changed: bool,
    /// Data files written for the patch.
    pub files: Vec<String>,
    /// Un-versioned files refreshed.
    pub meta: Vec<String>,
}

/// Write `bytes` beside `path` and rename it into place, so a reader never
/// sees half a file.
pub(crate) async fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    tokio::fs::write(&tmp, bytes).await?;
    tokio::fs::rename(&tmp, path).await
}

/// One sync. Idempotent: a run that finds the newest patch mirrored fetches
/// only the version list and the queue table, unless `force`.
pub async fn sync(mirror: &Mirror, force: bool) -> Result<SyncResult, DdragonError> {
    let _one = mirror.syncing.lock().await;
    let cdn = mirror.cdn();
    let (versions_bytes, versions) = cdn.versions().await?;
    let version = versions.first().cloned().ok_or(DdragonError::NoVersions)?;
    if !crate::r#static::is_version(&version) {
        // It becomes a directory name.
        return Err(DdragonError::Json {
            url: cdn.urls().versions(),
        });
    }

    // Before the version check: these have no version, so "nothing changed"
    // says nothing about them (v1).
    let meta = sync_meta(mirror).await;

    if !force && mirror.is_complete(&version).await {
        mirror.set_current(&version);
        return Ok(SyncResult {
            version,
            changed: false,
            files: vec![],
            meta,
        });
    }

    let dir = mirror.dir().join(&version);
    tokio::fs::create_dir_all(&dir).await?;
    tracing::info!(%version, "syncing Data Dragon");
    let mut files = vec![];
    for file in DATA_FILES {
        let url = cdn.urls().data(&version, &cdn.locale, file);
        match cdn.get(&url).await {
            Ok(bytes) => {
                write_atomic(&mirror.path(&version, file), &bytes).await?;
                files.push(file.to_string());
            }
            // Riot has dropped data files over time; one missing must not lose
            // the whole patch (v1).
            Err(e) => tracing::warn!(error = %e, file, %version, "Data Dragon file unavailable; skipping"),
        }
    }
    // Last: its presence is what marks the patch mirrored.
    write_atomic(&mirror.path(&version, VERSIONS_FILE), &versions_bytes).await?;
    mirror.set_current(&version);
    tracing::info!(%version, files = files.len(), meta = meta.len(), "Data Dragon sync complete");
    Ok(SyncResult {
        version,
        changed: true,
        files,
        meta,
    })
}

/// Refresh the queue table. A failure keeps what is on disk: it is a labelling
/// convenience and must not cost a patch its data (v1 `syncMeta`).
async fn sync_meta(mirror: &Mirror) -> Vec<String> {
    let url = mirror.cdn().urls().queues.clone();
    let written = async {
        tokio::fs::create_dir_all(mirror.dir().join(META_DIR)).await?;
        let bytes = mirror.cdn().get(&url).await?;
        write_atomic(&mirror.path(META_DIR, "queues"), &bytes).await?;
        Ok::<_, DdragonError>(())
    }
    .await;
    match written {
        Ok(()) => vec!["queues".to_string()],
        Err(e) => {
            tracing::warn!(error = %e, %url, "queue table unavailable; keeping what is on disk");
            vec![]
        }
    }
}

/// `ddragon:sync`'s payload (v1): `force` re-downloads a mirrored patch.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct SyncPayload {
    #[serde(default)]
    pub force: bool,
}

pub struct DdragonSync {
    pub mirror: Arc<Mirror>,
    pub hub: Hub,
}

impl DdragonSync {
    pub async fn run(&self, job: &Job) -> Result<(), JobError> {
        let payload: SyncPayload = job.payload()?;
        let result = sync(&self.mirror, payload.force)
            .await
            .map_err(|e| JobError::Retry(e.to_string()))?;
        if result.changed {
            events::publish(
                &self.hub,
                &Event::PatchNew {
                    version: result.version,
                },
            );
        }
        Ok(())
    }
}

pub struct DdragonSyncHandler(pub Arc<DdragonSync>);

impl Handler for DdragonSyncHandler {
    fn run<'a>(&'a self, job: &'a Job) -> BoxFuture<'a, Result<(), JobError>> {
        Box::pin(self.0.run(job))
    }
}
