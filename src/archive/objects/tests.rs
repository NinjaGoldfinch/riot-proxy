use std::ops::Range;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::stream::BoxStream;
use object_store::memory::InMemory;
use object_store::path::Path;
use object_store::{
    CopyOptions, GetOptions, GetResult, ListResult, MultipartUpload, ObjectMeta, ObjectStore,
    PutMultipartOptions, PutOptions, PutPayload, PutResult, RenameOptions, UploadPart,
};

use super::*;
use crate::config::Sources;

/// What [`Recording`] saw.
#[derive(Debug, Default)]
struct Calls {
    single_puts: usize,
    multipart_puts: usize,
    /// Size of every part handed to a multipart upload, in order.
    parts: Vec<usize>,
    aborts: usize,
}

/// An in-memory store that records how objects were written, and can fail a
/// multipart upload's `n`th part.
#[derive(Debug)]
struct Recording {
    inner: Arc<InMemory>,
    calls: Arc<Mutex<Calls>>,
    fail_part: Option<usize>,
}

impl Recording {
    fn objects(fail_part: Option<usize>) -> (Objects, Arc<Mutex<Calls>>) {
        let calls = Arc::new(Mutex::new(Calls::default()));
        let store = Self {
            inner: Arc::new(InMemory::new()),
            calls: calls.clone(),
            fail_part,
        };
        (Objects::from_store(Arc::new(store), true, "recording"), calls)
    }
}

impl std::fmt::Display for Recording {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Recording")
    }
}

#[derive(Debug)]
struct RecordingUpload {
    inner: Box<dyn MultipartUpload>,
    calls: Arc<Mutex<Calls>>,
    fail_part: Option<usize>,
}

fn generic(message: &str) -> object_store::Error {
    object_store::Error::Generic {
        store: "Recording",
        source: message.to_string().into(),
    }
}

#[async_trait]
impl MultipartUpload for RecordingUpload {
    fn put_part(&mut self, data: PutPayload) -> UploadPart {
        let n = {
            let mut calls = self.calls.lock().unwrap_or_else(|e| e.into_inner());
            calls.parts.push(data.content_length());
            calls.parts.len()
        };
        if self.fail_part == Some(n) {
            return Box::pin(async { Err(generic("part refused")) });
        }
        self.inner.put_part(data)
    }

    async fn complete(&mut self) -> object_store::Result<PutResult> {
        self.inner.complete().await
    }

    async fn abort(&mut self) -> object_store::Result<()> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).aborts += 1;
        self.inner.abort().await
    }
}

#[async_trait]
impl ObjectStore for Recording {
    async fn put_opts(
        &self,
        location: &Path,
        payload: PutPayload,
        opts: PutOptions,
    ) -> object_store::Result<PutResult> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).single_puts += 1;
        self.inner.put_opts(location, payload, opts).await
    }

    async fn put_multipart_opts(
        &self,
        location: &Path,
        opts: PutMultipartOptions,
    ) -> object_store::Result<Box<dyn MultipartUpload>> {
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .multipart_puts += 1;
        Ok(Box::new(RecordingUpload {
            inner: self.inner.put_multipart_opts(location, opts).await?,
            calls: self.calls.clone(),
            fail_part: self.fail_part,
        }))
    }

    async fn get_opts(&self, location: &Path, options: GetOptions) -> object_store::Result<GetResult> {
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

/// `len` bytes that differ at every offset within a part, so a misplaced
/// range or part shows.
fn body(len: usize) -> Bytes {
    (0..len).map(|i| (i % 251) as u8).collect::<Vec<u8>>().into()
}

const KEY: &str = "packs/matches/2026/10/01JABCDEFGHJKMNPQRSTVWXYZ0.pack";

#[tokio::test]
async fn put_then_ranged_get_round_trips() {
    let objects = Objects::in_memory();
    let data = body(10_000);
    objects.put(KEY, data.clone(), &[]).await.unwrap();
    assert_eq!(objects.get_range(KEY, 0..10_000).await.unwrap(), data);
    assert_eq!(
        objects.get_range(KEY, 1234..1300).await.unwrap(),
        data.slice(1234..1300)
    );
    // A range past the end is an error, never a short read.
    let err = objects.get_range(KEY, 9_990..10_010).await.unwrap_err();
    assert!(
        matches!(
            err,
            ObjectsError::ShortRead {
                wanted: 20,
                got: 10,
                ..
            }
        ),
        "{err}"
    );
    let err = objects
        .get_ranges(KEY, &[0..10, 9_995..10_001])
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            ObjectsError::ShortRead {
                wanted: 6,
                got: 5,
                ..
            }
        ),
        "{err}"
    );
    assert!(objects.get_range("packs/none.pack", 0..1).await.is_err());
}

#[tokio::test]
async fn several_ranges_come_back_in_one_call_in_the_order_asked() {
    let objects = Objects::in_memory();
    let data = body(100_000);
    objects.put(KEY, data.clone(), &[]).await.unwrap();
    let ranges: Vec<Range<u64>> = vec![50_000..50_100, 10..20, 99_000..100_000, 10..20];
    let got = objects.get_ranges(KEY, &ranges).await.unwrap();
    assert_eq!(got.len(), ranges.len());
    for (bytes, r) in got.iter().zip(&ranges) {
        assert_eq!(*bytes, data.slice(r.start as usize..r.end as usize));
    }
}

#[tokio::test]
async fn head_gives_size_and_metadata_and_none_when_missing() {
    let objects = Objects::in_memory();
    let before = jiff::Timestamp::now().as_millisecond();
    objects
        .put(KEY, body(4321), &[("sha256", "ab12"), ("bodies", "7")])
        .await
        .unwrap();
    let info = objects.head(KEY).await.unwrap().unwrap();
    assert_eq!(info.key, KEY);
    assert_eq!(info.size, 4321);
    assert!(info.last_modified_ms >= before - 1000, "{info:?}");
    assert_eq!(
        info.metadata,
        BTreeMap::from([
            ("bodies".to_string(), "7".to_string()),
            ("sha256".to_string(), "ab12".to_string())
        ]),
        "user metadata only; the content type is not in it"
    );
    assert_eq!(objects.head("packs/matches/none.pack").await.unwrap(), None);
}

#[tokio::test]
async fn up_to_the_threshold_is_one_put() {
    let (objects, calls) = Recording::objects(None);
    let data = body(MULTIPART_THRESHOLD);
    objects.put(KEY, data.clone(), &[("sha256", "x")]).await.unwrap();
    {
        let c = calls.lock().unwrap();
        assert_eq!((c.single_puts, c.multipart_puts), (1, 0));
    }
    assert_eq!(objects.get_range(KEY, 0..data.len() as u64).await.unwrap(), data);
}

#[tokio::test]
async fn above_the_threshold_goes_up_in_equal_parts_with_its_metadata() {
    let (objects, calls) = Recording::objects(None);
    let len = 2 * MULTIPART_THRESHOLD + 12_345;
    let data = body(len);
    objects
        .put(KEY, data.clone(), &[("sha256", "cafe"), ("bodies", "3")])
        .await
        .unwrap();
    {
        let c = calls.lock().unwrap();
        assert_eq!((c.single_puts, c.multipart_puts, c.aborts), (0, 1, 0));
        assert_eq!(c.parts, vec![MULTIPART_THRESHOLD, MULTIPART_THRESHOLD, 12_345]);
    }
    assert_eq!(objects.get_range(KEY, 0..len as u64).await.unwrap(), data);
    let info = objects.head(KEY).await.unwrap().unwrap();
    assert_eq!(info.size, len as u64);
    assert_eq!(info.metadata["sha256"], "cafe");
    assert_eq!(info.metadata["bodies"], "3");
}

#[tokio::test]
async fn a_failed_part_aborts_the_upload_and_leaves_no_object() {
    let (objects, calls) = Recording::objects(Some(2));
    let err = objects
        .put(KEY, body(3 * MULTIPART_THRESHOLD), &[])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("part refused"), "{err}");
    assert_eq!(calls.lock().unwrap().aborts, 1);
    assert_eq!(objects.head(KEY).await.unwrap(), None);
}

#[tokio::test]
async fn list_walks_a_prefix_in_key_order() {
    let objects = Objects::in_memory();
    for key in [
        "packs/timelines/2026/10/B.pack",
        "packs/matches/2026/11/A.pack",
        "packs/matches/2026/10/C.pack",
        "index/locations-2026-10-13.jsonl.zst",
    ] {
        objects.put(key, body(10), &[("sha256", "x")]).await.unwrap();
    }
    let keys = |list: Vec<ObjectInfo>| list.into_iter().map(|o| o.key).collect::<Vec<_>>();
    assert_eq!(
        keys(objects.list("packs/matches/").await.unwrap()),
        ["packs/matches/2026/10/C.pack", "packs/matches/2026/11/A.pack"]
    );
    assert_eq!(
        keys(objects.list("packs/matches").await.unwrap()),
        ["packs/matches/2026/10/C.pack", "packs/matches/2026/11/A.pack"],
        "with or without the trailing slash"
    );
    assert_eq!(objects.list("").await.unwrap().len(), 4);
    assert!(objects.list("packs/none/").await.unwrap().is_empty());
    let listed = objects.list("index/").await.unwrap();
    assert_eq!(listed[0].size, 10);
    assert!(listed[0].metadata.is_empty(), "list does not fetch metadata");
}

#[tokio::test]
async fn delete_removes_and_a_missing_key_is_fine() {
    let objects = Objects::in_memory();
    objects.put(KEY, body(10), &[]).await.unwrap();
    objects.delete(KEY).await.unwrap();
    assert_eq!(objects.head(KEY).await.unwrap(), None);
    objects.delete(KEY).await.unwrap();
}

#[tokio::test]
async fn malformed_keys_are_refused_before_any_request() {
    let objects = Objects::in_memory();
    for key in [
        "",
        "/packs/a.pack",
        "packs//a.pack",
        "packs/",
        "packs/../a.pack",
        "packs/./a",
    ] {
        let err = objects.put(key, body(1), &[]).await.unwrap_err();
        assert!(matches!(err, ObjectsError::Key(_)), "{key:?}: {err}");
        assert!(
            matches!(objects.head(key).await, Err(ObjectsError::Key(_))),
            "{key:?}"
        );
    }
}

fn config(vars: &[(&str, &str)]) -> Config {
    let mut env = vec![(
        "RIOT_API_KEY".to_string(),
        "RGAPI-test-key-for-config".to_string(),
    )];
    env.extend(vars.iter().map(|(k, v)| (k.to_string(), v.to_string())));
    Config::from_sources(Sources {
        env,
        ..Sources::default()
    })
    .unwrap()
}

#[tokio::test]
async fn a_local_directory_round_trips_and_drops_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("bucket");
    let url = format!("file://{}", root.display());
    let objects = Objects::from_config(&config(&[("ARCHIVE_OBJECT_URL", &url)])).unwrap();
    assert_eq!(objects.describe(), url);
    let data = body(MULTIPART_THRESHOLD + 10);
    objects.put(KEY, data.clone(), &[("sha256", "x")]).await.unwrap();
    assert!(
        root.join(KEY).is_file(),
        "written under the directory, which was created"
    );
    let info = objects.head(KEY).await.unwrap().unwrap();
    assert_eq!(info.size, data.len() as u64);
    assert!(info.metadata.is_empty());
    assert_eq!(
        objects.get_ranges(KEY, &[5..9, 0..2]).await.unwrap(),
        [data.slice(5..9), data.slice(0..2)]
    );
    assert_eq!(objects.list("packs/").await.unwrap().len(), 1);
    objects.delete(KEY).await.unwrap();
    assert_eq!(objects.head(KEY).await.unwrap(), None);
}

#[test]
fn an_s3_store_builds_without_a_request_and_never_shows_its_keys() {
    let c = config(&[
        ("ARCHIVE_OBJECT_URL", "s3://riot-proxy-archive"),
        (
            "ARCHIVE_OBJECT_ENDPOINT",
            "https://s3.ap-southeast-2.onidel.cloud",
        ),
        ("ARCHIVE_OBJECT_ACCESS_KEY", "AKIDEXAMPLEACCESS"),
        ("ARCHIVE_OBJECT_SECRET_KEY", "very-secret-value"),
    ]);
    let objects = Objects::from_config(&c).unwrap();
    assert_eq!(
        objects.describe(),
        "s3://riot-proxy-archive at https://s3.ap-southeast-2.onidel.cloud"
    );
    let shown = format!("{objects:?} {c:?}");
    assert!(
        !shown.contains("AKIDEXAMPLE") && !shown.contains("very-secret"),
        "{shown}"
    );
    assert!(shown.contains("[REDACTED]"));
}

#[test]
fn no_url_is_not_configured() {
    assert!(matches!(
        Objects::from_config(&config(&[])),
        Err(ObjectsError::NotConfigured)
    ));
}

#[test]
fn an_endpoint_that_starts_with_the_bucket_is_virtual_hosted() {
    let b = "riot-proxy-archive";
    assert!(virtual_hosted(
        "https://riot-proxy-archive.s3.ap-southeast-2.onidel.cloud",
        b
    ));
    assert!(!virtual_hosted("https://s3.ap-southeast-2.onidel.cloud", b));
    assert!(!virtual_hosted("https://riot-proxy-archive-2.example.com", b));
    assert!(!virtual_hosted("https://abc123.r2.cloudflarestorage.com", b));
    assert!(virtual_hosted("http://riot-proxy-archive.localhost:9000", b));
}
