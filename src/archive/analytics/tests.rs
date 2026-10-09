#![allow(clippy::unwrap_used, clippy::expect_used)]
//! A hand-computed fixture (plan P7-04 acceptance): five matches, worked out
//! on paper below, rebuilt and read back.
//!
//! Ladder: A and B are MASTER, C is DIAMOND; D is not on the ladder, so D
//! counts under UNKNOWN (ADR-105).
//! - M1 14.18 solo, 1800 s: A (champ 1, MIDDLE, win) vs C (champ 2, MIDDLE);
//!   D (champ 3, TOP, team 100) has no lane opponent. Bans: 10 by both teams,
//!   11 by team 200.
//! - M2 14.18 solo, 1200 s: A (champ 1, MIDDLE, loss) vs B (champ 2, MIDDLE, win). Ban: 11.
//! - M3 14.18 solo, 200 s, a remake: A (champ 1, MIDDLE, win, no K/D/A) vs C (champ 2).
//! - M4 14.17 solo: A on champ 1, an older patch.
//! - M5 14.18 flex: A on champ 1, another queue.
//!
//! Build facts (`match_builds`, BLD-02): A's core is 6655 › 3157 in M1, M2
//! and M3, and 3157 › 6655 in M4; B's in M2 is 3157 › 4645, with six items
//! and no starter or boots; C has one finished item in M1, so counts in no
//! build, and a row under another key scope that doesn't join; D has none.

use super::*;

fn db() -> (tempfile::TempDir, Db) {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("t.db"), 1).unwrap();
    (dir, db)
}

/// `(puuid, team, champion, position, win, kills/deaths/assists, cs, items, "runes|spells")`
type P = (
    &'static str,
    i64,
    i64,
    &'static str,
    bool,
    Option<(i64, i64, i64)>,
    i64,
    &'static str,
    &'static str,
);

/// `(match id, patch, queue, duration, remake, players, bans (team, turn, champion))`
type M = (
    &'static str,
    &'static str,
    i64,
    i64,
    i64,
    &'static [P],
    &'static [(i64, i64, i64)],
);

fn seed(c: &Connection) {
    c.execute_batch(
        "INSERT INTO ladder_entries (key_scope, platform, queue, puuid, tier, division, league_points, wins, losses,
           first_seen_crawl_id, last_seen_crawl_id, updated_at) VALUES
           ('s', 'kr', 'RANKED_SOLO_5x5', 'A', 'MASTER', 'I', 100, 1, 1, 'c', 'c', 1),
           ('s', 'kr', 'RANKED_SOLO_5x5', 'B', 'MASTER', 'I', 90, 1, 1, 'c', 'c', 1),
           ('s', 'kr', 'RANKED_SOLO_5x5', 'C', 'DIAMOND', 'I', 50, 1, 1, 'c', 'c', 1),
           -- Another platform's ladder does not count on kr's.
           ('s', 'euw1', 'RANKED_SOLO_5x5', 'B', 'IRON', 'IV', 0, 1, 1, 'c', 'c', 1);",
    )
    .unwrap();
    let matches: [M; 5] = [
        (
            "KR_1",
            "14.18",
            420,
            1800,
            0,
            &[
                (
                    "A",
                    100,
                    1,
                    "MIDDLE",
                    true,
                    Some((5, 2, 3)),
                    200,
                    "[3157,0,3020,0,0,0]",
                    r#"{"keystone":8112,"subStyle":8300}|[4,14]"#,
                ),
                (
                    "C",
                    200,
                    2,
                    "MIDDLE",
                    false,
                    Some((2, 5, 1)),
                    180,
                    "[3157,3157,0,0,0,0]",
                    r#"{"keystone":8010,"subStyle":8400}|[14,4]"#,
                ),
                (
                    "D",
                    100,
                    3,
                    "TOP",
                    false,
                    Some((0, 0, 0)),
                    0,
                    "[0,0,0,0,0,0]",
                    r#"{}|[4,12]"#,
                ),
            ],
            &[(100, 1, 10), (200, 6, 10), (200, 7, 11)],
        ),
        (
            "KR_2",
            "14.18",
            420,
            1200,
            0,
            &[
                (
                    "A",
                    100,
                    1,
                    "MIDDLE",
                    false,
                    Some((1, 1, 1)),
                    100,
                    "[3157,0,0,0,0,0]",
                    r#"{"keystone":8112,"subStyle":8300}|[14,4]"#,
                ),
                (
                    "B",
                    200,
                    2,
                    "MIDDLE",
                    true,
                    Some((3, 3, 3)),
                    150,
                    "[0,0,0,0,0,0]",
                    r#"{"keystone":8010,"subStyle":8400}|[4,14]"#,
                ),
            ],
            &[(100, 1, 11)],
        ),
        (
            "KR_3",
            "14.18",
            420,
            200,
            1,
            &[
                (
                    "A",
                    100,
                    1,
                    "MIDDLE",
                    true,
                    None,
                    10,
                    "[0,0,0,0,0,0]",
                    r#"{"keystone":8112,"subStyle":8300}|[4,14]"#,
                ),
                (
                    "C",
                    200,
                    2,
                    "MIDDLE",
                    false,
                    None,
                    5,
                    "[0,0,0,0,0,0]",
                    r#"{}|[4,14]"#,
                ),
            ],
            &[],
        ),
        (
            "KR_4",
            "14.17",
            420,
            1500,
            0,
            &[(
                "A",
                100,
                1,
                "MIDDLE",
                true,
                Some((9, 9, 9)),
                1,
                "[0,0,0,0,0,0]",
                r#"{}|[4,14]"#,
            )],
            &[],
        ),
        (
            "KR_5",
            "14.18",
            440,
            1500,
            0,
            &[(
                "A",
                100,
                1,
                "MIDDLE",
                true,
                Some((9, 9, 9)),
                1,
                "[0,0,0,0,0,0]",
                r#"{}|[4,14]"#,
            )],
            &[],
        ),
    ];
    for (id, patch, queue, duration, remake, players, bans) in matches {
        c.execute(
            "INSERT INTO matches (match_id, region, patch, queue_id, game_end_ms, body_zstd, body_size, archived_at,
               game_duration, remake, facts_version) VALUES (?1, 'asia', ?2, ?3, 1, x'00', 1, 1, ?4, ?5, 3)",
            params![id, patch, queue, duration, remake],
        )
        .unwrap();
        for (puuid, team, champ, pos, win, kda, cs, items, build) in players {
            let (runes, spells) = build.split_once('|').unwrap();
            // Gold, damage and vision are fixed multiples of CS, so the sums are easy to check.
            c.execute(
                "INSERT INTO match_facts (match_id, key_scope, puuid, team_id, position, champion_id, win,
                   kills, deaths, assists, cs, gold, damage, vision, items, runes, summoners, facts_version)
                 VALUES (?1, 's', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10 * 50, ?10 * 100, ?10 / 10, ?11, ?12, ?13, 3)",
                params![id, puuid, team, pos, champ, win, kda.map(|k| k.0), kda.map(|k| k.1), kda.map(|k| k.2), cs, items, runes, spells],
            )
            .unwrap();
        }
        for (team, turn, champ) in bans {
            c.execute(
                "INSERT INTO match_bans (match_id, team_id, pick_turn, champion_id) VALUES (?1, ?2, ?3, ?4)",
                params![id, team, turn, champ],
            )
            .unwrap();
        }
    }
    c.execute_batch(
        "INSERT INTO match_builds (match_id, key_scope, puuid, starter, boots, items, skills, skill_order,
           builds_version) VALUES
           ('KR_1', 's', 'A', '[1056,2003]', 3020, '[6655,3157,3089,3135]', 'QEWQQRQEQ', 'QEW', 1),
           ('KR_1', 's', 'C', '[1056,2003]', 3020, '[3157]', 'QWEQQRQ', 'QWE', 1),
           ('KR_1', 'other', 'C', '[1056]', NULL, '[3157,3089]', 'Q', 'QWE', 1),
           ('KR_2', 's', 'A', '[1056,2003]', 3047, '[6655,3157]', 'QWEQQRQ', 'QWE', 1),
           ('KR_2', 's', 'B', '[]', NULL, '[3157,4645,3089,3135,3003,3916]', 'QWEQQRQ', 'QWE', 1),
           ('KR_3', 's', 'A', '[1056]', NULL, '[6655,3157]', '', NULL, 1),
           ('KR_4', 's', 'A', '[]', NULL, '[3157,6655]', '', NULL, 1),
           ('KR_5', 's', 'A', '[1056,2003]', 3020, '[6655,3157]', 'QWE', 'QWE', 1);",
    )
    .unwrap();
}

fn scope(c: &Connection, patch_limit: u32) -> Scope {
    Scope {
        key_scope: "s".into(),
        platform: "kr".into(),
        queue: "RANKED_SOLO_5x5".into(),
        queue_id: 420,
        patches: recent_patches(c, 420, patch_limit).unwrap(),
        now: 77,
    }
}

async fn rebuild(db: &Db, patch_limit: u32) -> Written {
    db.write(move |c| {
        let s = scope(c, patch_limit);
        let mut w = rebuild_champions(c, &s)?;
        w.extend(rebuild_matchups(c, &s)?);
        w.extend(rebuild_builds(c, &s)?);
        Ok::<_, DbError>(w)
    })
    .await
    .unwrap()
}

async fn rows(db: &Db, sql: &'static str) -> Vec<Vec<i64>> {
    db.read(move |c| {
        let mut stmt = c.prepare(sql)?;
        let n = stmt.column_count();
        let out = stmt
            .query_map([], |r| {
                (0..n).map(|i| r.get::<_, i64>(i)).collect::<Result<Vec<_>, _>>()
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok::<_, DbError>(out)
    })
    .await
    .unwrap()
}

async fn text_rows(db: &Db, sql: &'static str) -> Vec<String> {
    db.read(move |c| {
        let mut stmt = c.prepare(sql)?;
        let out = stmt
            .query_map([], |r| r.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        Ok::<_, DbError>(out)
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn the_rebuild_matches_the_hand_computed_fixture() {
    let (_d, db) = db();
    db.write(|c| {
        seed(c);
        Ok::<_, DbError>(())
    })
    .await
    .unwrap();
    // Newest patch only: 14.18 (14.17's KR_4 is left out).
    let written = rebuild(&db, 1).await;
    assert_eq!(
        written,
        [
            ("champion_stats", 6),
            ("champion_matchups", 4),
            ("champion_items", 3),
            ("champion_runes", 3),
            ("champion_spells", 5),
            ("champion_builds", 3),
            ("champion_build_parts", 18),
        ]
    );

    // Slices: MASTER has A in KR_1/KR_2 and the remake KR_3; DIAMOND has C in
    // KR_1 and KR_3; UNKNOWN has D in KR_1.
    assert_eq!(
        text_rows(
            &db,
            "SELECT tier || ' ' || remake || ' ' || matches FROM analytics_slices ORDER BY tier, remake"
        )
        .await,
        [
            "DIAMOND 0 1",
            "DIAMOND 1 1",
            "MASTER 0 2",
            "MASTER 1 1",
            "UNKNOWN 0 1"
        ]
    );

    // Champion 1 at MASTER (A): KR_1 win + KR_2 loss; the remake apart.
    // kills 5+1, deaths 2+1, assists 3+1, cs 200+100, gold ×50, damage ×100,
    // vision cs/10 (20+10), duration of the stated games 1800+1200.
    assert_eq!(
        rows(
            &db,
            "SELECT remake, games, wins, matches_picked, stated_games, kills, deaths, assists, cs, gold,
                          damage, vision, duration_s, computed_at
                     FROM champion_stats WHERE tier = 'MASTER' AND champion_id = 1 ORDER BY remake"
        )
        .await,
        [
            vec![0, 2, 1, 2, 2, 6, 3, 4, 300, 15_000, 30_000, 30, 3000, 77],
            // The remake: no K/D/A, so no stated game and no duration.
            vec![1, 1, 1, 1, 0, 0, 0, 0, 10, 500, 1000, 1, 0, 77],
        ]
    );
    // B (MASTER) and C (DIAMOND) on champion 2; D, not on the ladder, under UNKNOWN.
    assert_eq!(
        text_rows(
            &db,
            "SELECT tier || ' ' || champion_id || ' ' || role || ' ' || remake || ' ' || games || '/' || wins
                          FROM champion_stats ORDER BY tier, champion_id, remake"
        )
        .await,
        [
            "DIAMOND 2 MIDDLE 0 1/0",
            "DIAMOND 2 MIDDLE 1 1/0",
            "MASTER 1 MIDDLE 0 2/1",
            "MASTER 1 MIDDLE 1 1/1",
            "MASTER 2 MIDDLE 0 1/1",
            "UNKNOWN 3 TOP 0 1/0",
        ]
    );

    // Bans: 10 in KR_1 once although both teams banned it; 11 in KR_1 and KR_2.
    // KR_1's bans count under UNKNOWN too, D's tier.
    assert_eq!(
        text_rows(
            &db,
            "SELECT tier || ' ' || champion_id || ' ' || bans FROM champion_bans ORDER BY tier, champion_id"
        )
        .await,
        [
            "DIAMOND 10 1",
            "DIAMOND 11 1",
            "MASTER 10 1",
            "MASTER 11 2",
            "UNKNOWN 10 1",
            "UNKNOWN 11 1"
        ]
    );

    // Matchups from both sides: 1 v 2 is A's (KR_1 win, KR_2 loss); 2 v 1 is
    // C's KR_1 loss and B's KR_2 win. D's lane has no opponent. The remake
    // apart.
    assert_eq!(
        text_rows(&db, "SELECT champion_id || ' v ' || opponent_id || ' ' || role || ' ' || remake || ' ' || games || '/' || wins
                          FROM champion_matchups ORDER BY champion_id, remake").await,
        ["1 v 2 MIDDLE 0 2/1", "1 v 2 MIDDLE 1 1/1", "2 v 1 MIDDLE 0 2/1", "2 v 1 MIDDLE 1 1/0"]
    );

    // Items: A's 3157 in KR_1 (win) and KR_2; 3020 in KR_1; C's two 3157s count once.
    assert_eq!(
        text_rows(
            &db,
            "SELECT champion_id || ' ' || item_id || ' ' || games || '/' || wins FROM champion_items
                         ORDER BY champion_id, item_id"
        )
        .await,
        ["1 3020 1/1", "1 3157 2/1", "2 3157 1/0"]
    );
    // Runes: an empty page is left out.
    assert_eq!(
        text_rows(&db, "SELECT champion_id || ' ' || keystone_id || '/' || sub_style_id || ' ' || remake || ' ' || games || '/' || wins
                          FROM champion_runes ORDER BY champion_id, remake").await,
        ["1 8112/8300 0 2/1", "1 8112/8300 1 1/1", "2 8010/8400 0 2/1"]
    );
    // Spells: [4,14] and [14,4] are one pair; D's [4,12] counts too.
    assert_eq!(
        text_rows(
            &db,
            "SELECT champion_id || ' ' || spell_a || '+' || spell_b || ' ' || remake || ' ' || games
                          FROM champion_spells ORDER BY champion_id, remake"
        )
        .await,
        [
            "1 4+14 0 2",
            "1 4+14 1 1",
            "2 4+14 0 2",
            "2 4+14 1 1",
            "3 4+12 0 1"
        ]
    );
}

#[tokio::test]
async fn reads_leave_remakes_out_unless_asked_and_sum_roles() {
    let (_d, db) = db();
    db.write(|c| {
        seed(c);
        Ok::<_, DbError>(())
    })
    .await
    .unwrap();
    rebuild(&db, 0).await;
    let read = |remakes: bool| Read {
        key_scope: "s".into(),
        platform: Some("kr".into()),
        queue: "RANKED_SOLO_5x5".into(),
        patch: Some("14.18".into()),
        tier: None,
        role: None,
        champion_id: None,
        min_games: 0,
        limit: 500,
        remakes,
    };
    assert_eq!(
        latest_patch(&db, "s", Some("kr"), "RANKED_SOLO_5x5")
            .await
            .unwrap()
            .as_deref(),
        Some("14.18")
    );
    let games = |rows: Vec<StatRow>| {
        rows.iter()
            .map(|r| (r.champion_id, r.tier.clone(), r.games))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        games(stats(&db, read(false)).await.unwrap()),
        [
            (1, "MASTER".into(), 2),
            (2, "DIAMOND".into(), 1),
            (2, "MASTER".into(), 1),
            (3, "UNKNOWN".into(), 1)
        ]
    );
    assert_eq!(
        games(stats(&db, read(true)).await.unwrap()),
        [
            (1, "MASTER".into(), 3),
            (2, "DIAMOND".into(), 2),
            (2, "MASTER".into(), 1),
            (3, "UNKNOWN".into(), 1)
        ]
    );
    let mut sl = slices(&db, read(true)).await.unwrap();
    sl.sort();
    assert_eq!(
        sl,
        [("DIAMOND".into(), 2), ("MASTER".into(), 3), ("UNKNOWN".into(), 1)]
    );
    // minGames and tier.
    let r = Read {
        tier: Some("MASTER".into()),
        min_games: 2,
        ..read(false)
    };
    assert_eq!(games(stats(&db, r).await.unwrap()), [(1, "MASTER".into(), 2)]);

    let facet_of = |f: Facet, remakes: bool| {
        let db = db.clone();
        async move {
            facet(
                &db,
                f,
                Read {
                    champion_id: Some(1),
                    ..read(remakes)
                },
            )
            .await
            .unwrap()
            .into_iter()
            .map(|r| (r.role, r.ids, r.games, r.wins))
            .collect::<Vec<_>>()
        }
    };
    assert_eq!(
        facet_of(Facet::Matchups, false).await,
        [("MIDDLE".into(), vec![2], 2, 1)]
    );
    assert_eq!(
        facet_of(Facet::Matchups, true).await,
        [("MIDDLE".into(), vec![2], 3, 2)]
    );
    assert_eq!(
        facet_of(Facet::Items, false).await,
        [
            (String::new(), vec![3157], 2, 1),
            (String::new(), vec![3020], 1, 1)
        ]
    );
    assert_eq!(
        facet_of(Facet::Runes, true).await,
        [(String::new(), vec![8112, 8300], 3, 2)]
    );
    assert_eq!(
        facet_of(Facet::Spells, false).await,
        [(String::new(), vec![4, 14], 2, 1)]
    );
}

#[tokio::test]
async fn a_patch_limit_keeps_older_patches_rows_and_patches_sort_numerically() {
    let (_d, db) = db();
    db.write(|c| {
        seed(c);
        c.execute_batch("UPDATE matches SET patch = '14.9' WHERE match_id = 'KR_4'")?;
        Ok::<_, DbError>(())
    })
    .await
    .unwrap();
    // Every patch: 14.9's KR_4 counts too.
    rebuild(&db, 0).await;
    assert_eq!(
        text_rows(&db, "SELECT DISTINCT patch FROM champion_stats ORDER BY patch").await,
        ["14.18", "14.9"]
    );
    // The newest one patch is 14.18 (numerically above 14.9). Rebuilding it
    // alone leaves 14.9's rows as they were.
    let patches = db.read(|c| recent_patches(c, 420, 1)).await.unwrap();
    assert_eq!(patches, Some(vec!["14.18".to_string()]));
    db.write(|c| {
        c.execute("DELETE FROM matches WHERE match_id = 'KR_1'", [])?;
        Ok::<_, DbError>(())
    })
    .await
    .unwrap();
    rebuild(&db, 1).await;
    assert_eq!(
        rows(&db, "SELECT games FROM champion_stats WHERE patch = '14.9'").await,
        [vec![1]],
        "an older patch is kept"
    );
    assert_eq!(
        rows(
            &db,
            "SELECT sum(games) FROM champion_stats WHERE patch = '14.18' AND remake = 0"
        )
        .await,
        [vec![2]],
        "the rebuilt patch is replaced, not added to"
    );
}

#[tokio::test]
async fn every_patch_sums_and_the_patch_list_is_newest_first() {
    let (_d, db) = db();
    db.write(|c| {
        seed(c);
        c.execute_batch("UPDATE matches SET patch = '14.9' WHERE match_id = 'KR_4'")?;
        Ok::<_, DbError>(())
    })
    .await
    .unwrap();
    rebuild(&db, 0).await;
    let read = |patch: Option<&str>| Read {
        key_scope: "s".into(),
        platform: Some("kr".into()),
        queue: "RANKED_SOLO_5x5".into(),
        patch: patch.map(str::to_string),
        tier: None,
        role: None,
        champion_id: Some(1),
        min_games: 0,
        limit: 500,
        remakes: false,
    };
    let games = |rows: Vec<StatRow>| {
        rows.iter()
            .map(|r| (r.tier.clone(), r.patch.clone(), r.games))
            .collect::<Vec<_>>()
    };
    // M1 and M2 on 14.18, M4 on 14.9: one row, labelled "all".
    assert_eq!(
        games(stats(&db, read(None)).await.unwrap()),
        [("MASTER".into(), "all".into(), 3)]
    );
    assert_eq!(
        games(stats(&db, read(Some("14.18"))).await.unwrap()),
        [("MASTER".into(), "14.18".into(), 2)]
    );
    let slices_all = slices(&db, read(None)).await.unwrap();
    assert!(slices_all.contains(&("MASTER".into(), 3)), "{slices_all:?}");

    let list = |remakes| {
        let db = db.clone();
        async move {
            patches(&db, "s", Some("kr"), "RANKED_SOLO_5x5", None, remakes)
                .await
                .unwrap()
                .into_iter()
                .map(|p| (p.patch, p.games))
                .collect::<Vec<_>>()
        }
    };
    // 14.18 above 14.9 (numeric); its games are A, B, C and D in M1 and M2.
    assert_eq!(list(false).await, [("14.18".into(), 5), ("14.9".into(), 1)]);
    // M3, the remake, adds A and C.
    assert_eq!(list(true).await, [("14.18".into(), 7), ("14.9".into(), 1)]);
    assert!(
        patches(&db, "s", Some("euw1"), "RANKED_SOLO_5x5", None, false)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_champions_patch_list_has_only_its_games() {
    let (_d, db) = db();
    db.write(|c| {
        seed(c);
        c.execute_batch("UPDATE matches SET patch = '14.9' WHERE match_id = 'KR_4'")?;
        Ok::<_, DbError>(())
    })
    .await
    .unwrap();
    rebuild(&db, 0).await;
    let list = |champion| {
        let db = db.clone();
        async move {
            patches(&db, "s", Some("kr"), "RANKED_SOLO_5x5", champion, false)
                .await
                .unwrap()
                .into_iter()
                .map(|p| (p.patch, p.games))
                .collect::<Vec<_>>()
        }
    };
    // Champion 1: twice on 14.18 (M1, M2), once on 14.9 (M4); every champion has 5 and 1.
    assert_eq!(list(Some(1)).await, [("14.18".into(), 2), ("14.9".into(), 1)]);
    assert_eq!(list(None).await, [("14.18".into(), 5), ("14.9".into(), 1)]);
    // A champion nobody played has no patches.
    assert!(list(Some(999)).await.is_empty());
}

#[tokio::test]
async fn a_lane_two_players_of_a_team_share_has_no_matchup() {
    let (_d, db) = db();
    db.write(|c| {
        seed(c);
        // KR_6: A and B both MIDDLE for team 100 against C; A's TOP game
        // against D is a lane like any other.
        c.execute_batch(
            "INSERT INTO matches (match_id, region, patch, queue_id, game_end_ms, body_zstd, body_size,
               archived_at, game_duration, remake, facts_version)
               VALUES ('KR_6', 'asia', '14.18', 420, 1, x'00', 1, 1, 1500, 0, 3);
             INSERT INTO match_facts (match_id, key_scope, puuid, team_id, position, champion_id, win, facts_version)
               VALUES ('KR_6', 's', 'A', 100, 'MIDDLE', 1, 1, 3), ('KR_6', 's', 'B', 100, 'MIDDLE', 4, 1, 3),
                      ('KR_6', 's', 'C', 200, 'MIDDLE', 2, 0, 3), ('KR_6', 's', 'E', 100, 'TOP', 5, 1, 3),
                      ('KR_6', 's', 'D', 200, 'TOP', 6, 0, 3);
             INSERT INTO ladder_entries (key_scope, platform, queue, puuid, tier, division, league_points,
               wins, losses, first_seen_crawl_id, last_seen_crawl_id, updated_at)
               VALUES ('s', 'kr', 'RANKED_SOLO_5x5', 'E', 'MASTER', 'I', 10, 1, 1, 'c', 'c', 1);",
        )?;
        Ok::<_, DbError>(())
    })
    .await
    .unwrap();
    rebuild(&db, 1).await;
    // The fixture's four rows, untouched by KR_6's shared MIDDLE (C's side
    // too: its opponent lane has two players), plus E's TOP game against D
    // and D's against E.
    assert_eq!(
        text_rows(&db, "SELECT champion_id || ' v ' || opponent_id || ' ' || role || ' ' || remake || ' ' || games || '/' || wins
                          FROM champion_matchups ORDER BY champion_id, remake").await,
        [
            "1 v 2 MIDDLE 0 2/1",
            "1 v 2 MIDDLE 1 1/1",
            "2 v 1 MIDDLE 0 2/1",
            "2 v 1 MIDDLE 1 1/0",
            "5 v 6 TOP 0 1/1",
            "6 v 5 TOP 0 1/0"
        ]
    );
}

/// The database has no `ANALYZE` statistics, so the plan is SQLite's guess and
/// the same for any table size (ADR-101). It must reach a lane's opponent by
/// the match: before, it walked every ladder player for each fact, which took
/// 44 s on a 16,600-game ladder.
#[tokio::test]
async fn the_matchups_plan_reaches_the_opponent_by_key() {
    let plan = plan_of(MATCHUPS.to_string()).await;
    assert!(step(&plan, "b").contains("match_id=?"), "{plan:#?}");
}

/// The facts reach each player's ladder entry and lookup by key, not by
/// walking either table per fact (ADR-105).
#[tokio::test]
async fn the_facts_plan_reaches_ladder_and_lookup_by_key() {
    let plan = plan_of(format!("SELECT {TIER} {}", ladder_facts(""))).await;
    assert!(step(&plan, "le").contains("puuid=?"), "{plan:#?}");
    assert!(step(&plan, "pr").contains("puuid=?"), "{plan:#?}");
}

/// `EXPLAIN QUERY PLAN` of `sql` over the fixture, bound as a rebuild binds it.
async fn plan_of(sql: String) -> Vec<String> {
    let (_d, db) = db();
    db.write(move |c| {
        seed(c);
        let s = scope(c, 0);
        let mut stmt = c.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?;
        let n = stmt.parameter_count();
        let binds: Vec<rusqlite::types::Value> = [
            s.key_scope.clone().into(),
            s.platform.clone().into(),
            s.queue.clone().into(),
            s.queue_id.into(),
            s.patches_json().map_or(rusqlite::types::Value::Null, Into::into),
            s.now.into(),
        ]
        .into_iter()
        .take(n)
        .collect();
        let plan = stmt
            .query_map(rusqlite::params_from_iter(binds), |r| r.get::<_, String>(3))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok::<_, DbError>(plan)
    })
    .await
    .unwrap()
}

fn step<'a>(plan: &'a [String], alias: &str) -> &'a str {
    plan.iter()
        .find(|l| l.starts_with(&format!("SEARCH {alias} ")) || l.starts_with(&format!("SCAN {alias}")))
        .unwrap_or_else(|| panic!("no step for {alias} in {plan:#?}"))
}

/// A participant the ladder does not hold counts at the tier their last
/// league lookup returned; a lookup newer than the ladder entry wins, an older
/// one doesn't; another queue's lookup doesn't apply; another platform's
/// matches don't count (ADR-105).
#[tokio::test]
async fn lookups_place_players_and_only_the_platforms_matches_count() {
    let (_d, db) = db();
    db.write(|c| {
        seed(c);
        c.execute_batch(
            "INSERT INTO player_ranks (key_scope, platform, queue, puuid, tier, division, league_points, fetched_at)
               VALUES ('s', 'kr', 'RANKED_SOLO_5x5', 'C', 'EMERALD', 'I', 10, 5),
                      ('s', 'kr', 'RANKED_SOLO_5x5', 'B', 'GOLD', 'I', 0, 0),
                      ('s', 'kr', 'RANKED_FLEX_SR', 'D', 'IRON', 'IV', 0, 5),
                      ('s', 'euw1', 'RANKED_SOLO_5x5', 'D', 'IRON', 'IV', 0, 5);
             INSERT INTO matches (match_id, region, patch, queue_id, game_end_ms, body_zstd, body_size,
               archived_at, game_duration, remake, facts_version)
               VALUES ('EUW1_1', 'europe', '14.18', 420, 1, x'00', 1, 1, 1500, 0, 3);
             INSERT INTO match_facts (match_id, key_scope, puuid, team_id, position, champion_id, win, facts_version)
               VALUES ('EUW1_1', 's', 'A', 100, 'MIDDLE', 1, 1, 3);",
        )?;
        Ok::<_, DbError>(())
    })
    .await
    .unwrap();
    rebuild(&db, 1).await;
    // C's lookup (5) is newer than C's ladder entry (1): EMERALD. B's (0) is
    // older: still MASTER. D's flex and euw1 ranks don't apply: UNKNOWN. A's
    // EUW1_1 game is not kr's.
    assert_eq!(
        text_rows(
            &db,
            "SELECT tier || ' ' || champion_id || ' ' || role || ' ' || remake || ' ' || games || '/' || wins
                          FROM champion_stats ORDER BY tier, champion_id, remake"
        )
        .await,
        [
            "EMERALD 2 MIDDLE 0 1/0",
            "EMERALD 2 MIDDLE 1 1/0",
            "MASTER 1 MIDDLE 0 2/1",
            "MASTER 1 MIDDLE 1 1/1",
            "MASTER 2 MIDDLE 0 1/1",
            "UNKNOWN 3 TOP 0 1/0",
        ]
    );
}

/// Set builds (BLD-02) on the newest patch, worked out from the build facts
/// in the module comment.
#[tokio::test]
async fn set_builds_match_the_hand_computed_fixture() {
    let (_d, db) = db();
    db.write(|c| {
        seed(c);
        Ok::<_, DbError>(())
    })
    .await
    .unwrap();
    rebuild(&db, 1).await;
    // A's 6655 › 3157 in KR_1 (win) and KR_2, the remake KR_3 apart; B's
    // 3157 › 4645. C's single item counts nowhere, and C's row under another
    // key scope doesn't join. KR_4 (14.17) and KR_5 (flex) are out of scope.
    assert_eq!(
        text_rows(
            &db,
            "SELECT champion_id || ' ' || role || ' ' || remake || ' ' || core || ' ' || games || '/' || wins
                     || ' ' || computed_at
               FROM champion_builds ORDER BY champion_id, remake"
        )
        .await,
        [
            "1 MIDDLE 0 [6655,3157] 2/1 77",
            "1 MIDDLE 1 [6655,3157] 1/1 77",
            "2 MIDDLE 0 [3157,4645] 1/1 77",
        ]
    );
    // Only KR_1's A has a 3rd and 4th item; boots and skill order split A's
    // two games; KR_1's [4,14] and KR_2's [14,4] are one spell pair. The
    // remake has no boots or skill order. B's empty starter and missing boots
    // count nowhere, nor does its 6th item.
    assert_eq!(
        text_rows(
            &db,
            "SELECT champion_id || ' ' || remake || ' ' || core || ' ' || part || ' ' || value || ' '
                     || games || '/' || wins
               FROM champion_build_parts ORDER BY champion_id, remake, part, value"
        )
        .await,
        [
            "1 0 [6655,3157] boots 3020 1/1",
            "1 0 [6655,3157] boots 3047 1/0",
            "1 0 [6655,3157] item3 3089 1/1",
            "1 0 [6655,3157] item4 3135 1/1",
            "1 0 [6655,3157] runes 8112:8300 2/1",
            "1 0 [6655,3157] skill_order QEW 1/1",
            "1 0 [6655,3157] skill_order QWE 1/0",
            "1 0 [6655,3157] spells 4:14 2/1",
            "1 0 [6655,3157] starter [1056,2003] 2/1",
            "1 1 [6655,3157] runes 8112:8300 1/1",
            "1 1 [6655,3157] spells 4:14 1/1",
            "1 1 [6655,3157] starter [1056] 1/1",
            "2 0 [3157,4645] item3 3089 1/1",
            "2 0 [3157,4645] item4 3135 1/1",
            "2 0 [3157,4645] item5 3003 1/1",
            "2 0 [3157,4645] runes 8010:8400 1/1",
            "2 0 [3157,4645] skill_order QWE 1/1",
            "2 0 [3157,4645] spells 4:14 1/1",
        ]
    );
    // A rebuild replaces the rows rather than adding to them.
    rebuild(&db, 1).await;
    assert_eq!(
        rows(&db, "SELECT sum(games) FROM champion_builds").await,
        [vec![4]]
    );
}

#[tokio::test]
async fn build_reads_sum_patches_and_roles_and_leave_remakes_out() {
    let (_d, db) = db();
    db.write(|c| {
        seed(c);
        // A plays KR_2 in TOP, so A's 6655 › 3157 has a row per role.
        c.execute_batch("UPDATE match_facts SET position = 'TOP' WHERE match_id = 'KR_2' AND puuid = 'A'")?;
        Ok::<_, DbError>(())
    })
    .await
    .unwrap();
    rebuild(&db, 0).await;
    let read = |patch: Option<&str>, role: Option<&str>, remakes: bool| Read {
        key_scope: "s".into(),
        platform: Some("kr".into()),
        queue: "RANKED_SOLO_5x5".into(),
        patch: patch.map(str::to_string),
        tier: None,
        role: role.map(str::to_string),
        champion_id: Some(1),
        min_games: 0,
        limit: 10,
        remakes,
    };
    let list = |r: Read| {
        let db = db.clone();
        async move {
            builds(&db, r)
                .await
                .unwrap()
                .into_iter()
                .map(|b| (b.core, b.games, b.wins))
                .collect::<Vec<_>>()
        }
    };
    // Every patch: KR_4's 3157 › 6655 (14.17) is a build of its own; roles summed.
    assert_eq!(
        list(read(None, None, false)).await,
        [([6655, 3157], 2, 1), ([3157, 6655], 1, 1)]
    );
    assert_eq!(
        list(read(Some("14.18"), None, false)).await,
        [([6655, 3157], 2, 1)]
    );
    assert_eq!(
        list(read(Some("14.18"), Some("TOP"), false)).await,
        [([6655, 3157], 1, 0)]
    );
    // The remake KR_3 added back.
    assert_eq!(
        list(read(Some("14.18"), None, true)).await,
        [([6655, 3157], 3, 2)]
    );
    // minGames and limit.
    assert_eq!(
        list(Read {
            min_games: 2,
            ..read(None, None, false)
        })
        .await,
        [([6655, 3157], 2, 1)]
    );
    assert_eq!(
        list(Read {
            limit: 1,
            ..read(None, None, false)
        })
        .await,
        [([6655, 3157], 2, 1)]
    );
    assert!(
        list(Read {
            champion_id: Some(999),
            ..read(None, None, false)
        })
        .await
        .is_empty()
    );

    let parts = |r: Read, cores: Vec<[i64; 2]>| {
        let db = db.clone();
        async move {
            build_parts(&db, r, cores)
                .await
                .unwrap()
                .into_iter()
                .map(|p| format!("{:?} {} {} {}/{}", p.core, p.part, p.value, p.games, p.wins))
                .collect::<Vec<_>>()
        }
    };
    // Both roles' rows summed; most played first within a part.
    assert_eq!(
        parts(read(None, None, false), vec![[6655, 3157]]).await,
        [
            "[6655, 3157] boots 3020 1/1",
            "[6655, 3157] boots 3047 1/0",
            "[6655, 3157] item3 3089 1/1",
            "[6655, 3157] item4 3135 1/1",
            "[6655, 3157] runes 8112:8300 2/1",
            "[6655, 3157] skill_order QEW 1/1",
            "[6655, 3157] skill_order QWE 1/0",
            "[6655, 3157] spells 4:14 2/1",
            "[6655, 3157] starter [1056,2003] 2/1",
        ]
    );
    // With remakes, KR_3's starter comes second; two cores at once.
    let both = parts(read(None, None, true), vec![[6655, 3157], [3157, 6655]]).await;
    assert!(
        both.contains(&"[3157, 6655] spells 4:14 1/1".to_string()),
        "{both:#?}"
    );
    let starters: Vec<_> = both
        .iter()
        .filter(|p| p.starts_with("[6655, 3157] starter"))
        .collect();
    assert_eq!(
        starters,
        [
            "[6655, 3157] starter [1056,2003] 2/1",
            "[6655, 3157] starter [1056] 1/1"
        ]
    );
    // One role; a core nobody built has no parts.
    assert_eq!(
        parts(read(None, Some("TOP"), false), vec![[6655, 3157]]).await,
        [
            "[6655, 3157] boots 3047 1/0",
            "[6655, 3157] runes 8112:8300 1/0",
            "[6655, 3157] skill_order QWE 1/0",
            "[6655, 3157] spells 4:14 1/0",
            "[6655, 3157] starter [1056,2003] 1/0",
        ]
    );
    assert!(parts(read(None, None, false), vec![[1, 2]]).await.is_empty());
}

/// The set-build players reach their build row by key, not by walking
/// `match_builds` per fact.
#[tokio::test]
async fn the_set_builds_plan_reaches_the_build_by_key() {
    let plan = plan_of(format!("{} SELECT * FROM p", set_build_players())).await;
    assert!(step(&plan, "b").contains("match_id=?"), "{plan:#?}");
}
