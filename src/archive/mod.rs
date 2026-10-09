//! The permanent store for immutable Riot data (docs/design/04): matches and
//! their facts; analytics follow in P7.

pub mod analytics;
pub mod facts;
pub mod matches;
pub mod player;
pub mod pool;
pub mod ranks;

use bytes::Bytes;
use futures_util::future::BoxFuture;

use crate::cache::keys::KeyScope;
use crate::clock::Clock;
use crate::db::Db;
use crate::fetcher::Archive;
use crate::riot::client::RiotRequest;

/// The fetcher's [`Archive`] over SQLite. A failing archive never fails a
/// request: reads fall through to Riot and writes are logged (v1 did the same).
#[derive(Debug, Clone)]
pub struct SqliteArchive {
    db: Db,
    /// Whose PUUIDs the fetched bodies carry, for `match_facts`.
    scope: KeyScope,
}

impl SqliteArchive {
    /// Every fetched match and timeline is stored: both are immutable. A timeline
    /// is stored once its match is (the foreign key). `ARCHIVE_TIMELINES` only
    /// decides whether archive jobs fetch timelines (ADR-085).
    pub fn new(db: Db, scope: KeyScope) -> Self {
        Self { db, scope }
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
                Kind::Timeline => match matches::put_timeline(&self.db, &id, body).await {
                    Ok(false) => {
                        tracing::debug!(match_id = %id, "timeline not archived: its match is not archived yet");
                        Ok(())
                    }
                    other => other.map(|_| ()),
                },
            };
            if let Err(e) = result {
                tracing::warn!(error = %e, match_id = %id, "archive write failed");
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
            if let Err(e) = ranks::record(&self.db, self.scope.as_str(), platform, &puuid, &body, now).await {
                tracing::warn!(error = %e, "player rank write failed");
            }
        })
    }
}
