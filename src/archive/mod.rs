//! The permanent store for immutable Riot data (docs/design/04): matches and
//! their facts; analytics follow in P7.

pub mod analytics;
pub mod builds;
pub mod facts;
pub mod matches;
pub mod player;
pub mod pool;
pub mod ranks;
pub mod tiers;

use bytes::Bytes;
use futures_util::future::BoxFuture;

use crate::cache::keys::KeyScope;
use crate::clock::Clock;
use crate::db::{Db, DbError};
use crate::fetcher::Archive;
use crate::jobs::archive::queue_missing_half_on;
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
}

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
        }
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

enum Kind {
    Match,
    Timeline,
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
                Kind::Match => matches::put(
                    &self.db,
                    &id,
                    region,
                    self.scope.as_str(),
                    body,
                    Clock::now().unix_ms,
                )
                .await
                .map(|_| ()),
                Kind::Timeline => matches::put_timeline(&self.db, &id, body).await,
            };
            match result {
                Ok(()) => self.queue_missing_half(id).await,
                Err(e) => tracing::warn!(error = %e, match_id = %id, "archive write failed"),
            }
        })
    }

    /// League entries: the player's ranks, for analytics (ADR-105).
    fn observe(&self, req: &RiotRequest, body: Bytes) -> BoxFuture<'_, ()> {
        let Some(puuid) = req
            .params
            .first()
            .filter(|_| req.endpoint.id == "league.entriesByPuuid")
        else {
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
}
