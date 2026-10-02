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
