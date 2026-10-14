//! The analytics routes' responses, kept in memory per aggregate generation
//! (PERF-03, ADR-169).
//!
//! The analytics tables change only when `aggregate:analytics` commits, so
//! between rebuilds a route's 200 and `ETag` are constants. Each is kept here
//! under its route and normalised query ([`Key`]) until:
//!
//! - the rebuild that changes the tables calls [`Analytics::clear`], after its
//!   last commit and before it announces `analytics.updated`. A clear bumps the
//!   generation, and a miss that began before it does not store what it read
//!   (`insert` checks the generation it started at);
//! - the Data Dragon version moves: an entry holds the version its names and
//!   tag were built with, and a lookup at another version is a miss;
//! - the LRU cap (`ANALYTICS_CACHE_ENTRIES`, `0` stores nothing) evicts it.
//!
//! Concurrent misses for one key share one computation ([`SingleFlight`],
//! keyed by generation too, so a request after a clear never joins a read
//! that began before it). Nothing here survives a restart; the next rebuild's
//! [`Analytics::warm`] fills what sites open first.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use axum::body::Bytes;

use super::{Reads, Request, Route, compute};
use crate::singleflight::{SingleFlight, WorkFailed};

/// A route and its normalised query: `route|key scope|championId|part|…`,
/// `~` for a part the query left open, as the `ETag` material writes it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Key(pub(super) String);

impl Key {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One route's 200: the bytes, their validator and the Data Dragon version
/// they were built at.
#[derive(Debug, PartialEq, Eq)]
pub struct Entry {
    pub body: Bytes,
    pub etag: String,
    pub version: Option<String>,
}

/// An analytics read failed; the cause is logged where it happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("an analytics read failed")]
pub struct Failed;

impl From<WorkFailed> for Failed {
    fn from(_: WorkFailed) -> Self {
        Self
    }
}

#[derive(Debug, Default)]
struct Lru {
    entries: HashMap<Key, (Arc<Entry>, u64)>,
    /// Bumped by every get and insert: an entry's last use.
    tick: u64,
    generation: u64,
}

/// The LRU itself: a map and a use counter under one lock, never held across
/// an `.await`. A full map evicts its least recently used entry on insert, a
/// scan of at most `ANALYTICS_CACHE_ENTRIES`, and only on a miss.
#[derive(Debug)]
pub struct Responses {
    inner: Mutex<Lru>,
    cap: usize,
}

impl Responses {
    pub fn new(cap: usize) -> Self {
        Self {
            inner: Mutex::default(),
            cap,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Lru> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The entry for `key` if it was built at Data Dragon `version`.
    pub fn get(&self, key: &Key, version: Option<&str>) -> Option<Arc<Entry>> {
        let mut lru = self.lock();
        lru.tick += 1;
        let tick = lru.tick;
        let (entry, used) = lru.entries.get_mut(key)?;
        if entry.version.as_deref() != version {
            return None;
        }
        *used = tick;
        Some(Arc::clone(entry))
    }

    /// The generation a computation starts at, for [`Responses::insert`].
    pub fn generation(&self) -> u64 {
        self.lock().generation
    }

    /// Store `entry` unless the cache is off or was cleared since `generation`.
    pub fn insert(&self, key: Key, entry: Arc<Entry>, generation: u64) -> bool {
        if self.cap == 0 {
            return false;
        }
        let mut lru = self.lock();
        if lru.generation != generation {
            return false;
        }
        if lru.entries.len() >= self.cap && !lru.entries.contains_key(&key) {
            let oldest = lru
                .entries
                .iter()
                .min_by_key(|(_, (_, used))| *used)
                .map(|(k, _)| k.clone());
            if let Some(k) = oldest {
                lru.entries.remove(&k);
            }
        }
        lru.tick += 1;
        let tick = lru.tick;
        lru.entries.insert(key, (entry, tick));
        true
    }

    /// Drop everything and start a new generation.
    pub fn clear(&self) {
        let mut lru = self.lock();
        lru.generation += 1;
        lru.entries.clear();
    }

    pub fn len(&self) -> usize {
        self.lock().entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The stored keys, oldest use first.
    pub fn keys(&self) -> Vec<Key> {
        let lru = self.lock();
        let mut keys: Vec<_> = lru
            .entries
            .iter()
            .map(|(k, (_, used))| (*used, k.clone()))
            .collect();
        keys.sort();
        keys.into_iter().map(|(_, k)| k).collect()
    }
}

/// What the analytics routes, the rebuild's warm and `/dev/reset` share:
/// the reads, the responses and the in-flight computations. One per process.
pub struct Analytics {
    pub reads: Reads,
    pub responses: Responses,
    flight: SingleFlight<(u64, Key), Arc<Entry>, Failed>,
    /// `LADDER_QUEUES`: the queues [`Analytics::warm`] fills.
    queues: Vec<String>,
}

impl std::fmt::Debug for Analytics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Analytics")
            .field("entries", &self.responses.len())
            .finish_non_exhaustive()
    }
}

/// The slices the sites open first, as each sends them (design/11's showcase
/// home; ninjagoldfinch.lol's champions page). `{q}` is a ladder queue and
/// `{p}` the platform just rebuilt.
const WARM: [(Route, &str); 5] = [
    (Route::Champions, "queue={q}&patch=all&minGames=0&limit=500"),
    (Route::Champions, "queue={q}&minGames=0&limit=500"),
    (Route::Champions, "platform={p}&queue={q}&patch=all&limit=500"),
    (Route::Patches, "queue={q}"),
    (Route::Patches, "platform={p}&queue={q}"),
];

impl Analytics {
    /// `cap` is `ANALYTICS_CACHE_ENTRIES`; `0` stores nothing.
    pub fn new(reads: Reads, cap: u32, queues: Vec<String>) -> Self {
        Self {
            reads,
            responses: Responses::new(cap as usize),
            flight: SingleFlight::new(),
            queues,
        }
    }

    /// Drop every response: the analytics tables changed.
    pub fn clear(&self) {
        self.responses.clear();
    }

    /// The entry for `q`: from memory, else computed (once for every
    /// concurrent caller) and stored. `true` when it came from memory.
    pub async fn get_or_compute(self: &Arc<Self>, q: Request) -> Result<(Arc<Entry>, bool), Failed> {
        let key = q.key(&self.reads.key_scope);
        let version = self.reads.mirror.current_version().await;
        if let Some(e) = self.responses.get(&key, version.as_deref()) {
            tracing::debug!(key = key.as_str(), "analytics response from memory");
            return Ok((e, true));
        }
        let generation = self.responses.generation();
        let this = Arc::clone(self);
        let stored = key.clone();
        let stored_key = key.as_str().to_string();
        let flight = self
            .flight
            .run((generation, key), move || async move {
                let before = this.reads.db.reads();
                let entry = Arc::new(compute(&this.reads, &q).await?);
                let kept = this.responses.insert(stored, Arc::clone(&entry), generation);
                tracing::debug!(
                    key = stored_key,
                    // Every Db::read in the meantime, this computation's and any other's.
                    reads = this.reads.db.reads() - before,
                    kept,
                    "analytics response read"
                );
                Ok(entry)
            })
            .await;
        flight.value.map(|e| (e, false))
    }

    /// Fill the slices sites open first after a rebuild of `platform`'s
    /// ladder, for every ladder queue: the clear dropped them all. Returns
    /// how many were computed; a failure is logged and skipped.
    pub async fn warm(self: &Arc<Self>, platform: &str) -> usize {
        let mut done = 0;
        for queue in &self.queues {
            for (route, query) in WARM {
                let raw = query.replace("{q}", queue).replace("{p}", platform);
                let params: HashMap<String, String> = raw
                    .split('&')
                    .filter_map(|kv| kv.split_once('='))
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect();
                let q = match Request::parse(route, None, &params, &self.reads) {
                    Ok(q) => q,
                    Err(e) => {
                        tracing::warn!(error = %e.message, %raw, "analytics warm: query rejected");
                        continue;
                    }
                };
                match self.get_or_compute(q).await {
                    Ok(_) => done += 1,
                    Err(e) => tracing::warn!(error = %e, %raw, "analytics warm failed"),
                }
            }
        }
        done
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(v: Option<&str>) -> Arc<Entry> {
        Arc::new(Entry {
            body: Bytes::from_static(b"{}"),
            etag: "W/\"x\"".into(),
            version: v.map(str::to_string),
        })
    }

    fn k(s: &str) -> Key {
        Key(s.to_string())
    }

    /// Design §6: at cap 2, A, B, C evicts A; A again evicts B; C is a hit.
    #[test]
    fn the_cap_evicts_the_least_recently_used() {
        let r = Responses::new(2);
        let g = r.generation();
        let v = Some("16.19.1");
        assert!(r.insert(k("A"), entry(v), g));
        assert!(r.insert(k("B"), entry(v), g));
        assert!(r.insert(k("C"), entry(v), g));
        assert!(r.get(&k("A"), v).is_none(), "A was the oldest");
        assert_eq!(r.keys(), vec![k("B"), k("C")]);
        assert!(r.insert(k("A"), entry(v), g));
        assert!(r.get(&k("B"), v).is_none(), "B was the oldest");
        assert!(r.get(&k("C"), v).is_some());
        assert_eq!(r.keys(), vec![k("A"), k("C")]);
    }

    #[test]
    fn a_get_keeps_an_entry() {
        let r = Responses::new(2);
        let (g, v) = (r.generation(), None);
        r.insert(k("A"), entry(v), g);
        r.insert(k("B"), entry(v), g);
        assert!(r.get(&k("A"), v).is_some());
        r.insert(k("C"), entry(v), g);
        assert_eq!(r.keys(), vec![k("A"), k("C")], "B, not A, was the oldest");
    }

    #[test]
    fn cap_zero_stores_nothing() {
        let r = Responses::new(0);
        assert!(!r.insert(k("A"), entry(None), r.generation()));
        assert!(r.is_empty());
    }

    /// F3: a miss that began before a clear doesn't store what it read.
    #[test]
    fn a_miss_from_before_a_clear_is_not_stored() {
        let r = Responses::new(4);
        let g = r.generation();
        r.insert(k("A"), entry(None), g);
        r.clear();
        assert!(r.is_empty());
        assert!(!r.insert(k("B"), entry(None), g), "stale generation");
        assert!(r.get(&k("B"), None).is_none());
        assert!(r.insert(k("B"), entry(None), r.generation()));
    }

    /// F12: an entry built at another Data Dragon version is a miss.
    #[test]
    fn another_data_dragon_version_is_a_miss() {
        let r = Responses::new(4);
        r.insert(k("A"), entry(Some("16.19.1")), r.generation());
        assert!(r.get(&k("A"), Some("16.20.1")).is_none());
        assert!(r.get(&k("A"), None).is_none());
        assert!(r.get(&k("A"), Some("16.19.1")).is_some());
    }
}
