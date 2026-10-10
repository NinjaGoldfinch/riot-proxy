//! `match_builds`: what each player bought and levelled, a pure derivation of
//! an archived match-v5 timeline and the mirrored `item.json` (BLD-01,
//! docs/IMPLEMENTATION.md §Post-release — BLD, docs/design/04 §Schema).
//!
//! The definitions are BLD's, one constant or function each:
//! - purchase order: a player's `ITEM_PURCHASED` events in time order, minus
//!   undone purchases ([`purchases`]); sales and their undos are ignored;
//! - finished item, boots: [`ItemCatalog`];
//! - starter: every purchase before [`BUILD_STARTER_MS`] that isn't a trinket;
//! - skill order: [`skill_order`], with R for Udyr (BLD-06);
//! - Viego's possession: [`possessed`] level-ups aren't counted (BLD-05).
//!
//! The extraction never panics on a strange body: an item id the catalogue
//! doesn't know is dropped, not guessed, a participant with no puuid is
//! skipped, and a timeline that doesn't parse gives no rows. Bump
//! [`BUILDS_VERSION`] whenever the output of [`extract`] changes;
//! `builds:extract` rewrites older rows.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

/// The version of [`extract`] that wrote a row (`match_builds.builds_version`).
pub const BUILDS_VERSION: i64 = 3;

/// Purchases before this many ms into the game are the starter (BLD). In the
/// match the definitions were checked on, starters were bought at 7–29 s and
/// no player's next purchase came before 89 s.
pub const BUILD_STARTER_MS: i64 = 60_000;

/// A finished item costs at least this much in total (BLD).
const FINISHED_MIN_GOLD: i64 = 1500;
/// Base Boots, which aren't a player's boots choice (BLD).
const BASE_BOOTS: i64 = 1001;
/// At most this many finished items are kept: an inventory's six slots.
const ITEMS_KEPT: usize = 6;
/// Level-ups kept in `skills`, as `"QWEQQRQ…"` (BLD).
const SKILLS_KEPT: usize = 15;
/// `SKILL_LEVEL_UP.skillSlot` 1–4.
const SKILL_KEYS: [char; 4] = ['Q', 'W', 'E', 'R'];
/// Viego, whose possession logs the possessed champion's ranks as his own
/// level-ups (BLD-05, ADR-131).
const VIEGO: i64 = 234;
/// Udyr, whose R is an ordinary ability ranked like Q, W and E, so his skill
/// order ranks all four (BLD-06, ADR-132).
const UDYR: i64 = 77;

/// One `match_builds` row, minus the `match_id` and `key_scope` the caller owns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildFact {
    pub puuid: String,
    /// Purchases before [`BUILD_STARTER_MS`], trinkets left out, sorted, with
    /// duplicates kept (two potions are two entries).
    pub starter: Vec<i64>,
    /// The first purchase that is boots by [`ItemCatalog::is_boots`].
    pub boots: Option<i64>,
    /// Finished items in purchase order, at most six.
    pub items: Vec<i64>,
    /// The first 15 normal level-ups, `"EQWWW…"`.
    pub skills: String,
    /// Q, W and E in the order they were maxed (`"QWE"`), and R too for
    /// Udyr (`"RWEQ"`); `None` with no level-ups.
    pub skill_order: Option<String>,
}

impl BuildFact {
    /// `starter` as stored: a JSON array.
    pub fn starter_json(&self) -> String {
        serde_json::to_string(&self.starter).unwrap_or_else(|_| "[]".into())
    }

    /// `items` as stored: a JSON array.
    pub fn items_json(&self) -> String {
        serde_json::to_string(&self.items).unwrap_or_else(|_| "[]".into())
    }
}

/// What a build needs to know about each item in Data Dragon's `item.json`.
#[derive(Debug, Clone, Default)]
pub struct ItemCatalog {
    items: HashMap<i64, Item>,
}

#[derive(Debug, Clone)]
struct Item {
    finished: bool,
    boots_tag: bool,
    trinket: bool,
    from: Vec<i64>,
}

#[derive(Deserialize)]
struct ItemJson {
    #[serde(default)]
    data: HashMap<String, ItemEntry>,
}

#[derive(Deserialize)]
struct ItemEntry {
    #[serde(default)]
    gold: Gold,
    #[serde(default)]
    into: Vec<String>,
    #[serde(default)]
    from: Vec<String>,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Deserialize, Default)]
struct Gold {
    #[serde(default)]
    total: i64,
    #[serde(default)]
    purchasable: bool,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CatalogError {
    #[error("not an item.json: {0}")]
    NotItemJson(String),
}

impl ItemCatalog {
    /// The catalogue of a Data Dragon `item.json`. An entry whose id isn't a
    /// number is left out.
    pub fn from_item_json(bytes: &[u8]) -> Result<Self, CatalogError> {
        let doc: ItemJson =
            serde_json::from_slice(bytes).map_err(|e| CatalogError::NotItemJson(e.to_string()))?;
        let items = doc
            .data
            .into_iter()
            .filter_map(|(id, e)| {
                let id = id.parse::<i64>().ok()?;
                let tagged = |t: &str| e.tags.iter().any(|x| x == t);
                // BLD: purchasable, builds into nothing, ≥ 1500 gold, and not
                // boots, a consumable or a trinket.
                let finished = e.gold.purchasable
                    && e.into.is_empty()
                    && e.gold.total >= FINISHED_MIN_GOLD
                    && !tagged("Boots")
                    && !tagged("Consumable")
                    && !tagged("Trinket");
                let item = Item {
                    finished,
                    boots_tag: tagged("Boots"),
                    trinket: tagged("Trinket"),
                    from: e.from.iter().filter_map(|f| f.parse().ok()).collect(),
                };
                Some((id, item))
            })
            .collect();
        Ok(Self { items })
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn knows(&self, id: i64) -> bool {
        self.items.contains_key(&id)
    }

    pub fn is_finished(&self, id: i64) -> bool {
        self.items.get(&id).is_some_and(|i| i.finished)
    }

    pub fn is_trinket(&self, id: i64) -> bool {
        self.items.get(&id).is_some_and(|i| i.trinket)
    }

    /// Tagged `Boots` other than base Boots, or built from such an item. The
    /// second rule catches upgrades with no `Boots` tag, such as Gunmetal
    /// Greaves (3172, from Berserker's Greaves).
    pub fn is_boots(&self, id: i64) -> bool {
        let tagged = |id: i64| id != BASE_BOOTS && self.items.get(&id).is_some_and(|i| i.boots_tag);
        tagged(id)
            || self
                .items
                .get(&id)
                .is_some_and(|i| i.from.iter().any(|&f| tagged(f)))
    }
}

#[derive(Deserialize)]
struct Timeline {
    info: TimelineInfo,
}

#[derive(Deserialize)]
struct TimelineInfo {
    #[serde(default)]
    participants: Vec<TimelineParticipant>,
    #[serde(default)]
    frames: Vec<Frame>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TimelineParticipant {
    participant_id: Option<i64>,
    puuid: Option<String>,
}

#[derive(Deserialize)]
struct Frame {
    #[serde(default)]
    events: Vec<Event>,
}

/// The fields of the events a build reads; every other event type parses to
/// one with nothing set and is ignored.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Event {
    #[serde(rename = "type", default)]
    kind: String,
    participant_id: Option<i64>,
    #[serde(default)]
    timestamp: i64,
    item_id: Option<i64>,
    before_id: Option<i64>,
    after_id: Option<i64>,
    skill_slot: Option<i64>,
    level_up_type: Option<String>,
}

/// A purchase: `(itemId, timestamp)`.
type Purchase = (i64, i64);

/// One player's purchase order: `ITEM_PURCHASED` in time order, minus undone
/// purchases. An `ITEM_UNDO` with `beforeId = X, afterId = 0` removes the
/// player's latest earlier purchase of X; one with `beforeId = 0` undoes a
/// sale, which a build ignores like the sale itself.
fn purchases(events: &[&Event]) -> Vec<Purchase> {
    let mut out: Vec<Purchase> = Vec::new();
    for e in events {
        match e.kind.as_str() {
            "ITEM_PURCHASED" => {
                if let Some(id) = e.item_id {
                    out.push((id, e.timestamp));
                }
            }
            "ITEM_UNDO" => {
                if let (Some(before), Some(0)) = (e.before_id, e.after_id)
                    && before != 0
                    && let Some(k) = out.iter().rposition(|&(id, t)| id == before && t <= e.timestamp)
                {
                    out.remove(k);
                }
            }
            _ => {}
        }
    }
    out
}

/// BLD-05: for Viego, a `SKILL_LEVEL_UP` at the same timestamp as one or more
/// of his own `ITEM_DESTROYED` events is a possessed champion's rank, logged
/// as Viego's: the possession swaps his inventory in that millisecond. Only
/// Viego: other champions' level-ups do coincide with a destroyed item now
/// and then, and those points are real.
fn possessed(champion: Option<i64>, events: &[&Event]) -> HashSet<i64> {
    if champion != Some(VIEGO) {
        return HashSet::new();
    }
    events
        .iter()
        .filter(|e| e.kind == "ITEM_DESTROYED")
        .map(|e| e.timestamp)
        .collect()
}

/// The skills a skill order ranks: Q, W and E, or all four for Udyr, whose R
/// is ranked like the others (BLD-06). For everyone else R is the ultimate,
/// taken when it can be.
fn ranked_skills(champion: Option<i64>) -> usize {
    if champion == Some(UDYR) { 4 } else { 3 }
}

/// The first `ranked` skills (`skillSlot` 1–`ranked`) ranked by points at the
/// end, a tie going to the skill that reached that count first; a skill never
/// levelled ranks after the others, in Q W E R order. `None` with no level-ups
/// at all.
fn skill_order(slots: &[usize], ranked: usize) -> Option<String> {
    if slots.is_empty() {
        return None;
    }
    let mut points = [0usize; 4];
    // The level-up (its index) at which each skill reached its final count.
    let mut reached = [usize::MAX; 4];
    for (k, &s) in slots.iter().enumerate() {
        if s < ranked
            && let Some(p) = points.get_mut(s)
        {
            *p += 1;
            reached[s] = k;
        }
    }
    let mut order: Vec<usize> = (0..ranked.min(SKILL_KEYS.len())).collect();
    order.sort_by_key(|&s| (std::cmp::Reverse(points[s]), reached[s]));
    Some(order.iter().map(|&s| SKILL_KEYS[s]).collect())
}

/// The build facts for every player in a timeline, in Riot's participant
/// order. A participant with no puuid, `participantId: 0` (not a player) and
/// a puuid seen twice are skipped; a timeline that doesn't parse gives `[]`.
///
/// `champions` is the match's puuid → championId (its `match_facts`); the
/// timeline doesn't name champions. A player missing from it is treated as
/// any champion but Viego.
pub fn extract(timeline: &[u8], catalog: &ItemCatalog, champions: &HashMap<String, i64>) -> Vec<BuildFact> {
    let Ok(t) = serde_json::from_slice::<Timeline>(timeline) else {
        return Vec::new();
    };
    let mut events: Vec<&Event> = t.info.frames.iter().flat_map(|f| f.events.iter()).collect();
    // Frames are in time order already; a stable sort keeps same-ms events in Riot's order.
    events.sort_by_key(|e| e.timestamp);
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(t.info.participants.len());
    for p in &t.info.participants {
        let (Some(pid), Some(puuid)) = (p.participant_id, p.puuid.as_deref()) else {
            continue;
        };
        if pid == 0 || puuid.is_empty() || !seen.insert(puuid) {
            continue;
        }
        let mine: Vec<&Event> = events
            .iter()
            .copied()
            .filter(|e| e.participant_id == Some(pid))
            .collect();
        let bought: Vec<Purchase> = purchases(&mine)
            .into_iter()
            .filter(|&(id, _)| catalog.knows(id))
            .collect();
        let mut starter: Vec<i64> = bought
            .iter()
            .filter(|&&(id, t)| t < BUILD_STARTER_MS && !catalog.is_trinket(id))
            .map(|&(id, _)| id)
            .collect();
        starter.sort_unstable();
        let boots = bought.iter().map(|&(id, _)| id).find(|&id| catalog.is_boots(id));
        let items = bought
            .iter()
            .map(|&(id, _)| id)
            .filter(|&id| catalog.is_finished(id))
            .take(ITEMS_KEPT)
            .collect();
        let champion = champions.get(puuid).copied();
        let possession = possessed(champion, &mine);
        let slots: Vec<usize> = mine
            .iter()
            .filter(|e| e.kind == "SKILL_LEVEL_UP" && e.level_up_type.as_deref() == Some("NORMAL"))
            .filter(|e| !possession.contains(&e.timestamp))
            .filter_map(|e| {
                e.skill_slot
                    .and_then(|s| usize::try_from(s - 1).ok())
                    .filter(|&s| s < SKILL_KEYS.len())
            })
            .collect();
        let skills = slots.iter().take(SKILLS_KEPT).map(|&s| SKILL_KEYS[s]).collect();
        out.push(BuildFact {
            puuid: puuid.to_string(),
            starter,
            boots,
            items,
            skills,
            skill_order: skill_order(&slots, ranked_skills(champion)),
        });
    }
    out
}

/// Replace a match's `match_builds` rows, in the caller's transaction.
pub fn write(
    conn: &rusqlite::Connection,
    match_id: &str,
    key_scope: &str,
    rows: &[BuildFact],
) -> Result<(), rusqlite::Error> {
    conn.execute("DELETE FROM match_builds WHERE match_id = ?1", [match_id])?;
    let mut stmt = conn.prepare_cached(
        "INSERT INTO match_builds
           (match_id, key_scope, puuid, starter, boots, items, skills, skill_order, builds_version)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )?;
    for b in rows {
        stmt.execute(rusqlite::params![
            match_id,
            key_scope,
            b.puuid,
            b.starter_json(),
            b.boots,
            b.items_json(),
            b.skills,
            b.skill_order,
            BUILDS_VERSION,
        ])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
