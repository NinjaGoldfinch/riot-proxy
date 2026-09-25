//! The permanent store for immutable Riot data (docs/design/04): matches now;
//! facts and analytics follow in P5-03 and P7.

pub mod matches;

use bytes::Bytes;
use futures_util::future::BoxFuture;

use crate::clock::Clock;
use crate::db::Db;
use crate::fetcher::Archive;
use crate::riot::client::RiotRequest;

/// The fetcher's [`Archive`] over SQLite. A failing archive never fails a
/// request: reads fall through to Riot and writes are logged (v1 did the same).
#[derive(Debug, Clone)]
pub struct SqliteArchive {
    db: Db,
    /// `ARCHIVE_TIMELINES`: also store timelines. Stored ones are served either way.
    timelines: bool,
}

impl SqliteArchive {
    pub fn new(db: Db, timelines: bool) -> Self {
        Self { db, timelines }
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
                Kind::Match => matches::put(&self.db, &id, region, body, Clock::now().unix_ms)
                    .await
                    .map(|_| ()),
                Kind::Timeline if self.timelines => match matches::put_timeline(&self.db, &id, body).await {
                    Ok(false) => {
                        tracing::debug!(match_id = %id, "timeline not archived: its match is not archived yet");
                        Ok(())
                    }
                    other => other.map(|_| ()),
                },
                Kind::Timeline => Ok(()),
            };
            if let Err(e) = result {
                tracing::warn!(error = %e, match_id = %id, "archive write failed");
            }
        })
    }
}
