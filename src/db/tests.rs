use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use rusqlite::{OptionalExtension, params};

use super::*;

fn open_temp(readers: usize) -> (tempfile::TempDir, Db) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Db::open(&dir.path().join("nested/riot-proxy.db"), readers).expect("open");
    (dir, db)
}

fn pragma(conn: &Connection, name: &str) -> rusqlite::Result<String> {
    conn.query_row(&format!("PRAGMA {name}"), [], |r| {
        r.get::<_, rusqlite::types::Value>(0).map(|v| match v {
            rusqlite::types::Value::Integer(i) => i.to_string(),
            rusqlite::types::Value::Text(t) => t,
            other => format!("{other:?}"),
        })
    })
}

#[tokio::test]
async fn writer_has_every_design_04_pragma() {
    let (_dir, db) = open_temp(1);
    let got: Vec<(String, String)> = db
        .write(|c| {
            [
                "journal_mode",
                "synchronous",
                "busy_timeout",
                "foreign_keys",
                "cache_size",
                "mmap_size",
                "temp_store",
                "wal_autocheckpoint",
            ]
            .iter()
            .map(|p| Ok((p.to_string(), pragma(c, p)?)))
            .collect::<Result<_, DbError>>()
        })
        .await
        .expect("pragmas");
    let expected = [
        ("journal_mode", "wal"),
        ("synchronous", "1"), // NORMAL
        ("busy_timeout", "5000"),
        ("foreign_keys", "1"),
        ("cache_size", "-65536"),
        ("mmap_size", "268435456"),
        ("temp_store", "2"), // MEMORY
        ("wal_autocheckpoint", "1000"),
    ];
    let expected: Vec<(String, String)> = expected
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    assert_eq!(got, expected);
}

#[tokio::test]
async fn readers_have_connection_pragmas_and_see_wal() {
    let (_dir, db) = open_temp(2);
    let got = db
        .read(|c| {
            Ok::<_, DbError>((
                pragma(c, "journal_mode")?,
                pragma(c, "busy_timeout")?,
                pragma(c, "cache_size")?,
                pragma(c, "mmap_size")?,
                pragma(c, "temp_store")?,
            ))
        })
        .await
        .expect("pragmas");
    assert_eq!(
        got,
        (
            "wal".into(),
            "5000".into(),
            "-65536".into(),
            "268435456".into(),
            "2".into()
        )
    );
}

#[tokio::test]
/// Design/04's tables, with the ladder (V0004, ADR-054) and analytics (V0005,
/// ADR-056) in v1's shape.
async fn migrations_create_exactly_the_design_04_tables() {
    let (_dir, db) = open_temp(1);
    let tables: Vec<String> = db
        .read(|c| {
            let mut stmt = c.prepare(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
            )?;
            let rows = stmt.query_map([], |r| r.get(0))?.collect::<Result<Vec<String>, _>>()?;
            Ok::<_, DbError>(rows)
        })
        .await
        .expect("tables");
    assert_eq!(
        tables,
        [
            "analytics_counted",
            "analytics_floor_totals",
            "analytics_match_totals",
            "analytics_retract",
            "analytics_runs",
            "analytics_slices",
            "analytics_stamped",
            "analytics_unsplit",
            "body_locations",
            "body_packs",
            "cache",
            "champion_ban_floor_totals",
            "champion_ban_totals",
            "champion_bans",
            "champion_build_parts",
            "champion_builds",
            "champion_items",
            "champion_matchup_build_parts",
            "champion_matchup_builds",
            "champion_matchups",
            "champion_runes",
            "champion_spells",
            "champion_stats",
            "champion_stats_totals",
            "consumers",
            "crawl_legs",
            "crawl_match_ids",
            "jobs",
            "ladder_crawls",
            "ladder_entries",
            "limiter_state",
            "match_bans",
            "match_builds",
            "match_facts",
            "matches",
            "metrics_history",
            "player_ranks",
            "players",
            "rank_lookup_queue",
            "rank_lookups",
            "refinery_schema_history",
            "timeline_gaps",
            "timelines",
        ]
    );
}

async fn names(db: &Db, kind: &'static str) -> Vec<String> {
    db.read(move |c| {
        let mut stmt = c.prepare(
            "SELECT name FROM sqlite_master WHERE type = ?1 AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )?;
        let rows = stmt
            .query_map([kind], |r| r.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        Ok::<_, DbError>(rows)
    })
    .await
    .expect("names")
}

/// P5-01: design 04's indexes, including the partial ones.
#[tokio::test]
async fn archive_indexes_exist() {
    let (_dir, db) = open_temp(1);
    let indexes = names(&db, "index").await;
    for ix in [
        "players_tracked",
        "matches_patch_queue",
        "matches_end",
        "facts_player",
        "cache_hard",
        "jobs_dedupe",
        "jobs_claim",
        "jobs_state_kind",
        "body_locations_pack",
        "matches_to_pack",
        "matches_to_free",
        "timelines_to_pack",
        "timelines_to_free",
        "matches_archived",
    ] {
        assert!(indexes.contains(&ix.to_string()), "{ix} missing from {indexes:?}");
    }
}

/// A database created at V0001 (P0–P4) upgrades in place and keeps its rows.
#[tokio::test]
async fn a_v1_database_upgrades_to_v2_and_keeps_its_data() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("riot-proxy.db");
    {
        let mut conn = Connection::open(&path).expect("open");
        embedded::migrations::runner()
            .set_grouped(true)
            .set_target(refinery::Target::Version(1))
            .run(&mut conn)
            .expect("V0001 only");
        conn.execute(
            "INSERT INTO consumers (id, name, key_sha256, scopes, created_at) VALUES ('c1', 'old', x'01', '[\"read\"]', 0)",
            [],
        )
        .expect("row");
        let tables: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = 'matches'",
                [],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(tables, 0, "V0001 has no archive yet");
    }
    let db = Db::open(&path, 1).expect("upgrade");
    assert!(names(&db, "table").await.contains(&"matches".to_string()));
    let (name, versions): (String, i64) = db
        .read(|c| {
            Ok::<_, DbError>((
                c.query_row("SELECT name FROM consumers WHERE id = 'c1'", [], |r| r.get(0))?,
                c.query_row("SELECT COUNT(*) FROM refinery_schema_history", [], |r| r.get(0))?,
            ))
        })
        .await
        .expect("read");
    assert_eq!((name.as_str(), versions), ("old", 29));
}

/// V0015 (SITE-08, ADR-123) backfills the slices aggregated before it, as the
/// rebuild would count them: a match once, whatever tiers its players are in;
/// a ban once per match; only matches archived by the slice's recompute, and
/// only slices that exist.
#[tokio::test]
async fn v0015_backfills_the_totals_of_every_aggregated_slice() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("riot-proxy.db");
    {
        let mut conn = Connection::open(&path).expect("open");
        embedded::migrations::runner()
            .set_grouped(true)
            .set_target(refinery::Target::Version(14))
            .run(&mut conn)
            .expect("to V0014");
        conn.execute_batch(
            "INSERT INTO ladder_entries (key_scope, platform, queue, puuid, tier, division, league_points, wins,
               losses, first_seen_crawl_id, last_seen_crawl_id, updated_at) VALUES
               ('s', 'kr', 'RANKED_SOLO_5x5', 'A', 'MASTER', 'I', 1, 1, 1, 'c', 'c', 1),
               ('s', 'kr', 'RANKED_SOLO_5x5', 'B', 'MASTER', 'I', 1, 1, 1, 'c', 'c', 1),
               ('s', 'kr', 'RANKED_SOLO_5x5', 'C', 'DIAMOND', 'I', 1, 1, 1, 'c', 'c', 1);
             -- KR_9 was archived after the slices' recompute (100); 14.10 and
             -- flex were never aggregated.
             INSERT INTO matches (match_id, region, patch, queue_id, game_end_ms, body_zstd, body_size,
               archived_at, game_duration, remake, facts_version) VALUES
               ('KR_1', 'asia', '14.18', 420, 1, x'00', 1, 10, 1800, 0, 3),
               ('KR_2', 'asia', '14.18', 420, 1, x'00', 1, 10, 1800, 0, 3),
               ('KR_3', 'asia', '14.18', 420, 1, x'00', 1, 10, 200, 1, 3),
               ('KR_9', 'asia', '14.18', 420, 1, x'00', 1, 500, 1800, 0, 3),
               ('KR_7', 'asia', '14.10', 420, 1, x'00', 1, 10, 1800, 0, 3),
               ('KR_5', 'asia', '14.18', 440, 1, x'00', 1, 10, 1800, 0, 3);
             INSERT INTO match_facts (match_id, key_scope, puuid, team_id, position, champion_id, win, facts_version)
               VALUES ('KR_1', 's', 'A', 100, 'MIDDLE', 1, 1, 3), ('KR_1', 's', 'C', 200, 'MIDDLE', 2, 0, 3),
                      ('KR_2', 's', 'A', 100, 'MIDDLE', 1, 0, 3), ('KR_2', 's', 'B', 200, 'MIDDLE', 2, 1, 3),
                      ('KR_3', 's', 'A', 100, 'MIDDLE', 1, 1, 3), ('KR_9', 's', 'A', 100, 'MIDDLE', 1, 1, 3),
                      ('KR_7', 's', 'A', 100, 'MIDDLE', 1, 1, 3), ('KR_5', 's', 'A', 100, 'MIDDLE', 1, 1, 3);
             INSERT INTO match_bans (match_id, team_id, pick_turn, champion_id) VALUES
               ('KR_1', 100, 1, 10), ('KR_1', 200, 6, 10), ('KR_1', 200, 7, 11), ('KR_2', 100, 1, 11),
               ('KR_9', 100, 1, 10);
             -- What a V0014 rebuild of 14.18 wrote: KR_1 in MASTER and DIAMOND.
             INSERT INTO analytics_slices (key_scope, platform, queue, tier, patch, remake, matches, computed_at)
               VALUES ('s', 'kr', 'RANKED_SOLO_5x5', 'MASTER', '14.18', 0, 2, 100),
                      ('s', 'kr', 'RANKED_SOLO_5x5', 'DIAMOND', '14.18', 0, 1, 100),
                      ('s', 'kr', 'RANKED_SOLO_5x5', 'MASTER', '14.18', 1, 1, 100);",
        )
        .expect("seed");
    }
    let db = Db::open(&path, 1).expect("upgrade");
    let totals = |db: Db| async move {
        db.read(|c| {
            let rows = |sql: &str| -> Result<Vec<String>, DbError> {
                let mut stmt = c.prepare(sql)?;
                let out = stmt.query_map([], |r| r.get(0))?.collect::<Result<Vec<String>, _>>()?;
                Ok(out)
            };
            Ok::<_, DbError>((
                rows(
                    "SELECT key_scope || ' ' || platform || ' ' || queue || ' ' || patch || ' ' || remake || ' '
                       || matches || ' ' || computed_at FROM analytics_match_totals ORDER BY remake",
                )?,
                rows(
                    "SELECT patch || ' ' || champion_id || ' ' || remake || ' ' || bans || ' ' || computed_at
                       FROM champion_ban_totals ORDER BY champion_id",
                )?,
            ))
        })
        .await
        .expect("totals")
    };
    let (matches, bans) = totals(db.clone()).await;
    assert_eq!(
        matches,
        [
            "s kr RANKED_SOLO_5x5 14.18 0 2 100",
            "s kr RANKED_SOLO_5x5 14.18 1 1 100"
        ]
    );
    assert_eq!(bans, ["14.18 10 0 1 100", "14.18 11 0 2 100"]);

    // A rebuild of 14.18 without KR_9 counts the same.
    db.write(|c| {
        c.execute("DELETE FROM matches WHERE match_id = 'KR_9'", [])?;
        // What `tiers:backfill` does before the first rebuild after V0016.
        for (id, queue_id) in crate::archive::tiers::unstamped(c, "", 100)? {
            crate::archive::tiers::stamp(c, &id, queue_id)?;
        }
        let scope = crate::archive::analytics::Scope {
            key_scope: "s".into(),
            platform: "kr".into(),
            queue: "RANKED_SOLO_5x5".into(),
            queue_id: 420,
            patches: Some(vec!["14.18".into()]),
            now: 100,
        };
        crate::archive::analytics::rebuild_champions(c, &scope)
    })
    .await
    .expect("rebuild");
    assert_eq!(totals(db).await, (matches, bans));
}

/// V0019 (DSK-04, ADR-138) moves each stamp onto its fact row and drops
/// `match_tiers`: a fact with no stamp reads NULL and is what `tiers:backfill`
/// finds; a match outside solo and flex stays NULL and is not found.
#[tokio::test]
async fn v0019_moves_the_stamps_onto_the_facts_and_drops_the_table() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("riot-proxy.db");
    {
        let mut conn = Connection::open(&path).expect("open");
        embedded::migrations::runner()
            .set_grouped(true)
            .set_target(refinery::Target::Version(18))
            .run(&mut conn)
            .expect("to V0018");
        conn.execute_batch(
            "INSERT INTO matches (match_id, region, patch, queue_id, game_end_ms, body_zstd, body_size, archived_at)
               VALUES ('KR_1', 'asia', '14.18', 420, 1, x'00', 1, 1),
                      ('KR_2', 'asia', '14.18', 420, 1, x'00', 1, 1),
                      ('KR_3', 'asia', '14.18', 450, 1, x'00', 1, 1);
             INSERT INTO match_facts (match_id, key_scope, puuid, team_id, champion_id, win, facts_version)
               VALUES ('KR_1', 's', 'a', 100, 1, 1, 3), ('KR_1', 's', 'b', 200, 2, 0, 3),
                      ('KR_2', 's', 'a', 100, 1, 1, 3), ('KR_2', 's', 'c', 200, 3, 0, 3),
                      ('KR_3', 's', 'a', 100, 1, 1, 3);
             INSERT INTO match_tiers (match_id, key_scope, puuid, platform, queue, tier, stamped_at)
               VALUES ('KR_1', 's', 'a', 'kr', 'RANKED_SOLO_5x5', 'MASTER', 1),
                      ('KR_1', 's', 'b', 'kr', 'RANKED_SOLO_5x5', 'UNKNOWN', 1),
                      ('KR_2', 's', 'a', 'kr', 'RANKED_SOLO_5x5', 'DIAMOND', 1);",
        )
        .expect("seed");
    }
    let db = Db::open(&path, 1).expect("migrate");
    assert!(!names(&db, "table").await.contains(&"match_tiers".to_string()));
    let (tiers, left): (Vec<String>, Vec<(String, i64)>) = db
        .read(|c| {
            let mut stmt = c.prepare(
                "SELECT match_id || ' ' || puuid || ' ' || coalesce(tier, 'NULL') FROM match_facts
                  ORDER BY match_id, puuid",
            )?;
            let tiers = stmt
                .query_map([], |r| r.get(0))?
                .collect::<Result<Vec<String>, _>>()?;
            Ok::<_, DbError>((tiers, crate::archive::tiers::unstamped(c, "", 10)?))
        })
        .await
        .expect("read");
    assert_eq!(
        tiers,
        [
            "KR_1 a MASTER",
            "KR_1 b UNKNOWN",
            "KR_2 a DIAMOND",
            "KR_2 c NULL",
            "KR_3 a NULL"
        ]
    );
    assert_eq!(
        left,
        [("KR_2".to_string(), 420)],
        "the ranked match with an unstamped fact"
    );
}

/// V0020 (DSK-06, ADR-140) rebuilds the key-only derived tables WITHOUT
/// ROWID: every row survives with its values, each table says so in its
/// DDL, and the foreign keys to `matches` still cascade. It recreated
/// `champion_stats_slice`, which V0025 then drops (PERF-02).
#[tokio::test]
async fn v0020_rebuilds_the_derived_tables_without_rowid_and_keeps_their_rows() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("riot-proxy.db");
    {
        let mut conn = Connection::open(&path).expect("open");
        embedded::migrations::runner()
            .set_grouped(true)
            .set_target(refinery::Target::Version(19))
            .run(&mut conn)
            .expect("to V0019");
        conn.execute_batch(
            "INSERT INTO matches (match_id, region, patch, queue_id, game_end_ms, body_zstd, body_size, archived_at)
               VALUES ('KR_1', 'asia', '14.18', 420, 1, x'00', 1, 1), ('KR_2', 'asia', '14.18', 420, 1, x'00', 1, 1);
             INSERT INTO match_builds (match_id, key_scope, puuid, starter, boots, items, skills, skill_order,
               builds_version) VALUES ('KR_2', 's', 'b', '[1056]', 3020, '[6655,3157]', 'QWE', 'QWE', 3),
                                      ('KR_1', 's', 'a', '[1055]', NULL, '[3157]', 'QEW', NULL, 3);
             INSERT INTO match_bans (match_id, team_id, pick_turn, champion_id)
               VALUES ('KR_2', 100, 1, 10), ('KR_1', 200, 2, 11), ('KR_1', 100, 1, 12);
             INSERT INTO analytics_slices (key_scope, platform, queue, tier, patch, remake, matches, computed_at)
               VALUES ('s', 'kr', 'RANKED_SOLO_5x5', 'MASTER', '14.18', 0, 2, 100);
             INSERT INTO analytics_match_totals (key_scope, platform, queue, patch, remake, matches, computed_at)
               VALUES ('s', 'kr', 'RANKED_SOLO_5x5', '14.18', 0, 2, 100);
             INSERT INTO champion_stats (key_scope, platform, queue, tier, patch, champion_id, role, remake, side,
               games, wins, matches_picked, stated_games, kills, deaths, assists, cs, gold, damage, vision,
               duration_s, computed_at) VALUES
               ('s', 'kr', 'RANKED_SOLO_5x5', 'MASTER', '14.18', 1, 'MIDDLE', 0, 'blue', 3, 2, 3, 3, 1, 2, 3, 4, 5, 6, 7, 8, 100);
             INSERT INTO champion_bans (key_scope, platform, queue, tier, patch, champion_id, remake, bans, computed_at)
               VALUES ('s', 'kr', 'RANKED_SOLO_5x5', 'MASTER', '14.18', 10, 0, 1, 100);
             INSERT INTO champion_ban_totals (key_scope, platform, queue, patch, champion_id, remake, bans, computed_at)
               VALUES ('s', 'kr', 'RANKED_SOLO_5x5', '14.18', 10, 0, 1, 100);
             INSERT INTO champion_matchups (key_scope, platform, queue, patch, champion_id, role, opponent_id, remake,
               tier, side, games, wins, computed_at)
               VALUES ('s', 'kr', 'RANKED_SOLO_5x5', '14.18', 1, 'MIDDLE', 2, 0, 'MASTER', 'blue', 3, 2, 100);
             INSERT INTO champion_items (key_scope, platform, queue, patch, champion_id, role, item_id, remake,
               tier, side, games, wins, computed_at)
               VALUES ('s', 'kr', 'RANKED_SOLO_5x5', '14.18', 1, 'MIDDLE', 3157, 0, 'MASTER', 'blue', 3, 2, 100);
             INSERT INTO champion_runes (key_scope, platform, queue, patch, champion_id, role, keystone_id,
               sub_style_id, remake, tier, side, games, wins, computed_at)
               VALUES ('s', 'kr', 'RANKED_SOLO_5x5', '14.18', 1, 'MIDDLE', 8112, 8300, 0, 'MASTER', 'blue', 3, 2, 100);
             INSERT INTO champion_spells (key_scope, platform, queue, patch, champion_id, role, spell_a, spell_b,
               remake, tier, side, games, wins, computed_at)
               VALUES ('s', 'kr', 'RANKED_SOLO_5x5', '14.18', 1, 'MIDDLE', 4, 14, 0, 'MASTER', 'blue', 3, 2, 100);
             INSERT INTO champion_builds (key_scope, platform, queue, patch, champion_id, role, remake, core,
               tier, side, games, wins, computed_at)
               VALUES ('s', 'kr', 'RANKED_SOLO_5x5', '14.18', 1, 'MIDDLE', 0, '[6655,3157]', 'MASTER', 'blue', 2, 1, 100);
             INSERT INTO champion_build_parts (key_scope, platform, queue, patch, champion_id, role, remake, core,
               part, value, tier, side, games, wins, computed_at)
               VALUES ('s', 'kr', 'RANKED_SOLO_5x5', '14.18', 1, 'MIDDLE', 0, '[6655,3157]', 'boots', '3020',
                       'MASTER', 'blue', 2, 1, 100);",
        )
        .expect("seed");
    }
    let db = Db::open(&path, 1).expect("migrate");
    const REBUILT: [&str; 13] = [
        "match_builds",
        "match_bans",
        "analytics_slices",
        "analytics_match_totals",
        "champion_stats",
        "champion_bans",
        "champion_ban_totals",
        "champion_matchups",
        "champion_items",
        "champion_runes",
        "champion_spells",
        "champion_builds",
        "champion_build_parts",
    ];
    let (ddl, counts, sample, cascade) = db
        .read(|c| {
            let mut ddl = vec![];
            let mut counts = vec![];
            for t in REBUILT {
                let sql: String = c.query_row(
                    "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    [t],
                    |r| r.get(0),
                )?;
                ddl.push((t, sql.trim_end().ends_with("WITHOUT ROWID")));
                counts.push(c.query_row(&format!("SELECT count(*) FROM {t}"), [], |r| r.get::<_, i64>(0))?);
            }
            let mut stmt = c.prepare(
                "SELECT match_id || ' ' || puuid || ' ' || starter || ' ' || coalesce(boots, '-') || ' ' || items
                   || ' ' || skills || ' ' || coalesce(skill_order, '-') FROM match_builds ORDER BY match_id",
            )?;
            let sample = stmt
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            let slice: i64 = c.query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'index' AND name = 'champion_stats_slice'",
                [],
                |r| r.get(0),
            )?;
            let fks: i64 = c.query_row(
                "SELECT (SELECT count(*) FROM pragma_foreign_key_list('match_builds'))
                      + (SELECT count(*) FROM pragma_foreign_key_list('match_bans'))",
                [],
                |r| r.get(0),
            )?;
            Ok::<_, DbError>((ddl, counts, sample, (slice, fks)))
        })
        .await
        .expect("read");
    assert!(ddl.iter().all(|(_, w)| *w), "{ddl:?}");
    assert_eq!(counts, [2, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1]);
    assert_eq!(
        sample,
        [
            "KR_1 a [1055] - [3157] QEW -",
            "KR_2 b [1056] 3020 [6655,3157] QWE QWE"
        ]
    );
    assert_eq!(
        cascade,
        (0, 2),
        "both foreign keys are back; V0025 dropped the slice index"
    );
    // The keys still cascade from matches.
    db.write(|c| {
        c.execute("DELETE FROM matches WHERE match_id = 'KR_1'", [])?;
        Ok::<_, DbError>(())
    })
    .await
    .expect("delete");
    let left: (i64, i64) = db
        .read(|c| {
            Ok::<_, DbError>(c.query_row(
                "SELECT (SELECT count(*) FROM match_builds), (SELECT count(*) FROM match_bans)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .await
        .expect("read");
    assert_eq!(left, (1, 1));
}

/// V0021 (DSK-07, ADR-141) rebuilds `match_facts` WITHOUT ROWID in key order,
/// keeps every row and its tier, recreates `facts_player`, drops `facts_champ`
/// (no reader), and the foreign key still cascades from `matches`.
#[tokio::test]
async fn v0021_rebuilds_match_facts_without_rowid_and_keeps_every_row() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("riot-proxy.db");
    {
        let mut conn = Connection::open(&path).expect("open");
        embedded::migrations::runner()
            .set_grouped(true)
            .set_target(refinery::Target::Version(20))
            .run(&mut conn)
            .expect("to V0020");
        conn.execute_batch(
            "INSERT INTO matches (match_id, region, patch, queue_id, game_end_ms, body_zstd, body_size, archived_at)
               VALUES ('KR_1', 'asia', '14.18', 420, 1, x'00', 1, 1), ('KR_2', 'asia', '14.18', 420, 1, x'00', 1, 1);
             INSERT INTO match_facts (match_id, key_scope, puuid, team_id, position, champion_id, win, kills, deaths,
               assists, items, runes, summoners, facts_version, cs, gold, damage, vision, tier)
               VALUES ('KR_2', 's', 'b', 200, 'MIDDLE', 2, 0, 1, 2, 3, '[3157]', '[8000,8010,8400,[],null]', '[4,14]',
                         4, 100, 5000, 10000, 10, 'DIAMOND'),
                      ('KR_1', 's', 'a', 100, NULL, 1, 1, NULL, NULL, NULL, '[]', NULL, NULL, 4, NULL, NULL, NULL,
                         NULL, NULL),
                      ('KR_1', 's', 'b', 200, 'TOP', 3, 0, 0, 0, 0, '[1055]', NULL, '[4,12]', 4, 1, 2, 3, 4, 'MASTER');",
        )
        .expect("seed");
    }
    let db = Db::open(&path, 1).expect("migrate");
    let indexes = names(&db, "index").await;
    assert!(indexes.contains(&"facts_player".to_string()), "{indexes:?}");
    assert!(!indexes.contains(&"facts_champ".to_string()), "{indexes:?}");
    let (ddl, rows, fks): (String, Vec<String>, i64) = db
        .read(|c| {
            let ddl = c.query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'match_facts'",
                [],
                |r| r.get(0),
            )?;
            let mut stmt = c.prepare(
                "SELECT match_id || ' ' || puuid || ' ' || team_id || ' ' || coalesce(position, '-') || ' '
                   || champion_id || ' ' || win || ' ' || coalesce(kills, '-') || ' ' || items || ' '
                   || coalesce(runes, '-') || ' ' || coalesce(summoners, '-') || ' ' || facts_version || ' '
                   || coalesce(cs, '-') || ' ' || coalesce(gold, '-') || ' ' || coalesce(tier, '-')
                   FROM match_facts ORDER BY match_id, puuid",
            )?;
            let rows = stmt
                .query_map([], |r| r.get(0))?
                .collect::<Result<Vec<String>, _>>()?;
            let fks = c.query_row(
                "SELECT count(*) FROM pragma_foreign_key_list('match_facts')",
                [],
                |r| r.get(0),
            )?;
            Ok::<_, DbError>((ddl, rows, fks))
        })
        .await
        .expect("read");
    assert!(ddl.trim_end().ends_with("WITHOUT ROWID"), "{ddl}");
    assert_eq!(
        rows,
        [
            "KR_1 a 100 - 1 1 - [] - - 4 - - -",
            "KR_1 b 200 TOP 3 0 0 [1055] - [4,12] 4 1 2 MASTER",
            "KR_2 b 200 MIDDLE 2 0 1 [3157] [8000,8010,8400,[],null] [4,14] 4 100 5000 DIAMOND",
        ]
    );
    assert_eq!(fks, 1);
    db.write(|c| {
        c.execute("DELETE FROM matches WHERE match_id = 'KR_1'", [])?;
        Ok::<_, DbError>(())
    })
    .await
    .expect("delete");
    let left: i64 = db
        .read(|c| Ok::<_, DbError>(c.query_row("SELECT count(*) FROM match_facts", [], |r| r.get(0))?))
        .await
        .expect("count");
    assert_eq!(left, 1, "the key cascades");
}

/// V0017 (FLT-01, ADR-129) keeps every analytics row, with tier and side ''
/// where it has none to give, and lists each ladder that had rows, so its
/// next rebuild covers every patch. `champion_stats` keeps its tier.
#[tokio::test]
async fn v0017_keeps_every_row_unsplit_and_lists_its_ladder() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("riot-proxy.db");
    {
        let mut conn = Connection::open(&path).expect("open");
        embedded::migrations::runner()
            .set_grouped(true)
            .set_target(refinery::Target::Version(16))
            .run(&mut conn)
            .expect("to V0016");
        conn.execute_batch(
            "INSERT INTO champion_stats (key_scope, platform, queue, tier, patch, champion_id, role, remake, games,
               wins, matches_picked, stated_games, kills, deaths, assists, cs, gold, damage, vision, duration_s,
               computed_at) VALUES
               ('s', 'kr', 'RANKED_SOLO_5x5', 'MASTER', '14.18', 1, 'MIDDLE', 0, 3, 2, 3, 3, 1, 2, 3, 4, 5, 6, 7, 8, 100),
               ('s', 'kr', 'RANKED_SOLO_5x5', 'DIAMOND', '14.10', 1, 'MIDDLE', 0, 1, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 90);
             INSERT INTO champion_matchups (key_scope, platform, queue, patch, champion_id, role, opponent_id, remake,
               games, wins, computed_at) VALUES ('s', 'kr', 'RANKED_SOLO_5x5', '14.18', 1, 'MIDDLE', 2, 0, 3, 2, 100);
             INSERT INTO champion_items (key_scope, platform, queue, patch, champion_id, role, item_id, remake,
               games, wins, computed_at) VALUES ('s', 'kr', 'RANKED_SOLO_5x5', '14.18', 1, 'MIDDLE', 3157, 0, 3, 2, 100);
             INSERT INTO champion_runes (key_scope, platform, queue, patch, champion_id, role, keystone_id,
               sub_style_id, remake, games, wins, computed_at)
               VALUES ('s', 'kr', 'RANKED_SOLO_5x5', '14.18', 1, 'MIDDLE', 8112, 8300, 0, 3, 2, 100);
             INSERT INTO champion_spells (key_scope, platform, queue, patch, champion_id, role, spell_a, spell_b,
               remake, games, wins, computed_at)
               VALUES ('s', 'kr', 'RANKED_SOLO_5x5', '14.18', 1, 'MIDDLE', 4, 14, 0, 3, 2, 100);
             -- Flex has set builds only, under another key scope.
             INSERT INTO champion_builds (key_scope, platform, queue, patch, champion_id, role, remake, core,
               games, wins, computed_at)
               VALUES ('t', 'euw1', 'RANKED_FLEX_SR', '14.18', 1, 'MIDDLE', 0, '[6655,3157]', 2, 1, 100);
             INSERT INTO champion_build_parts (key_scope, platform, queue, patch, champion_id, role, remake, core,
               part, value, games, wins, computed_at)
               VALUES ('t', 'euw1', 'RANKED_FLEX_SR', '14.18', 1, 'MIDDLE', 0, '[6655,3157]', 'boots', '3020', 2, 1, 100);",
        )
        .expect("seed");
    }
    let db = Db::open(&path, 1).expect("migrate");
    let rows = db
        .read(|c| {
            let mut out = vec![];
            for (t, cols) in [
                (
                    "champion_stats",
                    "tier, patch, games, kills, duration_s, computed_at",
                ),
                (
                    "champion_matchups",
                    "patch, opponent_id, games, wins, computed_at",
                ),
                ("champion_items", "patch, item_id, games, wins, computed_at"),
                (
                    "champion_runes",
                    "patch, keystone_id, sub_style_id, games, wins, computed_at",
                ),
                (
                    "champion_spells",
                    "patch, spell_a, spell_b, games, wins, computed_at",
                ),
                ("champion_builds", "key_scope, core, games, wins, computed_at"),
                (
                    "champion_build_parts",
                    "key_scope, core, part, value, games, wins, computed_at",
                ),
            ] {
                let tier = if t == "champion_stats" { "'-'" } else { "tier" };
                let mut stmt = c.prepare(&format!(
                    "SELECT '{t}' || ' [' || {tier} || '/' || side || '] ' || concat_ws(' ', {cols})
                       FROM {t} ORDER BY patch DESC"
                ))?;
                out.extend(
                    stmt.query_map([], |r| r.get::<_, String>(0))?
                        .collect::<Result<Vec<_>, _>>()?,
                );
            }
            let mut stmt =
                c.prepare("SELECT key_scope, platform, queue FROM analytics_unsplit ORDER BY key_scope")?;
            let listed = stmt
                .query_map([], |r| {
                    Ok(format!(
                        "{} {} {}",
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, DbError>((out, listed))
        })
        .await
        .expect("rows");
    assert_eq!(
        rows,
        (
            vec![
                "champion_stats [-/] MASTER 14.18 3 1 8 100".to_string(),
                "champion_stats [-/] DIAMOND 14.10 1 1 1 90".into(),
                "champion_matchups [/] 14.18 2 3 2 100".into(),
                "champion_items [/] 14.18 3157 3 2 100".into(),
                "champion_runes [/] 14.18 8112 8300 3 2 100".into(),
                "champion_spells [/] 14.18 4 14 3 2 100".into(),
                "champion_builds [/] t [6655,3157] 2 1 100".into(),
                "champion_build_parts [/] t [6655,3157] boots 3020 2 1 100".into(),
            ],
            vec!["s kr RANKED_SOLO_5x5".to_string(), "t euw1 RANKED_FLEX_SR".into()]
        )
    );
}

/// match_facts is a pure derivation of matches: deleting a match cascades (design 04).
/// A timeline has no foreign key to its match (TL-01): it may be stored first,
/// and outlives a deleted match.
#[tokio::test]
async fn archive_foreign_keys_behave_as_designed() {
    let (_dir, db) = open_temp(1);
    let left = db
        .write(|c| {
            c.execute_batch(
                "INSERT INTO timelines (match_id, body_zstd) VALUES ('EUW1_1', x'00');
                 INSERT INTO matches (match_id, region, patch, queue_id, game_end_ms, body_zstd, body_size, archived_at)
                   VALUES ('EUW1_1', 'europe', '14.18', 420, 1, x'00', 1, 1);
                 INSERT INTO match_facts (match_id, key_scope, puuid, team_id, champion_id, win, facts_version)
                   VALUES ('EUW1_1', 'abcd1234', 'P1', 100, 1, 1, 1), ('EUW1_1', 'abcd1234', 'P2', 200, 2, 0, 1);",
            )?;
            c.execute("DELETE FROM matches WHERE match_id = 'EUW1_1'", [])?;
            let count = |table: &str| {
                c.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get::<_, i64>(0))
            };
            Ok::<_, DbError>((count("match_facts")?, count("timelines")?))
        })
        .await
        .expect("write");
    assert_eq!(left, (0, 1));
}

/// V0013 rebuilds `timelines` without its foreign key and keeps every row.
#[tokio::test]
async fn timelines_survive_the_rebuild_without_their_foreign_key() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("riot-proxy.db");
    {
        let mut conn = Connection::open(&path).expect("open");
        conn.execute_batch("PRAGMA foreign_keys = ON;").expect("pragma");
        embedded::migrations::runner()
            .set_target(refinery::Target::Version(12))
            .run(&mut conn)
            .expect("migrate to V0012");
        conn.execute_batch(
            "INSERT INTO matches (match_id, region, patch, queue_id, game_end_ms, body_zstd, body_size, archived_at)
               VALUES ('EUW1_1', 'europe', '14.18', 420, 1, x'00', 1, 1);
             INSERT INTO timelines (match_id, body_zstd) VALUES ('EUW1_1', x'0102');",
        )
        .expect("seed");
    }
    let db = Db::open(&path, 1).expect("upgrade");
    let (row, fks): (Vec<u8>, i64) = db
        .read(|c| {
            Ok::<_, DbError>((
                c.query_row(
                    "SELECT body_zstd FROM timelines WHERE match_id = 'EUW1_1'",
                    [],
                    |r| r.get(0),
                )?,
                c.query_row(
                    "SELECT COUNT(*) FROM pragma_foreign_key_list('timelines')",
                    [],
                    |r| r.get(0),
                )?,
            ))
        })
        .await
        .expect("read");
    assert_eq!((row, fks), (vec![1, 2], 0));
}

#[tokio::test]
async fn facts_are_unique_per_match_and_player() {
    let (_dir, db) = open_temp(1);
    let dup = db
        .write(|c| {
            c.execute_batch(
                "INSERT INTO matches (match_id, region, patch, queue_id, game_end_ms, body_zstd, body_size, archived_at)
                   VALUES ('KR_1', 'asia', '14.18', 420, 1, x'00', 1, 1);
                 INSERT INTO match_facts (match_id, key_scope, puuid, team_id, champion_id, win, facts_version)
                   VALUES ('KR_1', 's', 'P', 100, 1, 1, 1);",
            )?;
            Ok::<_, DbError>(
                c.execute(
                    "INSERT INTO match_facts (match_id, key_scope, puuid, team_id, champion_id, win, facts_version)
                       VALUES ('KR_1', 's', 'P', 100, 1, 1, 1)",
                    [],
                )
                .is_err(),
            )
        })
        .await
        .expect("write");
    assert!(dup);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hundred_concurrent_writes_all_succeed() {
    let (_dir, db) = open_temp(2);
    let tasks: Vec<_> = (0..100)
        .map(|i| {
            let db = db.clone();
            tokio::spawn(async move {
                db.write(move |c| {
                    c.execute(
                        "INSERT INTO metrics_history (at, point) VALUES (?1, ?2)",
                        params![i, format!("{{\"i\":{i}}}")],
                    )
                    .map_err(DbError::from)
                })
                .await
            })
        })
        .collect();
    for t in tasks {
        assert_eq!(t.await.expect("join").expect("no SQLITE_BUSY"), 1);
    }
    let n: i64 = db
        .read(|c| Ok::<_, DbError>(c.query_row("SELECT COUNT(*) FROM metrics_history", [], |r| r.get(0))?))
        .await
        .expect("count");
    assert_eq!(n, 100);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn writes_never_overlap() {
    let (_dir, db) = open_temp(1);
    let in_flight = Arc::new(AtomicBool::new(false));
    let overlaps = Arc::new(AtomicUsize::new(0));
    let tasks: Vec<_> = (0..50)
        .map(|_| {
            let (db, in_flight, overlaps) = (db.clone(), in_flight.clone(), overlaps.clone());
            tokio::spawn(async move {
                db.write(move |_| {
                    if in_flight.swap(true, Ordering::SeqCst) {
                        overlaps.fetch_add(1, Ordering::SeqCst);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(1));
                    in_flight.store(false, Ordering::SeqCst);
                    Ok::<_, DbError>(())
                })
                .await
            })
        })
        .collect();
    for t in tasks {
        t.await.expect("join").expect("write");
    }
    assert_eq!(overlaps.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn migrations_apply_once_across_reopens() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("riot-proxy.db");
    let history = |db: Db| async move {
        db.read(|c| {
            Ok::<i64, DbError>(c.query_row("SELECT COUNT(*) FROM refinery_schema_history", [], |r| r.get(0))?)
        })
        .await
    };

    let first = Db::open(&path, 1).expect("first open");
    first
        .write(|c| {
            c.execute(
                "INSERT INTO consumers (id, name, key_sha256, scopes, created_at) VALUES ('c1', 'n', x'00', '[]', 0)",
                [],
            )
            .map_err(DbError::from)
        })
        .await
        .expect("insert");
    assert_eq!(history(first.clone()).await.expect("history"), 29);
    drop(first);

    let second = Db::open(&path, 1).expect("second open");
    assert_eq!(history(second.clone()).await.expect("history"), 29, "no re-run");
    let name: Option<String> = second
        .read(|c| {
            Ok::<_, DbError>(
                c.query_row("SELECT name FROM consumers WHERE id = 'c1'", [], |r| r.get(0))
                    .optional()?,
            )
        })
        .await
        .expect("row survives");
    assert_eq!(name.as_deref(), Some("n"));

    // And directly: running the embedded runner again is a no-op.
    let mut conn = Connection::open(&path).expect("raw open");
    let report = migrate(&mut conn).expect("migrate again");
    assert!(report.applied_migrations().is_empty());
}

#[tokio::test]
async fn readers_are_read_only() {
    let (_dir, db) = open_temp(1);
    let err = db
        .read(|c| {
            c.execute("INSERT INTO metrics_history (at, point) VALUES (1, '{}')", [])
                .map_err(DbError::from)
        })
        .await
        .expect_err("read-only");
    assert!(err.to_string().contains("readonly"), "{err}");
}

#[tokio::test]
async fn readers_see_committed_writes() {
    let (_dir, db) = open_temp(3);
    db.write(|c| {
        c.execute("INSERT INTO metrics_history (at, point) VALUES (42, '{}')", [])
            .map_err(DbError::from)
    })
    .await
    .expect("write");
    for _ in 0..3 {
        let at: i64 = db
            .read(|c| Ok::<_, DbError>(c.query_row("SELECT at FROM metrics_history", [], |r| r.get(0))?))
            .await
            .expect("read");
        assert_eq!(at, 42);
    }
}

#[tokio::test]
async fn a_panicking_write_does_not_kill_the_writer() {
    let (_dir, db) = open_temp(1);
    let err = db
        .write(|_| -> Result<(), DbError> { panic!("boom") })
        .await
        .expect_err("panic surfaces as an error");
    assert!(matches!(err, DbError::WriterGone), "{err:?}");

    let n = db
        .write(|c| {
            c.execute("INSERT INTO metrics_history (at, point) VALUES (1, '{}')", [])
                .map_err(DbError::from)
        })
        .await
        .expect("writer still alive");
    assert_eq!(n, 1);
}

#[tokio::test]
async fn a_panicking_transaction_is_rolled_back() {
    let (_dir, db) = open_temp(1);
    let _ = db
        .write(|c| -> Result<(), DbError> {
            let tx = c.transaction()?;
            tx.execute("INSERT INTO metrics_history (at, point) VALUES (7, '{}')", [])?;
            panic!("mid-transaction");
        })
        .await;
    let n: i64 = db
        .read(|c| Ok::<_, DbError>(c.query_row("SELECT COUNT(*) FROM metrics_history", [], |r| r.get(0))?))
        .await
        .expect("count");
    assert_eq!(n, 0);
}

#[tokio::test]
async fn a_panicking_read_returns_its_connection() {
    let (_dir, db) = open_temp(1);
    let err = db
        .read(|_| -> Result<(), DbError> { panic!("boom") })
        .await
        .expect_err("panic");
    assert!(matches!(err, DbError::ReaderPanicked), "{err:?}");
    // With a pool of one, this would hang or fail if the connection had been lost.
    db.read(|c| Ok::<_, DbError>(pragma(c, "journal_mode")?))
        .await
        .expect("pool intact");
    assert_eq!(db.readers_free(), 1);
}

/// Wait (up to 2 s) until `want` readers are idle, then assert it.
async fn wait_for_idle_readers(db: &Db, want: usize) {
    for _ in 0..200 {
        if db.readers_free() == want {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(db.readers_free(), want, "idle readers");
}

/// INC-01 (ADR-113): a `read` future dropped while its closure runs, as `try_join!`
/// does to a sibling and axum does when the client goes away, must not lose the
/// connection. Before the fix, two of these on a two-reader pool made every later
/// read fail with `PoolEmpty`.
#[tokio::test]
async fn a_cancelled_read_returns_its_connection() {
    let (_dir, db) = open_temp(2);
    let finished = Arc::new(AtomicUsize::new(0));
    for _ in 0..5 {
        let done = Arc::clone(&finished);
        let slow = db.read(move |c| {
            std::thread::sleep(std::time::Duration::from_millis(100));
            done.fetch_add(1, Ordering::SeqCst);
            Ok::<_, DbError>(pragma(c, "journal_mode")?)
        });
        let res = tokio::time::timeout(std::time::Duration::from_millis(10), slow).await;
        assert!(
            res.is_err(),
            "the read should still be running when it is dropped"
        );
    }
    // The first two were dropped mid-query; their closures still run to the end and
    // hand the connections back. The other three were dropped while waiting for a
    // reader, so theirs never ran.
    wait_for_idle_readers(&db, 2).await;
    assert_eq!(finished.load(Ordering::SeqCst), 2);
    // Both readers at once, so a lost connection can't hide behind the other one.
    let (a, b) = tokio::join!(
        db.read(|c| Ok::<_, DbError>(pragma(c, "journal_mode")?)),
        db.read(|c| Ok::<_, DbError>(pragma(c, "journal_mode")?)),
    );
    assert_eq!(a.expect("first reader"), "wal");
    assert_eq!(b.expect("second reader"), "wal");
}

/// Many reads cancelled together, with more waiting on the semaphore than there are
/// connections: whichever are dropped, the pool ends full and serves every reader.
#[tokio::test]
async fn many_cancelled_reads_leave_the_pool_full() {
    let (_dir, db) = open_temp(2);
    let reads: Vec<_> = (0..20)
        .map(|_| {
            let db = db.clone();
            tokio::spawn(async move {
                db.read(|_| {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    Ok::<_, DbError>(())
                })
                .await
            })
        })
        .collect();
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    for r in &reads {
        r.abort();
    }
    for r in reads {
        let _ = r.await;
    }
    // Aborted tasks may leave closures running on the blocking pool.
    wait_for_idle_readers(&db, 2).await;
    let all = futures_util::future::join_all(
        (0..10).map(|_| db.read(|c| Ok::<_, DbError>(pragma(c, "journal_mode")?))),
    )
    .await;
    assert!(all.iter().all(|r| matches!(r, Ok(m) if m == "wal")), "{all:?}");
}

#[tokio::test]
async fn open_async_and_custom_error_types() {
    #[derive(Debug)]
    enum AppError {
        Db,
        NotFound,
    }
    impl From<DbError> for AppError {
        fn from(_: DbError) -> Self {
            Self::Db
        }
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let db = Db::open_async(dir.path().join("a.db"), 1).await.expect("open");
    let res: Result<(), AppError> = db.read(|_| Err(AppError::NotFound)).await;
    assert!(matches!(res, Err(AppError::NotFound)));
    assert!(Db::default_readers() >= 1);
    assert_eq!(db.path(), dir.path().join("a.db"));
}

/// V0003 (ADR-043): CS and game length, nullable for rows archived before it.
#[tokio::test]
async fn cs_and_game_duration_columns_exist_and_are_nullable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Db::open(&dir.path().join("riot-proxy.db"), 1).expect("open");
    let cols = |table: &'static str| {
        let db = db.clone();
        async move {
            db.read(move |c| {
                let mut s = c.prepare(&format!(
                    "SELECT name, \"notnull\" FROM pragma_table_info('{table}')"
                ))?;
                let rows = s.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
                Ok::<_, DbError>(rows.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .expect("columns")
        }
    };
    assert!(cols("matches").await.contains(&("game_duration".to_string(), 0)));
    assert!(cols("match_facts").await.contains(&("cs".to_string(), 0)));
}

/// One live crawl per ladder (v1's partial unique index), and a crawl's legs and
/// match ids go with it.
#[tokio::test]
async fn one_running_crawl_per_ladder_and_its_state_cascades() {
    let (_dir, db) = open_temp(1);
    let (second_live, after_finish, legs) = db
        .write(|c| {
            let crawl = |id: &str| {
                format!(
                    "INSERT INTO ladder_crawls (id, key_scope, platform, queue, tier_floor, started_at)
                       VALUES ('{id}', 's', 'kr', 'RANKED_SOLO_5x5', 'MASTER', 1)"
                )
            };
            c.execute(&crawl("A"), [])?;
            let second_live = c.execute(&crawl("B"), []).is_ok();
            c.execute("UPDATE ladder_crawls SET status = 'completed' WHERE id = 'A'", [])?;
            let after_finish = c.execute(&crawl("B"), []).is_ok();
            c.execute_batch(
                "INSERT INTO crawl_legs (crawl_id, leg) VALUES ('A', 'ladder:apex:MASTER');
                 INSERT INTO crawl_match_ids (crawl_id, match_id) VALUES ('A', 'KR_1');
                 DELETE FROM ladder_crawls WHERE id = 'A';",
            )?;
            let legs: i64 = c.query_row(
                "SELECT (SELECT COUNT(*) FROM crawl_legs) + (SELECT COUNT(*) FROM crawl_match_ids)",
                [],
                |r| r.get(0),
            )?;
            Ok::<_, DbError>((second_live, after_finish, legs))
        })
        .await
        .expect("write");
    assert_eq!((second_live, after_finish, legs), (false, true, 0));
}

/// `jobs` rows and the `sqlite_stat1` row count for `jobs_lane_claim`.
fn add_jobs(c: &Connection, from: usize, to: usize) -> rusqlite::Result<()> {
    for i in from..to {
        c.execute(
            "INSERT INTO jobs (id, kind, priority, payload, state, run_after) VALUES (?1, 'poll:live', 1, '{}', 'pending', 0)",
            [format!("j{i}")],
        )?;
    }
    Ok(())
}

fn analysed_rows(c: &Connection) -> rusqlite::Result<Option<i64>> {
    c.query_row(
        "SELECT CAST(stat AS INTEGER) FROM sqlite_stat1 WHERE tbl = 'jobs' AND idx = 'jobs_lane_claim'",
        [],
        |r| r.get(0),
    )
    .optional()
}

#[tokio::test]
async fn opening_analyses_tables_that_have_rows_but_no_statistics() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("riot-proxy.db");
    let db = Db::open(&path, 1).expect("open");
    db.write(|c| {
        add_jobs(c, 0, 200)?;
        // Opened empty, so nothing to analyse in `jobs` yet.
        assert_eq!(analysed_rows(c)?, None);
        Ok::<_, DbError>(())
    })
    .await
    .expect("seed");
    drop(db);
    let db = Db::open(&path, 1).expect("reopen");
    // ADR-102: the reopen analysed `jobs`, and a reader sees it.
    let rows = db
        .read(|c| Ok::<_, DbError>(analysed_rows(c)?))
        .await
        .expect("read");
    assert_eq!(rows, Some(200));
}

#[tokio::test]
async fn optimize_reanalyses_a_table_that_grew_tenfold() {
    let (_dir, db) = open_temp(1);
    let rows = |from, to| {
        db.write(move |c| {
            add_jobs(c, from, to)?;
            optimize(c)?;
            Ok::<_, DbError>(analysed_rows(c)?)
        })
    };
    assert_eq!(rows(0, 100).await.expect("first"), Some(100));
    // Doubling is within SQLite's margin: the statistics stand.
    assert_eq!(rows(100, 200).await.expect("double"), Some(100));
    assert_eq!(rows(200, 1_000).await.expect("tenfold"), Some(1_000));
}

/// V0022 (ADM-02): every count over `jobs` reads `jobs_state_kind` alone,
/// never the table and its payloads.
#[tokio::test]
async fn job_counts_read_only_the_state_kind_index() {
    let (_dir, db) = open_temp(1);
    for sql in [
        crate::stats::QUEUE_COUNTS_SQL,
        crate::jobs::scheduler::STATS_SQL,
        crate::jobs::scheduler::PENDING_BY_KIND_SQL,
    ] {
        let plan: Vec<String> = db
            .read(move |c| {
                let mut stmt = c.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?;
                // Any parameters bind as NULL: the plan does not depend on them.
                let nulls = vec![rusqlite::types::Null; stmt.parameter_count()];
                let rows = stmt
                    .query_map(rusqlite::params_from_iter(nulls), |r| r.get(3))?
                    .collect::<Result<Vec<String>, _>>()?;
                Ok::<_, DbError>(rows)
            })
            .await
            .expect("plan");
        assert!(
            plan.iter().any(|d| d.contains("COVERING INDEX jobs_state_kind")),
            "{sql}: {plan:?}"
        );
    }
}

/// WR-01 (ADR-179): every job statement of a crawl's activity view seeks an
/// index, with and without statistics. Before, `kind LIKE 'ladder:%'` and the
/// downloads count read every pending or archive:match row from the table.
#[tokio::test]
async fn crawl_activity_reads_seek_their_indexes() {
    use crate::jobs::ladder::store::activity_sql;
    let (_dir, db) = open_temp(1);
    let plans = |db: Db| async move {
        db.read(|c| {
            let sql = activity_sql();
            let id = "01M5A4VKTJ7HDGXBV1V2ZPPG5S";
            let plan = |sql: &str, args: &[&dyn rusqlite::ToSql]| -> Result<String, DbError> {
                let mut stmt = c.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?;
                let rows = stmt
                    .query_map(args, |r| r.get::<_, String>(3))?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(rows.join(" | "))
            };
            Ok::<_, DbError>(vec![
                ("running", plan(&sql.running, &[&id])?),
                ("next", plan(&sql.next, &[&id, &1i64, &20i64])?),
                ("failed", plan(&sql.failed, &[&id, &20i64])?),
                ("done_recently", plan(&sql.done_recently, &[&id, &1i64])?),
                (
                    "downloads",
                    plan(&sql.downloads, &[&"OC1\\_%", &1i64, &1i64, &20_005i64])?,
                ),
            ])
        })
        .await
        .expect("plans")
    };
    let check = |plans: Vec<(&str, String)>, when: &str| {
        for (name, plan) in plans {
            assert!(!plan.contains("SCAN jobs"), "{name} {when}: {plan}");
            let want = if name == "downloads" {
                "COVERING INDEX jobs_crawl_matches (priority=? AND dedupe_key>? AND dedupe_key<?)"
            } else {
                "INDEX jobs_state_kind (state=? AND kind=?)"
            };
            assert!(plan.contains(want), "{name} {when}: {plan}");
        }
    };
    check(plans(db.clone()).await, "without statistics");

    // A queue shaped like production's: mostly pending archive:match.
    db.write(|c| {
        let tx = c.transaction()?;
        for i in 0..2_000 {
            let (state, finished) = if i % 4 == 0 {
                ("done", Some(i))
            } else {
                ("pending", None)
            };
            tx.execute(
                "INSERT INTO jobs (id, kind, dedupe_key, priority, payload, state, run_after, finished_at)
                 VALUES (?1, 'archive:match', ?2, 20005, '{}', ?3, 0, ?4)",
                params![format!("{i:026}"), format!("OC1_{i}"), state, finished],
            )?;
        }
        tx.execute(
            "INSERT INTO jobs (id, kind, priority, payload, state, run_after)
             VALUES ('X', 'ladder:apex', 20001, '{\"crawlId\":\"c\"}', 'pending', 0)",
            [],
        )?;
        tx.execute_batch("ANALYZE")?;
        tx.commit()?;
        Ok::<_, DbError>(())
    })
    .await
    .expect("seed");
    check(plans(db).await, "with statistics");
}

/// The activity view's kind list is every ladder kind (WR-01).
#[test]
fn crawl_activity_names_every_ladder_kind() {
    let ladder: Vec<&str> = crate::jobs::kinds::ALL
        .iter()
        .copied()
        .filter(|k| k.starts_with("ladder:"))
        .collect();
    assert_eq!(crate::jobs::ladder::store::LADDER_KINDS.to_vec(), ladder);
}

/// V0023 (OBJ-02, ADR-163) adds the pack tables and the state columns. Every
/// existing body is local: unpacked, not offloaded, its blob untouched, and
/// in the "to pack" indexes.
#[tokio::test]
async fn v0023_leaves_every_existing_body_local() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("riot-proxy.db");
    {
        let mut conn = Connection::open(&path).expect("open");
        embedded::migrations::runner()
            .set_grouped(true)
            .set_target(refinery::Target::Version(22))
            .run(&mut conn)
            .expect("to V0022");
        conn.execute_batch(
            "INSERT INTO matches (match_id, region, patch, queue_id, game_end_ms, body_zstd, body_size, archived_at)
               VALUES ('KR_1', 'asia', '14.18', 420, 1, x'28b52ffd0001', 9, 5),
                      ('KR_2', 'asia', '14.18', 420, 1, x'28b52ffd0002', 9, 6);
             INSERT INTO timelines (match_id, body_zstd) VALUES ('KR_1', x'28b52ffd0003');",
        )
        .expect("seed");
    }
    let db = Db::open(&path, 1).expect("migrate");
    let tables = names(&db, "table").await;
    assert!(tables.contains(&"body_packs".to_string()) && tables.contains(&"body_locations".to_string()));
    let (rows, to_pack, to_free, packs, located): (Vec<String>, i64, i64, i64, i64) = db
        .read(|c| {
            let mut stmt = c.prepare(
                "SELECT match_id || ' ' || hex(body_zstd) || ' ' || coalesce(packed_at, '-') || ' '
                        || coalesce(offloaded_at, '-') FROM matches
                 UNION ALL
                 SELECT 't' || match_id || ' ' || hex(body_zstd) || ' ' || coalesce(packed_at, '-') || ' '
                        || coalesce(offloaded_at, '-') FROM timelines ORDER BY 1",
            )?;
            let rows = stmt.query_map([], |r| r.get(0))?.collect::<Result<Vec<String>, _>>()?;
            let n = |sql: &str| c.query_row(sql, [], |r| r.get::<_, i64>(0));
            Ok::<_, DbError>((
                rows,
                n("SELECT (SELECT count(*) FROM matches WHERE packed_at IS NULL)
                        + (SELECT count(*) FROM timelines WHERE packed_at IS NULL)")?,
                n("SELECT (SELECT count(*) FROM matches WHERE packed_at IS NOT NULL AND offloaded_at IS NULL)
                        + (SELECT count(*) FROM timelines WHERE packed_at IS NOT NULL AND offloaded_at IS NULL)")?,
                n("SELECT count(*) FROM body_packs")?,
                n("SELECT count(*) FROM body_locations")?,
            ))
        })
        .await
        .expect("read");
    assert_eq!(
        rows,
        [
            "KR_1 28B52FFD0001 - -",
            "KR_2 28B52FFD0002 - -",
            "tKR_1 28B52FFD0003 - -"
        ]
    );
    assert_eq!((to_pack, to_free, packs, located), (3, 0, 0, 0));
}

/// V0025 (PERF-02, ADR-168) fills `champion_stats_totals` with
/// `champion_stats` summed over tier, key for key, `matches_picked` a plain
/// sum and `computed_at` the newest; the table is WITHOUT ROWID,
/// `champion_stats` keeps its rows, and `champion_stats_slice` is gone.
#[tokio::test]
async fn v0025_sums_champion_stats_over_tier_and_drops_the_slice_index() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("riot-proxy.db");
    {
        let mut conn = Connection::open(&path).expect("open");
        embedded::migrations::runner()
            .set_grouped(true)
            .set_target(refinery::Target::Version(24))
            .run(&mut conn)
            .expect("to V0024");
        // One key in three tiers; the same key on the other side; a remake;
        // another patch; another platform.
        conn.execute_batch(
            "INSERT INTO champion_stats (key_scope, platform, queue, tier, patch, champion_id, role, remake, side,
               games, wins, matches_picked, stated_games, kills, deaths, assists, cs, gold, damage, vision,
               duration_s, computed_at) VALUES
               ('s', 'kr', 'RANKED_SOLO_5x5', 'MASTER',  '14.18', 1, 'MIDDLE', 0, 'blue', 3, 2, 3, 3, 1, 2, 3, 4, 5, 6, 7, 8, 100),
               ('s', 'kr', 'RANKED_SOLO_5x5', 'DIAMOND', '14.18', 1, 'MIDDLE', 0, 'blue', 2, 1, 2, 1, 10, 20, 30, 40, 50, 60, 70, 80, 300),
               ('s', 'kr', 'RANKED_SOLO_5x5', 'UNKNOWN', '14.18', 1, 'MIDDLE', 0, 'blue', 1, 0, 1, 1, 100, 200, 300, 400, 500, 600, 700, 800, 200),
               ('s', 'kr', 'RANKED_SOLO_5x5', 'MASTER',  '14.18', 1, 'MIDDLE', 0, 'red', 4, 4, 4, 4, 1, 1, 1, 1, 1, 1, 1, 1, 100),
               ('s', 'kr', 'RANKED_SOLO_5x5', 'MASTER',  '14.18', 1, 'MIDDLE', 1, 'blue', 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 100),
               ('s', 'kr', 'RANKED_SOLO_5x5', 'DIAMOND', '14.17', 1, 'MIDDLE', 0, 'blue', 5, 2, 5, 5, 2, 2, 2, 2, 2, 2, 2, 2, 50),
               ('s', 'oc1', 'RANKED_SOLO_5x5', 'MASTER', '14.18', 1, 'MIDDLE', 0, 'blue', 6, 3, 6, 6, 3, 3, 3, 3, 3, 3, 3, 3, 100);",
        )
        .expect("seed");
    }
    let db = Db::open(&path, 1).expect("migrate");
    let (rows, drift, ddl, slice, stats): (Vec<String>, i64, String, i64, i64) = db
        .read(|c| {
            let mut stmt = c.prepare(
                "SELECT platform || ' ' || patch || ' ' || side || ' ' || remake || ': ' || games || ' ' || wins
                        || ' ' || matches_picked || ' ' || stated_games || ' ' || kills || ' ' || deaths || ' '
                        || assists || ' ' || cs || ' ' || gold || ' ' || damage || ' ' || vision || ' '
                        || duration_s || ' @' || computed_at
                   FROM champion_stats_totals ORDER BY key_scope, platform, queue, patch, champion_id, role, remake, side",
            )?;
            let rows = stmt.query_map([], |r| r.get(0))?.collect::<Result<Vec<String>, _>>()?;
            const KEY: &str = "key_scope, platform, queue, patch, champion_id, role, remake, side";
            let summed = format!(
                "SELECT {KEY}, sum(games), sum(wins), sum(matches_picked), sum(stated_games), sum(kills),
                        sum(deaths), sum(assists), sum(cs), sum(gold), sum(damage), sum(vision), sum(duration_s),
                        max(computed_at) FROM champion_stats GROUP BY {KEY}"
            );
            let totals = format!(
                "SELECT {KEY}, games, wins, matches_picked, stated_games, kills, deaths, assists, cs, gold, damage,
                        vision, duration_s, computed_at FROM champion_stats_totals"
            );
            let drift = c.query_row(
                &format!(
                    "SELECT (SELECT count(*) FROM ({totals} EXCEPT {summed}))
                          + (SELECT count(*) FROM ({summed} EXCEPT {totals}))"
                ),
                [],
                |r| r.get(0),
            )?;
            let ddl: String = c.query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'champion_stats_totals'",
                [],
                |r| r.get(0),
            )?;
            let slice = c.query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'index' AND name = 'champion_stats_slice'",
                [],
                |r| r.get(0),
            )?;
            let stats = c.query_row("SELECT count(*) FROM champion_stats", [], |r| r.get(0))?;
            Ok::<_, DbError>((rows, drift, ddl, slice, stats))
        })
        .await
        .expect("read");
    assert_eq!(
        rows,
        [
            "kr 14.17 blue 0: 5 2 5 5 2 2 2 2 2 2 2 2 @50",
            "kr 14.18 blue 0: 6 3 6 5 111 222 333 444 555 666 777 888 @300",
            "kr 14.18 red 0: 4 4 4 4 1 1 1 1 1 1 1 1 @100",
            "kr 14.18 blue 1: 1 1 1 0 0 0 0 0 0 0 0 0 @100",
            "oc1 14.18 blue 0: 6 3 6 6 3 3 3 3 3 3 3 3 @100",
        ]
    );
    assert_eq!(drift, 0);
    assert!(ddl.trim_end().ends_with("WITHOUT ROWID"), "{ddl}");
    assert_eq!(slice, 0, "champion_stats_slice is dropped");
    assert_eq!(stats, 7, "champion_stats keeps its rows");
}

/// V0026 (FLT-03, ADR-170) backfills each aggregated slice's floor totals as
/// a rebuild counts them: a match once in every floor at or below its best
/// stamped player, none for a match of UNKNOWN or unstamped players, a ban
/// by both teams once, only matches archived by the slice's recompute, only
/// slices that exist.
#[tokio::test]
async fn v0026_backfills_the_floors_of_every_aggregated_slice() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("riot-proxy.db");
    {
        let mut conn = Connection::open(&path).expect("open");
        embedded::migrations::runner()
            .set_grouped(true)
            .set_target(refinery::Target::Version(25))
            .run(&mut conn)
            .expect("to V0025");
        let mut sql = String::from(
            "INSERT INTO matches (match_id, region, patch, queue_id, game_end_ms, body_zstd, body_size, archived_at,
               game_duration, remake, facts_version) VALUES
               ('KR_1', 'asia', '14.18', 420, 1, x'00', 1, 50, 1500, 0, 3),
               ('KR_2', 'asia', '14.18', 420, 1, x'00', 1, 50, 1500, 0, 3),
               ('KR_3', 'asia', '14.18', 420, 1, x'00', 1, 50, 1500, 0, 3),
               ('KR_4', 'asia', '14.18', 420, 1, x'00', 1, 50, 200, 1, 3),
               ('KR_5', 'asia', '14.18', 440, 1, x'00', 1, 50, 1500, 0, 3),
               ('KR_6', 'asia', '14.17', 420, 1, x'00', 1, 50, 1500, 0, 3),
               ('KR_9', 'asia', '14.18', 420, 1, x'00', 1, 200, 1500, 0, 3),
               ('OC1_1', 'sea', '14.18', 420, 1, x'00', 1, 50, 1500, 0, 3);
             INSERT INTO match_bans (match_id, team_id, pick_turn, champion_id) VALUES
               ('KR_1', 100, 1, 9), ('KR_1', 200, 6, 9), ('KR_2', 100, 1, 10), ('KR_3', 100, 1, 11);
             INSERT INTO analytics_slices (key_scope, platform, queue, tier, patch, remake, matches, computed_at) VALUES
               ('s', 'kr', 'RANKED_SOLO_5x5', 'MASTER', '14.18', 0, 1, 100),
               ('s', 'kr', 'RANKED_SOLO_5x5', 'EMERALD', '14.18', 1, 1, 100),
               ('s', 'oc1', 'RANKED_SOLO_5x5', 'MASTER', '14.18', 0, 1, 100);
             INSERT INTO match_facts (match_id, key_scope, puuid, team_id, champion_id, win, facts_version, tier) VALUES
               ('KR_1', 's', 'A', 100, 1, 1, 3, 'MASTER'), ('KR_1', 's', 'B', 200, 2, 0, 3, 'GRANDMASTER'),
               ('KR_1', 's', 'C', 200, 3, 0, 3, 'UNKNOWN'), ('KR_1', 's', 'D', 100, 4, 1, 3, NULL),
               ('KR_3', 's', 'U1', 100, 1, 1, 3, 'UNKNOWN'), ('KR_3', 's', 'U2', 200, 2, 0, 3, NULL),
               ('KR_4', 's', 'E', 100, 1, 1, 3, 'EMERALD'),
               ('KR_5', 's', 'F', 100, 1, 1, 3, 'GOLD'),
               ('KR_6', 's', 'F', 100, 1, 1, 3, 'GOLD'),
               ('KR_9', 's', 'A', 100, 1, 1, 3, 'MASTER'),
               ('OC1_1', 's', 'O', 100, 1, 1, 3, 'MASTER')",
        );
        // KR_2 has a player in every tier, so it is in every floor.
        for (i, t) in crate::riot::ladder::tiers().enumerate() {
            sql.push_str(&format!(
                ", ('KR_2', 's', 'T{i}', {}, {}, 1, 3, '{t}')",
                100 + 100 * (i % 2),
                20 + i
            ));
        }
        sql.push(';');
        conn.execute_batch(&sql).expect("seed");
    }
    let db = Db::open(&path, 1).expect("migrate");
    let read = |db: Db| async move {
        db.read(|c| {
            let rows = |sql: &str| -> Result<Vec<String>, DbError> {
                let mut stmt = c.prepare(sql)?;
                let out = stmt.query_map([], |r| r.get(0))?.collect::<Result<Vec<String>, _>>()?;
                Ok(out)
            };
            Ok::<_, DbError>((
                rows(
                    "SELECT platform || ' ' || patch || ' ' || remake || ' ' || group_concat(floor_tier || '=' || matches, ',')
                       || ' @' || max(computed_at)
                       FROM (SELECT * FROM analytics_floor_totals ORDER BY floor_tier)
                      GROUP BY platform, queue, patch, remake ORDER BY platform, remake",
                )?,
                rows(
                    "SELECT champion_id || ' ' || group_concat(floor_tier || '=' || bans, ',')
                       FROM (SELECT * FROM champion_ban_floor_totals ORDER BY floor_tier)
                      GROUP BY champion_id ORDER BY champion_id",
                )?,
            ))
        })
        .await
        .expect("read")
    };
    let (floors, bans) = read(db.clone()).await;
    assert_eq!(
        floors,
        [
            "kr 14.18 0 BRONZE=2,CHALLENGER=1,DIAMOND=2,EMERALD=2,GOLD=2,GRANDMASTER=2,IRON=2,MASTER=2,PLATINUM=2,SILVER=2 @100",
            "kr 14.18 1 BRONZE=1,EMERALD=1,GOLD=1,IRON=1,PLATINUM=1,SILVER=1 @100",
            "oc1 14.18 0 BRONZE=1,DIAMOND=1,EMERALD=1,GOLD=1,IRON=1,MASTER=1,PLATINUM=1,SILVER=1 @100",
        ]
    );
    assert_eq!(
        bans,
        [
            "9 BRONZE=1,DIAMOND=1,EMERALD=1,GOLD=1,GRANDMASTER=1,IRON=1,MASTER=1,PLATINUM=1,SILVER=1",
            "10 BRONZE=1,CHALLENGER=1,DIAMOND=1,EMERALD=1,GOLD=1,GRANDMASTER=1,IRON=1,MASTER=1,PLATINUM=1,SILVER=1",
        ]
    );
    let (ddl, temp): (Vec<String>, i64) = db
        .read(|c| {
            let mut stmt = c.prepare(
                "SELECT sql FROM sqlite_master WHERE type = 'table'
                   AND name IN ('analytics_floor_totals', 'champion_ban_floor_totals')",
            )?;
            let ddl = stmt
                .query_map([], |r| r.get(0))?
                .collect::<Result<Vec<String>, _>>()?;
            let temp = c.query_row(
                "SELECT count(*) FROM sqlite_master WHERE name = 'v26_top'",
                [],
                |r| r.get(0),
            )?;
            Ok::<_, DbError>((ddl, temp))
        })
        .await
        .expect("ddl");
    assert_eq!(ddl.len(), 2);
    assert!(
        ddl.iter().all(|d| d.trim_end().ends_with("WITHOUT ROWID")),
        "{ddl:?}"
    );
    assert_eq!(temp, 0);

    // A rebuild of the kr ladder at the slices' time counts the same, once
    // KR_9 (archived after that recompute) is gone.
    db.write(|c| {
        c.execute("DELETE FROM matches WHERE match_id = 'KR_9'", [])?;
        let s = crate::archive::analytics::Scope {
            key_scope: "s".into(),
            platform: "kr".into(),
            queue: "RANKED_SOLO_5x5".into(),
            queue_id: 420,
            patches: Some(vec!["14.18".into()]),
            now: 100,
        };
        crate::archive::analytics::rebuild_champions(c, &s)?;
        Ok::<_, DbError>(())
    })
    .await
    .expect("rebuild");
    assert_eq!(read(db.clone()).await, (floors, bans));
}

/// V0027 (FLT-04, ADR-171) creates the set builds against a lane opponent,
/// empty (no backfill: the next rebuild fills them), WITHOUT ROWID, keyed as
/// champion_matchups up to the opponent and champion_builds after it.
#[tokio::test]
async fn v0027_creates_the_matchup_build_tables() {
    let (_dir, db) = open_temp(1);
    let got: Vec<(String, String, i64, Vec<String>)> = db
        .read(|c| {
            let mut out = vec![];
            for t in ["champion_matchup_builds", "champion_matchup_build_parts"] {
                let ddl: String =
                    c.query_row("SELECT sql FROM sqlite_master WHERE name = ?1", [t], |r| r.get(0))?;
                let rows: i64 = c.query_row(&format!("SELECT count(*) FROM {t}"), [], |r| r.get(0))?;
                let mut stmt = c.prepare(&format!(
                    "SELECT name FROM pragma_table_info('{t}') WHERE pk > 0 ORDER BY pk"
                ))?;
                let key = stmt
                    .query_map([], |r| r.get(0))?
                    .collect::<Result<Vec<String>, _>>()?;
                out.push((t.to_string(), ddl, rows, key));
            }
            Ok::<_, DbError>(out)
        })
        .await
        .expect("ddl");
    let head = "key_scope platform queue patch champion_id role opponent_id remake core";
    for (t, ddl, rows, key) in got {
        assert!(ddl.trim_end().ends_with("WITHOUT ROWID"), "{t}: {ddl}");
        assert_eq!(rows, 0, "{t} is not backfilled");
        let want = if t == "champion_matchup_builds" {
            format!("{head} tier side")
        } else {
            format!("{head} part value tier side")
        };
        assert_eq!(key.join(" "), want, "{t}");
    }
}

/// V0029 (THR-03, ADR-182): the stamp tables start empty; the runs keep
/// their rows as rebuilds; the trigger records, for a counted player whose
/// tier moves, one row per step that counted them, at the old tier, and
/// nothing for an uncounted match or an unchanged tier.
#[tokio::test]
async fn v0029_creates_the_stamp_tables_and_the_tier_trigger() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("riot-proxy.db");
    {
        let mut conn = Connection::open(&path).expect("open");
        embedded::migrations::runner()
            .set_grouped(true)
            .set_target(refinery::Target::Version(28))
            .run(&mut conn)
            .expect("to V0028");
        conn.execute_batch(
            "INSERT INTO analytics_runs (key_scope, platform, queue, at, status, ms, steps, rows, games)
               VALUES ('s', 'kr', 'RANKED_SOLO_5x5', 5, 'completed', 9, '{}', '{}', 3);
             INSERT INTO matches (match_id, region, patch, queue_id, game_end_ms, body_zstd, body_size, archived_at)
               VALUES ('KR_1', 'asia', '14.18', 420, 1, x'00', 1, 1), ('KR_2', 'asia', '14.18', 420, 1, x'00', 1, 1);
             INSERT INTO match_facts (match_id, key_scope, puuid, team_id, champion_id, win, facts_version, tier) VALUES
               ('KR_1', 's', 'A', 100, 1, 1, 4, 'UNKNOWN'), ('KR_1', 's', 'B', 200, 2, 0, 4, 'MASTER'),
               ('KR_2', 's', 'C', 100, 1, 1, 4, 'UNKNOWN');",
        )
        .expect("seed");
    }
    let db = Db::open(&path, 1).expect("to V0029");
    let got = db
        .write(|c| {
            let empty: i64 = c.query_row(
                "SELECT (SELECT count(*) FROM analytics_counted) + (SELECT count(*) FROM analytics_retract)
                      + (SELECT count(*) FROM analytics_stamped)",
                [],
                |r| r.get(0),
            )?;
            let run: (String, i64) =
                c.query_row("SELECT kind, games FROM analytics_runs", [], |r| Ok((r.get(0)?, r.get(1)?)))?;
            // KR_1 counted by champions (1) and items (4); KR_2 by nothing.
            c.execute_batch(
                "INSERT INTO analytics_counted (match_id, key_scope, bits) VALUES ('KR_1', 's', 5), ('KR_2', 's', 0);
                 UPDATE match_facts SET tier = 'MASTER' WHERE puuid = 'B';
                 UPDATE match_facts SET tier = 'MASTER' WHERE tier = 'UNKNOWN';
                 UPDATE match_facts SET tier = 'GRANDMASTER' WHERE puuid = 'A';",
            )?;
            let mut stmt =
                c.prepare("SELECT match_id, puuid, step, tier FROM analytics_retract ORDER BY match_id, puuid, step")?;
            let rows = stmt
                .query_map([], |r| {
                    Ok(format!(
                        "{} {} {} {}",
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, String>(3)?
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, DbError>((empty, run, rows))
        })
        .await
        .expect("trigger");
    assert_eq!(got.0, 0, "empty");
    assert_eq!(got.1, ("rebuild".to_string(), 3), "the run kept, as a rebuild");
    // A moved twice; the first move (from UNKNOWN) is kept.
    assert_eq!(got.2, ["KR_1 A 1 UNKNOWN", "KR_1 A 4 UNKNOWN"]);
}

/// PERF-03: a cached analytics hit is proven by this counter not moving.
#[tokio::test]
async fn the_reads_counter_counts_every_read() {
    let (_dir, db) = open_temp(2);
    let before = db.reads();
    for _ in 0..3 {
        db.read(|c| Ok::<_, DbError>(c.query_row("SELECT 1", [], |r| r.get::<_, i64>(0))?))
            .await
            .unwrap();
    }
    db.write(|c| Ok::<_, DbError>(c.execute_batch("SELECT 1")?))
        .await
        .unwrap();
    assert_eq!(db.reads() - before, 3, "writes don't count");
}

/// ADR-180's overhead figure: a 1 M-row insert in one transaction, row by row and
/// as one statement, on a plain connection, without the progress stamps and with
/// them at several intervals.
/// `cargo test --release --lib progress_handler_overhead -- --ignored --nocapture`
#[test]
#[ignore = "timing, run by hand"]
fn progress_handler_overhead() {
    fn insert(ops: Option<i32>) -> (std::time::Duration, std::time::Duration) {
        let dir = tempfile::tempdir().expect("tempdir");
        let conn = Connection::open(dir.path().join("bench.db")).expect("open");
        apply_pragmas(&conn, WRITER_PRAGMAS).expect("pragmas");
        let stamps = Arc::new(Stamps::new());
        if let Some(ops) = ops {
            let s = Arc::clone(&stamps);
            conn.progress_handler(
                ops,
                Some(move || {
                    s.progress();
                    false
                }),
            )
            .expect("handler");
        }
        conn.execute_batch("CREATE TABLE t (a INTEGER PRIMARY KEY, b TEXT, c INTEGER)")
            .expect("table");
        let started = std::time::Instant::now();
        conn.execute_batch("BEGIN").expect("begin");
        {
            let mut stmt = conn
                .prepare_cached("INSERT INTO t (b, c) VALUES (?1, ?2)")
                .expect("prepare");
            for i in 0..1_000_000_i64 {
                stmt.execute(params![format!("row-{i}"), i * 7]).expect("insert");
            }
        }
        conn.execute_batch("COMMIT").expect("commit");
        let one_by_one = started.elapsed();
        // The rebuild shape: one statement writing every row.
        let started = std::time::Instant::now();
        conn.execute_batch("CREATE TABLE u AS SELECT a, b, c * 2 AS c FROM t ORDER BY c DESC")
            .expect("bulk");
        (one_by_one, started.elapsed())
    }
    // Interleaved rounds, so drift in the machine's speed lands on every setting.
    let settings = [None, Some(1000), Some(100), Some(10)];
    let mut rows = vec![Vec::new(); settings.len()];
    let mut bulk = vec![Vec::new(); settings.len()];
    for _ in 0..11 {
        for (k, ops) in settings.iter().enumerate() {
            let (r, b) = insert(*ops);
            rows[k].push(r);
            bulk[k].push(b);
        }
    }
    let median = |v: &mut Vec<std::time::Duration>| {
        v.sort();
        v[v.len() / 2]
    };
    let (base_rows, base_bulk) = (median(&mut rows[0]), median(&mut bulk[0]));
    for (k, ops) in settings.iter().enumerate() {
        let (r, b) = (median(&mut rows[k]), median(&mut bulk[k]));
        let pct = |x: std::time::Duration, base: std::time::Duration| {
            100.0 * (x.as_secs_f64() / base.as_secs_f64() - 1.0)
        };
        println!(
            "ops={ops:?} row-by-row median={r:?} ({:+.1} %) bulk median={b:?} ({:+.1} %)",
            pct(r, base_rows),
            pct(b, base_bulk)
        );
    }
}

/// A write that blocks the writer until `release` is sent, after telling `started`.
fn hold_writer(db: &Db) -> (std::sync::mpsc::Sender<()>, tokio::task::JoinHandle<()>) {
    let (release, wait) = std::sync::mpsc::channel::<()>();
    let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
    let db = db.clone();
    let task = tokio::spawn(async move {
        db.write(move |_| {
            let _ = started_tx.send(());
            let _ = wait.recv();
            Ok::<_, DbError>(())
        })
        .await
        .expect("held write");
    });
    started_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("the held write started");
    (release, task)
}

async fn wait_for_idle_writer(db: &Db) {
    for _ in 0..500 {
        if !db.writer_status().busy() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
    panic!("writer still busy: {:?}", db.writer_status());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn writer_status_goes_idle_busy_idle_and_counts_the_queue() {
    let (_dir, db) = open_temp(1);
    db.write(|c| c.execute_batch("SELECT 1").map_err(DbError::from))
        .await
        .expect("warm-up write");
    wait_for_idle_writer(&db).await;
    let idle = db.writer_status();
    assert_eq!(idle.busy_since, None);
    assert_eq!(idle.busy_for, None);
    assert_eq!(idle.queued, 0);
    assert!(idle.last_finished > 0 && idle.last_progress > 0, "{idle:?}");

    let (release, held) = hold_writer(&db);
    let busy = db.writer_status();
    assert!(busy.busy(), "{busy:?}");
    // Unix times are derived per read, so two reads may differ by a millisecond.
    assert!(busy.busy_since.expect("busy_since") + 1 >= idle.last_finished);
    assert_eq!(busy.queued, 0);

    let queued: Vec<_> = (0..3)
        .map(|_| {
            let db = db.clone();
            tokio::spawn(async move { db.write(|_| Ok::<_, DbError>(())).await })
        })
        .collect();
    for _ in 0..500 {
        if db.writer_status().queued == 3 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
    assert_eq!(db.writer_status().queued, 3);

    release.send(()).expect("release");
    held.await.expect("held");
    for q in queued {
        q.await.expect("join").expect("queued write");
    }
    wait_for_idle_writer(&db).await;
    let after = db.writer_status();
    assert_eq!((after.busy_since, after.queued), (None, 0));
    assert_eq!(
        after.finished,
        busy.finished + 4,
        "the held write and the three queued"
    );
    assert!(after.last_finished + 1 >= busy.busy_since.expect("busy_since"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sqlite_work_stamps_progress_and_a_sleep_does_not() {
    let (_dir, db) = open_temp(1);
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let (go, wait) = std::sync::mpsc::channel::<()>();
    let job = {
        let db = db.clone();
        tokio::spawn(async move {
            db.write(move |c| {
                c.execute_batch("CREATE TABLE t (a INTEGER)")?;
                std::thread::sleep(std::time::Duration::from_millis(150));
                let _ = tx.send(());
                let _ = wait.recv();
                // Executing for a while: progress keeps up.
                c.execute_batch(
                    "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 300000) \
                     INSERT INTO t SELECT i FROM n",
                )?;
                Ok::<_, DbError>(())
            })
            .await
        })
    };
    rx.recv_timeout(std::time::Duration::from_secs(5)).expect("slept");
    // The sleep left no progress behind it.
    let stuck = db.writer_status();
    assert!(stuck.busy());
    assert!(
        stuck.since_progress.expect("stamped") >= std::time::Duration::from_millis(100),
        "{stuck:?}"
    );
    go.send(()).expect("go");
    job.await.expect("join").expect("job");
    let done = db.writer_status();
    assert!(
        done.last_progress >= stuck.last_progress + 100,
        "{stuck:?} {done:?}"
    );
}

/// Writes that each wait out `busy_timeout` back to back must not look fresh
/// because each one starts: only a job that finds the writer idle is progress.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_job_queued_behind_another_inherits_its_stamps() {
    let (_dir, db) = open_temp(1);
    let (release, held) = hold_writer(&db);
    let first = db.writer_status();
    let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
    let (go, wait) = std::sync::mpsc::channel::<()>();
    let second = {
        let db = db.clone();
        tokio::spawn(async move {
            db.write(move |_| {
                let _ = started_tx.send(());
                let _ = wait.recv();
                Ok::<_, DbError>(())
            })
            .await
        })
    };
    for _ in 0..500 {
        if db.writer_status().queued == 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    release.send(()).expect("release");
    started_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("the queued write started");
    let s = db.writer_status();
    assert!(s.busy(), "{s:?}");
    // Unix times are derived per read and may differ by a millisecond.
    assert!(
        s.last_progress.abs_diff(first.last_progress) <= 1,
        "{first:?} {s:?}"
    );
    assert!(
        s.since_progress.expect("stamped") >= std::time::Duration::from_millis(150),
        "{s:?}"
    );
    go.send(()).expect("go");
    held.await.expect("held");
    second.await.expect("join").expect("second");

    // Idle again: the next job is fresh.
    wait_for_idle_writer(&db).await;
    let (release, held) = hold_writer(&db);
    assert!(db.writer_status().since_progress.expect("stamped") < std::time::Duration::from_millis(100));
    release.send(()).expect("release");
    held.await.expect("held");
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_writer_cpu_clock_counts_work_and_not_sleep() {
    let (_dir, db) = open_temp(1);
    let before = db.writer_cpu_time().expect("a CPU clock on Linux");
    db.write(|_| {
        std::thread::sleep(std::time::Duration::from_millis(200));
        Ok::<_, DbError>(())
    })
    .await
    .expect("sleep");
    let slept = db.writer_cpu_time().expect("clock");
    assert!(
        slept - before < std::time::Duration::from_millis(50),
        "{before:?} {slept:?}"
    );
    db.write(|_| {
        let until = std::time::Instant::now() + std::time::Duration::from_millis(200);
        let mut x = 0_u64;
        while std::time::Instant::now() < until {
            x = std::hint::black_box(x.wrapping_add(1));
        }
        Ok::<_, DbError>(())
    })
    .await
    .expect("spin");
    let worked = db.writer_cpu_time().expect("clock");
    assert!(
        worked - slept >= std::time::Duration::from_millis(100),
        "{slept:?} {worked:?}"
    );
}

#[tokio::test]
async fn a_panicking_write_still_clears_busy_since() {
    let (_dir, db) = open_temp(1);
    let r = db.write(|_| -> Result<(), DbError> { panic!("boom") }).await;
    assert!(matches!(r, Err(DbError::WriterGone)));
    wait_for_idle_writer(&db).await;
    let s = db.writer_status();
    assert_eq!(s.busy_since, None);
    assert!(s.last_finished > 0);
}
