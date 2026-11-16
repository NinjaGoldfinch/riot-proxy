//! The permanent store for immutable Riot data (docs/design/04): matches and
//! their facts; analytics follow in P7.

pub mod analytics;
pub mod builds;
pub mod facts;
pub mod matches;
pub mod objects;
pub mod offload;
pub mod pack;
pub mod player;
pub mod pool;
pub mod ranks;
pub mod recovery;
pub mod tiers;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use bytes::Bytes;
use futures_util::future::BoxFuture;

use crate::cache::keys::KeyScope;
use crate::clock::Clock;
use crate::db::{Db, DbError};
use crate::fetcher::{Archive, Stored};
use crate::jobs::archive::queue_missing_half_on;
use crate::jobs::offload::Pending;
use crate::jobs::{Queue, priority};
use crate::riot::client::RiotRequest;

/// The fetcher's [`Archive`] over SQLite. A failing archive never fails a
/// request: reads fall through to Riot and writes are logged (v1 did the same).
#[derive(Debug, Clone)]
pub struct SqliteArchive {
    db: Db,
    /// Whose PUUIDs the fetched bodies carry, for `match_facts`.
    scope: KeyScope,
    /// Where the other half of a stored match or timeline is queued (TL-01).
    halves: Option<Halves>,
    /// `TIER_LATE_STAMP_DAYS`: how far back a league lookup places a
    /// player's `UNKNOWN` tier stamps (THR-02).
    late_stamp_days: u32,
    /// Counts stored bytes towards the next pack and queues the offload when
    /// one is full (OBJ-03); `None` without a bucket.
    pending: Option<Arc<Pending>>,
    /// Bodies handed to the write-behind and not yet stored, by archive key,
    /// so [`Archive::get`] serves them meanwhile (AVL-02), each with the
    /// handle that says when its write has landed (AVL-11).
    unstored: Arc<Mutex<Bodies>>,
}

type Bodies = HashMap<String, (Bytes, Stored)>;

#[derive(Debug, Clone)]
struct Halves {
    queue: Queue,
    /// `ARCHIVE_TIMELINES`: a stored match queues its timeline.
    want_timeline: bool,
}

impl SqliteArchive {
    /// Every fetched match and timeline is stored: both are immutable, and a
    /// timeline is stored even before its match (TL-01). `ARCHIVE_TIMELINES`
    /// decides whether archive jobs fetch timelines (ADR-085).
    pub fn new(db: Db, scope: KeyScope) -> Self {
        Self {
            db,
            scope,
            halves: None,
            late_stamp_days: tiers::LATE_STAMP_DAYS,
            pending: None,
            unstored: Arc::default(),
        }
    }

    /// Count every stored body towards the offload's next pack (OBJ-03).
    #[must_use]
    pub fn count_unpacked(mut self, pending: Arc<Pending>) -> Self {
        self.pending = Some(pending);
        self
    }

    /// `TIER_LATE_STAMP_DAYS` (THR-02); 0 places no earlier games.
    #[must_use]
    pub fn late_stamp_days(mut self, days: u32) -> Self {
        self.late_stamp_days = days;
        self
    }

    /// Queue the other half of whatever is stored, at the top priority (TL-01,
    /// ADR-118): a match's timeline when `want_timeline`, and a timeline's
    /// match always. A job fetches it, so a page of matches costs the caller no
    /// more of its rate limit than before.
    #[must_use]
    pub fn queue_missing_halves(mut self, queue: Queue, want_timeline: bool) -> Self {
        self.halves = Some(Halves { queue, want_timeline });
        self
    }

    async fn queue_missing_half(&self, match_id: String) {
        let Some(Halves { queue, want_timeline }) = &self.halves else {
            return;
        };
        let (want_timeline, now) = (*want_timeline, Clock::now().unix_ms);
        let queued = self
            .db
            .write(move |c| {
                queue_missing_half_on(c, &match_id, want_timeline, priority::INTERACTIVE, now)
                    .map_err(DbError::from)
            })
            .await;
        match queued {
            Ok(true) => queue.wake(),
            Ok(false) => {}
            Err(e) => tracing::warn!(error = %e, "could not queue a match's missing half"),
        }
    }
}

impl SqliteArchive {
    /// [`Archive::put`] for a match or timeline, [`Archive::observe`] for the rest.
    async fn write(&self, req: &RiotRequest, body: Bytes) {
        if req.endpoint.immutable {
            self.put(req, body).await;
        } else {
            self.observe(req, body).await;
        }
    }
}

enum Kind {
    Match,
    Timeline,
}

impl Kind {
    /// The body's key among [`SqliteArchive::unstored`].
    fn key(&self, id: &str) -> String {
        match self {
            Self::Match => format!("match:{id}"),
            Self::Timeline => format!("timeline:{id}"),
        }
    }
}

/// A body among the unstored ones until its detached write finishes or is
/// dropped. A later body under the same key is left alone.
struct Unstored {
    map: Arc<Mutex<Bodies>>,
    key: String,
    body: Bytes,
}

impl Unstored {
    fn hold(map: &Arc<Mutex<Bodies>>, key: String, body: Bytes, stored: Stored) -> Self {
        lock(map).insert(key.clone(), (body.clone(), stored));
        Self {
            map: Arc::clone(map),
            key,
            body,
        }
    }
}

impl Drop for Unstored {
    fn drop(&mut self) {
        let mut map = lock(&self.map);
        if map
            .get(&self.key)
            .is_some_and(|(b, _)| b.as_ptr() == self.body.as_ptr())
        {
            map.remove(&self.key);
        }
    }
}

fn lock(map: &Mutex<Bodies>) -> std::sync::MutexGuard<'_, Bodies> {
    map.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A write the write-behind had no room for, until someone makes it: dropped
/// unmade, it is logged and counted as the write-behind drops any other.
struct Unmade {
    db: Db,
    what: &'static str,
    bytes: usize,
    made: bool,
}

impl Unmade {
    /// The write was made after all.
    fn made(mut self) {
        self.made = true;
    }
}

impl Drop for Unmade {
    fn drop(&mut self) {
        if !self.made {
            self.db.behind_dropped(self.what, self.bytes);
        }
    }
}

/// Which archive table a request belongs to, and its match id.
fn target(req: &RiotRequest) -> Option<(Kind, &str)> {
    let kind = match req.endpoint.id {
        "match.byId" => Kind::Match,
        "match.timeline" => Kind::Timeline,
        _ => return None,
    };
    Some((kind, req.params.first()?.as_str()))
}

impl Archive for SqliteArchive {
    fn get(&self, req: &RiotRequest) -> BoxFuture<'_, Option<Bytes>> {
        let Some((kind, id)) = target(req) else {
            return Box::pin(async { None });
        };
        if let Some((body, _)) = lock(&self.unstored).get(&kind.key(id)).cloned() {
            return Box::pin(async move { Some(body) });
        }
        let id = id.to_string();
        Box::pin(async move {
            let found = match kind {
                Kind::Match => matches::get(&self.db, &id).await,
                Kind::Timeline => matches::get_timeline(&self.db, &id).await,
            };
            found.unwrap_or_else(|e| {
                tracing::warn!(error = %e, match_id = %id, "archive read failed");
                None
            })
        })
    }

    fn put(&self, req: &RiotRequest, body: Bytes) -> BoxFuture<'_, ()> {
        let Some((kind, id)) = target(req) else {
            return Box::pin(async {});
        };
        let (id, region) = (id.to_string(), req.target.scope());
        Box::pin(async move {
            let result = match kind {
                Kind::Match => matches::put_stored(
                    &self.db,
                    &id,
                    region,
                    self.scope.as_str(),
                    body,
                    Clock::now().unix_ms,
                )
                .await
                .map(|(_, stored)| (pack::Kind::Match, stored)),
                Kind::Timeline => matches::put_timeline_stored(&self.db, &id, body)
                    .await
                    .map(|stored| (pack::Kind::Timeline, stored)),
            };
            match result {
                Ok((kind, stored)) => {
                    // 0: a body already packed, archived again unchanged.
                    if let Some(pending) = self.pending.as_ref().filter(|_| stored > 0) {
                        pending.add(kind, stored as u64).await;
                    }
                    self.queue_missing_half(id).await;
                }
                Err(e) => tracing::warn!(error = %e, match_id = %id, "archive write failed"),
            }
        })
    }

    /// League entries: the player's ranks, for analytics (ADR-105).
    fn observe(&self, req: &RiotRequest, body: Bytes) -> BoxFuture<'_, ()> {
        let Some(puuid) = observed(req) else {
            return Box::pin(async {});
        };
        let (puuid, platform) = (puuid.clone(), req.target.scope());
        Box::pin(async move {
            let now = Clock::now().unix_ms;
            let since = tiers::late_since(now, self.late_stamp_days);
            if let Err(e) =
                ranks::record(&self.db, self.scope.as_str(), platform, &puuid, &body, now, since).await
            {
                tracing::warn!(error = %e, "player rank write failed");
            }
        })
    }

    /// A match or timeline body, or a player's ranks, written behind the
    /// response (AVL-02, ADR-210): on a detached task within the
    /// write-behind's bound, with the body served by [`Archive::get`] until
    /// it is stored. The handle resolves when the write has landed, for a
    /// caller that reads the archive next (AVL-11). Everything else is
    /// nothing to write.
    ///
    /// At the bound the write is not started and the body is not held: the
    /// handle is then the write itself. A job's fetch waits on it and so
    /// makes the write, which is how a job's write is never dropped; when
    /// only requests were waiting nobody does, and it is dropped and counted
    /// as before.
    fn store_behind<'a>(&'a self, req: &'a RiotRequest, body: Bytes) -> BoxFuture<'a, Stored> {
        let bytes = body.len();
        let (what, held) = match target(req) {
            Some((kind, id)) => ("archive", Some(kind.key(id))),
            None if observed(req).is_some() => ("ranks", None),
            None => return Box::pin(async { Stored::done() }),
        };
        let (this, owned) = (self.clone(), req.clone());
        let stored = match self.db.reserve_behind(bytes) {
            Some(room) => {
                let (landed, stored) = Stored::pending();
                let unstored =
                    held.map(|key| Unstored::hold(&self.unstored, key, body.clone(), stored.clone()));
                room.spawn(what, async move {
                    this.write(&owned, body).await;
                    // The row is there: readers go to it, then waiters go on.
                    drop(unstored);
                    drop(landed);
                });
                stored
            }
            None => {
                let unmade = Unmade {
                    db: self.db.clone(),
                    what,
                    bytes,
                    made: false,
                };
                Stored::when(async move {
                    this.write(&owned, body).await;
                    unmade.made();
                })
            }
        };
        Box::pin(async move { stored })
    }

    fn storing(&self, req: &RiotRequest) -> Stored {
        target(req)
            .and_then(|(kind, id)| lock(&self.unstored).get(&kind.key(id)).map(|(_, s)| s.clone()))
            .unwrap_or_default()
    }
}

/// The PUUID of a league-entries lookup, the one body [`Archive::observe`] reads.
fn observed(req: &RiotRequest) -> Option<&String> {
    req.params
        .first()
        .filter(|_| req.endpoint.id == "league.entriesByPuuid")
}
