//! The object store that holds packed match and timeline bodies (design/12,
//! OBJ-01): a thin wrapper over `object_store` with the six calls the pack
//! writer, the reads and the offload job need.
//!
//! - **S3** (`ARCHIVE_OBJECT_URL=s3://<bucket>`): Onidel in production, R2 as
//!   the fallback. Signed with the configured keys; TLS trusts the same
//!   bundled roots as the Riot client, never the host's store.
//! - **Local directory** (`file:///<dir>`): development and the offload tests.
//!   It cannot store metadata, so `put` drops it and `head` returns none.
//! - **In memory**: unit tests.
//!
//! Errors carry `object_store`'s message, which names the key and the HTTP
//! status but never a credential.

use std::collections::BTreeMap;
use std::fmt;
use std::ops::Range;
use std::sync::Arc;

use bytes::Bytes;
use futures_util::TryStreamExt;
use object_store::aws::AmazonS3Builder;
use object_store::local::LocalFileSystem;
use object_store::memory::InMemory;
use object_store::path::Path;
use object_store::{
    Attribute, AttributeValue, Attributes, Certificate, ClientOptions, GetOptions, ObjectMeta, ObjectStore,
    ObjectStoreExt, PutMultipartOptions, PutOptions, PutPayload, WriteMultipart,
};

use crate::config::{Config, ObjectUrl};

/// A body above this is uploaded in parts (design/12 §Offload job).
pub const MULTIPART_THRESHOLD: usize = 8 * 1024 * 1024;
/// Every part but the last is exactly this size: R2 needs equal parts and S3
/// at least 5 MiB.
const PART_BYTES: usize = MULTIPART_THRESHOLD;
/// `ARCHIVE_OBJECT_CONCURRENCY`'s default.
pub const DEFAULT_CONCURRENCY: usize = 16;
/// Parts of one upload in flight at once.
const PART_CONCURRENCY: usize = 4;
const CONTENT_TYPE: &str = "application/octet-stream";

#[derive(Debug, thiserror::Error)]
pub enum ObjectsError {
    #[error("object store: {0}")]
    Store(#[from] object_store::Error),
    #[error("object store: '{0}' is not a valid object key")]
    Key(String),
    #[error("object store: ARCHIVE_OBJECT_URL is not set")]
    NotConfigured,
    #[error("object store: '{key}' returned {got} bytes for a {wanted}-byte range")]
    ShortRead { key: String, wanted: u64, got: u64 },
}

/// What `head` and `list` report about one object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectInfo {
    pub key: String,
    pub size: u64,
    pub last_modified_ms: i64,
    /// User metadata (`x-amz-meta-*` on S3), names without the prefix. Empty
    /// from `list`, and always from a local directory.
    pub metadata: BTreeMap<String, String>,
}

impl ObjectInfo {
    fn new(meta: ObjectMeta, metadata: BTreeMap<String, String>) -> Self {
        Self {
            key: meta.location.to_string(),
            size: meta.size,
            last_modified_ms: meta.last_modified.timestamp_millis(),
            metadata,
        }
    }
}

/// A handle on the bucket. Cheap to clone.
#[derive(Clone)]
pub struct Objects {
    store: Arc<dyn ObjectStore>,
    /// Whether the back end stores user metadata (a local directory doesn't).
    metadata: bool,
    /// For logs: the URL and endpoint, never a key.
    describe: String,
    /// Packs read at once by a batch read (`ARCHIVE_OBJECT_CONCURRENCY`).
    concurrency: usize,
}

impl fmt::Debug for Objects {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Objects").field("store", &self.describe).finish()
    }
}

impl Objects {
    /// The store `ARCHIVE_OBJECT_*` names. Builds a client but makes no
    /// request.
    pub fn from_config(config: &Config) -> Result<Self, ObjectsError> {
        match config.archive_object_url.as_ref() {
            None => Err(ObjectsError::NotConfigured),
            Some(ObjectUrl::File(dir)) => {
                std::fs::create_dir_all(dir).map_err(|e| object_store::Error::Generic {
                    store: "LocalFileSystem",
                    source: Box::new(e),
                })?;
                Ok(Self::from_store(
                    Arc::new(LocalFileSystem::new_with_prefix(dir)?),
                    false,
                    &format!("file://{}", dir.display()),
                )
                .with_concurrency(config.archive_object_concurrency as usize))
            }
            Some(ObjectUrl::S3 { bucket }) => {
                let endpoint = config.archive_object_endpoint.as_deref().unwrap_or_default();
                let mut builder = AmazonS3Builder::new()
                    .with_bucket_name(bucket)
                    .with_region(&config.archive_object_region)
                    .with_endpoint(endpoint)
                    .with_virtual_hosted_style_request(virtual_hosted(endpoint, bucket))
                    .with_allow_http(endpoint.starts_with("http://"))
                    .with_client_options(client_options());
                if let (Some(access), Some(secret)) = (
                    &config.archive_object_access_key,
                    &config.archive_object_secret_key,
                ) {
                    builder = builder
                        .with_access_key_id(access.expose())
                        .with_secret_access_key(secret.expose());
                }
                Ok(Self::from_store(
                    Arc::new(builder.build()?),
                    true,
                    &format!("s3://{bucket} at {endpoint}"),
                )
                .with_concurrency(config.archive_object_concurrency as usize))
            }
        }
    }

    /// An empty store in memory, for tests.
    pub fn in_memory() -> Self {
        Self::from_store(Arc::new(InMemory::new()), true, "memory://")
    }

    /// Any `object_store` back end; `metadata` says whether it keeps user
    /// metadata.
    pub fn from_store(store: Arc<dyn ObjectStore>, metadata: bool, describe: &str) -> Self {
        Self {
            store,
            metadata,
            describe: describe.to_string(),
            concurrency: DEFAULT_CONCURRENCY,
        }
    }

    /// How many packs a batch read fetches at once; at least 1.
    #[must_use]
    pub fn with_concurrency(mut self, concurrency: usize) -> Self {
        self.concurrency = concurrency.max(1);
        self
    }

    pub fn concurrency(&self) -> usize {
        self.concurrency
    }

    /// The URL and endpoint, for the boot log. Never a key.
    pub fn describe(&self) -> &str {
        &self.describe
    }

    /// Store `body` at `key` with user metadata, replacing any object there.
    /// Above [`MULTIPART_THRESHOLD`] it goes up in parts; a failed part
    /// aborts the upload, so nothing is left half-written.
    pub async fn put(&self, key: &str, body: Bytes, metadata: &[(&str, &str)]) -> Result<(), ObjectsError> {
        let path = path(key)?;
        let attributes = self.attributes(metadata);
        if body.len() <= MULTIPART_THRESHOLD {
            let opts = PutOptions {
                attributes,
                ..PutOptions::default()
            };
            self.store.put_opts(&path, PutPayload::from(body), opts).await?;
            return Ok(());
        }
        let opts = PutMultipartOptions {
            attributes,
            ..PutMultipartOptions::default()
        };
        let upload = self.store.put_multipart_opts(&path, opts).await?;
        let mut writer = WriteMultipart::new_with_chunk_size(upload, PART_BYTES);
        let mut rest = body;
        while !rest.is_empty() {
            if let Err(e) = writer.wait_for_capacity(PART_CONCURRENCY).await {
                // Best effort: the upload is already failing.
                let _ = writer.abort().await;
                return Err(e.into());
            }
            let take = rest.len().min(PART_BYTES);
            writer.put(rest.split_to(take));
        }
        writer.finish().await?;
        Ok(())
    }

    /// Size, time and user metadata of `key`; `None` when there is no such
    /// object.
    pub async fn head(&self, key: &str) -> Result<Option<ObjectInfo>, ObjectsError> {
        let path = path(key)?;
        let opts = GetOptions {
            head: true,
            ..GetOptions::default()
        };
        match self.store.get_opts(&path, opts).await {
            Ok(result) => {
                let metadata = result
                    .attributes
                    .iter()
                    .filter_map(|(k, v)| match k {
                        Attribute::Metadata(name) => Some((name.to_string(), v.as_ref().to_string())),
                        _ => None,
                    })
                    .collect();
                Ok(Some(ObjectInfo::new(result.meta, metadata)))
            }
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// The bytes of `key` in `range` (one ranged GET). A range that runs past
    /// the end of the object is an error, never a short read.
    pub async fn get_range(&self, key: &str, range: Range<u64>) -> Result<Bytes, ObjectsError> {
        let bytes = self.store.get_range(&path(key)?, range.clone()).await?;
        whole(key, &range, bytes)
    }

    /// Several ranges of `key`, in the order asked. Ranges close together are
    /// merged into one request by `object_store`. Short reads are errors, as
    /// in [`Self::get_range`].
    pub async fn get_ranges(&self, key: &str, ranges: &[Range<u64>]) -> Result<Vec<Bytes>, ObjectsError> {
        let got = self.store.get_ranges(&path(key)?, ranges).await?;
        ranges
            .iter()
            .zip(got)
            .map(|(range, bytes)| whole(key, range, bytes))
            .collect()
    }

    /// Every object under `prefix` (a key prefix ending at a `/`, or empty
    /// for the whole bucket), sorted by key. User metadata is not listed.
    pub async fn list(&self, prefix: &str) -> Result<Vec<ObjectInfo>, ObjectsError> {
        let prefix = prefix.trim_end_matches('/');
        let prefix = if prefix.is_empty() {
            None
        } else {
            Some(path(prefix)?)
        };
        let mut out: Vec<ObjectInfo> = self
            .store
            .list(prefix.as_ref())
            .map_ok(|meta| ObjectInfo::new(meta, BTreeMap::new()))
            .try_collect()
            .await?;
        out.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(out)
    }

    /// Remove `key`. Removing a key that isn't there succeeds.
    pub async fn delete(&self, key: &str) -> Result<(), ObjectsError> {
        match self.store.delete(&path(key)?).await {
            Ok(()) | Err(object_store::Error::NotFound { .. }) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    fn attributes(&self, metadata: &[(&str, &str)]) -> Attributes {
        let mut attributes = Attributes::new();
        if !self.metadata {
            return attributes;
        }
        attributes.insert(Attribute::ContentType, AttributeValue::from(CONTENT_TYPE));
        for (name, value) in metadata {
            attributes.insert(
                Attribute::Metadata((*name).to_string().into()),
                AttributeValue::from((*value).to_string()),
            );
        }
        attributes
    }
}

/// `bytes` if it is all of `range`. HTTP clamps a range past the end of an
/// object to what exists; a pack body cut short must not pass as whole.
fn whole(key: &str, range: &Range<u64>, bytes: Bytes) -> Result<Bytes, ObjectsError> {
    let wanted = range.end.saturating_sub(range.start);
    let got = bytes.len() as u64;
    if got == wanted {
        Ok(bytes)
    } else {
        Err(ObjectsError::ShortRead {
            key: key.to_string(),
            wanted,
            got,
        })
    }
}

/// A key as `object_store` wants it: `/`-separated, no empty, `.` or `..`
/// segment, no leading or trailing `/`.
fn path(key: &str) -> Result<Path, ObjectsError> {
    Path::parse(key)
        .ok()
        .filter(|p| !key.is_empty() && p.as_ref() == key)
        .ok_or_else(|| ObjectsError::Key(key.to_string()))
}

/// Onidel documents virtual-hosted addressing (`<bucket>.s3.<region>.onidel.cloud`,
/// design/12). An endpoint whose host already starts with the bucket is used
/// as it is; any other endpoint is addressed path style (`<endpoint>/<bucket>`).
fn virtual_hosted(endpoint: &str, bucket: &str) -> bool {
    let host = endpoint.split_once("://").map_or(endpoint, |(_, rest)| rest);
    host.strip_prefix(bucket)
        .is_some_and(|rest| rest.starts_with('.'))
}

/// The bundled Mozilla roots only, as `riot::client` does: a static musl
/// binary must not depend on the host's certificate store.
fn client_options() -> ClientOptions {
    webpki_root_certs::TLS_SERVER_ROOT_CERTS
        .iter()
        .filter_map(|der| Certificate::from_der(der).ok())
        .fold(
            ClientOptions::new().with_no_system_certificates(true),
            ClientOptions::with_root_certificate,
        )
}

#[cfg(test)]
mod tests;
