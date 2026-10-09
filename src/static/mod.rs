//! The Data Dragon mirror on disk (design/07 §Option B; v1 `static/ddragon.ts`),
//! read side: which patch is current, and the mirrored files. `ddragon:sync`
//! (`jobs::ddragon`) writes it; `/v1/static/*` and `/ddragon/*` serve it.
//!
//! Data Dragon is not rate limited and never goes through the limiter (v1
//! §5.6). Images are mirrored on first request, not by the sync (`images`,
//! ADR-076; v1 left them on Riot's CDN).
//!
//! Layout: `DDRAGON_DIR/<version>/<file>.json` per patch, and
//! `DDRAGON_DIR/meta/queues.json` for the one un-versioned file. A patch
//! directory counts as mirrored once its `versions.json` exists: the sync
//! writes it last, so a sync that died half way is retried, not served.

pub mod champions;
pub mod images;

use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use crate::jobs::ddragon::Cdn;

/// Data files mirrored per patch (v1 `DATA_FILES`).
pub const DATA_FILES: [&str; 6] = [
    "champion",
    "item",
    "runesReforged",
    "summoner",
    "profileicon",
    "map",
];

/// `/v1/static/{file}`'s plural names for the same files (v1 `FILE_ALIASES`).
pub const FILE_ALIASES: [(&str, &str); 6] = [
    ("champions", "champion"),
    ("items", "item"),
    ("runes", "runesReforged"),
    ("summoner-spells", "summoner"),
    ("profile-icons", "profileicon"),
    ("maps", "map"),
];

/// The patch list, written into each patch directory last.
pub const VERSIONS_FILE: &str = "versions";

/// Where un-versioned static data lives, beside the patch directories (v1).
pub const META_DIR: &str = "meta";

/// Un-versioned files mirrored into [`META_DIR`] (v1 `META_FILES`).
pub const META_FILES: [&str; 1] = ["queues"];

/// A patch number and nothing else: `16.17.1`. Anything looser would let a
/// caller's `..` walk out of the mirror (v1 #51).
pub fn is_version(s: &str) -> bool {
    !s.is_empty()
        && s.split('.')
            .all(|seg| !seg.is_empty() && seg.bytes().all(|b| b.is_ascii_digit()))
}

/// Patches compared numerically, segment by segment, a missing segment being
/// zero (v1 `compareVersions`): string order puts `9.24.1` above `16.17.1`.
pub fn compare_versions(a: &str, b: &str) -> Ordering {
    let seg = |s: &str, i: usize| -> u64 { s.split('.').nth(i).and_then(|x| x.parse().ok()).unwrap_or(0) };
    let len = a.split('.').count().max(b.split('.').count());
    (0..len)
        .map(|i| seg(a, i).cmp(&seg(b, i)))
        .find(|o| o.is_ne())
        .unwrap_or(Ordering::Equal)
}

/// The Data Dragon version for a match's game build (SITE-03): the newest of
/// `versions` (Riot's `versions.json`, newest first) with the build's
/// `major.minor`, as the archive derives `matches.patch`. `16.20.824.8524` →
/// `16.20.1`. `None` for a malformed build or a patch the list lacks.
pub fn ddragon_version_for(build: &str, versions: &[String]) -> Option<String> {
    let mut parts = build.split('.');
    let (major, minor) = (parts.next()?, parts.next()?);
    if major.is_empty() || minor.is_empty() || !is_version(major) || !is_version(minor) {
        return None;
    }
    let prefix = format!("{major}.{minor}.");
    versions
        .iter()
        .filter(|v| v.starts_with(&prefix) && is_version(v))
        .max_by(|a, b| compare_versions(a, b))
        .cloned()
}

/// The data file a `/v1/static/{file}` name refers to, if any.
pub fn resolve_file(name: &str) -> Option<&'static str> {
    DATA_FILES.iter().find(|f| **f == name).copied().or_else(|| {
        FILE_ALIASES
            .iter()
            .find(|(alias, _)| *alias == name)
            .map(|(_, f)| *f)
    })
}

/// A patch's champion names, parsed once (`champions.rs`).
type ChampionNames = (String, Arc<HashMap<i64, String>>);

/// The mirror: its directory, the Data Dragon client that fills it, and the
/// current patch, remembered once found (v1 kept it in Redis).
pub struct Mirror {
    dir: PathBuf,
    cdn: Cdn,
    current: RwLock<Option<String>>,
    champions: Mutex<Option<ChampionNames>>,
    /// Riot's version list as the current patch mirrored it, parsed once.
    versions: Mutex<Option<(String, Arc<Vec<String>>)>>,
    /// The image file names each (patch, kind) allows (`images`).
    image_names: images::Names,
    /// One fill per image path at a time (`images`).
    filling: images::Filling,
    /// One sync at a time: the tick and an admin `force` can overlap.
    pub(crate) syncing: tokio::sync::Mutex<()>,
}

impl Mirror {
    pub fn new(dir: impl Into<PathBuf>, cdn: Cdn) -> Self {
        Self {
            dir: dir.into(),
            cdn,
            current: RwLock::new(None),
            champions: Mutex::new(None),
            versions: Mutex::new(None),
            image_names: Mutex::default(),
            filling: tokio::sync::Mutex::default(),
            syncing: tokio::sync::Mutex::new(()),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn cdn(&self) -> &Cdn {
        &self.cdn
    }

    /// The newest fully mirrored patch, if any. Found on disk once, then
    /// remembered; a sync moves it with [`Mirror::set_current`].
    pub async fn current_version(&self) -> Option<String> {
        if let Some(v) = self.current.read().ok().and_then(|g| g.clone()) {
            return Some(v);
        }
        let found = self.newest_on_disk().await?;
        if let Ok(mut g) = self.current.write() {
            *g = Some(found.clone());
        }
        Some(found)
    }

    pub(crate) fn set_current(&self, version: &str) {
        if let Ok(mut g) = self.current.write() {
            *g = Some(version.to_string());
        }
    }

    async fn newest_on_disk(&self) -> Option<String> {
        let mut entries = tokio::fs::read_dir(&self.dir).await.ok()?;
        let mut best: Option<String> = None;
        while let Ok(Some(entry)) = entries.next_entry().await {
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            // Patch directories only: `meta` sits beside them (v1).
            if !is_version(&name) || !self.is_complete(&name).await {
                continue;
            }
            if best.as_deref().is_none_or(|b| compare_versions(&name, b).is_gt()) {
                best = Some(name);
            }
        }
        best
    }

    /// Whether `version`'s sync finished (its `versions.json` exists).
    pub async fn is_complete(&self, version: &str) -> bool {
        is_version(version)
            && tokio::fs::try_exists(self.path(version, VERSIONS_FILE))
                .await
                .unwrap_or(false)
    }

    pub(crate) fn path(&self, dir: &str, file: &str) -> PathBuf {
        self.dir.join(dir).join(format!("{file}.json"))
    }

    /// A mirrored patch file's bytes: a [`DATA_FILES`] entry or `versions`,
    /// for `version` or the current patch. `None` when never synced.
    pub async fn read(&self, file: &str, version: Option<&str>) -> Option<Vec<u8>> {
        if !(DATA_FILES.contains(&file) || file == VERSIONS_FILE) {
            return None;
        }
        let version = match version {
            Some(v) => v.to_string(),
            None => self.current_version().await?,
        };
        // The second guard: the route validates too, but this is the one that
        // touches the filesystem (v1 `staticPath`).
        if !is_version(&version) {
            return None;
        }
        tokio::fs::read(self.path(&version, file)).await.ok()
    }

    /// Riot's version list from the current patch's `versions.json`, newest
    /// first; empty before the first sync.
    pub async fn versions(&self) -> Arc<Vec<String>> {
        let Some(current) = self.current_version().await else {
            return Arc::default();
        };
        if let Some((v, list)) = self.versions.lock().ok().and_then(|g| g.clone())
            && v == current
        {
            return list;
        }
        let list: Arc<Vec<String>> = Arc::new(
            self.read(VERSIONS_FILE, Some(&current))
                .await
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default(),
        );
        if let Ok(mut g) = self.versions.lock() {
            *g = Some((current, Arc::clone(&list)));
        }
        list
    }

    /// An un-versioned file's bytes, or `None` when it never synced.
    pub async fn read_meta(&self, file: &str) -> Option<Vec<u8>> {
        if !META_FILES.contains(&file) {
            return None;
        }
        tokio::fs::read(self.path(META_DIR, file)).await.ok()
    }
}

#[cfg(test)]
mod tests;
