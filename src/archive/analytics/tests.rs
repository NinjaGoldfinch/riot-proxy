#![allow(clippy::unwrap_used, clippy::expect_used)]
//! A hand-computed fixture (plan P7-04 acceptance): five matches, worked out
//! on paper below, rebuilt and read back.
//!
//! Ladder: A and B are MASTER, C is DIAMOND; D is not on the ladder.
//! - M1 14.18 solo, 1800 s: A (champ 1, MIDDLE, win) vs C (champ 2, MIDDLE);
//!   D (champ 3, TOP, team 100) has no lane opponent. Bans: 10 by both teams,
//!   11 by team 200.
//! - M2 14.18 solo, 1200 s: A (champ 1, MIDDLE, loss) vs B (champ 2, MIDDLE, win). Ban: 11.
//! - M3 14.18 solo, 200 s, a remake: A (champ 1, MIDDLE, win, no K/D/A) vs C (champ 2).
//! - M4 14.17 solo: A on champ 1, an older patch.
//! - M5 14.18 flex: A on champ 1, another queue.

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
            ("champion_stats", 5),
            ("champion_matchups", 4),
            ("champion_items", 3),
            ("champion_runes", 3),
            ("champion_spells", 4),
        ]
    );

    // Slices: MASTER has A in KR_1/KR_2 and the remake KR_3; DIAMOND has C in KR_1 and KR_3.
    assert_eq!(
        text_rows(
            &db,
            "SELECT tier || ' ' || remake || ' ' || matches FROM analytics_slices ORDER BY tier, remake"
        )
        .await,
        ["DIAMOND 0 1", "DIAMOND 1 1", "MASTER 0 2", "MASTER 1 1"]
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
    // B (MASTER) and C (DIAMOND) on champion 2; D is not on the ladder.
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
        ]
    );

    // Bans: 10 in KR_1 once although both teams banned it; 11 in KR_1 and KR_2.
    assert_eq!(
        text_rows(
            &db,
            "SELECT tier || ' ' || champion_id || ' ' || bans FROM champion_bans ORDER BY tier, champion_id"
        )
        .await,
        ["DIAMOND 10 1", "DIAMOND 11 1", "MASTER 10 1", "MASTER 11 2"]
    );

    // Matchups from the ladder player's side: 1 v 2 is A's (KR_1 win, KR_2
    // loss); 2 v 1 is C's KR_1 loss and B's KR_2 win. D's lane has no
    // opponent. The remake apart.
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
    // Spells: [4,14] and [14,4] are one pair.
    assert_eq!(
        text_rows(
            &db,
            "SELECT champion_id || ' ' || spell_a || '+' || spell_b || ' ' || remake || ' ' || games
                          FROM champion_spells ORDER BY champion_id, remake"
        )
        .await,
        ["1 4+14 0 2", "1 4+14 1 1", "2 4+14 0 2", "2 4+14 1 1"]
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
        patch: "14.18".into(),
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
            (2, "MASTER".into(), 1)
        ]
    );
    assert_eq!(
        games(stats(&db, read(true)).await.unwrap()),
        [
            (1, "MASTER".into(), 3),
            (2, "DIAMOND".into(), 2),
            (2, "MASTER".into(), 1)
        ]
    );
    let mut sl = slices(&db, read(true)).await.unwrap();
    sl.sort();
    assert_eq!(sl, [("DIAMOND".into(), 2), ("MASTER".into(), 3)]);
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
