//! The topic hub: one `broadcast::Sender` per topic, created on first subscribe
//! and dropped when its last subscriber leaves, so `player:<puuid>` topics cost
//! nothing once nobody follows that player. `firehose` receives every event.
//!
//! Event frames are serialised once by the publisher and shared: a
//! [`Utf8Bytes`] clone is a reference-count bump, whatever the fan-out.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use axum::extract::ws::Utf8Bytes;
use tokio::sync::{broadcast, watch};

use crate::metrics::WS_CONNECTIONS;
use crate::ws::protocol::{FIREHOSE, Topic};

/// design/06: capacity 256 per topic. A socket that falls further behind gets a
/// `resync` frame instead of the events it missed.
pub const CAPACITY: usize = 256;

/// Why the server closed a socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Closing {
    Open,
    /// Graceful shutdown: 1001 (v1).
    Shutdown,
    /// The consumer's key was revoked (ADR-045).
    Revoked,
}

#[derive(Debug)]
struct Conn {
    consumer_id: String,
    close: watch::Sender<Closing>,
}

#[derive(Debug)]
struct Inner {
    capacity: usize,
    topics: Mutex<HashMap<String, broadcast::Sender<Utf8Bytes>>>,
    conns: Mutex<HashMap<u64, Conn>>,
    next_conn: AtomicU64,
    subscriptions: AtomicU64,
    /// Events published, by name (v1 `eventCounts`, for the metrics snapshot).
    published: Mutex<HashMap<String, u64>>,
}

/// Cheap to clone; clones share the topics and connections.
#[derive(Debug, Clone)]
pub struct Hub {
    inner: Arc<Inner>,
}

impl Default for Hub {
    fn default() -> Self {
        Self::with_capacity(CAPACITY)
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A registered socket. Dropping it unregisters the connection.
#[derive(Debug)]
pub struct Registration {
    hub: Hub,
    id: u64,
    pub close: watch::Receiver<Closing>,
}

impl Drop for Registration {
    fn drop(&mut self) {
        let mut conns = lock(&self.hub.inner.conns);
        conns.remove(&self.id);
        #[allow(clippy::cast_precision_loss)]
        metrics::gauge!(WS_CONNECTIONS).set(conns.len() as f64);
    }
}

impl Hub {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Inner {
                capacity: capacity.max(1),
                topics: Mutex::new(HashMap::new()),
                conns: Mutex::new(HashMap::new()),
                next_conn: AtomicU64::new(0),
                subscriptions: AtomicU64::new(0),
                published: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Send a pre-serialised event frame to `topic` and to the firehose.
    /// Returns how many receivers it reached; publishing to a topic nobody holds
    /// is free (v1: losing an event must never fail its producer).
    pub fn publish(&self, topic: &Topic, event: &str, frame: Utf8Bytes) -> usize {
        *lock(&self.inner.published).entry(event.to_string()).or_default() += 1;
        let topics = lock(&self.inner.topics);
        let mut reached = 0;
        if let Some(tx) = topics.get(topic.as_str()) {
            reached += tx.send(frame.clone()).unwrap_or(0);
        }
        if !topic.is_firehose()
            && let Some(tx) = topics.get(FIREHOSE)
        {
            reached += tx.send(frame).unwrap_or(0);
        }
        reached
    }

    /// A receiver for `topic`, creating the channel if this is its first subscriber.
    pub fn subscribe(&self, topic: &Topic) -> broadcast::Receiver<Utf8Bytes> {
        self.inner.subscriptions.fetch_add(1, Ordering::Relaxed);
        let mut topics = lock(&self.inner.topics);
        topics
            .entry(topic.as_str().to_string())
            .or_insert_with(|| broadcast::channel(self.inner.capacity).0)
            .subscribe()
    }

    /// Call after dropping a receiver: forgets the channel once nobody holds it.
    pub fn release(&self, topic: &Topic) {
        self.inner.subscriptions.fetch_sub(1, Ordering::Relaxed);
        let mut topics = lock(&self.inner.topics);
        if topics
            .get(topic.as_str())
            .is_some_and(|tx| tx.receiver_count() == 0)
        {
            topics.remove(topic.as_str());
        }
    }

    /// Sockets holding `topic`. `metrics` ticks only while this is non-zero
    /// (v1: "costs nothing while nobody watches").
    pub fn receivers(&self, topic: &Topic) -> usize {
        lock(&self.inner.topics)
            .get(topic.as_str())
            .map_or(0, broadcast::Sender::receiver_count)
    }

    /// Channels currently alive (one per topic somebody holds).
    pub fn live_topics(&self) -> usize {
        lock(&self.inner.topics).len()
    }

    pub fn connections(&self) -> usize {
        lock(&self.inner.conns).len()
    }

    /// Topics held, summed over sockets (v1 `subscriptionCount`).
    pub fn subscriptions(&self) -> u64 {
        self.inner.subscriptions.load(Ordering::Relaxed)
    }

    /// Events published since start, by name (v1 `eventCounts`).
    pub fn event_counts(&self) -> HashMap<String, u64> {
        lock(&self.inner.published).clone()
    }

    /// Register a socket for `consumer_id`.
    pub fn register(&self, consumer_id: &str) -> Registration {
        let id = self.inner.next_conn.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = watch::channel(Closing::Open);
        let mut conns = lock(&self.inner.conns);
        conns.insert(
            id,
            Conn {
                consumer_id: consumer_id.to_string(),
                close: tx,
            },
        );
        #[allow(clippy::cast_precision_loss)]
        metrics::gauge!(WS_CONNECTIONS).set(conns.len() as f64);
        Registration {
            hub: self.clone(),
            id,
            close: rx,
        }
    }

    /// Close every socket of a consumer whose key was revoked (ADR-045). v1 left
    /// them open for as long as the client kept them. Returns how many.
    pub fn close_consumer(&self, consumer_id: &str) -> usize {
        let conns = lock(&self.inner.conns);
        conns
            .values()
            .filter(|c| c.consumer_id == consumer_id)
            .map(|c| c.close.send(Closing::Revoked))
            .filter(Result::is_ok)
            .count()
    }

    /// Close every socket with 1001 (graceful shutdown, v1 `stop`).
    pub fn shutdown(&self) {
        for c in lock(&self.inner.conns).values() {
            let _ = c.close.send(Closing::Shutdown);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::ws::protocol::{PATCH, Topic};

    const P: &str = "NkQRxdiN3U3pEek5MWbWgaxzG_hpH5imJ9Ttch8ql5KM7D6p6Bh-Hbbvn6UoFVdGUBBIvcnEJv72qw";

    #[tokio::test]
    async fn topics_are_isolated_and_the_firehose_sees_everything() {
        let hub = Hub::new();
        let (patch, player, fire) = (Topic::named(PATCH), Topic::player(P), Topic::named(FIREHOSE));
        let mut a = hub.subscribe(&patch);
        let mut b = hub.subscribe(&player);
        let mut f = hub.subscribe(&fire);
        assert_eq!(hub.publish(&patch, "patch.new", "p1".into()), 2);
        assert_eq!(hub.publish(&player, "game.started", "g1".into()), 2);
        assert_eq!(a.recv().await.unwrap().as_str(), "p1");
        assert!(a.try_recv().is_err(), "patch never sees player events");
        assert_eq!(b.recv().await.unwrap().as_str(), "g1");
        assert_eq!(f.recv().await.unwrap().as_str(), "p1");
        assert_eq!(f.recv().await.unwrap().as_str(), "g1");
        assert_eq!(hub.event_counts()["patch.new"], 1);
    }

    #[tokio::test]
    async fn a_topic_lives_only_while_held() {
        let hub = Hub::new();
        let t = Topic::player(P);
        assert_eq!(
            hub.publish(&t, "game.started", "x".into()),
            0,
            "nobody listening is free"
        );
        let r1 = hub.subscribe(&t);
        let r2 = hub.subscribe(&t);
        assert_eq!(
            (hub.receivers(&t), hub.live_topics(), hub.subscriptions()),
            (2, 1, 2)
        );
        drop(r1);
        hub.release(&t);
        assert_eq!(hub.live_topics(), 1);
        drop(r2);
        hub.release(&t);
        assert_eq!(
            (hub.receivers(&t), hub.live_topics(), hub.subscriptions()),
            (0, 0, 0)
        );
    }

    #[tokio::test]
    async fn lagging_receivers_learn_how_much_they_missed() {
        let hub = Hub::with_capacity(4);
        let t = Topic::named(PATCH);
        let mut r = hub.subscribe(&t);
        for i in 0..10 {
            hub.publish(&t, "patch.new", i.to_string().into());
        }
        assert!(matches!(
            r.recv().await,
            Err(broadcast::error::RecvError::Lagged(6))
        ));
        assert_eq!(r.recv().await.unwrap().as_str(), "6");
    }

    #[tokio::test]
    async fn revoking_closes_only_that_consumers_sockets() {
        let hub = Hub::new();
        let a1 = hub.register("a");
        let a2 = hub.register("a");
        let b = hub.register("b");
        assert_eq!(hub.connections(), 3);
        assert_eq!(hub.close_consumer("a"), 2);
        assert_eq!(*a1.close.borrow(), Closing::Revoked);
        assert_eq!(*a2.close.borrow(), Closing::Revoked);
        assert_eq!(*b.close.borrow(), Closing::Open);
        hub.shutdown();
        assert_eq!(*b.close.borrow(), Closing::Shutdown);
        drop((a1, a2));
        assert_eq!(hub.connections(), 1);
    }
}
