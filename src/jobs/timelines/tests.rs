#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

fn db() -> (tempfile::TempDir, Db) {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("riot-proxy.db"), 2).unwrap();
    (dir, db)
}

/// An archived match row: only the columns the backfill reads matter.
fn put(c: &Connection, id: &str, patch: &str, queue: i64, end: i64) {
    c.execute(
        "INSERT INTO matches (match_id, region, patch, queue_id, game_end_ms, body_zstd, body_size, archived_at)
         VALUES (?1, 'x', ?2, ?3, ?4, x'00', 1, 0)",
        rusqlite::params![id, patch, queue, end],
    )
    .unwrap();
}

fn timeline(c: &Connection, id: &str) {
    c.execute(
        "INSERT INTO timelines (match_id, body_zstd) VALUES (?1, x'00')",
        [id],
    )
    .unwrap();
}

async fn with<T: Send + 'static>(db: &Db, f: impl FnOnce(&mut Connection) -> T + Send + 'static) -> T {
    db.write(move |c| Ok::<_, DbError>(f(c))).await.unwrap()
}

/// Three patches over sea, asia and an ARAM game; one timeline archived.
async fn archive(db: &Db) {
    with(db, |c| {
        put(c, "OC1_1", "16.18", 420, 1_000);
        put(c, "OC1_2", "16.18", 420, 3_000);
        put(c, "OC1_3", "16.9", 420, 9_000);
        put(c, "OC1_4", "16.10", 440, 2_000);
        put(c, "OC1_5", "16.10", 420, 5_000);
        put(c, "OC1_6", "16.10", 450, 6_000); // ARAM: never a build
        put(c, "PH2_7", "16.10", 420, 4_000); // sea too
        put(c, "KR_8", "16.10", 420, 7_000); // asia
        put(c, "OC1_9", "16.10", 420, 8_000);
        timeline(c, "OC1_9");
    })
    .await;
}

#[tokio::test]
async fn newest_patch_first_then_newest_game() {
    let (_d, db) = db();
    archive(&db).await;
    let (patches, batch) = with(&db, |c| {
        let patches = patches_on(c, 10).unwrap();
        let batch = next_batch_on(c, Region::Sea, &patches, 10).unwrap();
        (patches, batch)
    })
    .await;
    // 16.18 above 16.10 above 16.9: by number, not as text.
    assert_eq!(patches, ["16.18", "16.10", "16.9"]);
    let ids: Vec<&str> = batch.iter().map(|(id, _)| id.as_str()).collect();
    // Within a patch the newest game first; ARAM, asia and the match with a timeline left out.
    assert_eq!(ids, ["OC1_2", "OC1_1", "OC1_5", "PH2_7", "OC1_4", "OC1_3"]);
    assert_eq!(batch[2].1, "16.10");
}

#[tokio::test]
async fn the_bound_and_the_batch_size_are_respected() {
    let (_d, db) = db();
    archive(&db).await;
    let (two, small) = with(&db, |c| {
        let patches = patches_on(c, 2).unwrap();
        (
            next_batch_on(c, Region::Sea, &patches, 10).unwrap(),
            next_batch_on(c, Region::Sea, &patches, 3).unwrap(),
        )
    })
    .await;
    assert!(
        two.iter().all(|(_, p)| p != "16.9"),
        "16.9 is outside the newest two"
    );
    assert_eq!(two.len(), 5);
    assert_eq!(small.len(), 3);
    assert!(
        with(&db, |c| patches_on(c, 0).unwrap()).await.is_empty(),
        "0 is off"
    );
}

#[tokio::test]
async fn a_marked_gap_is_not_asked_for_again() {
    let (_d, db) = db();
    archive(&db).await;
    let batch = with(&db, |c| {
        mark_gap_on(c, "OC1_2", NOT_FOUND, 1).unwrap();
        mark_gap_on(c, "OC1_2", NOT_FOUND, 2).unwrap(); // marking twice is fine
        let patches = patches_on(c, 10).unwrap();
        next_batch_on(c, Region::Sea, &patches, 10).unwrap()
    })
    .await;
    assert!(batch.iter().all(|(id, _)| id != "OC1_2"));
    assert_eq!(batch.len(), 5);
}

#[tokio::test]
async fn progress_counts_per_patch_and_per_region() {
    let (_d, db) = db();
    archive(&db).await;
    with(&db, |c| mark_gap_on(c, "OC1_3", NOT_FOUND, 1).unwrap()).await;
    let p = progress(&db, 2, 10).await.unwrap();
    assert_eq!(p.configured_patches, 2);
    let row = |patch: &str| {
        p.patches
            .iter()
            .find(|x| x.patch.as_deref() == Some(patch))
            .unwrap()
            .clone()
    };
    assert_eq!(
        row("16.10"),
        PatchProgress {
            patch: Some("16.10".into()),
            matches: 5, // the ARAM game isn't ranked
            with_timeline: 1,
            not_found: 0,
            left: 4,
        }
    );
    assert_eq!((row("16.9").not_found, row("16.9").left), (1, 0));
    assert_eq!((p.totals.matches, p.totals.left), (8, 6));
    let region = |r: &str| p.regions.iter().find(|x| x.region == r).unwrap().clone();
    assert_eq!(
        (region("sea").left, region("sea").patch.as_deref()),
        (5, Some("16.18"))
    );
    assert_eq!(
        (region("asia").left, region("asia").patch.as_deref()),
        (1, Some("16.10"))
    );
    assert_eq!((region("europe").left, region("europe").patch), (0, None));
    // Off and no preview asked for: nothing to report.
    assert!(progress(&db, 0, 0).await.unwrap().patches.is_empty());
}

#[test]
fn a_regions_job_is_deduped_by_region_in_its_own_lane() {
    let j = job(Region::Asia);
    assert_eq!(j.priority, priority::TIMELINE_BACKFILL);
    // Below every other band.
    const { assert!(priority::TIMELINE_BACKFILL > priority::MAINTENANCE) };
    assert_eq!(j.dedupe_key.as_deref(), Some("asia"));
    let lane = crate::jobs::lanes::of(kinds::TIMELINES_BACKFILL, &j.payload).unwrap();
    assert_eq!((lane.lane, lane.method), ("asia", "match.timeline"));
}
