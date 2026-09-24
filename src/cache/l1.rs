//! L1: the in-memory response cache (docs/design/04 §Cache tiers), a weight-bounded
//! `moka` cache of every TTL'd endpoint, negative entries included.
//!
//! Freshness is decided here from each entry's own `soft_expires`/`hard_expires`
//! (tokio time, so tests can pause it). moka's per-entry expiry is set to the hard
//! TTL only to reclaim memory; it runs on real time and never decides freshness.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use moka::Expiry;
use moka::future::Cache;
use tokio::time::Instant;

use crate::riot::endpoints::Ttls;

/// Per-entry bookkeeping counted against the weight budget, on top of key and body.
const ENTRY_OVERHEAD: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheEntry {
    /// 200 for a positive entry, 404 for a negative one.
    pub status: u16,
    /// Riot's bytes, untouched.
    pub body: Bytes,
    /// When this *content* was first seen. A refresh that returns byte-identical
    /// bytes keeps it, so `X-Cache-Age` reports content age, not fetch age (design 04).
    pub content_at: Instant,
    /// Fresh until.
    pub soft_expires: Instant,
    /// Servable as `STALE` until; gone after.
    pub hard_expires: Instant,
}

impl CacheEntry {
    pub fn is_negative(&self) -> bool {
        self.status == 404
    }

    /// Seconds since `content_at` (`X-Cache-Age`).
    pub fn age(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.content_at)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    /// Before `soft_expires`.
    Fresh(Arc<CacheEntry>),
    /// Between `soft_expires` and `hard_expires`: serve, and refresh in the background.
    Stale(Arc<CacheEntry>),
    Miss,
}

/// moka's memory-reclaim expiry: the entry's hard TTL at insert time.
struct HardTtl;

impl Expiry<String, Arc<CacheEntry>> for HardTtl {
    fn expire_after_create(
        &self,
        _key: &String,
        value: &Arc<CacheEntry>,
        _created_at: std::time::Instant,
    ) -> Option<Duration> {
        Some(value.hard_expires.saturating_duration_since(Instant::now()))
    }

    fn expire_after_update(
        &self,
        key: &String,
        value: &Arc<CacheEntry>,
        updated_at: std::time::Instant,
        _duration_until_expiry: Option<Duration>,
    ) -> Option<Duration> {
        self.expire_after_create(key, value, updated_at)
    }
}

#[derive(Clone)]
pub struct L1 {
    cache: Cache<String, Arc<CacheEntry>>,
}

impl std::fmt::Debug for L1 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("L1")
            .field("entries", &self.cache.entry_count())
            .finish()
    }
}

fn weight(key: &str, entry: &CacheEntry) -> u32 {
    u32::try_from(key.len() + entry.body.len() + ENTRY_OVERHEAD).unwrap_or(u32::MAX)
}

impl L1 {
    /// A cache holding at most `max_bytes` of keys, bodies and overhead (`CACHE_L1_MAX_MB`).
    pub fn new(max_bytes: u64) -> Self {
        let cache = Cache::builder()
            .max_capacity(max_bytes)
            .weigher(|k: &String, v: &Arc<CacheEntry>| weight(k, v))
            .expire_after(HardTtl)
            .build();
        Self { cache }
    }

    pub fn from_config(config: &crate::config::Config) -> Self {
        Self::new(u64::from(config.cache_l1_max_mb) * 1024 * 1024)
    }

    pub async fn get(&self, key: &str) -> Lookup {
        let Some(entry) = self.cache.get(key).await else {
            return Lookup::Miss;
        };
        let now = Instant::now();
        if now < entry.soft_expires {
            Lookup::Fresh(entry)
        } else if now < entry.hard_expires {
            Lookup::Stale(entry)
        } else {
            self.cache.invalidate(key).await;
            Lookup::Miss
        }
    }

    /// Store a positive response. `ttls.soft`/`hard` must be set (immutable endpoints
    /// go to the archive, not here); if not, nothing is cached.
    pub async fn put(&self, key: &str, body: Bytes, ttls: &Ttls) -> Option<Arc<CacheEntry>> {
        let (soft, hard) = (ttls.soft?, ttls.hard?);
        Some(self.insert(key, 200, body, soft, hard).await)
    }

    /// Store a negative (404) entry for `ttl`. Negatives are never served stale.
    pub async fn put_negative(&self, key: &str, ttl: Duration) -> Arc<CacheEntry> {
        self.insert(key, 404, Bytes::new(), ttl, ttl).await
    }

    /// Insert an entry that already carries its times (L2 warm, P3-03).
    pub async fn insert_entry(&self, key: &str, entry: CacheEntry) {
        self.cache.insert(key.to_string(), Arc::new(entry)).await;
    }

    async fn insert(
        &self,
        key: &str,
        status: u16,
        body: Bytes,
        soft: Duration,
        hard: Duration,
    ) -> Arc<CacheEntry> {
        let now = Instant::now();
        // Keep content_at when the new bytes are the old bytes (design 04).
        let content_at = match self.cache.get(key).await {
            Some(prev) if prev.status == status && prev.body == body => prev.content_at,
            _ => now,
        };
        let entry = Arc::new(CacheEntry {
            status,
            body,
            content_at,
            soft_expires: now + soft,
            hard_expires: now + hard.max(soft),
        });
        self.cache.insert(key.to_string(), Arc::clone(&entry)).await;
        entry
    }

    pub async fn invalidate(&self, key: &str) {
        self.cache.invalidate(key).await;
    }

    /// Remove every key matching `pred` (admin purge, P5-05). Returns how many.
    pub async fn invalidate_where(&self, pred: impl Fn(&str) -> bool) -> usize {
        let keys: Vec<Arc<String>> = self
            .cache
            .iter()
            .filter(|(k, _)| pred(k))
            .map(|(k, _)| k)
            .collect();
        for k in &keys {
            self.cache.invalidate(k.as_str()).await;
        }
        keys.len()
    }

    /// Apply moka's pending evictions (tests and metrics snapshots).
    pub async fn sync(&self) {
        self.cache.run_pending_tasks().await;
    }

    pub fn entry_count(&self) -> u64 {
        self.cache.entry_count()
    }

    pub fn weighted_size(&self) -> u64 {
        self.cache.weighted_size()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::advance;

    fn ttls(soft: u64, hard: u64) -> Ttls {
        Ttls {
            soft: Some(Duration::from_secs(soft)),
            hard: Some(Duration::from_secs(hard)),
            negative: None,
        }
    }

    fn l1() -> L1 {
        L1::new(16 * 1024 * 1024)
    }

    fn fresh(l: &Lookup) -> bool {
        matches!(l, Lookup::Fresh(_))
    }

    fn stale(l: &Lookup) -> bool {
        matches!(l, Lookup::Stale(_))
    }

    #[tokio::test(start_paused = true)]
    async fn fresh_then_stale_then_miss() {
        let l = l1();
        l.put("k", Bytes::from_static(b"{}"), &ttls(30, 120))
            .await
            .unwrap();
        assert!(fresh(&l.get("k").await));
        advance(Duration::from_secs(29)).await;
        assert!(fresh(&l.get("k").await));
        advance(Duration::from_secs(1)).await;
        assert!(stale(&l.get("k").await), "at exactly soft TTL");
        advance(Duration::from_secs(89)).await;
        assert!(stale(&l.get("k").await));
        advance(Duration::from_secs(1)).await;
        assert_eq!(l.get("k").await, Lookup::Miss, "at exactly hard TTL");
        assert_eq!(l.get("never").await, Lookup::Miss);
    }

    #[tokio::test(start_paused = true)]
    async fn negative_entries_are_never_stale() {
        let l = l1();
        let e = l.put_negative("k", Duration::from_secs(30)).await;
        assert!(e.is_negative());
        assert!(e.body.is_empty());
        match l.get("k").await {
            Lookup::Fresh(e) => assert_eq!(e.status, 404),
            other => panic!("{other:?}"),
        }
        advance(Duration::from_secs(30)).await;
        assert_eq!(l.get("k").await, Lookup::Miss);
    }

    #[tokio::test(start_paused = true)]
    async fn byte_identical_refresh_keeps_content_at() {
        let l = l1();
        let first = l
            .put("k", Bytes::from_static(b"{\"a\":1}"), &ttls(30, 120))
            .await
            .unwrap();
        advance(Duration::from_secs(45)).await;
        let same = l
            .put("k", Bytes::from_static(b"{\"a\":1}"), &ttls(30, 120))
            .await
            .unwrap();
        assert_eq!(
            same.content_at, first.content_at,
            "content unchanged: age keeps counting"
        );
        assert_eq!(same.age(Instant::now()), Duration::from_secs(45));
        assert_eq!(
            same.soft_expires,
            Instant::now() + Duration::from_secs(30),
            "but freshness restarts"
        );

        advance(Duration::from_secs(5)).await;
        let changed = l
            .put("k", Bytes::from_static(b"{\"a\":2}"), &ttls(30, 120))
            .await
            .unwrap();
        assert_eq!(changed.content_at, Instant::now(), "new content resets the age");
    }

    #[tokio::test(start_paused = true)]
    async fn a_negative_after_a_positive_resets_content_at() {
        let l = l1();
        l.put("k", Bytes::from_static(b""), &ttls(30, 120)).await.unwrap();
        advance(Duration::from_secs(10)).await;
        let neg = l.put_negative("k", Duration::from_secs(30)).await;
        assert_eq!(
            neg.content_at,
            Instant::now(),
            "an empty 200 is not the same content as a 404"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn immutable_ttls_are_not_cached_here() {
        let l = l1();
        let none = Ttls {
            soft: None,
            hard: None,
            negative: None,
        };
        assert!(l.put("k", Bytes::from_static(b"x"), &none).await.is_none());
        assert_eq!(l.get("k").await, Lookup::Miss);
    }

    #[tokio::test]
    async fn evicts_by_weight() {
        let l = L1::new(1024 * 1024);
        let body = Bytes::from(vec![b'x'; 200 * 1024]);
        for i in 0..10 {
            l.put(&format!("k{i}"), body.clone(), &ttls(3600, 3600))
                .await
                .unwrap();
        }
        l.sync().await;
        assert!(l.weighted_size() <= 1024 * 1024, "{} bytes", l.weighted_size());
        assert!(
            l.entry_count() < 10 && l.entry_count() >= 1,
            "{} entries",
            l.entry_count()
        );
    }

    #[tokio::test]
    async fn an_entry_larger_than_the_budget_is_not_kept() {
        let l = L1::new(64 * 1024);
        l.put("big", Bytes::from(vec![0u8; 128 * 1024]), &ttls(60, 60))
            .await
            .unwrap();
        l.sync().await;
        assert_eq!(l.get("big").await, Lookup::Miss);
    }

    #[tokio::test(start_paused = true)]
    async fn invalidate_where_removes_matching_keys() {
        let l = l1();
        for k in [
            "s:summoner.byPuuid:h:A",
            "s:summoner.byPuuid:h:B",
            "s:league.entriesByPuuid:h:A",
        ] {
            l.put(k, Bytes::from_static(b"{}"), &ttls(60, 60)).await.unwrap();
        }
        assert_eq!(l.invalidate_where(|k| k.ends_with(":A")).await, 2);
        assert_eq!(l.get("s:summoner.byPuuid:h:A").await, Lookup::Miss);
        assert!(fresh(&l.get("s:summoner.byPuuid:h:B").await));
    }
}
