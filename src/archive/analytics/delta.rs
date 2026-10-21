//! Incremental analytics (THR-03, ADR-182): the matches each rebuild step has
//! counted, and `aggregate:delta`'s batches, which count the rest a few
//! dozen at a time instead of rebuilding a ladder.
//!
//! - `analytics_counted.bits` holds one bit per rebuild step ([`STEPS`]) for
//!   each `(match, key scope)`. A rebuild step sets its bit for every match it
//!   read, in its own transaction; a match is written with `bits = 0` when its
//!   facts are ([`on_facts_written`]); a delta batch counts each match's
//!   missing steps and sets all six, in one transaction.
//! - A tier that moves after a step counted it (`late_stamp`, `tiers::stamp`)
//!   leaves an `analytics_retract` row per counting step with the tier it was
//!   counted at (V0029's trigger). A batch takes that count back and adds the
//!   match again at its tier now.
//! - A match's first `match_builds` rows clear only its set-build bit
//!   ([`on_builds_written`]); anything else that rewrites a counted match's
//!   inputs marks its ladder dirty ([`mark_dirty`]), and the ladder gets no
//!   delta until a full rebuild has stamped it again ([`mark_stamped`]).
//!
//! Every analytics table is a sum over disjoint matches (plan §3), so a
//! batch's rows add to the table's: a delta's output is a rebuild's.

use std::collections::BTreeMap;
use std::time::Instant;

use rusqlite::{Connection, OptionalExtension, params};

use super::{ALL_STEPS, Facts, REPORTED, SETS_STEP, STEPS, Scope, Stmt, totals_from};
use crate::archive::facts::FACTS_VERSION;
use crate::db::DbError;
use crate::riot::routing::Platform;

/// The first batch of a run, before its size follows the measured time.
pub const FIRST_BATCH: usize = 50;

/// The smallest batch the sizing goes down to.
pub const MIN_BATCH: usize = 5;

/// The next batch's size (plan §1.3): this one's scaled to the target time
/// by its statements' time (`sql_s`, the commit left out, since a checkpoint
/// in it says nothing about the batch), at most doubled, within
/// [`MIN_BATCH`] and `cap`.
pub fn next_batch(n: usize, sql_s: f64, target_ms: u32, cap: usize) -> usize {
    let cap = cap.max(MIN_BATCH);
    let ms = (sql_s * 1000.0).max(1.0);
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    let want = ((n.max(1) as f64) * f64::from(target_ms) / ms).round() as usize;
    want.min(n.max(MIN_BATCH) * 2).clamp(MIN_BATCH, cap)
}

/// `(lowest, highest)` exclusive bounds of a platform's match ids: every id
/// that starts with `OC1_`, as `LATE_STAMP` reads them (`tiers.rs`).
fn id_range(platform: &str) -> (String, String) {
    let p = platform.to_uppercase();
    (format!("{p}_"), format!("{p}`"))
}

// ── Stamps written by the rebuild and the archive ──────────────────────────

/// Set step `bit` on every match a rebuild step for `s` read, and drop that
/// step's retract rows for them: it has just counted them at their tiers now.
/// The match set is the step's (`ladder_facts`' platform, queue, patches and
/// key scope), read by id range. In the step's transaction.
pub(crate) fn stamp_step(c: &Connection, s: &Scope, bit: i64) -> Result<(), DbError> {
    // Each statement names ?6, so it takes all six of `insert`'s parameters.
    s.insert(
        c,
        &format!(
            "INSERT INTO analytics_counted (match_id, key_scope, bits)
             SELECT m.match_id, ?1, {bit} FROM matches m
              WHERE m.match_id > upper(?2) || '_' AND m.match_id < upper(?2) || '`'
                AND m.queue_id = ?4 AND m.patch IS NOT NULL
                AND (?5 IS NULL OR m.patch IN (SELECT value FROM json_each(?5)))
                AND EXISTS (SELECT 1 FROM match_facts f WHERE f.match_id = m.match_id AND f.key_scope = ?1)
                AND ?6 IS NOT NULL
             ON CONFLICT (match_id, key_scope) DO UPDATE SET bits = bits | {bit} WHERE bits & {bit} = 0"
        ),
    )?;
    s.insert(
        c,
        &format!(
            "DELETE FROM analytics_retract WHERE key_scope = ?1 AND step = {bit} AND ?6 IS NOT NULL
               AND match_id IN (
                 SELECT m.match_id FROM matches m
                  WHERE m.match_id IN (SELECT r.match_id FROM analytics_retract r
                                        WHERE r.key_scope = ?1 AND r.step = {bit})
                    AND m.match_id > upper(?2) || '_' AND m.match_id < upper(?2) || '`'
                    AND m.queue_id = ?4 AND m.patch IS NOT NULL
                    AND (?5 IS NULL OR m.patch IN (SELECT value FROM json_each(?5))))"
        ),
    )?;
    Ok(())
}

/// After a match's facts are written for `key_scope` (archive, re-archive,
/// `facts:reextract`): a solo or flex match gets its row, uncounted; and if
/// a step had counted it from facts of an older `FACTS_VERSION`
/// (`old_version`, before this write), its ladder is dirty (plan §4.3).
pub fn on_facts_written(
    c: &Connection,
    match_id: &str,
    key_scope: &str,
    old_version: Option<i64>,
    now: i64,
) -> rusqlite::Result<()> {
    c.prepare_cached(
        "INSERT OR IGNORE INTO analytics_counted (match_id, key_scope, bits)
         SELECT ?1, ?2, 0 FROM matches WHERE match_id = ?1 AND queue_id IN (420, 440)",
    )?
    .execute(params![match_id, key_scope])?;
    if old_version.is_none_or(|v| v < FACTS_VERSION) && bits(c, match_id, key_scope)? != 0 {
        mark_dirty(c, match_id, key_scope, now)?;
    }
    Ok(())
}

/// After `builds::write` replaced a match's `match_builds` (BLD-01). Its
/// first rows (`had_rows` false) only add set builds, which no step counted
/// (nothing to count): the set-build bit is cleared, so the next delta adds
/// them. Rows replacing others (a `BUILDS_VERSION` bump) mark the ladder
/// dirty if the set builds had counted them.
pub fn on_builds_written(
    c: &Connection,
    match_id: &str,
    key_scope: &str,
    had_rows: bool,
    now: i64,
) -> rusqlite::Result<()> {
    if !had_rows {
        c.prepare_cached(&format!(
            "UPDATE analytics_counted SET bits = bits & ~{SETS_STEP}
              WHERE match_id = ?1 AND key_scope = ?2 AND bits & {SETS_STEP}"
        ))?
        .execute(params![match_id, key_scope])?;
        c.prepare_cached(&format!(
            "DELETE FROM analytics_retract WHERE match_id = ?1 AND key_scope = ?2 AND step = {SETS_STEP}"
        ))?
        .execute(params![match_id, key_scope])?;
    } else if bits(c, match_id, key_scope)? & SETS_STEP != 0 {
        mark_dirty(c, match_id, key_scope, now)?;
    }
    Ok(())
}

/// A match's counted steps; 0 without a row.
pub fn bits(c: &Connection, match_id: &str, key_scope: &str) -> rusqlite::Result<i64> {
    Ok(
        c.prepare_cached("SELECT bits FROM analytics_counted WHERE match_id = ?1 AND key_scope = ?2")?
            .query_row(params![match_id, key_scope], |r| r.get(0))
            .optional()?
            .unwrap_or(0),
    )
}

/// The match's ladder no longer agrees with its stamps: no delta for it
/// until a full rebuild that started after `now` stamps it again. A match on
/// no ranked ladder marks nothing.
pub fn mark_dirty(c: &Connection, match_id: &str, key_scope: &str, now: i64) -> rusqlite::Result<()> {
    let queue_id: Option<i64> = c
        .prepare_cached("SELECT queue_id FROM matches WHERE match_id = ?1")?
        .query_row([match_id], |r| r.get(0))
        .optional()?;
    let (Some(platform), Some(queue)) = (
        Platform::from_match_id(match_id),
        queue_id.and_then(crate::archive::tiers::ranked_queue),
    ) else {
        return Ok(());
    };
    c.prepare_cached(
        "INSERT INTO analytics_stamped (key_scope, platform, queue, stamped_at, dirty_at) VALUES (?1, ?2, ?3, NULL, ?4)
         ON CONFLICT (key_scope, platform, queue) DO UPDATE SET dirty_at = excluded.dirty_at",
    )?
    .execute(params![key_scope, platform.as_str(), queue, now])?;
    Ok(())
}

/// A full rebuild that started at `started` has stamped every match of the
/// ladder: deltas may run for it, unless something marked it dirty since.
pub fn mark_stamped(
    c: &Connection,
    key_scope: &str,
    platform: &str,
    queue: &str,
    started: i64,
    now: i64,
) -> rusqlite::Result<()> {
    c.execute(
        "INSERT INTO analytics_stamped (key_scope, platform, queue, stamped_at, dirty_at) VALUES (?1, ?2, ?3, ?5, NULL)
         ON CONFLICT (key_scope, platform, queue) DO UPDATE SET stamped_at = excluded.stamped_at, dirty_at = NULL
          WHERE dirty_at IS NULL OR dirty_at < ?4",
        params![key_scope, platform, queue, started, now],
    )?;
    Ok(())
}

/// Whether deltas may run for the ladder: a full rebuild stamped it and
/// nothing has marked it dirty since.
pub fn ready(c: &Connection, key_scope: &str, platform: &str, queue: &str) -> rusqlite::Result<bool> {
    Ok(c.query_row(
        "SELECT 1 FROM analytics_stamped WHERE key_scope = ?1 AND platform = ?2 AND queue = ?3
               AND stamped_at IS NOT NULL AND dirty_at IS NULL",
        params![key_scope, platform, queue],
        |_| Ok(()),
    )
    .optional()?
    .is_some())
}

/// The key scope's ladders deltas may run for, any platform: a crawl started
/// by hand on a platform outside `LADDER_PLATFORMS` is stamped by its
/// rebuild and counted from then on.
pub fn ready_ladders(c: &Connection, key_scope: &str) -> rusqlite::Result<Vec<(String, String)>> {
    c.prepare_cached(
        "SELECT platform, queue FROM analytics_stamped
          WHERE key_scope = ?1 AND stamped_at IS NOT NULL AND dirty_at IS NULL ORDER BY platform, queue",
    )?
    .query_map([key_scope], |r| Ok((r.get(0)?, r.get(1)?)))?
    .collect()
}

/// Matches waiting for a delta, every ladder: rows with a step missing.
pub fn uncounted(c: &Connection, key_scope: &str) -> rusqlite::Result<i64> {
    c.query_row(
        "SELECT count(*) FROM analytics_counted WHERE key_scope = ?1 AND bits <> 63",
        [key_scope],
        |r| r.get(0),
    )
}

// ── The delta ───────────────────────────────────────────────────────────────

/// Up to `limit` of a ladder's uncounted matches after `after` (empty: from
/// the start), in id order, whose facts are at the current `FACTS_VERSION`;
/// a stale match waits for `facts:reextract`.
pub fn pending(
    c: &Connection,
    key_scope: &str,
    platform: &str,
    queue_id: i64,
    after: &str,
    limit: usize,
) -> rusqlite::Result<Vec<String>> {
    let (lo, hi) = id_range(platform);
    let lo = if after > lo.as_str() {
        after.to_string()
    } else {
        lo
    };
    let mut stmt = c.prepare_cached(
        "SELECT c.match_id FROM analytics_counted c JOIN matches m ON m.match_id = c.match_id
          WHERE c.key_scope = ?1 AND c.bits <> 63 AND c.match_id > ?2 AND c.match_id < ?3
            AND m.queue_id = ?4 AND m.facts_version >= ?5
          ORDER BY c.match_id LIMIT ?6",
    )?;
    stmt.query_map(
        params![
            key_scope,
            lo,
            hi,
            queue_id,
            FACTS_VERSION,
            i64::try_from(limit).unwrap_or(i64::MAX)
        ],
        |r| r.get(0),
    )?
    .collect()
}

/// Up to `limit` of a ladder's matches with a count to take back, in id order.
pub fn retracting(
    c: &Connection,
    key_scope: &str,
    platform: &str,
    queue_id: i64,
    limit: usize,
) -> rusqlite::Result<Vec<String>> {
    let (lo, hi) = id_range(platform);
    let mut stmt = c.prepare_cached(
        "SELECT DISTINCT r.match_id FROM analytics_retract r JOIN matches m ON m.match_id = r.match_id
          WHERE r.key_scope = ?1 AND r.match_id > ?2 AND r.match_id < ?3
            AND m.queue_id = ?4 AND m.facts_version >= ?5
          ORDER BY r.match_id LIMIT ?6",
    )?;
    stmt.query_map(
        params![
            key_scope,
            lo,
            hi,
            queue_id,
            FACTS_VERSION,
            i64::try_from(limit).unwrap_or(i64::MAX)
        ],
        |r| r.get(0),
    )?
    .collect()
}

/// What one batch did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Counted {
    /// False when the ladder wasn't ready: nothing was written.
    pub ready: bool,
    /// Matches with a step counted.
    pub added: i64,
    /// Matches with a count taken back first.
    pub retracted: i64,
    /// Rows upserted per [`REPORTED`] table.
    pub rows: BTreeMap<&'static str, i64>,
    /// The batch's statements, and its `COMMIT`.
    pub sql_s: f64,
    pub commit_s: f64,
}

/// `match_facts`' columns, for a batch's copy with the counted tiers.
const FACT_COLUMNS: &str = "f.match_id, f.key_scope, f.puuid, f.team_id, f.position, f.champion_id, f.win, f.kills, \
     f.deaths, f.assists, f.items, f.runes, f.summoners, f.facts_version, f.cs, f.gold, f.damage, f.vision";

/// A table's key and summed columns, from its schema.
struct Columns {
    key: Vec<String>,
    sums: Vec<String>,
}

fn columns(c: &Connection, table: &str) -> rusqlite::Result<Columns> {
    let mut stmt = c.prepare_cached("SELECT name, pk FROM pragma_table_info(?1) ORDER BY cid")?;
    let rows = stmt
        .query_map([table], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut key: Vec<(i64, String)> = rows
        .iter()
        .filter(|(_, pk)| *pk > 0)
        .map(|(n, pk)| (*pk, n.clone()))
        .collect();
    key.sort();
    Ok(Columns {
        key: key.into_iter().map(|(_, n)| n).collect(),
        sums: rows
            .into_iter()
            .filter(|(n, pk)| *pk == 0 && n != "computed_at")
            .map(|(n, _)| n)
            .collect(),
    })
}

impl Columns {
    fn key(&self) -> String {
        self.key.join(", ")
    }

    /// `ON CONFLICT … DO UPDATE` adding the row's counts to the table's.
    fn upsert(&self) -> String {
        let add = self
            .sums
            .iter()
            .map(|s| format!("{s} = {s} + excluded.{s}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "ON CONFLICT ({}) DO UPDATE SET {add}, computed_at = excluded.computed_at",
            self.key()
        )
    }
}

/// How a batch writes one statement's rows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Sign {
    /// Straight into the table, adding to its rows (no count taken back in
    /// this batch, the common case).
    Upsert,
    /// Into the batch's staging copy, merged once every step has run.
    Stage,
    /// Into the staging copy negated: a count taken back.
    Minus,
}

/// Every table a step writes, with its staging copy's name.
fn staged(table: &str) -> String {
    format!("temp.delta_{table}")
}

/// Count `ids` for the ladder `s` in one transaction (plan §7): each match's
/// missing steps from its facts now, and for a match whose tier moved since
/// a step counted it, that count taken back first; then all six bits set and
/// its retract rows gone. Nothing is written if the ladder isn't ready.
pub fn count_batch(c: &mut Connection, s: &Scope, ids: &[String]) -> Result<Counted, DbError> {
    let started = Instant::now();
    let tx = c.transaction()?;
    if !ready(&tx, &s.key_scope, &s.platform, &s.queue)? {
        return Ok(Counted::default());
    }
    let tables: Vec<&'static str> = STEPS.iter().flat_map(|st| st.tables.iter().copied()).collect();
    let mut drop = String::from(
        "DROP TABLE IF EXISTS temp.delta_batch; DROP TABLE IF EXISTS temp.delta_facts;
         DROP TABLE IF EXISTS temp.delta_old; DROP TABLE IF EXISTS temp.delta_neg;",
    );
    for t in &tables {
        drop.push_str(&format!("DROP TABLE IF EXISTS {};", staged(t)));
    }
    tx.execute_batch(&drop)?;
    // Each batch match: its bits, the steps it has a count to take back for
    // (only ones that counted it), and the steps to count now.
    tx.execute_batch(
        "CREATE TEMP TABLE delta_batch (match_id TEXT PRIMARY KEY, bits INTEGER NOT NULL,
           sub INTEGER NOT NULL DEFAULT 0, todo INTEGER NOT NULL DEFAULT 0)",
    )?;
    let json = serde_json::to_string(ids).unwrap_or_else(|_| "[]".into());
    tx.execute(
        "INSERT OR IGNORE INTO temp.delta_batch (match_id, bits)
         SELECT j.value, coalesce(c.bits, 0) FROM json_each(?2) j
           LEFT JOIN analytics_counted c ON c.match_id = j.value AND c.key_scope = ?1",
        params![s.key_scope, json],
    )?;
    tx.execute(
        "UPDATE temp.delta_batch SET
           sub = bits & coalesce((SELECT sum(DISTINCT r.step) FROM analytics_retract r
                                   WHERE r.match_id = delta_batch.match_id AND r.key_scope = ?1), 0)",
        [&s.key_scope],
    )?;
    tx.execute_batch(&format!(
        "UPDATE temp.delta_batch SET todo = (~bits & {ALL_STEPS}) | sub"
    ))?;
    let (added, retracted): (i64, i64) = tx.query_row(
        "SELECT count(*) FILTER (WHERE todo <> 0), count(*) FILTER (WHERE sub <> 0) FROM temp.delta_batch",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    // The facts, read by key: without the CROSS JOIN the planner scanned
    // match_facts for them (2.2 s a batch, plan §7).
    tx.execute(
        "CREATE TEMP TABLE delta_facts AS SELECT f.* FROM temp.delta_batch b
           CROSS JOIN match_facts f ON f.match_id = b.match_id AND f.key_scope = ?1
          WHERE b.todo <> 0",
        [&s.key_scope],
    )?;
    let cols: BTreeMap<&str, Columns> = tables
        .iter()
        .map(|t| Ok((*t, columns(&tx, t)?)))
        .collect::<rusqlite::Result<_>>()?;
    for t in &tables {
        tx.execute_batch(&format!(
            "CREATE TEMP TABLE delta_{t} AS SELECT * FROM main.{t} WHERE 0"
        ))?;
    }
    let taking_back = retracted > 0;
    let mut rows: BTreeMap<&'static str, i64> = REPORTED.iter().map(|t| (*t, 0)).collect();
    for step in &STEPS {
        let bit = step.bit;
        let any_sub: bool = tx.query_row(
            &format!("SELECT EXISTS (SELECT 1 FROM temp.delta_batch WHERE sub & {bit})"),
            [],
            |r| r.get(0),
        )?;
        if any_sub {
            // The facts as this step counted them: a moved player at the tier
            // its retract row recorded.
            tx.execute(
                &format!(
                    "CREATE TEMP TABLE delta_old AS
                     SELECT {FACT_COLUMNS}, CASE WHEN r.match_id IS NOT NULL THEN r.tier ELSE f.tier END AS tier
                       FROM temp.delta_facts f
                       LEFT JOIN analytics_retract r ON r.match_id = f.match_id AND r.key_scope = f.key_scope
                        AND r.puuid = f.puuid AND r.step = {bit}
                      WHERE f.match_id IN (SELECT match_id FROM temp.delta_batch WHERE sub & {bit})"
                ),
                [],
            )?;
            let old = Facts {
                table: "temp.delta_old",
                only: "",
            };
            run(&tx, s, (step.sql)(old), Sign::Minus, &cols, &mut rows)?;
            tx.execute_batch("DROP TABLE temp.delta_old")?;
        }
        let only = format!(" AND f.match_id IN (SELECT match_id FROM temp.delta_batch WHERE todo & {bit})");
        let now = Facts {
            table: "temp.delta_facts",
            only: &only,
        };
        let sign = if taking_back { Sign::Stage } else { Sign::Upsert };
        run(&tx, s, (step.sql)(now), sign, &cols, &mut rows)?;
    }
    // The totals are the batch's stats rows, either sign, summed over tier.
    s.insert(
        &tx,
        &totals_from("temp.delta_champion_stats").replacen(
            "INSERT INTO champion_stats_totals",
            "INSERT INTO temp.delta_champion_stats_totals",
            1,
        ),
    )?;
    for t in &tables {
        let staged_rows: i64 =
            tx.query_row(&format!("SELECT count(*) FROM {}", staged(t)), [], |r| r.get(0))?;
        if staged_rows == 0 {
            continue;
        }
        let col = &cols[t];
        let (key, sums) = (col.key(), &col.sums);
        let summed = sums
            .iter()
            .map(|x| format!("sum({x})"))
            .collect::<Vec<_>>()
            .join(", ");
        let n = tx.execute(
            &format!(
                "INSERT INTO {t} ({key}, {}, computed_at)
                 SELECT {key}, {summed}, max(computed_at) FROM {} GROUP BY {key}
                 {}",
                sums.join(", "),
                staged(t),
                col.upsert()
            ),
            [],
        )?;
        if let Some(r) = rows.get_mut(t) {
            *r += i64::try_from(n).unwrap_or(0);
        }
        if taking_back {
            // A rebuild never writes a row that counts nothing.
            let count = sums.first().map_or("games", String::as_str);
            tx.execute(
                &format!(
                    "DELETE FROM {t} WHERE {count} <= 0 AND ({key}) IN (SELECT {key} FROM {})",
                    staged(t)
                ),
                [],
            )?;
        }
    }
    tx.execute(
        &format!(
            "INSERT INTO analytics_counted (match_id, key_scope, bits)
             SELECT match_id, ?1, {ALL_STEPS} FROM temp.delta_batch WHERE true
             ON CONFLICT (match_id, key_scope) DO UPDATE SET bits = {ALL_STEPS}"
        ),
        [&s.key_scope],
    )?;
    tx.execute(
        "DELETE FROM analytics_retract WHERE key_scope = ?1
           AND match_id IN (SELECT match_id FROM temp.delta_batch)",
        [&s.key_scope],
    )?;
    tx.execute_batch(&drop)?;
    let sql_s = started.elapsed().as_secs_f64();
    let committing = Instant::now();
    tx.commit()?;
    Ok(Counted {
        ready: true,
        added,
        retracted,
        rows,
        sql_s,
        commit_s: committing.elapsed().as_secs_f64(),
    })
}

/// Run one step's statements for a batch.
fn run(
    c: &Connection,
    s: &Scope,
    stmts: Vec<Stmt>,
    sign: Sign,
    cols: &BTreeMap<&str, Columns>,
    rows: &mut BTreeMap<&'static str, i64>,
) -> Result<(), DbError> {
    for stmt in stmts {
        match stmt {
            Stmt::Exec(sql) if sql.contains("?6") => {
                s.insert(c, &sql)?;
            }
            Stmt::Exec(sql) => c.execute_batch(&sql)?,
            // Summed from the staged stats once every step has run.
            Stmt::Totals => {}
            Stmt::Into {
                table,
                cols: list,
                select,
            } => {
                let col = &cols[table];
                // The stats are always staged: the totals are summed from them.
                let sign = if table == "champion_stats" && sign == Sign::Upsert {
                    Sign::Stage
                } else {
                    sign
                };
                match sign {
                    Sign::Upsert => {
                        let n = s.insert(
                            c,
                            &format!("INSERT INTO {table} ({list}) {select} {}", col.upsert()),
                        )?;
                        if let Some(r) = rows.get_mut(table) {
                            *r += n;
                        }
                    }
                    Sign::Stage => {
                        s.insert(c, &format!("INSERT INTO {} ({list}) {select}", staged(table)))?;
                    }
                    Sign::Minus => {
                        c.execute_batch(&format!(
                            "DROP TABLE IF EXISTS temp.delta_neg;
                             CREATE TEMP TABLE delta_neg AS SELECT * FROM main.{table} WHERE 0"
                        ))?;
                        s.insert(c, &format!("INSERT INTO temp.delta_neg ({list}) {select}"))?;
                        let key = col.key();
                        let neg = col
                            .sums
                            .iter()
                            .map(|x| format!("-{x}"))
                            .collect::<Vec<_>>()
                            .join(", ");
                        c.execute_batch(&format!(
                            "INSERT INTO {} ({key}, {}, computed_at)
                             SELECT {key}, {neg}, computed_at FROM temp.delta_neg;
                             DROP TABLE temp.delta_neg",
                            staged(table),
                            col.sums.join(", ")
                        ))?;
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
