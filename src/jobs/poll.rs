//! The poll handlers (docs/design/06 §Job catalogue; v1 `processors.ts`):
//! `poll:live`, `poll:rank` and `poll:matches`, one job per tracked player per
//! tick. Each diffs what Riot says now against the state on the player's row,
//! which v1 kept in Redis, and publishes an event only on a transition.
//!
//! Every Riot call is `Priority::Bulk` (design/06), so polls never take the
//! headroom interactive traffic needs.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::json;

use crate::archive::matches;
use crate::clock::Clock;
use crate::events::{self, Event, Rank};
use crate::fetcher::{FetchError, FetchOptions, Fetcher};
use crate::http::ErrorCode;
use crate::jobs::scheduler::{Handler, Job, JobError, NewJob, Queue};
use crate::jobs::{kinds, priority};
use crate::players::{self, PollState};
use crate::riot::endpoints::Endpoint;
use crate::riot::routing::Platform;
use crate::routes::passthrough::request;
use crate::ws::Hub;

/// v1 `POLL_PAGE`: what one tick reads when nothing has gone wrong.
pub const POLL_PAGE: i64 = 5;
/// v1 `CATCHUP_PAGE`: catch-up pages are read wide.
pub const CATCHUP_PAGE: i64 = 100;
/// v1: a finished game's match is polled a minute later, when Riot has it.
pub const MATCH_NUDGE: Duration = Duration::from_secs(60);

/// What the poll handlers share.
#[derive(Clone)]
pub struct PollContext {
    pub fetcher: Fetcher,
    pub queue: Queue,
    pub hub: Hub,
    pub key_scope: String,
    /// `TRACK_CATCHUP_LIMIT`: how deep a match poll pages before handing over to a backfill.
    pub catchup_limit: u32,
    /// `LOOKUP_BACKFILL_LIMIT`: how far that backfill walks.
    pub backfill_limit: u32,
    /// `ARCHIVE_TIMELINES`.
    pub archive_timelines: bool,
}

/// Every poll job's payload (v1 `PollPlayerJob`).
#[derive(Debug, Deserialize)]
pub struct PollPlayer {
    pub puuid: String,
    pub platform: String,
}

/// A fetch failure: retried with backoff, or a yield when the limiter has no room.
fn retry(e: &FetchError) -> JobError {
    JobError::from_fetch(e)
}

fn store(e: &crate::db::DbError) -> JobError {
    JobError::Retry(format!("store: {e}"))
}

impl PollContext {
    fn db(&self) -> &crate::db::Db {
        self.queue.db()
    }

    fn parse(job: &Job) -> Result<(PollPlayer, Platform), JobError> {
        let p: PollPlayer = job.payload()?;
        let platform = Platform::parse(&p.platform).map_err(|e| JobError::Fail(e.message))?;
        Ok((p, platform))
    }

    async fn get(
        &self,
        id: &'static str,
        platform: Platform,
        params: &[&str],
        query: &[(&str, Option<String>)],
        region: bool,
    ) -> Result<bytes::Bytes, FetchError> {
        let target = Endpoint::by_id(id).and_then(|e| {
            if region {
                e.target_for_region(platform.region())
            } else {
                Some(e.target_for_platform(platform))
            }
        });
        let req = request(id, target, params, query).map_err(FetchError::from)?;
        self.fetcher.fetch(req, FetchOptions::JOB).await.map(|r| r.body)
    }

    // ── poll:live ───────────────────────────────────────────────────────────

    /// spectator-v5 against the stored game: a new game is `game.started`, no
    /// game after one is `game.ended` plus a match poll a minute later (v1).
    pub async fn poll_live(&self, job: &Job) -> Result<(), JobError> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct ActiveGame {
            game_id: Option<i64>,
            game_queue_config_id: Option<i64>,
            #[serde(default)]
            participants: Vec<Participant>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Participant {
            puuid: Option<String>,
            champion_id: Option<i64>,
        }

        let (p, platform) = Self::parse(job)?;
        let Some(player) = players::get(self.db(), &self.key_scope, &p.puuid)
            .await
            .map_err(|e| store(&e))?
        else {
            return Ok(()); // no longer known under this key
        };
        let game = match self
            .get("spectator.activeGame", platform, &[&p.puuid], &[], false)
            .await
        {
            Ok(body) => serde_json::from_slice::<ActiveGame>(&body).ok(),
            // 404 is the normal "not in game" answer, negatively cached (v1).
            Err(e) if e.api.code == ErrorCode::NotFound => None,
            Err(e) => return Err(retry(&e)),
        };
        let current = game.as_ref().and_then(|g| g.game_id);
        let previous = player.in_game_id;

        match (current, previous) {
            (Some(now), before) if Some(now) != before => {
                self.set(&p.puuid, PollState::InGame(Some(now))).await?;
                let game = game.as_ref();
                let me = game.and_then(|g| {
                    g.participants
                        .iter()
                        .find(|x| x.puuid.as_deref() == Some(&p.puuid))
                });
                events::publish(
                    &self.hub,
                    &Event::GameStarted {
                        puuid: p.puuid.clone(),
                        platform: platform.as_str().into(),
                        game_id: now,
                        queue_id: game.and_then(|g| g.game_queue_config_id),
                        champion_id: me.and_then(|m| m.champion_id),
                    },
                );
            }
            (None, Some(ended)) => {
                self.set(&p.puuid, PollState::InGame(None)).await?;
                events::publish(
                    &self.hub,
                    &Event::GameEnded {
                        puuid: p.puuid.clone(),
                        platform: Some(platform.as_str().into()),
                        game_id: ended,
                        queue_id: None,
                        champion_id: None,
                    },
                );
                // The match appears shortly after the game ends. Deduped with
                // the tick's own poll: whichever is pending covers both (v1).
                let mut nudge = NewJob::new(
                    kinds::POLL_MATCHES,
                    priority::POLL,
                    json!({"puuid": p.puuid, "platform": platform.as_str()}),
                )
                .dedupe(p.puuid.clone());
                let delay = i64::try_from(MATCH_NUDGE.as_millis()).unwrap_or(i64::MAX);
                nudge.run_after = Some(Clock::now().unix_ms.saturating_add(delay));
                self.queue.enqueue(nudge).await.map_err(|e| store(&e))?;
            }
            _ => {}
        }
        Ok(())
    }

    async fn set(&self, puuid: &str, state: PollState) -> Result<(), JobError> {
        players::set_poll_state(self.db(), &self.key_scope, puuid, state)
            .await
            .map(|_| ())
            .map_err(|e| store(&e))
    }

    // ── poll:rank ───────────────────────────────────────────────────────────

    /// league-v4 entries against the stored snapshot: one `rank.changed` per
    /// queue whose standing moved. The first observation is a baseline (v1).
    pub async fn poll_rank(&self, job: &Job) -> Result<(), JobError> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Entry {
            queue_type: Option<String>,
            tier: Option<String>,
            rank: Option<String>,
            league_points: Option<i64>,
        }

        let (p, platform) = Self::parse(job)?;
        let Some(player) = players::get(self.db(), &self.key_scope, &p.puuid)
            .await
            .map_err(|e| store(&e))?
        else {
            return Ok(());
        };
        let body = self
            .get("league.entriesByPuuid", platform, &[&p.puuid], &[], false)
            .await
            .map_err(|e| retry(&e))?;
        let entries: Vec<Entry> = serde_json::from_slice(&body).unwrap_or_default();
        let snapshot: BTreeMap<String, Rank> = entries
            .into_iter()
            .filter_map(|e| {
                Some((
                    e.queue_type?,
                    Rank {
                        tier: e.tier,
                        rank: e.rank,
                        lp: e.league_points,
                    },
                ))
            })
            .collect();
        let json = serde_json::to_string(&snapshot).unwrap_or_else(|_| "{}".into());
        self.set(&p.puuid, PollState::LastRank(json)).await?;

        let Some(previous) = player
            .last_rank
            .and_then(|r| serde_json::from_str::<BTreeMap<String, Rank>>(&r).ok())
        else {
            return Ok(()); // a baseline, not a change
        };
        for (queue, after) in snapshot {
            let before = previous.get(&queue).cloned();
            if before.as_ref() == Some(&after) {
                continue;
            }
            events::publish(
                &self.hub,
                &Event::RankChanged {
                    puuid: p.puuid.clone(),
                    queue,
                    before,
                    after: Some(after),
                },
            );
        }
        Ok(())
    }

    // ── poll:matches ────────────────────────────────────────────────────────

    /// Page back from the newest match to the stored cursor (v1 #46), queue
    /// the unarchived ones, and hand anything deeper than `TRACK_CATCHUP_LIMIT`
    /// to a backfill.
    pub async fn poll_matches(&self, job: &Job) -> Result<(), JobError> {
        let (p, platform) = Self::parse(job)?;
        let Some(player) = players::get(self.db(), &self.key_scope, &p.puuid)
            .await
            .map_err(|e| store(&e))?
        else {
            return Ok(());
        };
        let cursor = player.last_seen_match_id;
        let cap = i64::from(self.catchup_limit);

        let mut collected: Vec<String> = Vec::new();
        let mut start = 0i64;
        let caught_up = loop {
            let count = if start == 0 { POLL_PAGE } else { CATCHUP_PAGE };
            let body = self
                .get(
                    "match.idsByPuuid",
                    platform,
                    &[&p.puuid],
                    &[
                        ("start", Some(start.to_string())),
                        ("count", Some(count.to_string())),
                    ],
                    true,
                )
                .await
                .map_err(|e| retry(&e))?;
            let ids: Vec<String> = serde_json::from_slice(&body).unwrap_or_default();
            if ids.is_empty() {
                break true;
            }
            // Everything above the cursor is new; the cursor itself we have.
            if let Some(cut) = cursor.as_ref().and_then(|c| ids.iter().position(|id| id == c)) {
                collected.extend(ids.into_iter().take(cut));
                break true;
            }
            let n = i64::try_from(ids.len()).unwrap_or(i64::MAX);
            collected.extend(ids);
            // Never polled, catch-up off, or the end of their history (v1).
            if cursor.is_none() || cap <= 0 || n < count {
                break true;
            }
            start += n;
            if start >= cap {
                break false;
            }
        };

        if !caught_up {
            tracing::warn!(puuid = %p.puuid, cap, "match poll fell behind its catch-up limit; queuing a backfill");
            let backfill = NewJob::new(
                kinds::BACKFILL_PLAYER,
                priority::BACKFILL,
                json!({"puuid": p.puuid, "platform": platform.as_str(), "limit": self.backfill_limit, "reason": "catchup"}),
            )
            .dedupe(p.puuid.clone());
            self.queue.enqueue(backfill).await.map_err(|e| store(&e))?;
        }

        let Some(newest) = collected.first().cloned() else {
            return Ok(());
        };
        let unarchived = matches::filter_unarchived(self.db(), &collected)
            .await
            .map_err(|e| JobError::Retry(format!("archive: {e}")))?;
        let depth: std::collections::HashMap<&str, i64> = collected
            .iter()
            .enumerate()
            .map(|(i, id)| (id.as_str(), i64::try_from(i).unwrap_or(i64::MAX)))
            .collect();
        let jobs: Vec<NewJob> = unarchived
            .iter()
            .map(|id| {
                let d = depth.get(id.as_str()).copied().unwrap_or(0);
                NewJob::new(
                    kinds::ARCHIVE_MATCH,
                    archive_priority(d),
                    json!({"matchId": id, "puuid": p.puuid, "fetchTimeline": self.archive_timelines}),
                )
                .dedupe(id.clone())
            })
            .collect();
        let queued = self.queue.enqueue_all(jobs).await.map_err(|e| store(&e))?;
        // v1 moved the cursor only when something was left to archive, so a
        // player whose new games were already archived (by a lookup, say) was
        // re-paged every tick. The cursor is the newest id seen, archived or not.
        self.set(&p.puuid, PollState::LastSeenMatch(newest)).await?;
        tracing::debug!(puuid = %p.puuid, new = collected.len(), queued, "match poll");
        Ok(())
    }
}

/// design/06: a just-finished game (the first page) is interactive-band;
/// deeper matches rank by depth in blocks of ten (`100 + depth / 10`, v1 #31),
/// globally, so anyone's newest ten beat anyone's hundredth.
pub fn archive_priority(depth: i64) -> i64 {
    if depth < POLL_PAGE {
        priority::INTERACTIVE
    } else {
        priority::ARCHIVE_DEPTH + depth / 10
    }
}

macro_rules! handler {
    ($name:ident, $method:ident) => {
        pub struct $name(pub Arc<PollContext>);

        impl Handler for $name {
            fn run<'a>(&'a self, job: &'a Job) -> BoxFuture<'a, Result<(), JobError>> {
                Box::pin(self.0.$method(job))
            }
        }
    };
}

handler!(PollLive, poll_live);
handler!(PollRank, poll_rank);
handler!(PollMatches, poll_matches);
