//! The crawl's tables (V0004, ADR-054): the run log `ladder_crawls`, the
//! ladder `ladder_entries`, and `crawl_legs`, a crawl's outstanding jobs.
//!
//! Every change that can end a stage happens in one write transaction: a leg
//! row is deleted, and if it was the last, the crawl moves on in the same
//! commit. A re-run of a leg that already ended finds no row and changes
//! nothing, so a crash between a leg's work and its job being marked done
//! can never end a stage twice or strand it.

use rusqlite::{OptionalExtension, Transaction, params};
use serde::Serialize;

use crate::db::{Db, DbError};

/// What a crawl row says (v1 `LadderCrawl`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Crawl {
    pub id: String,
    pub platform: String,
    pub queue: String,
    pub tier_floor: String,
    pub status: String,
    pub phase: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub counters: Counters,
    pub legs_failed: i64,
}

/// v1's six crawl counters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Counters {
    pub pages_fetched: i64,
    pub entries_seen: i64,
    pub players_discovered: i64,
    pub backfills_enqueued: i64,
    pub match_ids_seen: i64,
    pub matches_queued: i64,
}

const COLUMNS: &str = "id, platform, queue, tier_floor, status, phase, started_at, finished_at, \
    pages_fetched, entries_seen, players_discovered, backfills_enqueued, match_ids_seen, \
    matches_queued, legs_failed";

fn crawl_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Crawl> {
    Ok(Crawl {
        id: r.get(0)?,
        platform: r.get(1)?,
        queue: r.get(2)?,
        tier_floor: r.get(3)?,
        status: r.get(4)?,
        phase: r.get(5)?,
        started_at: r.get(6)?,
        finished_at: r.get(7)?,
        counters: Counters {
            pages_fetched: r.get(8)?,
            entries_seen: r.get(9)?,
            players_discovered: r.get(10)?,
            backfills_enqueued: r.get(11)?,
            match_ids_seen: r.get(12)?,
            matches_queued: r.get(13)?,
        },
        legs_failed: r.get(14)?,
    })
}

/// A new crawl row, or `None` when one is already running for this ladder
/// (the partial unique index decides, v1).
pub fn insert_crawl(
    tx: &Transaction<'_>,
    id: &str,
    key_scope: &str,
    platform: &str,
    queue: &str,
    tier_floor: &str,
    now: i64,
) -> Result<Option<Crawl>, DbError> {
    Ok(tx
        .query_row(
            &format!(
                "INSERT INTO ladder_crawls (id, key_scope, platform, queue, tier_floor, started_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT DO NOTHING
                 RETURNING {COLUMNS}"
            ),
            params![id, key_scope, platform, queue, tier_floor, now],
            crawl_row,
        )
        .optional()?)
}

/// The running crawl of one ladder, if any.
pub fn running_on(
    tx: &rusqlite::Connection,
    key_scope: &str,
    platform: &str,
    queue: &str,
) -> Result<Option<Crawl>, DbError> {
    Ok(tx
        .query_row(
            &format!(
                "SELECT {COLUMNS} FROM ladder_crawls
                 WHERE key_scope = ?1 AND platform = ?2 AND queue = ?3 AND status = 'running'"
            ),
            params![key_scope, platform, queue],
            crawl_row,
        )
        .optional()?)
}

pub async fn get(db: &Db, key_scope: &str, id: &str) -> Result<Option<Crawl>, DbError> {
    let (scope, id) = (key_scope.to_string(), id.to_string());
    db.read(move |c| {
        Ok(c.query_row(
            &format!("SELECT {COLUMNS} FROM ladder_crawls WHERE key_scope = ?1 AND id = ?2"),
            params![scope, id],
            crawl_row,
        )
        .optional()?)
    })
    .await
}

/// Record legs a fan-out is about to queue. Before the jobs, in the same
/// transaction: a leg must be known before it can end (v1).
pub fn add_legs(tx: &Transaction<'_>, crawl_id: &str, legs: &[String]) -> Result<(), DbError> {
    let mut stmt = tx.prepare_cached("INSERT OR IGNORE INTO crawl_legs (crawl_id, leg) VALUES (?1, ?2)")?;
    for leg in legs {
        stmt.execute(params![crawl_id, leg])?;
    }
    Ok(())
}

/// A leg's row: `Some(cursor)` while outstanding (`cursor` is a walk's next
/// page, if it has one), `None` once it has ended.
pub async fn leg(db: &Db, crawl_id: &str, leg: &str) -> Result<Option<Option<i64>>, DbError> {
    let (crawl, leg) = (crawl_id.to_string(), leg.to_string());
    db.read(move |c| {
        Ok(c.query_row(
            "SELECT cursor FROM crawl_legs WHERE crawl_id = ?1 AND leg = ?2",
            params![crawl, leg],
            |r| r.get(0),
        )
        .optional()?)
    })
    .await
}

/// How many legs a crawl still has (the admin list's `pendingLegs`).
pub fn pending_legs(c: &rusqlite::Connection, crawl_id: &str) -> Result<i64, DbError> {
    Ok(c.query_row(
        "SELECT COUNT(*) FROM crawl_legs WHERE crawl_id = ?1",
        [crawl_id],
        |r| r.get(0),
    )?)
}

/// One player's standing on one ladder, as a page reported it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub puuid: String,
    pub tier: String,
    pub division: String,
    pub league_points: i64,
    pub wins: i64,
    pub losses: i64,
    pub veteran: bool,
    pub inactive: bool,
    pub fresh_blood: bool,
    pub hot_streak: bool,
}

/// Where a page's entries go, and what it moves.
pub struct Page<'a> {
    pub key_scope: &'a str,
    pub crawl_id: &'a str,
    pub platform: &'a str,
    pub queue: &'a str,
    pub entries: &'a [Entry],
    /// A walk's leg and the page after this one; `None` for an apex league.
    pub cursor: Option<(&'a str, i64)>,
    pub now: i64,
}

/// One page, in one transaction: the entries (`first_seen` set once,
/// `last_seen` restamped, v1), every player as a known but untracked player,
/// the crawl's counters, and the walk's cursor. A crash before the commit
/// re-walks the page; after it, resumes on the next.
pub async fn write_page(db: &Db, page: Page<'_>) -> Result<(), DbError> {
    let key_scope = page.key_scope.to_string();
    let crawl_id = page.crawl_id.to_string();
    let platform = page.platform.to_string();
    let queue = page.queue.to_string();
    let entries = page.entries.to_vec();
    let cursor = page.cursor.map(|(leg, next)| (leg.to_string(), next));
    let now = page.now;
    db.write(move |c| {
        let tx = c.transaction()?;
        {
            let mut entry = tx.prepare_cached(
                "INSERT INTO ladder_entries (key_scope, platform, queue, puuid, tier, division,
                   league_points, wins, losses, veteran, inactive, fresh_blood, hot_streak,
                   first_seen_crawl_id, last_seen_crawl_id, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?14, ?15)
                 ON CONFLICT (key_scope, platform, queue, puuid) DO UPDATE SET
                   tier = excluded.tier, division = excluded.division,
                   league_points = excluded.league_points, wins = excluded.wins,
                   losses = excluded.losses, veteran = excluded.veteran,
                   inactive = excluded.inactive, fresh_blood = excluded.fresh_blood,
                   hot_streak = excluded.hot_streak,
                   last_seen_crawl_id = excluded.last_seen_crawl_id,
                   updated_at = excluded.updated_at",
            )?;
            // Never tracked: a crawl must not sign thousands of players up for
            // a poll (v1 `upsertDiscoveredPlayers`).
            let mut player = tx.prepare_cached(
                "INSERT INTO players (key_scope, puuid, platform, tracked, updated_at)
                 VALUES (?1, ?2, ?3, 0, ?4)
                 ON CONFLICT (key_scope, puuid) DO UPDATE SET
                   platform = excluded.platform, updated_at = excluded.updated_at",
            )?;
            for e in &entries {
                entry.execute(params![
                    key_scope,
                    platform,
                    queue,
                    e.puuid,
                    e.tier,
                    e.division,
                    e.league_points,
                    e.wins,
                    e.losses,
                    e.veteran,
                    e.inactive,
                    e.fresh_blood,
                    e.hot_streak,
                    crawl_id,
                    now
                ])?;
                player.execute(params![key_scope, e.puuid, platform, now])?;
            }
        }
        let n = i64::try_from(entries.len()).unwrap_or(i64::MAX);
        tx.execute(
            "UPDATE ladder_crawls SET pages_fetched = pages_fetched + 1,
               entries_seen = entries_seen + ?2, players_discovered = players_discovered + ?2
             WHERE id = ?1",
            params![crawl_id, n],
        )?;
        if let Some((leg, next)) = cursor {
            tx.execute(
                "UPDATE crawl_legs SET cursor = ?3 WHERE crawl_id = ?1 AND leg = ?2",
                params![crawl_id, leg, next],
            )?;
        }
        tx.commit()?;
        Ok(())
    })
    .await
}

/// What ending a leg did to its crawl.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended {
    /// Not the last leg, or ended already, or the crawl is not running.
    Nothing,
    /// The stage is over; the crawl is now in `phase` (the row as committed).
    Phase(Crawl),
    /// The crawl is over (`status` completed or failed).
    Finished(Crawl),
}

/// How a stage's end moves the crawl on, decided inside the transaction that
/// removed its last leg. `next` may queue the next stage's jobs on `tx`.
pub type NextStage<'f> = dyn FnMut(&Transaction<'_>, &Crawl) -> Result<Stage, DbError> + Send + 'f;

/// The decision `NextStage` returns.
pub enum Stage {
    /// Move to this phase.
    Phase(&'static str),
    /// The crawl is complete.
    Complete,
}

/// End `leg` of `crawl_id` (failed or not). If it was the last, the crawl
/// ends `failed` when any leg failed (v1), and otherwise moves on as `next`
/// decides, all in this transaction.
pub fn end_leg(
    tx: &Transaction<'_>,
    crawl_id: &str,
    leg: &str,
    failed: bool,
    now: i64,
    next: &mut NextStage<'_>,
) -> Result<Ended, DbError> {
    let removed = tx.execute(
        "DELETE FROM crawl_legs WHERE crawl_id = ?1 AND leg = ?2",
        params![crawl_id, leg],
    )?;
    if removed == 0 {
        return Ok(Ended::Nothing);
    }
    if failed {
        tx.execute(
            "UPDATE ladder_crawls SET legs_failed = legs_failed + 1 WHERE id = ?1",
            [crawl_id],
        )?;
    }
    if pending_legs(tx, crawl_id)? > 0 {
        return Ok(Ended::Nothing);
    }
    let Some(crawl) = tx
        .query_row(
            &format!("SELECT {COLUMNS} FROM ladder_crawls WHERE id = ?1 AND status = 'running'"),
            [crawl_id],
            crawl_row,
        )
        .optional()?
    else {
        return Ok(Ended::Nothing);
    };
    // A crawl that has seen part of a ladder must not spend a match budget on
    // that part as though it were the whole (v1).
    if crawl.legs_failed > 0 {
        return finish(tx, crawl_id, "failed", now).map(|c| c.map_or(Ended::Nothing, Ended::Finished));
    }
    match next(tx, &crawl)? {
        Stage::Complete => {
            finish(tx, crawl_id, "completed", now).map(|c| c.map_or(Ended::Nothing, Ended::Finished))
        }
        Stage::Phase(phase) => Ok(tx
            .query_row(
                &format!(
                    "UPDATE ladder_crawls SET phase = ?2 WHERE id = ?1 AND status = 'running'
                     RETURNING {COLUMNS}"
                ),
                params![crawl_id, phase],
                crawl_row,
            )
            .optional()?
            .map_or(Ended::Nothing, Ended::Phase)),
    }
}

/// End a running crawl with `status`, dropping what is left of its working
/// state (v1 `finishCrawl` + `clearCrawlState`). `None` when it was not
/// running: a cancel already recorded is never overwritten (v1).
pub fn finish(
    tx: &Transaction<'_>,
    crawl_id: &str,
    status: &str,
    now: i64,
) -> Result<Option<Crawl>, DbError> {
    let crawl = tx
        .query_row(
            &format!(
                "UPDATE ladder_crawls SET status = ?2, finished_at = ?3
                 WHERE id = ?1 AND status = 'running'
                 RETURNING {COLUMNS}"
            ),
            params![crawl_id, status, now],
            crawl_row,
        )
        .optional()?;
    if crawl.is_some() {
        tx.execute("DELETE FROM crawl_legs WHERE crawl_id = ?1", [crawl_id])?;
        tx.execute("DELETE FROM crawl_match_ids WHERE crawl_id = ?1", [crawl_id])?;
    }
    Ok(crawl)
}

/// Whose match ids the collect stage walks: everyone this crawl stamped on
/// the ladder, minus players whose walk started since the crawl did (v1
/// `listCrawlBackfillCandidates`). Best first, so a crawl cancelled part-way
/// has collected the top of the ladder.
pub fn collect_candidates(
    tx: &Transaction<'_>,
    key_scope: &str,
    crawl: &Crawl,
) -> Result<Vec<String>, DbError> {
    let mut stmt = tx.prepare(
        "SELECT le.puuid FROM ladder_entries le
           LEFT JOIN players p ON p.key_scope = le.key_scope AND p.puuid = le.puuid
          WHERE le.key_scope = ?1 AND le.platform = ?2 AND le.queue = ?3
            AND le.last_seen_crawl_id = ?4
            AND coalesce(json_extract(p.backfill_state, '$.startedAt'), 0) < ?5
          ORDER BY le.league_points DESC, le.puuid DESC",
    )?;
    let rows = stmt
        .query_map(
            params![key_scope, crawl.platform, crawl.queue, crawl.id, crawl.started_at],
            |r| r.get(0),
        )?
        .collect::<Result<Vec<String>, _>>()?;
    Ok(rows)
}

/// Add to a crawl's counters, in SQL: legs bump the same row concurrently (v1).
pub fn bump(tx: &rusqlite::Connection, crawl_id: &str, column: Counter, by: i64) -> Result<(), DbError> {
    if by != 0 {
        let col = column.as_str();
        tx.execute(
            &format!("UPDATE ladder_crawls SET {col} = {col} + ?2 WHERE id = ?1"),
            params![crawl_id, by],
        )?;
    }
    Ok(())
}

/// The counters a later stage moves.
#[derive(Debug, Clone, Copy)]
pub enum Counter {
    BackfillsEnqueued,
    MatchIdsSeen,
    MatchesQueued,
}

impl Counter {
    fn as_str(self) -> &'static str {
        match self {
            Self::BackfillsEnqueued => "backfills_enqueued",
            Self::MatchIdsSeen => "match_ids_seen",
            Self::MatchesQueued => "matches_queued",
        }
    }
}

/// Add a page of ids to the crawl's set; returns how many were new. The set
/// is what makes a match shared by ten players one fetch (v1).
pub async fn add_match_ids(db: &Db, crawl_id: &str, ids: Vec<String>) -> Result<i64, DbError> {
    let crawl = crawl_id.to_string();
    db.write(move |c| {
        let tx = c.transaction()?;
        let mut added = 0;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT OR IGNORE INTO crawl_match_ids (crawl_id, match_id) VALUES (?1, ?2)",
            )?;
            for id in &ids {
                added += stmt.execute(params![crawl, id])?;
            }
        }
        tx.commit()?;
        Ok(i64::try_from(added).unwrap_or(i64::MAX))
    })
    .await
}

/// The next `n` ids of the set, left in place until their jobs exist (v1).
pub async fn peek_match_ids(db: &Db, crawl_id: &str, n: usize) -> Result<Vec<String>, DbError> {
    let crawl = crawl_id.to_string();
    let n = i64::try_from(n).unwrap_or(i64::MAX);
    db.read(move |c| {
        let mut stmt =
            c.prepare("SELECT match_id FROM crawl_match_ids WHERE crawl_id = ?1 ORDER BY match_id LIMIT ?2")?;
        let rows = stmt
            .query_map(params![crawl, n], |r| r.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        Ok(rows)
    })
    .await
}

/// Ids handed on: remove them from the set.
pub fn drop_match_ids(tx: &Transaction<'_>, crawl_id: &str, ids: &[String]) -> Result<(), DbError> {
    let mut stmt = tx.prepare_cached("DELETE FROM crawl_match_ids WHERE crawl_id = ?1 AND match_id = ?2")?;
    for id in ids {
        stmt.execute(params![crawl_id, id])?;
    }
    Ok(())
}

/// Stamp a player's walk as started (v1 `markBackfillStarted`), keeping the
/// rest of their backfill state.
pub async fn mark_walk_started(db: &Db, key_scope: &str, puuid: &str, now: i64) -> Result<(), DbError> {
    let (scope, puuid) = (key_scope.to_string(), puuid.to_string());
    db.write(move |c| {
        c.execute(
            "UPDATE players SET backfill_state = json_set(coalesce(backfill_state, '{}'), '$.startedAt', ?3)
              WHERE key_scope = ?1 AND puuid = ?2",
            params![scope, puuid, now],
        )?;
        Ok(())
    })
    .await
}

/// Stamp a player's history as accounted for (v1 `markBackfillComplete`).
pub async fn mark_walk_complete(
    db: &Db,
    key_scope: &str,
    puuid: &str,
    depth: i64,
    now: i64,
) -> Result<(), DbError> {
    let (scope, puuid) = (key_scope.to_string(), puuid.to_string());
    db.write(move |c| {
        c.execute(
            "UPDATE players SET backfill_state =
               json_set(coalesce(backfill_state, '{}'), '$.doneAt', ?3, '$.depth', ?4)
              WHERE key_scope = ?1 AND puuid = ?2",
            params![scope, puuid, now, depth],
        )?;
        Ok(())
    })
    .await
}

/// Recent crawls, newest first, with their outstanding legs (v1 `listCrawls`).
pub async fn list(
    db: &Db,
    key_scope: &str,
    platform: Option<String>,
    queue: Option<String>,
    limit: i64,
) -> Result<Vec<(Crawl, i64)>, DbError> {
    let scope = key_scope.to_string();
    db.read(move |c| {
        let mut stmt = c.prepare(&format!(
            "SELECT {COLUMNS} FROM ladder_crawls
              WHERE key_scope = ?1 AND (?2 IS NULL OR platform = ?2) AND (?3 IS NULL OR queue = ?3)
              ORDER BY started_at DESC, id DESC LIMIT ?4"
        ))?;
        let crawls = stmt
            .query_map(params![scope, platform, queue, limit], crawl_row)?
            .collect::<Result<Vec<_>, _>>()?;
        crawls
            .into_iter()
            .map(|cr| {
                let pending = pending_legs(c, &cr.id)?;
                Ok((cr, pending))
            })
            .collect()
    })
    .await
}

/// Why a cancel was refused.
#[derive(Debug, thiserror::Error)]
pub enum CancelError {
    #[error("no such crawl")]
    NotFound,
    #[error("crawl {id} is already {status}")]
    NotRunning { id: String, status: String },
    #[error(transparent)]
    Db(#[from] DbError),
}

/// Cancel a running crawl: the row is marked (never a finished one, v1), its
/// working state dropped and its queued legs cancelled. Running legs stop at
/// their next status check. Returns the row and how many jobs were dropped.
pub async fn cancel(db: &Db, key_scope: &str, id: &str, now: i64) -> Result<(Crawl, usize), CancelError> {
    let (scope, id) = (key_scope.to_string(), id.to_string());
    db.write(move |c| {
        let tx = c.transaction().map_err(DbError::from)?;
        let existing = tx
            .query_row(
                &format!("SELECT {COLUMNS} FROM ladder_crawls WHERE key_scope = ?1 AND id = ?2"),
                params![scope, id],
                crawl_row,
            )
            .optional()
            .map_err(DbError::from)?
            .ok_or(CancelError::NotFound)?;
        let Some(crawl) = finish(&tx, &id, "cancelled", now)? else {
            return Err(CancelError::NotRunning {
                id,
                status: existing.status,
            });
        };
        let dropped = crate::jobs::scheduler::cancel_pending_on(
            &tx,
            &[
                crate::jobs::kinds::LADDER_APEX,
                crate::jobs::kinds::LADDER_WALK,
                crate::jobs::kinds::LADDER_COLLECT,
                crate::jobs::kinds::LADDER_ARCHIVE,
            ],
            "crawlId",
            &id,
            now,
        )
        .map_err(DbError::from)?;
        tx.commit().map_err(DbError::from)?;
        Ok((crawl, dropped))
    })
    .await
}
