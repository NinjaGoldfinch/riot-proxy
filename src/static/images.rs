//! Data Dragon images, mirrored on first request (DEV-05, ADR-076): a miss is
//! fetched from Riot's CDN, written beside the patch's JSON and served from
//! disk ever after. `/ddragon/*` needs no key, so a request may only name an
//! image the mirrored patch itself lists (`data.*.image.full`); anything else
//! is a 404 without a fetch.
//!
//! Layout: `DDRAGON_DIR/<version>/img/<kind>/<file>`, which is Data Dragon's
//! own `/cdn/<version>/img/<kind>/<file>`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::Deserialize;

use super::{Mirror, is_version};
use crate::jobs::ddragon::{DdragonError, write_atomic};

/// Image kinds served, and the data file listing each kind's images.
pub const IMAGE_KINDS: [(&str, &str); 4] = [
    ("champion", "champion"),
    ("profileicon", "profileicon"),
    ("item", "item"),
    ("spell", "summoner"),
];

pub(super) type Names = Mutex<HashMap<(String, &'static str), Arc<HashSet<String>>>>;
pub(super) type Filling = tokio::sync::Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>;

#[derive(Debug, thiserror::Error)]
pub enum ImageError {
    /// Not a mirrored patch, kind or listed image, or Riot has no such file.
    #[error("no such image")]
    NotFound,
    #[error(transparent)]
    Upstream(#[from] DdragonError),
}

#[derive(Deserialize)]
struct DataFile {
    #[serde(default)]
    data: HashMap<String, Entry>,
}

#[derive(Deserialize)]
struct Entry {
    image: Option<Image>,
}

#[derive(Deserialize)]
struct Image {
    full: Option<String>,
}

/// A name that is a plain file name: `Aatrox.png`, never a path.
fn safe_name(file: &str) -> bool {
    file.ends_with(".png")
        && !file.starts_with('.')
        && file
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.')
}

/// Every `data.*.image.full` in a data file.
pub fn parse_names(bytes: &[u8]) -> HashSet<String> {
    let Ok(file) = serde_json::from_slice::<DataFile>(bytes) else {
        return HashSet::new();
    };
    file.data
        .into_values()
        .filter_map(|e| e.image?.full)
        .filter(|f| safe_name(f))
        .collect()
}

impl Mirror {
    /// An image's bytes: from disk, or fetched once and kept.
    pub async fn image(&self, version: &str, kind: &str, file: &str) -> Result<Vec<u8>, ImageError> {
        let data_file = IMAGE_KINDS
            .iter()
            .find(|(k, _)| *k == kind)
            .map(|(_, f)| *f)
            .ok_or(ImageError::NotFound)?;
        if !is_version(version) || !safe_name(file) {
            return Err(ImageError::NotFound);
        }
        let path = self.dir().join(version).join("img").join(kind).join(file);
        if let Ok(bytes) = tokio::fs::read(&path).await {
            return Ok(bytes);
        }
        if !self.is_complete(version).await || !self.image_names(version, data_file).await.contains(file) {
            return Err(ImageError::NotFound);
        }

        // One fetch per path: a page asking for the same icon ten times at
        // once must not cost ten downloads.
        let gate = {
            let mut filling = self.filling.lock().await;
            Arc::clone(filling.entry(path.clone()).or_default())
        };
        let filled = async {
            let _one = gate.lock().await;
            if let Ok(bytes) = tokio::fs::read(&path).await {
                return Ok(bytes);
            }
            let cdn = self.cdn();
            let bytes = match cdn.image(&cdn.urls().image(version, kind, file)).await {
                Err(DdragonError::Status { status: 404, .. }) => return Err(ImageError::NotFound),
                other => other?,
            };
            if let Some(dir) = path.parent() {
                tokio::fs::create_dir_all(dir).await.map_err(DdragonError::from)?;
            }
            write_atomic(&path, &bytes).await.map_err(DdragonError::from)?;
            Ok(bytes)
        }
        .await;
        self.filling.lock().await.remove(&path);
        filled
    }

    /// The image names a patch's data file lists, parsed once per patch.
    async fn image_names(&self, version: &str, data_file: &'static str) -> Arc<HashSet<String>> {
        let key = (version.to_string(), data_file);
        if let Some(names) = self.image_names.lock().ok().and_then(|g| g.get(&key).cloned()) {
            return names;
        }
        let names = Arc::new(
            self.read(data_file, Some(version))
                .await
                .map(|b| parse_names(&b))
                .unwrap_or_default(),
        );
        if let Ok(mut g) = self.image_names.lock() {
            g.insert(key, Arc::clone(&names));
        }
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_come_from_image_full_and_must_be_plain_files() {
        let names = parse_names(
            br#"{"data": {
                "Aatrox": {"image": {"full": "Aatrox.png"}},
                "1": {"image": {"full": "1.png"}},
                "x": {"image": {"full": "../escape.png"}},
                "y": {"image": {"full": "a/b.png"}},
                "z": {}
            }}"#,
        );
        let mut got: Vec<_> = names.into_iter().collect();
        got.sort();
        assert_eq!(got, ["1.png", "Aatrox.png"]);
        assert!(parse_names(b"not json").is_empty());
    }

    #[test]
    fn only_plain_png_names_are_safe() {
        for ok in ["Aatrox.png", "1001.png", "SummonerFlash.png", "Kai_Sa.png"] {
            assert!(safe_name(ok), "{ok}");
        }
        for bad in [
            "..png",
            ".png",
            "a/b.png",
            "a\\b.png",
            "x.json",
            "x.png.tmp",
            "%2e.png",
        ] {
            assert!(!safe_name(bad), "{bad}");
        }
    }
}
