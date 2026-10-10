//! Each participant's tier as it was when their ranked match was archived
//! (V0016, THR-02, ADR-127). Analytics read it in place of the current
//! ladder, so a player's old games stay in the tier they were played at.
//!
//! A match is stamped in the transaction that archives it; `tiers:backfill`
//! stamps the matches archived before V0016. The tier is ADR-105's: the newer
//! of the ladder's and the last league lookup's for the match's platform and
//! queue, else [`UNKNOWN_TIER`]. A stamp is never redone, except that a later
//! rank upgrades an `UNKNOWN` one on a recent match ([`late_stamp`]).

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

/// Every participant of `?1` at their tier. Binds `?2` platform, `?3` queue,
/// `?4` the stamp time.
const STAMP: &str =
    "INSERT OR IGNORE INTO match_tiers (match_id, key_scope, puuid, platform, queue, tier, stamped_at)
     SELECT f.match_id, f.key_scope, f.puuid, ?2, ?3,
       CASE WHEN pr.tier IS NOT NULL AND (le.tier IS NULL OR pr.fetched_at > le.updated_at)
         THEN pr.tier ELSE coalesce(le.tier, 'UNKNOWN') END,
       ?4
       FROM match_facts f
       LEFT JOIN ladder_entries le ON le.key_scope = f.key_scope AND le.platform = ?2
         AND le.queue = ?3 AND le.puuid = f.puuid
       LEFT JOIN player_ranks pr ON pr.key_scope = f.key_scope AND pr.platform = ?2
         AND pr.queue = ?3 AND pr.puuid = f.puuid
      WHERE f.match_id = ?1";

/// Stamp the participants of a match whose facts are written. A match outside
/// solo and flex, or whose id names no platform, writes nothing; a participant
/// already stamped keeps their stamp. Returns the rows written.
pub fn stamp(c: &Connection, match_id: &str, queue_id: i64, now: i64) -> rusqlite::Result<usize> {
    let (Some(platform), Some(queue)) = (Platform::from_match_id(match_id), ranked_queue(queue_id)) else {
        return Ok(0);
    };
    c.prepare_cached(STAMP)?
        .execute(params![match_id, platform.as_str(), queue, now])
}

/// Upgrade one player's `UNKNOWN` stamps on a platform and queue to `tier`,
/// for matches that ended at or after `since` (unix ms). Called with a rank
/// the ladder or a league lookup just returned. Returns the rows changed.
#[allow(clippy::too_many_arguments)]
pub fn late_stamp(
    c: &Connection,
    key_scope: &str,
    platform: &str,
    queue: &str,
    puuid: &str,
    tier: &str,
    since: i64,
    now: i64,
) -> rusqlite::Result<usize> {
    if tier == UNKNOWN_TIER {
        return Ok(0);
    }
    c.prepare_cached(
        "UPDATE match_tiers SET tier = ?5, stamped_at = ?7
          WHERE key_scope = ?1 AND platform = ?2 AND queue = ?3 AND puuid = ?4 AND tier = 'UNKNOWN'
            AND (SELECT m.game_end_ms FROM matches m WHERE m.match_id = match_tiers.match_id) >= ?6",
    )?
    .execute(params![key_scope, platform, queue, puuid, tier, since, now])
}

/// Where a late stamp's window starts: `days` before `now`. With 0 days it is
/// after every match, so nothing is upgraded.
pub fn late_since(now: i64, days: u32) -> i64 {
    if days == 0 {
        return i64::MAX;
    }
    now.saturating_sub(i64::from(days).saturating_mul(DAY_MS))
}

/// Ranked matches with facts and no stamp, after `after` in id order, up to
/// `limit`, with their queue ids: what `tiers:backfill` stamps.
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
       AND EXISTS (SELECT 1 FROM match_facts f WHERE f.match_id = m.match_id)
       AND NOT EXISTS (SELECT 1 FROM match_tiers t WHERE t.match_id = m.match_id)";

#[cfg(test)]
mod tests;
