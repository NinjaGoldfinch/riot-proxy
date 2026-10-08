//! Data Dragon images, mirrored on first request (DEV-05, ADR-076): a miss is
//! fetched from Riot's CDN, written beside the patch's JSON and served from
//! disk ever after. `/ddragon/*` needs no key, so a request may only name an
//! image the mirrored patch itself lists (`data.*.image.full`); anything else
//! is a 404 without a fetch.
//!
//! Layout: `DDRAGON_DIR/<version>/img/<kind>/<file>`, which is Data Dragon's
//! own `/cdn/<version>/img/<kind>/<file>`.
//!
//! Rune icons (DEV-14) are the exception: Data Dragon keeps them unversioned at
//! `/cdn/img/<icon>`, `icon` being runesReforged.json's `perk-images/…` path.
//! They are kept per patch all the same, at `DDRAGON_DIR/<version>/img/<icon>`,
//! and only an icon that patch's runesReforged.json lists is fetched.

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

/// The first segment of every rune icon path in runesReforged.json.
pub const RUNE_DIR: &str = "perk-images";

pub(super) type Names = Mutex<HashMap<(String, &'static str), Arc<HashSet<String>>>>;
/// Reads a data file's image names.
type Parser = fn(&[u8]) -> HashSet<String>;
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

/// runesReforged.json: styles, each with slots of runes, all with an `icon`.
#[derive(Deserialize)]
struct RuneStyle {
    icon: Option<String>,
    #[serde(default)]
    slots: Vec<RuneSlot>,
}

#[derive(Deserialize)]
struct RuneSlot {
    #[serde(default)]
    runes: Vec<Rune>,
}

#[derive(Deserialize)]
struct Rune {
    icon: Option<String>,
}

/// A relative path under [`RUNE_DIR`] whose every segment is a plain name:
/// `perk-images/Styles/Domination/Electrocute/Electrocute.png`.
fn safe_icon(path: &str) -> bool {
    let mut segments = path.split('/');
    segments.next() == Some(RUNE_DIR)
        && path.ends_with(".png")
        && segments.clone().count() > 0
        && segments.all(|s| {
            !s.is_empty()
                && !s.starts_with('.')
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.')
        })
}

/// Every style and rune `icon` in runesReforged.json.
pub fn parse_rune_icons(bytes: &[u8]) -> HashSet<String> {
    let Ok(styles) = serde_json::from_slice::<Vec<RuneStyle>>(bytes) else {
        return HashSet::new();
    };
    styles
        .into_iter()
        .flat_map(|style| {
            let runes = style.slots.into_iter().flat_map(|s| s.runes).map(|r| r.icon);
            std::iter::once(style.icon).chain(runes)
        })
        .flatten()
        .filter(|i| safe_icon(i))
        .collect()
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
        let url = self.cdn().urls().image(version, kind, file);
        self.fill(version, (data_file, parse_names), file, path, &url)
            .await
    }

    /// A rune or rune style icon, `icon` being its runesReforged.json path
    /// (`perk-images/Styles/…`): from disk, or fetched once and kept.
    pub async fn rune_image(&self, version: &str, icon: &str) -> Result<Vec<u8>, ImageError> {
        if !is_version(version) || !safe_icon(icon) {
            return Err(ImageError::NotFound);
        }
        let path = self.dir().join(version).join("img").join(icon);
        let url = self.cdn().urls().rune_image(icon);
        self.fill(version, ("runesReforged", parse_rune_icons), icon, path, &url)
            .await
    }

    /// `path`'s bytes, or `url`'s once `listed` names `name` for a complete patch.
    async fn fill(
        &self,
        version: &str,
        listed: (&'static str, Parser),
        name: &str,
        path: PathBuf,
        url: &str,
    ) -> Result<Vec<u8>, ImageError> {
        if let Ok(bytes) = tokio::fs::read(&path).await {
            return Ok(bytes);
        }
        if !self.is_complete(version).await || !self.image_names(version, listed).await.contains(name) {
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
            let bytes = match self.cdn().image(url).await {
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
    async fn image_names(
        &self,
        version: &str,
        (data_file, parse): (&'static str, Parser),
    ) -> Arc<HashSet<String>> {
        let key = (version.to_string(), data_file);
        if let Some(names) = self.image_names.lock().ok().and_then(|g| g.get(&key).cloned()) {
            return names;
        }
        let names = Arc::new(
            self.read(data_file, Some(version))
                .await
                .map(|b| parse(&b))
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
    fn rune_icons_come_from_styles_and_runes_and_must_stay_under_perk_images() {
        let icons = parse_rune_icons(
            br#"[{"id": 8100, "icon": "perk-images/Styles/7200_Domination.png", "slots": [
                {"runes": [{"id": 8112, "icon": "perk-images/Styles/Domination/Electrocute/Electrocute.png"},
                           {"id": 1, "icon": "perk-images/../../riot-proxy.db.png"},
                           {"id": 2, "icon": "img/champion/Ahri.png"},
                           {"id": 3}]}]}]"#,
        );
        let mut got: Vec<_> = icons.into_iter().collect();
        got.sort();
        assert_eq!(
            got,
            [
                "perk-images/Styles/7200_Domination.png",
                "perk-images/Styles/Domination/Electrocute/Electrocute.png"
            ]
        );
        assert!(parse_rune_icons(b"{}").is_empty());
        for bad in [
            "perk-images",
            "perk-images/",
            "perk-images/a.png/",
            "perk-images//a.png",
            "perk-images/.a.png",
            "perk-images/a b.png",
            "perk-images\\a.png",
            "/perk-images/a.png",
        ] {
            assert!(!safe_icon(bad), "{bad}");
        }
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
