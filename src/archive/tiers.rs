//! Each participant's tier as it was when their ranked match was archived
//! (`match_facts.tier`; V0016 and THR-02 as `match_tiers`, folded into the
//! fact row by V0019, DSK-04, ADR-138). Analytics read it in place of the
//! current ladder, so a player's old games stay in the tier they were played
//! at.
//!
//! A match is stamped in the transaction that archives it (and a
//! re-extraction stamps any fact it adds); `tiers:backfill` stamps the matches
//! left with no tier. The tier is ADR-105's: the newer of the ladder's and the
//! last league lookup's for the match's platform and queue, else
//! [`UNKNOWN_TIER`]. A stamp is never redone, except that a later rank
//! upgrades an `UNKNOWN` one on a recent match ([`late_stamp`]).
//! `facts::write` never touches the column, so a re-extraction keeps it.
//!
//! `tiers:backfill` sweeps each match-id prefix in id order from a cursor
//! (`tiers_sweep`, V0033, ADR-213), so the boot check ([`sweep_due`]) and the
//! sweep read only the ids past the cursors, a bounded window at a time,
//! never the whole archive.

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
/// solo and flex writes nothing; a participant already stamped keeps their
/// stamp. A ranked match whose id names no platform (a match-only prefix, or
/// one this build doesn't know) is stamped `UNKNOWN` throughout: no ladder or
/// lookup places a player there, and a fact left NULL would be looked for on
/// every boot (ADR-213). Returns the rows stamped.
pub fn stamp(c: &Connection, match_id: &str, queue_id: i64) -> rusqlite::Result<usize> {
    let Some(queue) = ranked_queue(queue_id) else {
        return Ok(0);
    };
    // A NULL platform matches no rank row, so the coalesce gives `UNKNOWN`.
    let platform = Platform::from_match_id(match_id).map(Platform::as_str);
    c.prepare_cached(STAMP)?
        .execute(params![match_id, platform, queue])
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
/// to `limit`, with their queue ids. It walks every match from `after` to its
/// hits, a table row each (no index on `matches` holds `queue_id`), so it is
/// for tests and small archives; `tiers:backfill` reads a [`window`] at a
/// time from each prefix's cursor.
pub fn unstamped(c: &Connection, after: &str, limit: i64) -> rusqlite::Result<Vec<(String, i64)>> {
    let mut stmt = c.prepare_cached(
        "SELECT m.match_id, m.queue_id FROM matches m
          WHERE m.match_id > ?1 AND m.queue_id IN (420, 440)
            AND EXISTS (SELECT 1 FROM match_facts f WHERE f.match_id = m.match_id AND f.tier IS NULL)
          ORDER BY m.match_id LIMIT ?2",
    )?;
    stmt.query_map(params![after, limit], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect()
}

/// One match-id prefix of the archive and how far `tiers:backfill` has swept
/// it (`tiers_sweep`, V0033).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sweep {
    /// The ids' part before `_`, as they spell it (`EUW1`).
    pub prefix: String,
    /// The highest id of the prefix a sweep has passed; `None` (no row, or
    /// `''`) when no sweep has.
    pub swept_to: Option<String>,
}

impl Sweep {
    /// Where the rest of the prefix starts, exclusive: its cursor, or `P_`,
    /// which sorts before every `P_…` id.
    pub fn after(&self) -> String {
        self.swept_to
            .clone()
            .unwrap_or_else(|| format!("{}_", self.prefix))
    }

    /// Where the prefix ends, exclusive: `` ` `` is the character after `_`,
    /// so every `P_…` id sorts below `` P` `` and no other prefix's id falls
    /// between them.
    pub fn before(&self) -> String {
        format!("{}`", self.prefix)
    }
}

/// The first id at or after `?1`: one seek on `matches`' key.
pub(crate) const NEXT_ID: &str =
    "SELECT match_id FROM matches WHERE match_id >= ?1 ORDER BY match_id LIMIT 1";

/// Every match-id prefix in the archive, in id order, with its cursor: one
/// seek on `matches`' key per prefix, from the end of the one before (a skip
/// scan), and one cursor read by key. A few dozen index reads, whatever the
/// archive's size. An id with no `_` names no prefix and is stepped over (no
/// region could have fetched it).
pub fn sweeps(c: &Connection) -> rusqlite::Result<Vec<Sweep>> {
    let mut next = c.prepare_cached(NEXT_ID)?;
    let mut cursor = c.prepare_cached("SELECT swept_to FROM tiers_sweep WHERE prefix = ?1")?;
    let mut out = Vec::new();
    let mut from = String::new();
    while let Some(id) = next.query_row([&from], |r| r.get::<_, String>(0)).optional()? {
        let Some((prefix, _)) = id.split_once('_') else {
            // The next id after this one: NUL sorts below every character.
            from = format!("{id}\0");
            continue;
        };
        let swept_to = cursor
            .query_row([prefix], |r| r.get::<_, String>(0))
            .optional()?
            .filter(|s| !s.is_empty());
        let sweep = Sweep {
            prefix: prefix.to_string(),
            swept_to,
        };
        from = sweep.before();
        out.push(sweep);
    }
    Ok(out)
}

/// The next stretch of a prefix past its cursor.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Window {
    /// The window's last id, where the cursor goes once its matches are
    /// stamped; `None` when nothing is past the cursor.
    pub last: Option<String>,
    /// Whether the window holds as many ids as were asked for, so more may
    /// follow.
    pub full: bool,
    /// Its ranked matches with a fact unstamped, in id order, with their
    /// queue ids.
    pub todo: Vec<(String, i64)>,
}

/// The window's end, from the key alone. Binds `?1` after, `?2` before, `?3`
/// the window's size.
pub(crate) const WINDOW_END: &str = "SELECT max(match_id), count(*) FROM
       (SELECT match_id FROM matches WHERE match_id > ?1 AND match_id < ?2 ORDER BY match_id LIMIT ?3)";

/// The window's unstamped ranked matches: a row of `matches` for each id in
/// it (`queue_id`), and its facts by key. Binds `?1` after, `?2` the window's
/// last id.
pub(crate) const WINDOW_TODO: &str = "SELECT m.match_id, m.queue_id FROM matches m
     WHERE m.match_id > ?1 AND m.match_id <= ?2 AND m.queue_id IN (420, 440)
       AND EXISTS (SELECT 1 FROM match_facts f WHERE f.match_id = m.match_id AND f.tier IS NULL)
     ORDER BY m.match_id";

/// The first `size` ids after `after` and before `before`, and those of them
/// `tiers:backfill` has to stamp. Reads at most `size` rows of `matches`.
pub fn window(c: &Connection, after: &str, before: &str, size: i64) -> rusqlite::Result<Window> {
    let (last, n): (Option<String>, i64) = c
        .prepare_cached(WINDOW_END)?
        .query_row(params![after, before, size], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let Some(last) = last else {
        return Ok(Window::default());
    };
    let todo = c
        .prepare_cached(WINDOW_TODO)?
        .query_map(params![after, last], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(Window {
        last: Some(last),
        full: n >= size,
        todo,
    })
}

/// Move `prefix`'s cursor to `swept_to`, in the transaction that stamped
/// every match up to it. A cursor never moves back: two sweeps passing one
/// prefix (`tiers:backfill`'s and `aggregate:delta`'s) keep the further one.
pub fn advance(c: &Connection, prefix: &str, swept_to: &str, now: i64) -> rusqlite::Result<()> {
    c.prepare_cached(
        "INSERT INTO tiers_sweep (prefix, swept_to, swept_at) VALUES (?1, ?2, ?3)
         ON CONFLICT (prefix) DO UPDATE SET swept_to = max(swept_to, excluded.swept_to),
           swept_at = excluded.swept_at",
    )?
    .execute(params![prefix, swept_to, now])?;
    Ok(())
}

/// Why `tiers:backfill` has work to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Due {
    /// A prefix no sweep has passed: a new archive or platform, or a reset
    /// cursor.
    Unswept(String),
    /// A ranked match past the prefix's cursor has a fact with no tier.
    Unstamped(String),
    /// More ids are past the prefix's cursor than the check reads; a sweep
    /// moves the cursor up.
    Behind(String),
}

impl Due {
    /// The prefix, and why, for the log.
    pub fn parts(&self) -> (&str, &'static str) {
        match self {
            Self::Unswept(p) => (p, "never swept"),
            Self::Unstamped(p) => (p, "unstamped facts past the cursor"),
            Self::Behind(p) => (p, "cursor behind"),
        }
    }
}

/// Whether `tiers:backfill` has anything to do (the boot check, ADR-213): a
/// prefix never swept, a ranked match with an unstamped fact among the first
/// `tail` ids past a prefix's cursor, or more than `tail` ids past it. Reads
/// [`sweeps`] and at most `tail` rows of `matches` a prefix, never an id
/// below a cursor.
pub fn sweep_due(c: &Connection, tail: i64) -> rusqlite::Result<Option<Due>> {
    for s in sweeps(c)? {
        if s.swept_to.is_none() {
            return Ok(Some(Due::Unswept(s.prefix)));
        }
        let w = window(c, &s.after(), &s.before(), tail)?;
        if !w.todo.is_empty() {
            return Ok(Some(Due::Unstamped(s.prefix)));
        }
        if w.full {
            return Ok(Some(Due::Behind(s.prefix)));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests;
