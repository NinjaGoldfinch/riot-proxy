use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::stream::BoxStream;
use object_store::local::LocalFileSystem;
use object_store::path::Path;
use object_store::{
    CopyOptions, GetOptions, GetResult, ListResult, MultipartUpload, ObjectMeta, ObjectStore,
    PutMultipartOptions, PutOptions, PutPayload, PutResult, RenameOptions,
};

use super::*;
use crate::archive::offload::{free, write_pack};
use crate::db::DbError;

const NOW: i64 = 1_791_000_000_000;
const DAY: i64 = 86_400_000;

/// A local directory that remembers every key read (GET or HEAD).
#[derive(Debug)]
struct Reads {
    inner: LocalFileSystem,
    keys: Arc<Mutex<Vec<String>>>,
}

impl std::fmt::Display for Reads {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Reads")
    }
}

#[async_trait]
impl ObjectStore for Reads {
    async fn put_opts(
        &self,
        location: &Path,
        payload: PutPayload,
        opts: PutOptions,
    ) -> object_store::Result<PutResult> {
        self.inner.put_opts(location, payload, opts).await
    }

    async fn put_multipart_opts(
        &self,
        location: &Path,
        opts: PutMultipartOptions,
    ) -> object_store::Result<Box<dyn MultipartUpload>> {
        self.inner.put_multipart_opts(location, opts).await
    }

    async fn get_opts(&self, location: &Path, options: GetOptions) -> object_store::Result<GetResult> {
        self.keys
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(location.to_string());
        self.inner.get_opts(location, options).await
    }

    fn delete_stream(
        &self,
        locations: BoxStream<'static, object_store::Result<Path>>,
    ) -> BoxStream<'static, object_store::Result<Path>> {
        self.inner.delete_stream(locations)
    }

    fn list(&self, prefix: Option<&Path>) -> BoxStream<'static, object_store::Result<ObjectMeta>> {
        self.inner.list(prefix)
    }

    async fn list_with_delimiter(&self, prefix: Option<&Path>) -> object_store::Result<ListResult> {
        self.inner.list_with_delimiter(prefix).await
    }

    async fn copy_opts(&self, from: &Path, to: &Path, options: CopyOptions) -> object_store::Result<()> {
        self.inner.copy_opts(from, to, options).await
    }

    async fn rename_opts(&self, from: &Path, to: &Path, options: RenameOptions) -> object_store::Result<()> {
        self.inner.rename_opts(from, to, options).await
    }
}

struct Env {
    dir: tempfile::TempDir,
    db: Db,
    objects: Objects,
    reads: Arc<Mutex<Vec<String>>>,
}

impl Env {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("riot-proxy.db"), 2).unwrap();
        let bucket = dir.path().join("bucket");
        std::fs::create_dir_all(&bucket).unwrap();
        let reads = Arc::new(Mutex::new(Vec::new()));
        let store = Reads {
            inner: LocalFileSystem::new_with_prefix(&bucket).unwrap(),
            keys: reads.clone(),
        };
        let objects = Objects::from_store(Arc::new(store), false, "file://bucket");
        assert!(db.attach_objects(objects.clone()));
        Self {
            dir,
            db,
            objects,
            reads,
        }
    }

    fn file(&self, key: &str) -> std::path::PathBuf {
        self.dir.path().join("bucket").join(key)
    }

    /// Pack keys read since the last call.
    fn packs_read(&self) -> BTreeSet<String> {
        std::mem::take(&mut *self.reads.lock().unwrap())
            .into_iter()
            .filter(|k| k.starts_with("packs/"))
            .collect()
    }

    /// Seed `ids` of `kind` and write them as one pack at `at`; free the
    /// first half, so packs hold both freed and still-local bodies.
    async fn pack(&self, kind: Kind, ids: &[&str], at: i64) -> String {
        seed(&self.db, kind, ids).await;
        let list: Vec<String> = ids.iter().map(ToString::to_string).collect();
        let written = write_pack(&self.db, &self.objects, kind, &list, at)
            .await
            .unwrap()
            .unwrap();
        free(&self.db, kind, &list[..list.len() / 2], at + 1)
            .await
            .unwrap();
        written.key
    }

    async fn empty_map(&self, packs_too: bool) {
        self.db
            .write(move |c| {
                c.execute("DELETE FROM body_locations", [])?;
                if packs_too {
                    c.execute("DELETE FROM body_packs", [])?;
                }
                Ok::<_, DbError>(())
            })
            .await
            .unwrap();
        assert!(tables(&self.db).await.unwrap().locations.is_empty());
    }
}

fn body(id: &str) -> Vec<u8> {
    format!(
        "{{\"metadata\":{{\"matchId\":\"{id}\"}},\"pad\":\"{}\"}}",
        "x".repeat(200)
    )
    .into_bytes()
}

async fn seed(db: &Db, kind: Kind, ids: &[&str]) {
    let rows: Vec<(String, Vec<u8>)> = ids
        .iter()
        .map(|id| (id.to_string(), zstd::encode_all(body(id).as_slice(), 3).unwrap()))
        .collect();
    db.write(move |c| {
        for (id, blob) in &rows {
            match kind {
                Kind::Match => c.execute(
                    "INSERT INTO matches (match_id, region, patch, queue_id, game_end_ms, body_zstd, body_size, archived_at)
                     VALUES (?1, 'asia', '16.19', 420, 1, ?2, 1, 1)",
                    rusqlite::params![id, blob],
                )?,
                Kind::Timeline => c.execute(
                    "INSERT INTO timelines (match_id, body_zstd) VALUES (?1, ?2)",
                    rusqlite::params![id, blob],
                )?,
            };
        }
        Ok::<_, DbError>(())
    })
    .await
    .unwrap();
}

/// Two match packs and a timeline pack.
async fn three_packs(e: &Env) -> Vec<String> {
    vec![
        e.pack(Kind::Match, &["OC1_1", "OC1_2", "OC1_3", "OC1_4"], NOW)
            .await,
        e.pack(Kind::Timeline, &["OC1_1", "OC1_2"], NOW + 10).await,
        e.pack(Kind::Match, &["KR_1", "KR_2"], NOW + 20).await,
    ]
}

#[tokio::test]
async fn a_snapshot_round_trips_both_tables_exactly() {
    let e = Env::new();
    three_packs(&e).await;
    let before = tables(&e.db).await.unwrap();
    assert_eq!((before.packs.len(), before.locations.len()), (3, 8));

    let s = write_snapshot(&e.db, &e.objects, NOW + DAY).await.unwrap();
    assert_eq!(s.date, date_of(NOW + DAY));
    assert_eq!((s.packs, s.locations), (3, 8));
    assert_eq!(
        s.last_pack.as_deref(),
        before.packs.last().map(|p| p.pack_id.as_str())
    );
    assert!(e.file(&packs_key(&s.date)).exists());
    assert!(e.file(&locations_key(&s.date)).exists());

    e.empty_map(true).await;
    e.packs_read();
    let r = restore_index(&e.db, &e.objects).await.unwrap();
    assert_eq!(r.snapshot.as_deref(), Some(s.date.as_str()));
    assert_eq!((r.packs, r.locations), (3, 8));
    assert_eq!(
        r.reindexed,
        Reindexed::default(),
        "nothing is newer than the snapshot"
    );
    assert_eq!(tables(&e.db).await.unwrap(), before);
    assert!(
        e.packs_read().is_empty(),
        "a restore with nothing newer reads no pack"
    );
}

#[tokio::test]
async fn a_snapshot_carries_the_last_pack_and_row_count_in_its_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("riot-proxy.db"), 2).unwrap();
    let objects = Objects::in_memory();
    seed(&db, Kind::Match, &["OC1_1", "OC1_2"]).await;
    let ids = vec!["OC1_1".to_string(), "OC1_2".to_string()];
    write_pack(&db, &objects, Kind::Match, &ids, NOW)
        .await
        .unwrap()
        .unwrap();
    let s = write_snapshot(&db, &objects, NOW).await.unwrap();
    let last = s.last_pack.clone().unwrap();
    let packs = objects.head(&packs_key(&s.date)).await.unwrap().unwrap();
    assert_eq!(packs.metadata.get("last-pack"), Some(&last));
    assert_eq!(packs.metadata.get("rows").map(String::as_str), Some("1"));
    let locations = objects.head(&locations_key(&s.date)).await.unwrap().unwrap();
    assert_eq!(locations.metadata.get("last-pack"), Some(&last));
    assert_eq!(locations.metadata.get("rows").map(String::as_str), Some("2"));
}

#[tokio::test]
async fn reindex_rebuilds_an_emptied_body_locations_exactly() {
    let e = Env::new();
    three_packs(&e).await;
    let before = tables(&e.db).await.unwrap();

    e.empty_map(false).await;
    let r = reindex(&e.db, &e.objects, None).await.unwrap();
    assert_eq!((r.packs, r.bodies, r.failed.len()), (3, 8, 0));
    assert_eq!(tables(&e.db).await.unwrap(), before);

    // With body_packs gone too, each pack's row comes back from the bucket.
    e.empty_map(true).await;
    reindex(&e.db, &e.objects, None).await.unwrap();
    assert_eq!(tables(&e.db).await.unwrap(), before);

    // And running it over an intact map changes nothing.
    reindex(&e.db, &e.objects, None).await.unwrap();
    assert_eq!(tables(&e.db).await.unwrap(), before);
}

#[tokio::test]
async fn restore_reads_only_the_packs_newer_than_its_snapshot() {
    let e = Env::new();
    three_packs(&e).await;
    write_snapshot(&e.db, &e.objects, NOW + DAY).await.unwrap();
    let newer: BTreeSet<String> = [
        e.pack(Kind::Match, &["EUW1_1", "EUW1_2"], NOW + DAY + 10).await,
        e.pack(Kind::Timeline, &["KR_1", "KR_2"], NOW + DAY + 20).await,
    ]
    .into();
    let before = tables(&e.db).await.unwrap();
    assert_eq!(before.packs.len(), 5);

    e.empty_map(true).await;
    e.packs_read();
    let r = restore_index(&e.db, &e.objects).await.unwrap();
    assert_eq!((r.packs, r.reindexed.packs, r.reindexed.bodies), (3, 2, 4));
    assert_eq!(tables(&e.db).await.unwrap(), before);
    assert_eq!(e.packs_read(), newer, "only the two newer packs are read");
}

#[tokio::test]
async fn a_corrupt_newest_snapshot_falls_back_to_the_one_before() {
    let e = Env::new();
    three_packs(&e).await;
    write_snapshot(&e.db, &e.objects, NOW).await.unwrap();
    let newer = e.pack(Kind::Match, &["EUW1_1", "EUW1_2"], NOW + DAY).await;
    let s = write_snapshot(&e.db, &e.objects, NOW + DAY).await.unwrap();
    let before = tables(&e.db).await.unwrap();

    // One flipped byte in the newest locations: the frame checksum fails.
    let path = e.file(&locations_key(&s.date));
    let mut bytes = std::fs::read(&path).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xff;
    std::fs::write(&path, bytes).unwrap();

    e.empty_map(true).await;
    e.packs_read();
    let r = restore_index(&e.db, &e.objects).await.unwrap();
    assert_eq!(r.snapshot, Some(date_of(NOW)));
    assert_eq!(r.skipped.len(), 1);
    assert_eq!(r.skipped[0].0, s.date);
    assert_eq!(tables(&e.db).await.unwrap(), before);
    assert_eq!(e.packs_read(), BTreeSet::from([newer]));
}

#[tokio::test]
async fn a_snapshot_whose_halves_disagree_is_passed_over() {
    let e = Env::new();
    three_packs(&e).await;
    write_snapshot(&e.db, &e.objects, NOW).await.unwrap();
    let before = tables(&e.db).await.unwrap();
    // The newest packs object lists no pack, so its locations point nowhere.
    let s = write_snapshot(&e.db, &e.objects, NOW + DAY).await.unwrap();
    let empty = zstd::encode_all(&b""[..], 3).unwrap();
    std::fs::write(e.file(&packs_key(&s.date)), empty).unwrap();

    e.empty_map(true).await;
    let r = restore_index(&e.db, &e.objects).await.unwrap();
    assert_eq!(r.snapshot, Some(date_of(NOW)));
    assert!(r.skipped[0].1.contains("does not list"), "{:?}", r.skipped);
    assert_eq!(tables(&e.db).await.unwrap(), before);
}

#[tokio::test]
async fn with_no_readable_snapshot_every_pack_is_reindexed() {
    let e = Env::new();
    three_packs(&e).await;
    let before = tables(&e.db).await.unwrap();
    // Half a snapshot (no locations object) does not count.
    let s = write_snapshot(&e.db, &e.objects, NOW).await.unwrap();
    std::fs::remove_file(e.file(&locations_key(&s.date))).unwrap();

    e.empty_map(true).await;
    let r = restore_index(&e.db, &e.objects).await.unwrap();
    assert_eq!(r.snapshot, None);
    assert_eq!(r.reindexed.packs, 3);
    assert_eq!(tables(&e.db).await.unwrap(), before);
}

#[tokio::test]
async fn an_unreadable_pack_is_reported_and_the_rest_are_recorded() {
    let e = Env::new();
    let keys = three_packs(&e).await;
    std::fs::write(e.file(&keys[1]), b"not a pack at all").unwrap();
    e.empty_map(true).await;
    let r = reindex(&e.db, &e.objects, None).await.unwrap();
    assert_eq!(r.packs, 2);
    assert_eq!(r.failed.len(), 1);
    assert_eq!(r.failed[0].0, keys[1]);
    assert!(r.failed[0].1.contains("not a pack"), "{:?}", r.failed);
    let t = tables(&e.db).await.unwrap();
    assert!(t.locations.iter().all(|l| l.kind == Kind::Match));
    assert_eq!(t.locations.len(), 6);
}

#[tokio::test]
async fn reindex_since_reads_only_newer_packs_and_the_newest_pack_keeps_a_body() {
    let e = Env::new();
    let first = e.pack(Kind::Match, &["OC1_1", "OC1_2"], NOW).await;
    // OC1_2 in a newer pack too (as compaction will do): the newer one holds it.
    e.db.write(|c| {
        c.execute(
            "UPDATE matches SET packed_at = NULL, offloaded_at = NULL WHERE match_id = 'OC1_2'",
            [],
        )?;
        Ok::<_, DbError>(())
    })
    .await
    .unwrap();
    let ids = vec!["OC1_2".to_string()];
    write_pack(&e.db, &e.objects, Kind::Match, &ids, NOW + 10)
        .await
        .unwrap()
        .unwrap();
    let before = tables(&e.db).await.unwrap();
    let oc1_1 = &before.locations[0];
    assert_eq!(
        before.packs[0].live_bytes, oc1_1.len,
        "only OC1_1 is live in the first pack"
    );

    e.empty_map(true).await;
    e.packs_read();
    let (_, since) = pack::parse_key(&first).unwrap();
    let r = reindex(&e.db, &e.objects, Some(since)).await.unwrap();
    assert_eq!(r.packs, 1);
    assert_eq!(e.packs_read().len(), 1);
    assert!(
        !tables(&e.db)
            .await
            .unwrap()
            .locations
            .iter()
            .any(|l| l.match_id == "OC1_1")
    );

    // A full reindex records the older pack first, then the newer one takes
    // OC1_2 back from it.
    reindex(&e.db, &e.objects, None).await.unwrap();
    assert_eq!(tables(&e.db).await.unwrap(), before);
}

#[tokio::test]
async fn a_restore_marks_located_bodies_packed() {
    let e = Env::new();
    e.pack(Kind::Match, &["OC1_1", "OC1_2"], NOW).await;
    write_snapshot(&e.db, &e.objects, NOW).await.unwrap();
    // A database restored from before the pack was recorded.
    e.db.write(|c| {
        c.execute("UPDATE matches SET packed_at = NULL WHERE match_id = 'OC1_2'", [])?;
        Ok::<_, DbError>(())
    })
    .await
    .unwrap();
    e.empty_map(true).await;
    restore_index(&e.db, &e.objects).await.unwrap();
    let unpacked: i64 =
        e.db.read(|c| {
            Ok::<_, DbError>(c.query_row(
                "SELECT count(*) FROM matches WHERE packed_at IS NULL",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(unpacked, 0);
}

#[test]
fn a_pack_key_parses_back_to_its_kind_and_id() {
    let id = Ulid::from_parts(NOW as u64, 7);
    for kind in [Kind::Match, Kind::Timeline] {
        assert_eq!(pack::parse_key(&pack::key(kind, id)), Some((kind, id)));
    }
    let good = pack::key(Kind::Match, id);
    for bad in [
        "index/packs-2026-10-14.jsonl.zst".to_string(),
        good.replace(".pack", ".tmp"),
        good.replace("/2026/", "/2025/"),
        good.replace("matches", "players"),
        "packs/matches/2026/10/not-a-ulid.pack".to_string(),
    ] {
        assert_eq!(pack::parse_key(&bad), None, "{bad}");
    }
}

#[tokio::test]
async fn a_locations_snapshot_cut_to_an_empty_frame_is_passed_over() {
    let e = Env::new();
    three_packs(&e).await;
    write_snapshot(&e.db, &e.objects, NOW).await.unwrap();
    let before = tables(&e.db).await.unwrap();
    let s = write_snapshot(&e.db, &e.objects, NOW + DAY).await.unwrap();
    // Decodes and parses (no lines), but locates nothing in live packs.
    let empty = zstd::encode_all(&b""[..], 3).unwrap();
    std::fs::write(e.file(&locations_key(&s.date)), empty).unwrap();

    e.empty_map(true).await;
    let r = restore_index(&e.db, &e.objects).await.unwrap();
    assert_eq!(r.snapshot, Some(date_of(NOW)));
    assert!(r.skipped[0].1.contains("bytes located"), "{:?}", r.skipped);
    assert_eq!(tables(&e.db).await.unwrap(), before);
}
