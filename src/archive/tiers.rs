//! Each participant's tier as it was when their ranked match was archived
//! (`match_facts.tier`; V0016 and THR-02 as `match_tiers`, folded into the
//! fact row by V0019, DSK-04, ADR-138). Analytics read it in place of the
//! current ladder, so a player's old games stay in the tier they were played
//! at.
//!
//! A match is stamped in the transaction that archives it; `tiers:backfill`
//! stamps the matches left with no tier. The tier is ADR-105's: the newer of
//! the ladder's and the last league lookup's for the match's platform and
//! queue, else [`UNKNOWN_TIER`]. A stamp is never redone, except that a later
//! rank upgrades an `UNKNOWN` one on a recent match ([`late_stamp`]).
//! `facts::write` never touches the column, so a re-extraction keeps it.

use rusqlite::{Connection, OptionalExtension, params};

use crate::archive::analytics::UNKNOWN_TIER;
use crate::riot::ladder::QUEUE_IDS;
use crate::riot::routing::Platform;

/// `TIER_LATE_STAMP_DAYS`' default.
pub const LATE_STAMP_DAYS: u32 = 14;

/// One day in unix ms.
pub const DAY_MS: i64 = 86_400_000;

/// league-v4's queue for a match-v5 queue id; `None` outside solo and flex.
pub fn ranked_queue(queue_id: i64) -> Option<&'static str> {
    QUEUE_IDS
        .iter()
        .find(|(_, id)| i64::from(*id) == queue_id)
        .map(|(name, _)| *name)
}

/// Every unstamped participant of `?1` at their tier. Binds `?2` platform,
/// `?3` queue. The one-row subquery always answers, so a player neither the
/// ladder nor a lookup placed gets `UNKNOWN`, not NULL.
const STAMP: &str = "UPDATE match_facts SET tier = coalesce(
       (SELECT CASE WHEN pr.tier IS NOT NULL AND (le.tier IS NULL OR pr.fetched_at > le.updated_at)
                    THEN pr.tier ELSE le.tier END
          FROM (SELECT 1)
          LEFT JOIN ladder_entries le ON le.key_scope = match_facts.key_scope AND le.platform = ?2
            AND le.queue = ?3 AND le.puuid = match_facts.puuid
          LEFT JOIN player_ranks pr ON pr.key_scope = match_facts.key_scope AND pr.platform = ?2
            AND pr.queue = ?3 AND pr.puuid = match_facts.puuid),
       'UNKNOWN')
     WHERE match_id = ?1 AND tier IS NULL";

/// Stamp the participants of a match whose facts are written. A match outside
/// solo and flex, or whose id names no platform, writes nothing; a participant
/// already stamped keeps their stamp. Returns the rows stamped.
pub fn stamp(c: &Connection, match_id: &str, queue_id: i64) -> rusqlite::Result<usize> {
    let (Some(platform), Some(queue)) = (Platform::from_match_id(match_id), ranked_queue(queue_id)) else {
        return Ok(0);
    };
    c.prepare_cached(STAMP)?
        .execute(params![match_id, platform.as_str(), queue])
}

/// Upgrade one player's `UNKNOWN` stamps on a platform and queue to `tier`,
/// for matches that ended at or after `since` (unix ms). Called with a rank
/// the ladder or a league lookup just returned. The platform is the match
/// id's prefix and the queue the match's queue id, so the player's facts are
/// read through `facts_player` and each match checked by key. Returns the
/// rows changed.
pub fn late_stamp(
    c: &Connection,
    key_scope: &str,
    platform: &str,
    queue: &str,
    puuid: &str,
    tier: &str,
    since: i64,
) -> rusqlite::Result<usize> {
    let Some(queue_id) = crate::riot::ladder::queue_id(queue) else {
        return Ok(0);
    };
    if tier == UNKNOWN_TIER {
        return Ok(0);
    }
    c.prepare_cached(LATE_STAMP)?
        .execute(params![key_scope, platform, queue_id, puuid, tier, since])
}

/// Binds `?1` key scope, `?2` platform, `?3` queue id, `?4` puuid, `?5` the
/// tier, `?6` the window's start.
const LATE_STAMP: &str = "UPDATE match_facts SET tier = ?5
     WHERE key_scope = ?1 AND puuid = ?4 AND tier = 'UNKNOWN'
       AND match_id > upper(?2) || '_' AND match_id < upper(?2) || '`'
       AND EXISTS (SELECT 1 FROM matches m WHERE m.match_id = match_facts.match_id
                     AND m.queue_id = ?3 AND m.game_end_ms >= ?6)";

/// Where a late stamp's window starts: `days` before `now`. With 0 days it is
/// after every match, so nothing is upgraded.
pub fn late_since(now: i64, days: u32) -> i64 {
    if days == 0 {
        return i64::MAX;
    }
    now.saturating_sub(i64::from(days).saturating_mul(DAY_MS))
}

/// Ranked matches with a fact left unstamped, after `after` in id order, up
/// to `limit`, with their queue ids: what `tiers:backfill` stamps.
pub fn unstamped(c: &Connection, after: &str, limit: i64) -> rusqlite::Result<Vec<(String, i64)>> {
    let mut stmt = c.prepare_cached(&format!(
        "SELECT m.match_id, m.queue_id {UNSTAMPED} ORDER BY m.match_id LIMIT ?2"
    ))?;
    stmt.query_map(params![after, limit], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect()
}

/// Whether any match is left for `tiers:backfill`.
pub fn any_unstamped(c: &Connection) -> rusqlite::Result<bool> {
    Ok(
        c.query_row(&format!("SELECT 1 {UNSTAMPED} LIMIT 1"), params![""], |_| Ok(()))
            .optional()?
            .is_some(),
    )
}

/// Binds `?1`, the id to start after.
const UNSTAMPED: &str = "FROM matches m
     WHERE m.match_id > ?1 AND m.queue_id IN (420, 440)
       AND EXISTS (SELECT 1 FROM match_facts f WHERE f.match_id = m.match_id AND f.tier IS NULL)";

#[cfg(test)]
mod tests;
