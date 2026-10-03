//! `riot-proxy migrate-v1` (plan P8-02) on a real v1 dump: 20 matches, their
//! one timeline, 12 players and 2 consumers, from v1's own migrations on
//! Postgres 17 (`tests/fixtures/v1-dump/v1.dump`, and `v1-data.sql`, its
//! `pg_restore --data-only` text).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::process::Command;
use std::time::Instant;

use riot_proxy::archive::matches;
use riot_proxy::cli::migrate_v1::{Report, import};
use riot_proxy::db::{Db, DbError};
use serde_json::Value;

const DATA: &str = "tests/fixtures/v1-dump/v1-data.sql";
const DUMP: &str = "tests/fixtures/v1-dump/v1.dump";
/// The key scope v1 stored with the fixture's players.
const SCOPE: &str = "deadbeef";

fn db() -> (tempfile::TempDir, Db) {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("riot-proxy.db"), 2).unwrap();
    (dir, db)
}

async fn run(db: &Db) -> Report {
    import(db, std::fs::File::open(DATA).unwrap(), SCOPE)
        .await
        .unwrap()
}

async fn count(db: &Db, sql: &'static str) -> i64 {
    db.read(move |c| Ok::<_, DbError>(c.query_row(sql, [], |r| r.get(0))?))
        .await
        .unwrap()
}

/// The fixture's v1 body for a match, from the COPY text.
fn v1_body(id: &str) -> Value {
    let text = std::fs::read_to_string(DATA).unwrap();
    let row = text.lines().find(|l| l.starts_with(&format!("{id}\t"))).unwrap();
    let field = row.split('\t').nth(2).unwrap();
    let raw = riot_proxy::cli::migrate_v1::unescape(field.as_bytes()).unwrap();
    serde_json::from_slice(&raw).unwrap()
}

#[tokio::test]
async fn the_v1_archive_and_players_arrive_and_consumers_do_not() {
    let (_d, db) = db();
    let report = run(&db).await;
    assert_eq!((report.matches, report.timelines, report.players), (19, 1, 12));
    // v1 stored a body v2 cannot archive (no gameEndTimestamp): named, not fatal.
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(report.skipped[0].0, "KR_9100019");
    assert!(
        report.skipped[0].1.contains("gameEndTimestamp"),
        "{:?}",
        report.skipped
    );
    assert_eq!(report.ignored.get("consumers"), Some(&2));
    assert_eq!(report.player_scopes.get(SCOPE), Some(&12));

    // Bodies are v1's (JSON-equal: jsonb already reordered Riot's bytes),
    // escapes and non-ASCII included.
    for id in ["KR_9100003", "KR_9100004", "KR_9100016"] {
        let stored: Value = serde_json::from_slice(&matches::get(&db, id).await.unwrap().unwrap()).unwrap();
        assert_eq!(stored, v1_body(id), "{id}");
    }
    let tab: Value =
        serde_json::from_slice(&matches::get(&db, "KR_9100003").await.unwrap().unwrap()).unwrap();
    assert_eq!(tab["info"]["participants"][0]["riotIdGameName"], "Tab\tand\\back");
    let korean: Value =
        serde_json::from_slice(&matches::get(&db, "KR_9100004").await.unwrap().unwrap()).unwrap();
    assert_eq!(
        korean["info"]["participants"][1]["riotIdGameName"],
        "새벽\n줄바꿈"
    );
    assert!(matches::get_timeline(&db, "KR_9100000").await.unwrap().is_some());

    // Facts re-derived at the current version, under the key scope given; the
    // remake flagged, bans written, archive stamps from v1's fetched_at.
    assert_eq!(count(&db, "SELECT COUNT(*) FROM matches").await, 19);
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM matches WHERE facts_version = 3").await,
        19
    );
    assert_eq!(
        count(&db, "SELECT COUNT(DISTINCT key_scope) FROM match_facts").await,
        1
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM match_facts WHERE key_scope = 'deadbeef'"
        )
        .await,
        // Sixteen ranked games of ten, two Arena games of eighteen, the remake of ten.
        16 * 10 + 2 * 18 + 10
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM matches WHERE remake = 1").await,
        1
    );
    assert!(count(&db, "SELECT COUNT(*) FROM match_bans").await > 0);
    assert_eq!(
        count(
            &db,
            "SELECT archived_at FROM matches WHERE match_id = 'KR_9100000'"
        )
        .await,
        1_789_898_400_123
    );

    // Players: tracked, names, cursor and v1's backfill stamps.
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM players WHERE tracked = 1").await,
        2
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM players WHERE game_name IS NULL").await,
        4
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM players WHERE last_seen_match_id = 'KR_9100000'"
        )
        .await,
        1
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM players WHERE json_extract(backfill_state, '$.doneAt') IS NOT NULL AND json_extract(backfill_state, '$.depth') = 500").await,
        1
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM players WHERE json_extract(backfill_state, '$.startedAt') IS NOT NULL"
        )
        .await,
        2
    );
    assert_eq!(count(&db, "SELECT COUNT(*) FROM consumers").await, 0);

    // Running it again changes nothing: every write is an upsert.
    let again = run(&db).await;
    assert_eq!((again.matches, again.players), (19, 12));
    assert_eq!(count(&db, "SELECT COUNT(*) FROM matches").await, 19);
    assert_eq!(count(&db, "SELECT COUNT(*) FROM players").await, 12);
}

#[test]
fn the_cli_imports_the_text_form_and_the_custom_dump_when_pg_restore_exists() {
    let bin = env!("CARGO_BIN_EXE_riot-proxy");
    let tmp = tempfile::tempdir().unwrap();
    let cmd = |from: &str| {
        Command::new(bin)
            .env_clear()
            // For pg_restore, and nothing else of the developer's.
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("RIOT_API_KEY", common::TEST_KEY)
            .env("DATA_DIR", tmp.path())
            .current_dir(tmp.path())
            .args(["migrate-v1", "--from", from, "--key-scope", SCOPE])
            .output()
            .unwrap()
    };
    let text = format!("{}/{DATA}", env!("CARGO_MANIFEST_DIR"));
    let out = cmd(&text);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("imported 19 matches"), "{stdout}");
    assert!(stdout.contains("1 timelines, 12 players"), "{stdout}");
    assert!(stdout.contains("skipped KR_9100019"), "{stdout}");
    assert!(stdout.contains("2 consumers not migrated"), "{stdout}");

    // The custom-format dump goes through pg_restore, when there is one.
    if Command::new("pg_restore").arg("--version").output().is_err() {
        eprintln!("pg_restore not on PATH: the custom-format path is not exercised here");
        return;
    }
    let dump = format!("{}/{DUMP}", env!("CARGO_MANIFEST_DIR"));
    let out = cmd(&dump);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("imported 19 matches"), "{stdout}");
}

/// Plan P8-02 acceptance: ≥ 1 000 matches/s on the fixture loop. Release
/// only (zstd and the JSON parse are C and Rust at -O0 in a debug build):
///     cargo test --release --test migrate_v1 -- --ignored --nocapture
#[tokio::test(flavor = "multi_thread")]
#[ignore = "throughput; run in release"]
async fn imports_at_least_a_thousand_matches_a_second() {
    let (_d, db) = db();
    let fixture = std::fs::read(DATA).unwrap();
    let loops = 60;
    let started = Instant::now();
    let mut imported = 0;
    for _ in 0..loops {
        imported += import(&db, std::io::Cursor::new(fixture.clone()), SCOPE)
            .await
            .unwrap()
            .matches;
    }
    let secs = started.elapsed().as_secs_f64();
    #[allow(clippy::cast_precision_loss)]
    let rate = imported as f64 / secs;
    println!("{imported} matches in {secs:.2}s: {rate:.0} matches/s");
    assert!(rate >= 1000.0, "{rate:.0} matches/s");
}
