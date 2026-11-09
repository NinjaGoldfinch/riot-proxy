#![allow(clippy::unwrap_used, clippy::expect_used)]
//! THR-02's stamps, on the real ranked solo match the archive tests use.

use bytes::Bytes;

use super::*;
use crate::archive::{matches, ranks};
use crate::db::{Db, DbError};
use crate::jobs::ladder::store;

/// A real ranked solo match (tests/fixtures/replay, recorded in P3-06): kr,
/// queue 420, ten participants, ended at [`END`].
const MATCH: &[u8] = include_bytes!("../../../tests/fixtures/replay/cold-lookup/06-match.byId.body");
const MATCH_ID: &str = "KR_8393343196";
const END: i64 = 1_790_247_623_902;
const SCOPE: &str = "s";
const SOLO: &str = "RANKED_SOLO_5x5";

fn db() -> (tempfile::TempDir, Db) {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("t.db"), 1).unwrap();
    (dir, db)
}

/// The match's participants, in Riot's order.
fn puuids() -> Vec<String> {
    let body: serde_json::Value = serde_json::from_slice(MATCH).unwrap();
    body["metadata"]["participants"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap().to_string())
        .collect()
}

async fn on_ladder(db: &Db, platform: &str, queue: &str, puuid: &str, tier: &str, at: i64) {
    let (platform, queue, puuid, tier) = (
        platform.to_string(),
        queue.to_string(),
        puuid.to_string(),
        tier.to_string(),
    );
    db.write(move |c| {
        c.execute(
            "INSERT INTO ladder_entries (key_scope, platform, queue, puuid, tier, division, league_points, wins,
               losses, first_seen_crawl_id, last_seen_crawl_id, updated_at)
             VALUES ('s', ?1, ?2, ?3, ?4, 'I', 0, 1, 1, 'c', 'c', ?5)
             ON CONFLICT (key_scope, platform, queue, puuid) DO UPDATE SET tier = excluded.tier,
               updated_at = excluded.updated_at",
            rusqlite::params![platform, queue, puuid, tier, at],
        )?;
        Ok::<_, DbError>(())
    })
    .await
    .unwrap();
}

async fn archive(db: &Db, body: &'static [u8], now: i64) {
    matches::put(db, MATCH_ID, "asia", SCOPE, Bytes::from_static(body), now)
        .await
        .unwrap();
}

/// `puuid → tier` for the match's stamped facts (`tier IS NOT NULL`).
async fn stamps(db: &Db) -> std::collections::BTreeMap<String, String> {
    db.read(|c| {
        let mut stmt = c.prepare(
            "SELECT puuid, tier, key_scope FROM match_facts WHERE match_id = ?1 AND tier IS NOT NULL",
        )?;
        let rows = stmt
            .query_map([MATCH_ID], |r| {
                assert_eq!(r.get::<_, String>(2)?, SCOPE);
                Ok((r.get(0)?, r.get(1)?))
            })?
            .collect::<Result<_, _>>()?;
        Ok::<_, DbError>(rows)
    })
    .await
    .unwrap()
}

fn tier_of<'a>(s: &'a std::collections::BTreeMap<String, String>, puuid: &str) -> &'a str {
    &s[puuid]
}

#[tokio::test]
async fn archiving_stamps_every_participant_once() {
    let (_d, db) = db();
    let p = puuids();
    on_ladder(&db, "kr", SOLO, &p[0], "MASTER", 1).await;
    on_ladder(&db, "kr", SOLO, &p[1], "GRANDMASTER", 1).await;
    // A league lookup newer than the ladder entry wins (ADR-105).
    on_ladder(&db, "kr", SOLO, &p[2], "MASTER", 1).await;
    ranks::record(
        &db,
        SCOPE,
        "kr",
        &p[2],
        br#"[{"queueType":"RANKED_SOLO_5x5","tier":"DIAMOND","rank":"I","leaguePoints":1}]"#,
        5,
        i64::MAX,
    )
    .await
    .unwrap();
    // Flex and another platform don't place a solo game on kr.
    on_ladder(&db, "kr", "RANKED_FLEX_SR", &p[3], "GOLD", 1).await;
    on_ladder(&db, "euw1", SOLO, &p[4], "IRON", 1).await;
    archive(&db, MATCH, 100).await;

    let s = stamps(&db).await;
    assert_eq!(s.len(), 10);
    assert_eq!(tier_of(&s, &p[0]), "MASTER");
    assert_eq!(tier_of(&s, &p[1]), "GRANDMASTER");
    assert_eq!(tier_of(&s, &p[2]), "DIAMOND");
    for q in &p[3..] {
        assert_eq!(tier_of(&s, q), "UNKNOWN", "{q}");
    }

    // Promoted, then archived again: the first stamp stays.
    on_ladder(&db, "kr", SOLO, &p[0], "CHALLENGER", 200).await;
    archive(&db, MATCH, 300).await;
    let again = stamps(&db).await;
    assert_eq!(tier_of(&again, &p[0]), "MASTER");
    assert_eq!(again, s);
}

#[tokio::test]
async fn a_match_outside_solo_and_flex_is_not_stamped() {
    let (_d, db) = db();
    let aram = String::from_utf8(MATCH.to_vec())
        .unwrap()
        .replacen("\"queueId\":420", "\"queueId\":450", 1);
    let aram: &'static [u8] = Box::leak(aram.into_bytes().into_boxed_slice());
    archive(&db, aram, 100).await;
    assert!(stamps(&db).await.is_empty());
    assert!(
        db.read(|c| Ok::<_, DbError>(unstamped(c, "", 1)?))
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn reextracting_facts_leaves_the_stamps_alone() {
    let (_d, db) = db();
    let p = puuids();
    on_ladder(&db, "kr", SOLO, &p[0], "MASTER", 1).await;
    archive(&db, MATCH, 100).await;
    let before = stamps(&db).await;
    on_ladder(&db, "kr", SOLO, &p[0], "CHALLENGER", 200).await;
    // What `facts:reextract` does for each match.
    let derived = matches::derive(MATCH).unwrap();
    db.write(move |c| {
        let tx = c.transaction()?;
        matches::write_derived(&tx, MATCH_ID, SCOPE, &derived)?;
        tx.commit()?;
        Ok::<_, DbError>(())
    })
    .await
    .unwrap();
    assert_eq!(stamps(&db).await, before);
}

/// A rank seen later places a player's `UNKNOWN` stamps on matches inside
/// the window, never older ones, and never changes a known tier.
#[tokio::test]
async fn a_later_rank_places_recent_unknown_stamps_only() {
    let (_d, db) = db();
    let p = puuids();
    on_ladder(&db, "kr", SOLO, &p[0], "MASTER", 1).await;
    archive(&db, MATCH, 100).await;
    let solo = |tier: &str| {
        format!(r#"[{{"queueType":"RANKED_SOLO_5x5","tier":"{tier}","rank":"I","leaguePoints":1}}]"#)
    };

    // The match ended before the window: p[1] stays UNKNOWN.
    ranks::record(&db, SCOPE, "kr", &p[1], solo("EMERALD").as_bytes(), 500, END + 1)
        .await
        .unwrap();
    assert_eq!(tier_of(&stamps(&db).await, &p[1]), "UNKNOWN");
    // Inside it: placed and restamped.
    ranks::record(&db, SCOPE, "kr", &p[1], solo("EMERALD").as_bytes(), 600, END)
        .await
        .unwrap();
    let s = stamps(&db).await;
    assert_eq!(tier_of(&s, &p[1]), "EMERALD");
    // A known tier, whether stamped on archive or placed late, stays.
    for q in [&p[0], &p[1]] {
        ranks::record(&db, SCOPE, "kr", q, solo("IRON").as_bytes(), 700, 0)
            .await
            .unwrap();
    }
    let after = stamps(&db).await;
    assert_eq!(tier_of(&after, &p[0]), "MASTER");
    assert_eq!(tier_of(&after, &p[1]), "EMERALD");
    // Another queue's or platform's rank doesn't apply.
    ranks::record(
        &db,
        SCOPE,
        "kr",
        &p[2],
        br#"[{"queueType":"RANKED_FLEX_SR","tier":"GOLD","rank":"I","leaguePoints":1}]"#,
        800,
        0,
    )
    .await
    .unwrap();
    ranks::record(&db, SCOPE, "euw1", &p[3], solo("GOLD").as_bytes(), 800, 0)
        .await
        .unwrap();
    let after = stamps(&db).await;
    assert_eq!(tier_of(&after, &p[2]), "UNKNOWN");
    assert_eq!(tier_of(&after, &p[3]), "UNKNOWN");
}

/// A ladder page places its players' recent `UNKNOWN` stamps too.
#[tokio::test]
async fn a_ladder_page_places_recent_unknown_stamps() {
    let (_d, db) = db();
    let p = puuids();
    archive(&db, MATCH, 100).await;
    let entry = |puuid: &str, tier: &str| store::Entry {
        puuid: puuid.to_string(),
        tier: tier.to_string(),
        division: "I".into(),
        league_points: 0,
        wins: 1,
        losses: 1,
        veteran: false,
        inactive: false,
        fresh_blood: false,
        hot_streak: false,
    };
    let page = |entries: &[store::Entry], late_since: i64| {
        let db = db.clone();
        let entries = entries.to_vec();
        async move {
            store::write_page(
                &db,
                store::Page {
                    key_scope: SCOPE,
                    crawl_id: "c",
                    platform: "kr",
                    queue: SOLO,
                    entries: &entries,
                    cursor: None,
                    apex_tier: None,
                    now: 900,
                    late_since,
                },
            )
            .await
            .unwrap();
        }
    };
    page(&[entry(&p[0], "MASTER")], END + 1).await;
    assert_eq!(tier_of(&stamps(&db).await, &p[0]), "UNKNOWN");
    page(&[entry(&p[0], "MASTER"), entry(&p[1], "DIAMOND")], END).await;
    let s = stamps(&db).await;
    assert_eq!(tier_of(&s, &p[0]), "MASTER");
    assert_eq!(tier_of(&s, &p[1]), "DIAMOND");
    assert_eq!(tier_of(&s, &p[2]), "UNKNOWN");
}

#[test]
fn the_late_stamp_window() {
    assert_eq!(late_since(100 * DAY_MS, 14), 86 * DAY_MS);
    assert_eq!(late_since(100, 0), i64::MAX, "0 days places nothing");
    assert_eq!(late_since(0, 14), -14 * DAY_MS);
    assert_eq!(ranked_queue(420), Some(SOLO));
    assert_eq!(ranked_queue(440), Some("RANKED_FLEX_SR"));
    assert_eq!(ranked_queue(450), None);
}

/// Matches archived before V0016 are found and stamped at today's tiers.
#[tokio::test]
async fn unstamped_matches_are_found_and_stamped() {
    let (_d, db) = db();
    let p = puuids();
    on_ladder(&db, "kr", SOLO, &p[0], "MASTER", 1).await;
    archive(&db, MATCH, 100).await;
    db.write(|c| {
        c.execute("UPDATE match_facts SET tier = NULL", [])?;
        Ok::<_, DbError>(())
    })
    .await
    .unwrap();
    let found = db
        .read(|c| {
            Ok::<_, DbError>((
                !unstamped(c, "", 1)?.is_empty(),
                unstamped(c, "", 10)?,
                unstamped(c, MATCH_ID, 10)?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(found, (true, vec![(MATCH_ID.to_string(), 420)], vec![]));
    let n = db
        .write(|c| Ok::<_, DbError>(stamp(c, MATCH_ID, 420)?))
        .await
        .unwrap();
    assert_eq!(n, 10);
    assert_eq!(tier_of(&stamps(&db).await, &p[0]), "MASTER");
    assert!(
        db.read(|c| Ok::<_, DbError>(unstamped(c, "", 1)?))
            .await
            .unwrap()
            .is_empty()
    );
    // One fact left unstamped is enough to bring the match back.
    let p9 = p[9].clone();
    db.write(move |c| {
        c.execute(
            "UPDATE match_facts SET tier = NULL WHERE match_id = ?1 AND puuid = ?2",
            rusqlite::params![MATCH_ID, p9],
        )?;
        Ok::<_, DbError>(())
    })
    .await
    .unwrap();
    assert!(
        !db.read(|c| Ok::<_, DbError>(unstamped(c, "", 1)?))
            .await
            .unwrap()
            .is_empty()
    );
    let n = db
        .write(|c| Ok::<_, DbError>(stamp(c, MATCH_ID, 420)?))
        .await
        .unwrap();
    assert_eq!(n, 1, "only the unstamped fact");
}

/// A re-extraction keeps every row's stamp and drops only a participant the
/// body no longer names (DSK-04: facts are upserted, not replaced).
#[tokio::test]
async fn reextracting_drops_only_a_participant_gone_from_the_body() {
    let (_d, db) = db();
    let p = puuids();
    on_ladder(&db, "kr", SOLO, &p[0], "MASTER", 1).await;
    archive(&db, MATCH, 100).await;
    let mut derived = matches::derive(MATCH).unwrap();
    let gone = p[9].clone();
    derived.1.retain(|f| f.puuid != gone);
    assert_eq!(derived.1.len(), 9);
    db.write(move |c| {
        let tx = c.transaction()?;
        matches::write_derived(&tx, MATCH_ID, SCOPE, &derived)?;
        tx.commit()?;
        Ok::<_, DbError>(())
    })
    .await
    .unwrap();
    let s = stamps(&db).await;
    assert_eq!(s.len(), 9);
    assert!(!s.contains_key(&gone));
    assert_eq!(tier_of(&s, &p[0]), "MASTER");
    let rows: i64 = db
        .read(|c| {
            Ok::<_, DbError>(c.query_row(
                "SELECT count(*) FROM match_facts WHERE match_id = ?1",
                [MATCH_ID],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(rows, 9);
}

/// The late stamp reads one player's facts through `facts_player` and checks
/// each match by its key: no scan of the archive (DSK-04).
#[tokio::test]
async fn the_late_stamp_plan_reads_the_players_facts_by_index() {
    let (_d, db) = db();
    let plan = db
        .read(|c| {
            let mut stmt = c.prepare(&format!("EXPLAIN QUERY PLAN {LATE_STAMP}"))?;
            let rows = stmt
                .query_map(params!["s", "kr", 420, "p", "GOLD", 0], |r| r.get::<_, String>(3))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, DbError>(rows)
        })
        .await
        .unwrap();
    let text = plan.join("\n");
    assert!(
        text.contains(
            "SEARCH match_facts USING INDEX facts_player (key_scope=? AND puuid=? AND match_id>? AND match_id<?)"
        ),
        "{text}"
    );
    assert!(
        text.contains("SEARCH m USING INDEX sqlite_autoindex_matches_1 (match_id=?)"),
        "{text}"
    );
    assert!(!text.contains("SCAN"), "{text}");
}

// ── AVL-06 (ADR-213): the fall-through and the sweep's cursors ──────────────

/// A made-up prefix that names no platform. Not `ME1`: ME-02 makes that one a
/// platform.
const NOWHERE: &str = "ZZ9_8393343196";

/// `puuid → tier` for one match's facts, NULL included.
async fn tiers_of(db: &Db, id: &'static str) -> std::collections::BTreeMap<String, Option<String>> {
    db.read(move |c| {
        let mut stmt = c.prepare("SELECT puuid, tier FROM match_facts WHERE match_id = ?1")?;
        let rows = stmt
            .query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        Ok::<_, DbError>(rows)
    })
    .await
    .unwrap()
}

/// A ranked match whose id names no platform is stamped `UNKNOWN` for every
/// participant, whatever the ladder holds, so it can't stay unstamped and be
/// looked for on every boot; a participant already stamped keeps their tier.
#[tokio::test]
async fn a_ranked_match_whose_prefix_is_no_platform_is_stamped_unknown() {
    let (_d, db) = db();
    let p = puuids();
    assert_eq!(Platform::from_match_id(NOWHERE), None);
    // Ranked on kr: not the match's platform, so it places no one.
    on_ladder(&db, "kr", SOLO, &p[0], "MASTER", 1).await;
    matches::put(&db, NOWHERE, "asia", SCOPE, Bytes::from_static(MATCH), 100)
        .await
        .unwrap();
    let t = tiers_of(&db, NOWHERE).await;
    assert_eq!(t.len(), 10);
    assert!(t.values().all(|v| v.as_deref() == Some(UNKNOWN_TIER)), "{t:?}");

    // A participant with a tier keeps it; only the NULL ones are written.
    let p0 = p[0].clone();
    db.write(move |c| {
        c.execute(
            "UPDATE match_facts SET tier = CASE WHEN puuid = ?2 THEN 'GOLD' ELSE NULL END WHERE match_id = ?1",
            rusqlite::params![NOWHERE, p0],
        )?;
        Ok::<_, DbError>(())
    })
    .await
    .unwrap();
    let n = db
        .write(|c| Ok::<_, DbError>(stamp(c, NOWHERE, 440)?))
        .await
        .unwrap();
    assert_eq!(n, 9);
    let t = tiers_of(&db, NOWHERE).await;
    assert_eq!(t[&p[0]].as_deref(), Some("GOLD"));
    assert_eq!(
        t.values().filter(|v| v.as_deref() == Some(UNKNOWN_TIER)).count(),
        9
    );
    assert!(
        db.read(|c| Ok::<_, DbError>(unstamped(c, "", 1)?))
            .await
            .unwrap()
            .is_empty()
    );
}

/// Outside solo and flex the fall-through writes nothing either (ADR-127).
#[tokio::test]
async fn a_non_ranked_match_whose_prefix_is_no_platform_is_not_stamped() {
    let (_d, db) = db();
    let aram = String::from_utf8(MATCH.to_vec())
        .unwrap()
        .replacen("\"queueId\":420", "\"queueId\":450", 1);
    matches::put(&db, NOWHERE, "asia", SCOPE, Bytes::from(aram), 100)
        .await
        .unwrap();
    let t = tiers_of(&db, NOWHERE).await;
    assert_eq!(t.len(), 10);
    assert!(t.values().all(Option::is_none), "{t:?}");
    let n = db
        .write(|c| Ok::<_, DbError>(stamp(c, NOWHERE, 450)?))
        .await
        .unwrap();
    assert_eq!(n, 0);
}

/// `n` ranked solo matches `{prefix}_{first}…` with two facts each, unstamped
/// (what `tiers:backfill` finds after V0016).
fn seed(c: &rusqlite::Connection, prefix: &str, first: i64, n: i64) {
    for i in first..first + n {
        let id = format!("{prefix}_{i}");
        c.execute(
            "INSERT INTO matches (match_id, region, patch, queue_id, game_end_ms, body_zstd, body_size, archived_at)
             VALUES (?1, 'asia', '16.20', 420, 0, x'00', 1, 0)",
            [&id],
        )
        .unwrap();
        for puuid in ["a", "b"] {
            c.execute(
                "INSERT INTO match_facts (match_id, key_scope, puuid, team_id, champion_id, win, facts_version)
                 VALUES (?1, 's', ?2, 100, 1, 1, 1)",
                [id.as_str(), puuid],
            )
            .unwrap();
        }
    }
}

/// What `tiers:backfill` does (`jobs::analytics`), on one connection: a
/// window of `size` ids past each prefix's cursor a pass, until no window is
/// full. Returns the matches stamped.
fn sweep_all(c: &mut rusqlite::Connection, size: i64, now: i64) -> usize {
    let mut stamped = 0;
    loop {
        let mut more = false;
        let tx = c.transaction().unwrap();
        for s in sweeps(&tx).unwrap() {
            let w = window(&tx, &s.after(), &s.before(), size).unwrap();
            for (id, queue_id) in &w.todo {
                if stamp(&tx, id, *queue_id).unwrap() > 0 {
                    stamped += 1;
                }
            }
            if let Some(last) = &w.last {
                advance(&tx, &s.prefix, last, now).unwrap();
            }
            more |= w.full;
        }
        tx.commit().unwrap();
        if !more {
            return stamped;
        }
    }
}

fn cursors(c: &rusqlite::Connection) -> Vec<(String, String)> {
    let mut stmt = c
        .prepare("SELECT prefix, swept_to FROM tiers_sweep ORDER BY prefix")
        .unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// A raw connection to a migrated database, for the synchronous tests.
fn raw() -> (tempfile::TempDir, Db, rusqlite::Connection) {
    let (d, db) = db();
    let c = rusqlite::Connection::open(d.path().join("t.db")).unwrap();
    (d, db, c)
}

fn prefixes(c: &rusqlite::Connection) -> Vec<(String, Option<String>)> {
    sweeps(c)
        .unwrap()
        .into_iter()
        .map(|s| (s.prefix, s.swept_to))
        .collect()
}

/// The prefixes come from seeks on the key, in id order, each with its
/// cursor; an id with no `_` is stepped over, and a prefix that starts a
/// longer one doesn't take its ids.
#[test]
fn the_prefixes_are_found_by_seeking_the_key() {
    let (_d, _db, mut c) = raw();
    seed(&c, "KR", 1, 2);
    seed(&c, "KRX", 1, 1);
    seed(&c, "EUW1", 5, 1);
    c.execute(
        "INSERT INTO matches (match_id, region, patch, queue_id, game_end_ms, body_zstd, body_size, archived_at)
         VALUES ('NOPREFIX', 'asia', '16.20', 420, 0, x'00', 1, 0)",
        [],
    )
    .unwrap();
    // "KRX_1" sorts before "KR_1": 'X' comes before '_'.
    assert_eq!(
        prefixes(&c),
        [
            ("EUW1".to_string(), None),
            ("KRX".to_string(), None),
            ("KR".to_string(), None)
        ]
    );
    let kr = Sweep {
        prefix: "KR".into(),
        swept_to: None,
    };
    let w = window(&c, &kr.after(), &kr.before(), 10).unwrap();
    assert_eq!(w.last.as_deref(), Some("KR_2"));
    assert_eq!(w.todo.len(), 2, "KR's own matches, not KRX's");
    // An empty cursor is a prefix never swept (a reset).
    c.execute("INSERT INTO tiers_sweep VALUES ('KR', '', 1)", [])
        .unwrap();
    assert_eq!(prefixes(&c)[2], ("KR".to_string(), None));
    assert_eq!(sweep_all(&mut c, 10, 2), 4);
    assert_eq!(
        prefixes(&c),
        [
            ("EUW1".to_string(), Some("EUW1_5".to_string())),
            ("KRX".to_string(), Some("KRX_1".to_string())),
            ("KR".to_string(), Some("KR_2".to_string()))
        ]
    );
}

/// A sweep stamps each prefix a window at a time and moves its cursor to the
/// last id it passed; afterwards the boot check reads only past the cursors,
/// so a NULL left below one is not looked for, while one past it is.
#[test]
fn a_sweep_stamps_and_advances_each_prefix_and_the_check_reads_past_the_cursors() {
    let (_d, _db, mut c) = raw();
    seed(&c, "KR", 100, 5);
    seed(&c, "EUW1", 100, 3);
    seed(&c, "ZZ9", 100, 2);
    assert_eq!(sweep_due(&c, 2).unwrap(), Some(Due::Unswept("EUW1".into())));

    // Windows of 2: KR takes three passes, and each match is stamped once.
    assert_eq!(sweep_all(&mut c, 2, 7), 10);
    assert_eq!(
        cursors(&c),
        [
            ("EUW1".to_string(), "EUW1_102".to_string()),
            ("KR".to_string(), "KR_104".to_string()),
            ("ZZ9".to_string(), "ZZ9_101".to_string())
        ]
    );
    let unknown: i64 = c
        .query_row(
            "SELECT count(*) FROM match_facts WHERE match_id LIKE 'ZZ9%' AND tier = 'UNKNOWN'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(unknown, 4, "the prefix that is no platform is stamped too");
    assert!(unstamped(&c, "", 1).unwrap().is_empty());
    assert_eq!(sweep_due(&c, 2).unwrap(), None);
    assert_eq!(sweep_all(&mut c, 2, 8), 0, "a second sweep stamps nothing");

    // Below a cursor nothing is read: a NULL put there isn't found.
    c.execute("UPDATE match_facts SET tier = NULL WHERE match_id = 'KR_101'", [])
        .unwrap();
    assert_eq!(sweep_due(&c, 2).unwrap(), None);
    // Past it, it is.
    seed(&c, "KR", 200, 1);
    assert_eq!(sweep_due(&c, 2).unwrap(), Some(Due::Unstamped("KR".into())));
    assert_eq!(sweep_all(&mut c, 2, 9), 1);
    assert_eq!(cursors(&c)[1], ("KR".to_string(), "KR_200".to_string()));

    // More past a cursor than the check reads: due, to move the cursor up.
    seed(&c, "EUW1", 200, 3);
    c.execute(
        "UPDATE match_facts SET tier = 'GOLD' WHERE match_id LIKE 'EUW1_2%'",
        [],
    )
    .unwrap();
    assert_eq!(sweep_due(&c, 2).unwrap(), Some(Due::Behind("EUW1".into())));
    assert_eq!(sweep_due(&c, 4).unwrap(), None);
    assert_eq!(sweep_all(&mut c, 2, 10), 0);
    assert_eq!(sweep_due(&c, 2).unwrap(), None);

    // A cursor reset makes the prefix due again, and the next sweep finds
    // what was left below the old cursor.
    c.execute("DELETE FROM tiers_sweep WHERE prefix = 'KR'", [])
        .unwrap();
    assert_eq!(sweep_due(&c, 2).unwrap(), Some(Due::Unswept("KR".into())));
    assert_eq!(sweep_all(&mut c, 2, 11), 1, "KR_101");
}

/// A match archived after the sweep is stamped by its own archive, so the
/// next check finds nothing, having read only that one id past the cursor.
#[tokio::test]
async fn a_match_archived_after_the_sweep_is_already_stamped() {
    let (_d, db, mut c) = raw();
    seed(&c, "KR", 1, 3);
    assert_eq!(sweep_all(&mut c, 500, 1), 3);
    archive(&db, MATCH, 100).await;
    let (due, past) = db
        .read(|c| {
            let kr = sweeps(c)?.into_iter().find(|s| s.prefix == "KR").unwrap();
            Ok::<_, DbError>((sweep_due(c, 500)?, window(c, &kr.after(), &kr.before(), 500)?))
        })
        .await
        .unwrap();
    assert_eq!(due, None);
    assert_eq!(past.last.as_deref(), Some(MATCH_ID));
    assert!(past.todo.is_empty() && !past.full);
}

/// The check's statements seek `matches`' key and read a range bounded on
/// both sides, never a scan of the table (as WR-01 asserts its plans).
#[tokio::test]
async fn the_check_reads_a_bounded_range_of_the_key() {
    let (_d, db) = db();
    let [next, end, todo] = db
        .read(|c| {
            let plan = |sql: &str, args: &[&dyn rusqlite::ToSql]| -> Result<String, DbError> {
                let mut stmt = c.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?;
                let rows = stmt
                    .query_map(args, |r| r.get::<_, String>(3))?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(rows.join(" | "))
            };
            Ok::<_, DbError>([
                plan(NEXT_ID, &[&"KR`"])?,
                plan(WINDOW_END, &[&"KR_1", &"KR`", &500i64])?,
                plan(WINDOW_TODO, &[&"KR_1", &"KR_9"])?,
            ])
        })
        .await
        .unwrap();
    assert!(
        next.contains("SEARCH matches USING COVERING INDEX") && next.contains("(match_id>?)"),
        "{next}"
    );
    assert!(
        end.contains("SEARCH matches USING COVERING INDEX") && end.contains("(match_id>? AND match_id<?)"),
        "{end}"
    );
    assert!(
        todo.contains("SEARCH m USING INDEX") && todo.contains("(match_id>? AND match_id<?)"),
        "{todo}"
    );
    assert!(
        todo.contains("SEARCH f EXISTS USING PRIMARY KEY (match_id=?)"),
        "{todo}"
    );
    for p in [&next, &end, &todo] {
        // A co-routine's or a constant row's SCAN reads no table.
        assert!(!p.contains("SCAN m") && !p.contains("SCAN f"), "{p}");
    }
}
