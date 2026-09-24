//! Response cache: canonical keys (P3-01), in-memory L1 (P3-02) and SQLite L2
//! (P3-03). docs/design/04 §Cache tiers.

pub mod keys;
pub mod l1;
pub mod l2;

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;

use self::l1::{CacheEntry, L1, Lookup};
use self::l2::L2Writer;
use crate::riot::endpoints::{Endpoint, Ttls};

/// L1 in front, with write-behind to L2 for endpoints that persist.
#[derive(Debug)]
pub struct ResponseCache {
    pub l1: L1,
    l2: Option<L2Writer>,
}

impl ResponseCache {
    pub fn new(l1: L1, l2: Option<L2Writer>) -> Self {
        Self { l1, l2 }
    }

    pub async fn get(&self, key: &str) -> Lookup {
        self.l1.get(key).await
    }

    /// Cache a 200. Persisted to L2 when the endpoint is in an L2 tier.
    pub async fn put(
        &self,
        key: &str,
        endpoint: &Endpoint,
        body: Bytes,
        ttls: &Ttls,
    ) -> Option<Arc<CacheEntry>> {
        let entry = self.l1.put(key, body, ttls).await?;
        self.persist(key, endpoint, &entry);
        Some(entry)
    }

    /// Cache a 404 for `ttl`. Persisted to L2 like a positive entry would be.
    pub async fn put_negative(&self, key: &str, endpoint: &Endpoint, ttl: Duration) -> Arc<CacheEntry> {
        let entry = self.l1.put_negative(key, ttl).await;
        self.persist(key, endpoint, &entry);
        entry
    }

    fn persist(&self, key: &str, endpoint: &Endpoint, entry: &CacheEntry) {
        if endpoint.persist_l2
            && let Some(l2) = &self.l2
        {
            l2.enqueue(key, entry);
        }
    }

    /// Flush pending L2 writes (shutdown).
    pub async fn shutdown(&self) {
        if let Some(l2) = &self.l2 {
            l2.shutdown().await;
        }
    }
}
