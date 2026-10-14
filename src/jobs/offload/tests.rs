use std::path::PathBuf;

use bytes::Bytes;

use super::*;
use crate::archive::{matches, pack};

const DAY: i64 = DAY_MS;
/// "Now" in every test: day 100.
const NOW: i64 = 100 * DAY;

fn db() -> (tempfile::TempDir, Db) {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("riot-proxy.db"), 2).unwrap();
    (dir, db)
}

/// About 1 KB of zstd per body, each different.
fn body(id: &str) -> Vec<u8> {
    let noise: String = (0..1200)
        .map(|i| {
            char::from(
                b'a' + ((i * 7 + id.len() * 13 + id.bytes().map(usize::from).sum::<usize>() * (i + 1)) % 26)
                    as u8,
            )
        })
        .collect();
    format!("{{\"metadata\":{{\"matchId\":\"{id}\"}},\"noise\":\"{noise}\"}}").into_bytes()
}

fn blob(id: &str) -> Vec<u8> {
    zstd::encode_all(body(id).as_slice(), 3).unwrap()
}

/// Matches archived at `at`, each with a timeline unless `timelines` is false.
async fn seed(db: &Db, ids: &[&str], at: i64, timelines: bool) {
    let rows: Vec<String> = ids.iter().map(ToString::to_string).collect();
    db.write(move |c| {
        for id in &rows {
            c.execute(
                "INSERT INTO matches (match_id, region, patch, queue_id, game_end_ms, body_zstd, body_size, archived_at)
                 VALUES (?1, 'asia', '16.19', 420, 1, ?2, 1, ?3)",
                rusqlite::params![id, blob(id), at],
            )?;
            if timelines {
                c.execute(
                    "INSERT INTO timelines (match_id, body_zstd) VALUES (?1, ?2)",
                    rusqlite::params![id, blob(id)],
                )?;
            }
        }
        Ok::<_, DbError>(())
    })
    .await
    .unwrap();
}

struct Env {
    _dir: tempfile::TempDir,
    db: Db,
    objects: Objects,
    ctx: OffloadContext,
}

/// `target` bytes a pack; `local_days` window.
fn env(target: u64, local_days: u32) -> Env {
    let (dir, db) = db();
    let objects = Objects::in_memory();
    assert!(db.attach_objects(objects.clone()));
    env_with(dir, db, objects, target, local_days)
}

fn env_with(dir: tempfile::TempDir, db: Db, objects: Objects, target: u64, local_days: u32) -> Env {
    let queue = Queue::new(db.clone());
    let ctx = OffloadContext {
        db: db.clone(),
        objects: objects.clone(),
        pending: Arc::new(Pending::new(queue.clone(), target)),
        local_days,
        target_bytes: target,
        pace: Duration::from_millis(50),
    };
    Env {
        _dir: dir,
        db,
        objects,
        ctx,
    }
}

async fn strings(db: &Db, sql: &'static str) -> Vec<String> {
    db.read(move |c| {
        let mut stmt = c.prepare(sql)?;
        let out = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok::<_, DbError>(out)
    })
    .await
    .unwrap()
}

async fn offload_jobs(db: &Db) -> i64 {
    db.read(|c| {
        Ok::<_, DbError>(c.query_row(
            "SELECT count(*) FROM jobs WHERE kind = 'archive:offload' AND state = 'pending'",
            [],
            |r| r.get(0),
        )?)
    })
    .await
    .unwrap()
}

/// Each row as `id:packed:freed`, `-` for unset.
async fn states(db: &Db, table: &'static str) -> Vec<String> {
    let sql: &'static str = match table {
        "matches" => {
            "SELECT match_id || ':' || coalesce(packed_at, '-') || ':' || coalesce(offloaded_at, '-')
               FROM matches ORDER BY match_id"
        }
        _ => {
            "SELECT match_id || ':' || coalesce(packed_at, '-') || ':' || coalesce(offloaded_at, '-')
               FROM timelines ORDER BY match_id"
        }
    };
    strings(db, sql).await
}

fn ids(n: usize, prefix: &str) -> Vec<String> {
    (0..n).map(|i| format!("{prefix}_{i:03}")).collect()
}

fn refs(v: &[String]) -> Vec<&str> {
    v.iter().map(String::as_str).collect()
}

#[tokio::test(start_paused = true)]
async fn a_full_pack_queues_one_offload_and_less_queues_none() {
    let e = env(10_000, 14);
    let p = &e.ctx.pending;
    p.add(Kind::Match, 4_000).await;
    p.add(Kind::Timeline, 9_999).await;
    assert_eq!(offload_jobs(&e.db).await, 0, "no kind has a full pack");
    p.add(Kind::Match, 6_000).await;
    assert_eq!(offload_jobs(&e.db).await, 1);
    // A second full pack, of the other kind, while one is pending: no duplicate.
    p.add(Kind::Timeline, 50_000).await;
    p.add(Kind::Match, 50_000).await;
    assert_eq!(offload_jobs(&e.db).await, 1);
    assert_eq!((p.bytes(Kind::Match), p.bytes(Kind::Timeline)), (60_000, 59_999));
}

/// The bug seen on production (2026-10-14): a full pack reached while a run
/// was going was absorbed by the dedupe, and the trigger stayed latched, so
/// no later full pack queued anything until a restart.
#[tokio::test(start_paused = true)]
async fn a_trigger_absorbed_by_a_running_job_tries_again_after_it_ends() {
    let e = env(10_000, 14);
    let p = &e.ctx.pending;
    enqueue(&e.ctx.pending.queue).await.unwrap();
    e.db.write(|c| {
        Ok::<_, DbError>(
            c.execute(
                "UPDATE jobs SET state = 'running' WHERE kind = 'archive:offload'",
                [],
            )
            .map(|_| ())?,
        )
    })
    .await
    .unwrap();
    p.add(Kind::Timeline, 20_000).await;
    assert_eq!(offload_jobs(&e.db).await, 0, "absorbed: the run is going");

    e.db.write(|c| {
        Ok::<_, DbError>(
            c.execute(
                "UPDATE jobs SET state = 'done' WHERE kind = 'archive:offload'",
                [],
            )
            .map(|_| ())?,
        )
    })
    .await
    .unwrap();
    p.add(Kind::Timeline, 1).await;
    assert_eq!(offload_jobs(&e.db).await, 0, "not asked again within a minute");
    tokio::time::advance(RETRY_ABSORBED).await;
    p.add(Kind::Timeline, 1).await;
    assert_eq!(
        offload_jobs(&e.db).await,
        1,
        "the next body after the run queues one"
    );
    p.add(Kind::Timeline, 1).await;
    tokio::time::advance(RETRY_ABSORBED).await;
    p.add(Kind::Timeline, 1).await;
    assert_eq!(offload_jobs(&e.db).await, 1, "latched once a job was created");
}

#[tokio::test(start_paused = true)]
async fn a_run_reseeds_the_counter_from_the_database() {
    let e = env(1 << 30, 14);
    seed(&e.db, &["KR_1", "KR_2"], NOW, true).await;
    e.ctx.pending.add(Kind::Match, 123).await;
    let report = e.ctx.run(NOW).await.unwrap();
    assert_eq!(
        report,
        Report::default(),
        "nothing is due: under a pack, inside the window"
    );
    let one = blob("KR_1").len() as u64 + blob("KR_2").len() as u64;
    assert_eq!(e.ctx.pending.bytes(Kind::Match), one);
    assert_eq!(e.ctx.pending.bytes(Kind::Timeline), one);
}

#[tokio::test(start_paused = true)]
async fn new_bodies_are_packed_once_a_pack_is_full_without_waiting_for_the_window() {
    let all = ids(30, "KR");
    let target = 10 * blob("KR_000").len() as u64;
    let e = env(target, 14);
    seed(&e.db, &refs(&all), NOW, true).await;
    let report = e.ctx.run(NOW).await.unwrap();
    assert!(report.packs >= 4, "{report:?}");
    assert_eq!(report.freed, 0, "inside the window: nothing freed");
    // Every pack is one kind, in match-id order.
    for o in e.objects.list("packs/").await.unwrap() {
        let kind = if o.key.starts_with("packs/matches/") {
            Kind::Match
        } else {
            Kind::Timeline
        };
        let bytes = e.objects.get_range(&o.key, 0..o.size).await.unwrap();
        let entries = pack::read(kind, &bytes).unwrap();
        let got: Vec<&str> = entries.iter().map(|x| x.match_id.as_str()).collect();
        let mut sorted = got.clone();
        sorted.sort_unstable();
        assert_eq!(got, sorted, "{}", o.key);
    }
    // Packed rows keep their local copy; what is left is less than a pack.
    let packed = strings(
        &e.db,
        "SELECT count(*) || ' ' || sum(length(body_zstd) > 0) FROM matches WHERE packed_at IS NOT NULL",
    )
    .await;
    let n: usize = packed[0].split(' ').next().unwrap().parse().unwrap();
    assert!(n >= 20, "{packed:?}");
    assert_eq!(packed[0], format!("{n} {n}"));
    assert!(e.ctx.pending.bytes(Kind::Match) < target);
    assert_eq!(
        matches::get(&e.db, "KR_000").await.unwrap().unwrap().as_ref(),
        body("KR_000"),
        "a packed, unfreed body reads locally"
    );
}

#[tokio::test(start_paused = true)]
async fn less_than_a_pack_waits_unless_some_of_it_has_left_the_window() {
    let e = env(1 << 30, 14);
    seed(&e.db, &["KR_1", "KR_2"], NOW - 13 * DAY, true).await;
    assert_eq!(
        e.ctx.run(NOW).await.unwrap().packs,
        0,
        "all inside the window: wait"
    );
    seed(&e.db, &["KR_0"], NOW - 15 * DAY, true).await;
    let r = e.ctx.run(NOW).await.unwrap();
    assert_eq!(
        (r.packs, r.packed_bodies),
        (2, 6),
        "one partial pack per kind, all of it"
    );
    assert_eq!(
        states(&e.db, "matches").await,
        [
            format!("KR_0:{NOW}:{NOW}"),
            format!("KR_1:{NOW}:-"),
            format!("KR_2:{NOW}:-")
        ],
        "only the one past the window is freed"
    );
    assert_eq!(states(&e.db, "timelines").await, states(&e.db, "matches").await);
}

#[tokio::test(start_paused = true)]
async fn freeing_takes_only_packed_bodies_past_the_window() {
    let e = env(1 << 30, 14);
    seed(&e.db, &["KR_old", "KR_new"], NOW - 20 * DAY, true).await;
    // KR_new is archived again inside the window.
    e.db.write(|c| {
        Ok::<_, DbError>(
            c.execute(
                "UPDATE matches SET archived_at = ?1 WHERE match_id = 'KR_new'",
                [NOW - DAY],
            )
            .map(|_| ())?,
        )
    })
    .await
    .unwrap();
    // KR_unpacked is past the window but its pack failed to happen.
    e.ctx.run(NOW).await.unwrap();
    seed(&e.db, &["KR_unpacked"], NOW - 30 * DAY, false).await;
    let r = offload::free(&e.db, Kind::Match, &["KR_unpacked".to_string()], NOW)
        .await
        .unwrap();
    assert_eq!(r, 0, "an unpacked body is never freed");
    assert_eq!(
        states(&e.db, "matches").await,
        [
            format!("KR_new:{NOW}:-"),
            format!("KR_old:{NOW}:{NOW}"),
            "KR_unpacked:-:-".to_string()
        ]
    );
    // With no window, every packed body goes.
    let e0 = OffloadContext {
        local_days: 0,
        ..clone_ctx(&e.ctx)
    };
    let r = e0.run(NOW + 1).await.unwrap();
    assert_eq!(r.packs, 1, "KR_unpacked, now past a zero window");
    assert_eq!(
        strings(
            &e.db,
            "SELECT CAST(count(*) AS TEXT) FROM matches WHERE offloaded_at IS NULL"
        )
        .await,
        ["0"]
    );
}

fn clone_ctx(c: &OffloadContext) -> OffloadContext {
    OffloadContext {
        db: c.db.clone(),
        objects: c.objects.clone(),
        pending: Arc::clone(&c.pending),
        local_days: c.local_days,
        target_bytes: c.target_bytes,
        pace: c.pace,
    }
}

/// A timeline's window runs from its match's archive time; one whose match
/// is not archived yet stays.
#[tokio::test(start_paused = true)]
async fn a_timeline_is_freed_by_its_matchs_age() {
    let e = env(1 << 30, 14);
    seed(&e.db, &["KR_1"], NOW - 20 * DAY, true).await;
    let orphan = "KR_9";
    e.db.write(move |c| {
        Ok::<_, DbError>(
            c.execute(
                "INSERT INTO timelines (match_id, body_zstd) VALUES (?1, ?2)",
                rusqlite::params![orphan, blob(orphan)],
            )
            .map(|_| ())?,
        )
    })
    .await
    .unwrap();
    e.ctx.run(NOW).await.unwrap();
    assert_eq!(
        states(&e.db, "timelines").await,
        [format!("KR_1:{NOW}:{NOW}"), format!("KR_9:{NOW}:-")],
        "packed with the old one, but kept: no match says how old it is"
    );
}

/// A store that refuses writes: a local "bucket" rooted at a plain file.
fn broken_store(dir: &tempfile::TempDir) -> Objects {
    let file: PathBuf = dir.path().join("not-a-dir");
    std::fs::write(&file, b"x").unwrap();
    Objects::from_store(
        Arc::new(object_store::local::LocalFileSystem::new_with_prefix(&file).unwrap()),
        false,
        "broken",
    )
}

#[tokio::test(start_paused = true)]
async fn a_failed_upload_marks_nothing_and_fails_the_job() {
    let (dir, db) = db();
    let objects = broken_store(&dir);
    let e = env_with(dir, db, objects, 1 << 30, 0);
    seed(&e.db, &["KR_1"], NOW - 20 * DAY, true).await;
    let err = e.ctx.run(NOW).await.unwrap_err();
    assert!(matches!(err, JobError::Retry(_)), "{err:?}");
    assert_eq!(states(&e.db, "matches").await, ["KR_1:-:-"]);
    assert_eq!(
        strings(&e.db, "SELECT CAST(count(*) AS TEXT) FROM body_packs").await,
        ["0"]
    );
    assert_eq!(
        matches::get(&e.db, "KR_1").await.unwrap().unwrap().as_ref(),
        body("KR_1")
    );
}

/// A crash between the upload and the transaction leaves an orphan object;
/// the rows are still unpacked and the next run writes a new pack.
#[tokio::test(start_paused = true)]
async fn a_crash_after_the_upload_leaves_an_orphan_and_the_next_run_packs_again() {
    let e = env(1 << 30, 0);
    seed(&e.db, &["KR_1", "KR_2"], NOW - 20 * DAY, false).await;
    let orphan = pack::build(
        ulid::Ulid::from_parts(1, 1),
        Kind::Match,
        vec![
            ("KR_1".into(), Bytes::from(blob("KR_1"))),
            ("KR_2".into(), Bytes::from(blob("KR_2"))),
        ],
    )
    .unwrap();
    e.objects
        .put(&orphan.key(), orphan.bytes.clone(), &[])
        .await
        .unwrap();

    let r = e.ctx.run(NOW).await.unwrap();
    assert_eq!((r.packs, r.freed), (1, 2));
    let packs = strings(&e.db, "SELECT pack_id FROM body_packs").await;
    assert_eq!(packs.len(), 1);
    assert_ne!(packs[0], orphan.id.to_string());
    assert_eq!(
        e.objects.list("packs/").await.unwrap().len(),
        2,
        "the orphan stays for OBJ-06"
    );
    let got = matches::get_many(&e.db, &["KR_1".into(), "KR_2".into()])
        .await
        .unwrap();
    assert_eq!(got["KR_2"].as_ref(), body("KR_2"));
}

#[tokio::test(start_paused = true)]
async fn the_gauges_and_the_counter_describe_what_is_left() {
    let e = env(1 << 30, 14);
    seed(&e.db, &["KR_1"], NOW - 20 * DAY, true).await;
    seed(&e.db, &["KR_2"], NOW, true).await;
    e.ctx.run(NOW).await.unwrap();
    let left = |kind| offload::backlog(&e.db, kind, NOW - 14 * DAY);
    assert_eq!(
        left(Kind::Match).await.unwrap(),
        offload::Backlog {
            to_pack: 0,
            to_free: 0,
            unpacked_bytes: 0
        },
        "a partial pack with an old body takes the new one too"
    );
    assert_eq!(e.ctx.pending.bytes(Kind::Timeline), 0);
}

#[tokio::test(start_paused = true)]
async fn maintenance_queues_the_offload_after_each_run() {
    let (dir, db) = db();
    let queue = Queue::new(db.clone());
    let m = crate::jobs::maintenance::Maintenance::new(db.clone(), dir.path().join("backups"), 0)
        .then_offload(queue.clone());
    m.run_once(NOW).await.unwrap();
    assert_eq!(offload_jobs(&db).await, 1);
    m.run_once(NOW + DAY).await.unwrap();
    assert_eq!(offload_jobs(&db).await, 1, "deduped while pending");

    // Without a bucket, maintenance queues nothing.
    let (dir2, db2) = self::db();
    let plain = crate::jobs::maintenance::Maintenance::new(db2.clone(), dir2.path().join("backups"), 0);
    plain.run_once(NOW).await.unwrap();
    assert_eq!(offload_jobs(&db2).await, 0);
}

/// The two "to free" lookups read indexes only: matches by its partial index,
/// a timeline's match by `matches_archived` (V0024), never a match body.
#[tokio::test]
async fn the_free_lookups_never_read_a_match_row() {
    let (_dir, db) = db();
    let plans = db
        .read(|c| {
            let mut out = Vec::new();
            for sql in [
                "EXPLAIN QUERY PLAN SELECT count(*) FROM matches
                  WHERE packed_at IS NOT NULL AND offloaded_at IS NULL AND archived_at < 5",
                "EXPLAIN QUERY PLAN SELECT count(*) FROM timelines t JOIN matches m ON m.match_id = t.match_id
                  WHERE t.packed_at IS NOT NULL AND t.offloaded_at IS NULL AND m.archived_at < 5",
            ] {
                let mut stmt = c.prepare(sql)?;
                let rows = stmt
                    .query_map([], |r| r.get::<_, String>(3))?
                    .collect::<Result<Vec<_>, _>>()?;
                out.push(rows.join(" | "));
            }
            Ok::<_, DbError>(out)
        })
        .await
        .unwrap();
    assert!(plans[0].contains("COVERING INDEX matches_to_free"), "{plans:?}");
    assert!(plans[1].contains("COVERING INDEX timelines_to_free"), "{plans:?}");
    assert!(plans[1].contains("COVERING INDEX matches_archived"), "{plans:?}");
}
