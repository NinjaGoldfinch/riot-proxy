#![allow(clippy::unwrap_used, clippy::expect_used)]
//! THR-03: a delta's output is a rebuild's, table by table, however the
//! matches are batched, interleaved with a rebuild, or moved between tiers
//! after they were counted. Each test builds its matches from a seeded
//! generator in the shape of the hand-computed fixture (`../tests.rs`): two
//! teams of five, lanes, a mirror now and then, a shared lane, players with
//! no position, `UNKNOWN` and unstamped tiers, remakes, flex games, another
//! platform's games, items, runes, spells, bans and set builds.

use proptest::prelude::*;
use rusqlite::Connection;

use super::*;
use crate::archive::analytics::{STEPS, Scope, rebuild_step};
use crate::archive::tiers;

#[test]
fn a_batch_follows_its_measured_time() {
    // 50 matches in 300 ms against a 150 ms target: half as many next.
    assert_eq!(next_batch(50, 0.300, 150, 500), 25);
    // A fast batch grows, but at most doubles.
    assert_eq!(next_batch(50, 0.010, 150, 500), 100);
    // Never under the floor, never over the cap.
    assert_eq!(next_batch(5, 5.0, 150, 500), MIN_BATCH);
    assert_eq!(next_batch(400, 0.001, 150, 500), 500);
    // A cap below the floor is the floor.
    assert_eq!(next_batch(50, 0.1, 150, 1), MIN_BATCH);
}

fn conn() -> Connection {
    let mut c = Connection::open_in_memory().unwrap();
    c.execute_batch("PRAGMA foreign_keys = ON").unwrap();
    crate::db::migrate(&mut c).unwrap();
    c
}

fn kr() -> Scope {
    Scope {
        key_scope: "s".into(),
        platform: "kr".into(),
        queue: "RANKED_SOLO_5x5".into(),
        queue_id: 420,
        patches: None,
        now: 7,
    }
}

/// xorshift64*: the same matches for the same seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: i64) -> i64 {
        i64::try_from(self.next() % u64::try_from(n).unwrap()).unwrap()
    }

    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[usize::try_from(self.below(i64::try_from(xs.len()).unwrap())).unwrap()]
    }
}

const POSITIONS: [&str; 5] = ["TOP", "JUNGLE", "MIDDLE", "BOTTOM", "UTILITY"];

/// One generated match, archived with its facts at the current version, its
/// tier stamps as given, its bans and set builds, and its `analytics_counted`
/// row (as `write_derived` writes it).
fn add_match(c: &Connection, rng: &mut Rng, id: &str) {
    let patch = *rng.pick(&["14.17", "14.18"]);
    let queue = if rng.below(10) == 0 { 440 } else { 420 };
    let remake = i64::from(rng.below(20) == 0);
    c.execute(
        "INSERT INTO matches (match_id, region, patch, queue_id, game_end_ms, body_zstd, body_size, archived_at,
           game_duration, remake, facts_version) VALUES (?1, 'asia', ?2, ?3, 10, x'00', 1, 1, ?4, ?5, ?6)",
        params![id, patch, queue, 600 + rng.below(1800), remake, FACTS_VERSION],
    )
    .unwrap();
    let winner = if rng.below(2) == 0 { 100 } else { 200 };
    for p in 0..10_i64 {
        let team = if p < 5 { 100 } else { 200 };
        let position = match rng.below(10) {
            0 => None,
            1 => Some(POSITIONS[usize::try_from((p + 1) % 5).unwrap()]),
            _ => Some(POSITIONS[usize::try_from(p % 5).unwrap()]),
        };
        // A small pool, so rows collide across matches and mirrors happen.
        let champion = 1 + rng.below(6);
        let tier = *rng.pick(&[
            Some("MASTER"),
            Some("MASTER"),
            Some("GRANDMASTER"),
            Some("DIAMOND"),
            Some("UNKNOWN"),
            Some("UNKNOWN"),
            None,
        ]);
        let kda = (rng.below(20) != 0).then(|| (rng.below(10), rng.below(10), rng.below(10)));
        let items: Vec<i64> = (0..6).map(|_| *rng.pick(&[0, 3157, 3089, 6655, 3020])).collect();
        let runes = if rng.below(20) == 0 {
            "{}".to_string()
        } else {
            format!(
                "[8000,{},{},[],null]",
                rng.pick(&[8112, 8010]),
                rng.pick(&[8300, 8400])
            )
        };
        let spells = *rng.pick(&["[4,14]", "[14,4]", "[4,12]"]);
        let puuid = format!("P{}", rng.below(40));
        let cs = rng.below(300);
        c.execute(
            "INSERT OR IGNORE INTO match_facts (match_id, key_scope, puuid, team_id, position, champion_id, win,
               kills, deaths, assists, cs, gold, damage, vision, items, runes, summoners, facts_version, tier)
             VALUES (?1, 's', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10 * 50, ?10 * 100, ?10 / 10, ?11, ?12, ?13, ?14, ?15)",
            params![
                id,
                puuid,
                team,
                position,
                champion,
                team == winner,
                kda.map(|k| k.0),
                kda.map(|k| k.1),
                kda.map(|k| k.2),
                cs,
                serde_json::to_string(&items).unwrap(),
                runes,
                spells,
                FACTS_VERSION,
                tier
            ],
        )
        .unwrap();
        if rng.below(5) != 0 {
            let n = 1 + rng.below(4);
            let built: Vec<i64> = (0..n).map(|_| *rng.pick(&[6655, 3157, 3089, 4645])).collect();
            c.execute(
                "INSERT OR IGNORE INTO match_builds (match_id, key_scope, puuid, starter, boots, items, skills,
                   skill_order, builds_version) VALUES (?1, 's', ?2, ?3, ?4, ?5, 'QWE', ?6, 3)",
                params![
                    id,
                    puuid,
                    *rng.pick(&["[1056,2003]", "[]"]),
                    *rng.pick(&[Some(3020), None]),
                    serde_json::to_string(&built).unwrap(),
                    *rng.pick(&[Some("QWE"), Some("QEW"), None])
                ],
            )
            .unwrap();
        }
    }
    for turn in 0..rng.below(4) {
        c.execute(
            "INSERT OR IGNORE INTO match_bans (match_id, team_id, pick_turn, champion_id) VALUES (?1, ?2, ?3, ?4)",
            params![id, if turn % 2 == 0 { 100 } else { 200 }, turn, 1 + rng.below(6)],
        )
        .unwrap();
    }
    on_facts_written(c, id, "s", None, 1).unwrap();
}

/// `n` matches from `seed`, every tenth on another platform.
fn add_matches(c: &Connection, seed: u64, from: usize, n: usize) {
    let mut rng = Rng(seed.wrapping_mul(2_654_435_761).max(1));
    for i in from..from + n {
        let id = if i % 10 == 9 {
            format!("EUW1_{i}")
        } else {
            format!("KR_{i}")
        };
        add_match(c, &mut rng, &id);
    }
}

/// Every analytics table's rows, `computed_at` left out.
fn snapshot(c: &Connection) -> Vec<(String, Vec<String>)> {
    let mut out = vec![];
    for t in STEPS.iter().flat_map(|s| s.tables.iter()) {
        let cols: Vec<String> = c
            .prepare(&format!(
                "SELECT name FROM pragma_table_info('{t}') WHERE name <> 'computed_at' ORDER BY cid"
            ))
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let row = cols
            .iter()
            .map(|c| format!("quote({c})"))
            .collect::<Vec<_>>()
            .join(" || '|' || ");
        let rows = c
            .prepare(&format!("SELECT {row} FROM {t} ORDER BY 1"))
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        out.push(((*t).to_string(), rows));
    }
    out
}

/// The given rebuild steps (indexes into [`STEPS`]), each its own transaction.
fn rebuild_steps(c: &mut Connection, steps: std::ops::Range<usize>) {
    for step in &STEPS[steps] {
        let tx = c.transaction().unwrap();
        rebuild_step(&tx, &kr(), step).unwrap();
        tx.commit().unwrap();
    }
}

/// A full rebuild, then the ladder stamped.
fn rebuild(c: &mut Connection) {
    rebuild_steps(c, 0..STEPS.len());
    mark_stamped(c, "s", "kr", "RANKED_SOLO_5x5", 0, 1).unwrap();
}

/// Count every pending match in batches cycling through `sizes`; the batches run.
/// A batch that leaves its matches pending fails here rather than looping.
fn delta(c: &mut Connection, sizes: &[usize]) -> usize {
    let mut n = 0;
    loop {
        assert!(n < 1_000, "the pending matches never ran out");
        let size = sizes[n % sizes.len()];
        let mut ids = retracting(c, "s", "kr", 420, size).unwrap();
        for id in pending(c, "s", "kr", 420, "", size).unwrap() {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        if ids.is_empty() {
            return n;
        }
        let done = count_batch(c, &kr(), &ids).unwrap();
        assert!(done.ready, "the ladder is stamped");
        n += 1;
    }
}

/// A fresh database: `n` matches from `seed`, the ladder stamped with nothing
/// counted yet (as a fresh install's first rebuild leaves it).
fn fresh(seed: u64, n: usize) -> Connection {
    let mut c = conn();
    rebuild(&mut c);
    add_matches(&c, seed, 0, n);
    c
}

/// What a rebuild makes of the same archive now, on the same connection:
/// the rebuild replaces every row, so it is the reference.
fn rebuilt(c: &mut Connection) -> Vec<(String, Vec<String>)> {
    rebuild(c);
    snapshot(c)
}

fn bits_of(c: &Connection) -> Vec<(String, i64)> {
    c.prepare("SELECT match_id, bits FROM analytics_counted ORDER BY match_id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]

    /// The brief's property: random matches in random batches add up to the
    /// rebuild, all sixteen tables; a delta after it then finds nothing.
    #[test]
    fn a_delta_in_any_batches_equals_a_rebuild(
        seed in 1u64..u64::MAX,
        n in 1usize..30,
        sizes in proptest::collection::vec(1usize..9, 1..5),
    ) {
        let mut c = fresh(seed, n);
        delta(&mut c, &sizes);
        let counted = snapshot(&c);
        prop_assert_eq!(&counted, &rebuilt(&mut c));
        prop_assert_eq!(delta(&mut c, &[7]), 0, "rebuild then delta: nothing left");
        prop_assert_eq!(&snapshot(&c), &counted);
    }
}

#[test]
fn every_counted_match_of_the_ladder_has_all_six_bits_and_no_other_does() {
    let mut c = fresh(3, 20);
    delta(&mut c, &[4]);
    for (id, bits) in bits_of(&c) {
        let solo_kr: bool = c
            .query_row(
                "SELECT queue_id = 420 AND match_id LIKE 'KR\\_%' ESCAPE '\\' FROM matches WHERE match_id = ?1",
                [&id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(bits, if solo_kr { ALL_STEPS } else { 0 }, "{id}");
    }
}

#[test]
fn a_crash_between_two_batches_loses_nothing_and_counts_nothing_twice() {
    let mut c = fresh(5, 12);
    let first: Vec<String> = pending(&c, "s", "kr", 420, "", 4).unwrap();
    count_batch(&mut c, &kr(), &first).unwrap();
    // The second batch fails at its last statement, the stamp: every count
    // it upserted before goes with it.
    c.execute_batch(
        "CREATE TEMP TABLE crash (x); INSERT INTO crash VALUES (1);
         CREATE TEMP TRIGGER crash BEFORE UPDATE ON analytics_counted WHEN EXISTS (SELECT 1 FROM temp.crash)
         BEGIN SELECT RAISE(ABORT, 'crash'); END;",
    )
    .unwrap();
    let before = snapshot(&c);
    let second: Vec<String> = pending(&c, "s", "kr", 420, "", 4).unwrap();
    assert!(count_batch(&mut c, &kr(), &second).is_err());
    assert_eq!(snapshot(&c), before, "the failed batch left nothing");
    c.execute_batch("DELETE FROM temp.crash").unwrap();
    delta(&mut c, &[4]);
    let counted = snapshot(&c);
    assert_eq!(counted, rebuilt(&mut c));
}

#[test]
fn a_re_archived_match_is_not_counted_again() {
    let mut c = fresh(7, 6);
    // As the archive leaves them: every player stamped in the archiving transaction.
    for (id, queue_id) in tiers::unstamped(&c, "", 100).unwrap() {
        tiers::stamp(&c, &id, queue_id).unwrap();
    }
    delta(&mut c, &[3]);
    let counted = snapshot(&c);
    let bits = bits_of(&c);
    // The archive writes the same facts again: same version, stamps kept.
    for (id, _) in &bits {
        on_facts_written(&c, id, "s", Some(FACTS_VERSION), 2).unwrap();
        tiers::stamp(&c, id, 420).unwrap();
    }
    assert_eq!(bits_of(&c), bits);
    assert!(ready(&c, "s", "kr", "RANKED_SOLO_5x5").unwrap());
    assert_eq!(delta(&mut c, &[3]), 0);
    assert_eq!(snapshot(&c), counted);
}

#[test]
fn a_match_with_a_master_and_a_grandmaster_counts_once_in_each_tier_and_once_in_all() {
    let mut c = conn();
    rebuild(&mut c);
    c.execute_batch(&format!(
        "INSERT INTO matches (match_id, region, patch, queue_id, game_end_ms, body_zstd, body_size, archived_at,
           game_duration, remake, facts_version) VALUES ('KR_1', 'asia', '14.18', 420, 1, x'00', 1, 1, 1500, 0, {FACTS_VERSION});
         INSERT INTO match_facts (match_id, key_scope, puuid, team_id, position, champion_id, win, facts_version, tier)
           VALUES ('KR_1', 's', 'A', 100, 'MIDDLE', 1, 1, {FACTS_VERSION}, 'MASTER'),
                  ('KR_1', 's', 'B', 200, 'MIDDLE', 2, 0, {FACTS_VERSION}, 'GRANDMASTER');"
    ))
    .unwrap();
    on_facts_written(&c, "KR_1", "s", None, 1).unwrap();
    delta(&mut c, &[5]);
    let slices: Vec<(String, i64)> = c
        .prepare("SELECT tier, matches FROM analytics_slices ORDER BY tier")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(slices, [("GRANDMASTER".into(), 1), ("MASTER".into(), 1)]);
    let total: i64 = c
        .query_row("SELECT matches FROM analytics_match_totals", [], |r| r.get(0))
        .unwrap();
    assert_eq!(total, 1);
    let counted = snapshot(&c);
    assert_eq!(counted, rebuilt(&mut c));
}

#[test]
fn an_unstamped_ladder_takes_no_delta_until_a_full_rebuild_stamps_it() {
    // As after V0029: matches the tables already count, no stamps.
    let mut c = conn();
    add_matches(&c, 11, 0, 10);
    rebuild_steps(&mut c, 0..STEPS.len());
    let before = snapshot(&c);
    c.execute("UPDATE analytics_counted SET bits = 0", []).unwrap();
    assert!(!ready(&c, "s", "kr", "RANKED_SOLO_5x5").unwrap());
    let ids = pending(&c, "s", "kr", 420, "", 50).unwrap();
    assert!(!ids.is_empty());
    let done = count_batch(&mut c, &kr(), &ids).unwrap();
    assert!(!done.ready);
    assert_eq!(snapshot(&c), before, "nothing counted twice");
    // The full rebuild stamps every match and changes no total.
    rebuild(&mut c);
    assert!(ready(&c, "s", "kr", "RANKED_SOLO_5x5").unwrap());
    assert_eq!(snapshot(&c), before);
    assert_eq!(delta(&mut c, &[5]), 0, "nothing left to count");
}

#[test]
fn a_delta_between_a_rebuilds_steps_gives_the_rebuilds_output() {
    let mut c = fresh(13, 10);
    rebuild_steps(&mut c, 0..1);
    add_matches(&c, 14, 100, 8);
    delta(&mut c, &[3]);
    add_matches(&c, 15, 200, 8);
    rebuild_steps(&mut c, 1..STEPS.len());
    // The last arrivals miss only the champions step.
    let last: Vec<i64> = bits_of(&c)
        .into_iter()
        .filter(|(id, _)| {
            id.strip_prefix("KR_")
                .and_then(|n| n.parse::<u32>().ok())
                .is_some_and(|n| n >= 200)
        })
        .map(|(_, b)| b)
        .collect();
    assert!(last.iter().all(|b| *b == 0 || *b == ALL_STEPS - 1), "{last:?}");
    delta(&mut c, &[3]);
    let counted = snapshot(&c);
    assert_eq!(counted, rebuilt(&mut c));
}

/// Every `UNKNOWN` fact of the archive, `(match, puuid)`.
fn unknowns(c: &Connection) -> Vec<(String, String)> {
    c.prepare("SELECT match_id, puuid FROM match_facts WHERE tier = 'UNKNOWN' AND match_id LIKE 'KR%' ORDER BY 1, 2")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn a_late_stamp_after_counting_is_taken_back_and_counted_at_the_new_tier() {
    let mut c = fresh(17, 16);
    delta(&mut c, &[5]);
    let (_, puuid) = unknowns(&c).into_iter().next().unwrap();
    let moved = tiers::late_stamp(&c, "s", "kr", "RANKED_SOLO_5x5", &puuid, "CHALLENGER", 0).unwrap();
    assert!(moved > 0);
    let back: i64 = c
        .query_row("SELECT count(DISTINCT step) FROM analytics_retract", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(back, 6, "every step had counted the player");
    delta(&mut c, &[2, 5]);
    let none: i64 = c
        .query_row("SELECT count(*) FROM analytics_retract", [], |r| r.get(0))
        .unwrap();
    assert_eq!(none, 0);
    let counted = snapshot(&c);
    assert_eq!(counted, rebuilt(&mut c));
}

#[test]
fn a_late_stamp_between_a_rebuilds_steps_takes_back_only_the_steps_that_counted_it() {
    let mut c = fresh(19, 16);
    rebuild_steps(&mut c, 0..1);
    let (_, puuid) = unknowns(&c).into_iter().next().unwrap();
    tiers::late_stamp(&c, "s", "kr", "RANKED_SOLO_5x5", &puuid, "CHALLENGER", 0).unwrap();
    rebuild_steps(&mut c, 1..STEPS.len());
    let steps: Vec<i64> = c
        .prepare("SELECT DISTINCT step FROM analytics_retract")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(steps, [1], "only champions counted the old tier");
    delta(&mut c, &[4]);
    let counted = snapshot(&c);
    assert_eq!(counted, rebuilt(&mut c));
}

#[test]
fn a_tier_that_moves_twice_is_taken_back_at_the_one_counted() {
    let mut c = fresh(23, 16);
    delta(&mut c, &[6]);
    // An unstamped player counted nowhere is stamped UNKNOWN by the backfill
    // (no ladder here), then placed by a rank lookup.
    let (id, puuid): (String, String) = c
        .query_row(
            "SELECT match_id, puuid FROM match_facts WHERE tier IS NULL AND match_id LIKE 'KR%'
               AND match_id IN (SELECT match_id FROM matches WHERE queue_id = 420) LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    tiers::stamp(&c, &id, 420).unwrap();
    tiers::late_stamp(&c, "s", "kr", "RANKED_SOLO_5x5", &puuid, "MASTER", 0).unwrap();
    let recorded: Vec<Option<String>> = c
        .prepare("SELECT DISTINCT tier FROM analytics_retract WHERE puuid = ?1 AND match_id = ?2")
        .unwrap()
        .query_map([&puuid, &id], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(recorded, [None], "counted unstamped, so taken back unstamped");
    delta(&mut c, &[6]);
    let counted = snapshot(&c);
    assert_eq!(counted, rebuilt(&mut c));
}

#[test]
fn a_retract_deletes_the_rows_it_empties() {
    let mut c = conn();
    rebuild(&mut c);
    c.execute_batch(&format!(
        "INSERT INTO matches (match_id, region, patch, queue_id, game_end_ms, body_zstd, body_size, archived_at,
           game_duration, remake, facts_version) VALUES ('KR_1', 'asia', '14.18', 420, 1, x'00', 1, 1, 1500, 0, {FACTS_VERSION});
         INSERT INTO match_facts (match_id, key_scope, puuid, team_id, position, champion_id, win, facts_version, tier)
           VALUES ('KR_1', 's', 'A', 100, 'MIDDLE', 1, 1, {FACTS_VERSION}, 'UNKNOWN');"
    ))
    .unwrap();
    on_facts_written(&c, "KR_1", "s", None, 1).unwrap();
    delta(&mut c, &[5]);
    tiers::late_stamp(&c, "s", "kr", "RANKED_SOLO_5x5", "A", "MASTER", 0).unwrap();
    delta(&mut c, &[5]);
    let unknown: i64 = c
        .query_row(
            "SELECT (SELECT count(*) FROM analytics_slices WHERE tier = 'UNKNOWN')
                  + (SELECT count(*) FROM champion_stats WHERE tier = 'UNKNOWN')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(unknown, 0, "no row left counting nothing");
    let counted = snapshot(&c);
    assert_eq!(counted, rebuilt(&mut c));
}

/// The `n`th solo match on kr, in id order: one the ladder counts.
fn solo(c: &Connection, n: i64) -> String {
    c.query_row(
        "SELECT match_id FROM matches WHERE queue_id = 420 AND match_id LIKE 'KR%' ORDER BY match_id LIMIT 1 OFFSET ?1",
        [n],
        |r| r.get(0),
    )
    .unwrap()
}

#[test]
fn a_matchs_first_builds_clear_only_its_set_build_step() {
    let mut c = fresh(29, 10);
    let id = solo(&c, 0);
    // A match counted before its timeline came: no builds then.
    c.execute("DELETE FROM match_builds WHERE match_id = ?1", [&id])
        .unwrap();
    delta(&mut c, &[4]);
    let before = snapshot(&c);
    let rows = vec![crate::archive::builds::BuildFact {
        puuid: c
            .query_row(
                "SELECT puuid FROM match_facts WHERE match_id = ?1 LIMIT 1",
                [&id],
                |r| r.get(0),
            )
            .unwrap(),
        starter: vec![1056],
        boots: Some(3020),
        items: vec![6655, 3157, 3089],
        skills: "QWE".into(),
        skill_order: Some("QWE".into()),
    }];
    crate::archive::builds::write(&c, &id, "s", &rows).unwrap();
    assert_eq!(bits(&c, &id, "s").unwrap(), ALL_STEPS - SETS_STEP);
    delta(&mut c, &[4]);
    let after = snapshot(&c);
    for ((t, a), (_, b)) in before.iter().zip(&after) {
        let sets = STEPS[5].tables.contains(&t.as_str());
        assert!(sets || a == b, "{t} changed, but only the set builds should");
    }
    assert_eq!(after, rebuilt(&mut c));
    // Rows that replace others: the ladder waits for a full rebuild.
    crate::archive::builds::write(&c, &id, "s", &rows).unwrap();
    assert!(!ready(&c, "s", "kr", "RANKED_SOLO_5x5").unwrap());
}

#[test]
fn stale_facts_wait_and_rewriting_counted_ones_makes_the_ladder_dirty() {
    let mut c = fresh(31, 6);
    let (stale, counted) = (solo(&c, 0), solo(&c, 1));
    c.execute(
        "UPDATE matches SET facts_version = ?1 WHERE match_id = ?2",
        params![FACTS_VERSION - 1, stale],
    )
    .unwrap();
    delta(&mut c, &[4]);
    assert_eq!(bits(&c, &stale, "s").unwrap(), 0, "waits for facts:reextract");
    // facts:reextract rewrites a counted match: dirty from then on.
    on_facts_written(&c, &counted, "s", Some(FACTS_VERSION - 1), 50).unwrap();
    assert!(!ready(&c, "s", "kr", "RANKED_SOLO_5x5").unwrap());
    // A rebuild that began before the rewrite doesn't stamp it ready again.
    mark_stamped(&c, "s", "kr", "RANKED_SOLO_5x5", 50, 60).unwrap();
    assert!(!ready(&c, "s", "kr", "RANKED_SOLO_5x5").unwrap());
    mark_stamped(&c, "s", "kr", "RANKED_SOLO_5x5", 51, 60).unwrap();
    assert!(ready(&c, "s", "kr", "RANKED_SOLO_5x5").unwrap());
    // A first archive is no rewrite.
    add_matches(&c, 32, 50, 1);
    assert!(ready(&c, "s", "kr", "RANKED_SOLO_5x5").unwrap());
}
