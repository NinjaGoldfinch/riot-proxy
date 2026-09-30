//! Champion id → display name from the mirror's `champion.json` (v1
//! `static/champions.ts`). Parsed once per mirrored patch and kept in memory:
//! the file is hundreds of champions, and a sync moves the patch at most once
//! per `DDRAGON_SYNC_S`.

use std::collections::HashMap;
use std::sync::Arc;

use serde::Deserialize;

use super::Mirror;

#[derive(Deserialize)]
struct ChampionFile {
    #[serde(default)]
    data: HashMap<String, Entry>,
}

#[derive(Deserialize)]
struct Entry {
    key: Option<String>,
    name: Option<String>,
}

/// `champion.json`'s `data.<Id>.{key, name}` as id → name. Entries without a
/// numeric key or a name are skipped.
pub fn parse(bytes: &[u8]) -> HashMap<i64, String> {
    let Ok(file) = serde_json::from_slice::<ChampionFile>(bytes) else {
        return HashMap::new();
    };
    file.data
        .into_values()
        .filter_map(|e| Some((e.key?.parse().ok()?, e.name?)))
        .collect()
}

impl Mirror {
    /// Names for a batch of champion ids. Ids the mirror does not know (not
    /// synced yet, or unknown to this patch) are absent, never guessed (v1).
    pub async fn champion_names(&self, ids: &[i64]) -> HashMap<i64, String> {
        let Some(version) = self.current_version().await else {
            return HashMap::new();
        };
        let by_id = self.champions_for(&version).await;
        ids.iter()
            .filter_map(|id| by_id.get(id).map(|n| (*id, n.clone())))
            .collect()
    }

    async fn champions_for(&self, version: &str) -> Arc<HashMap<i64, String>> {
        if let Some((v, map)) = self.champions.lock().ok().and_then(|g| g.clone())
            && v == version
        {
            return map;
        }
        let map = Arc::new(
            self.read("champion", Some(version))
                .await
                .map(|b| parse(&b))
                .unwrap_or_default(),
        );
        if let Ok(mut g) = self.champions.lock() {
            *g = Some((version.to_string(), Arc::clone(&map)));
        }
        map
    }
}
