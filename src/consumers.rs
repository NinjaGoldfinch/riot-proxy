//! Consumer keys: generation, hashing and the `consumers` table. The key format is
//! v1's (`rpx_` + 32 base64url chars). Only the sha256 is stored; the plaintext is
//! returned once to the caller that created it (v1 §12.1).

use base64::Engine;
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

use crate::db::{Db, DbError};

/// v1 `src/keys.ts`.
pub const KEY_PREFIX: &str = "rpx_";
/// v1 `DEFAULT_QUOTA_PER_MIN`, also the column default.
pub const DEFAULT_QUOTA_PER_MIN: u32 = 600;
/// Name v1's migrate step gave the seeded admin consumer.
pub const BOOTSTRAP_NAME: &str = "bootstrap-admin";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Read,
    Admin,
}

impl std::str::FromStr for Scope {
    type Err = ConsumerError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "read" => Ok(Self::Read),
            "admin" => Ok(Self::Admin),
            other => Err(ConsumerError::Invalid(format!(
                "unknown scope '{other}' (expected read or admin)"
            ))),
        }
    }
}

impl std::fmt::Display for Scope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Read => "read",
            Self::Admin => "admin",
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConsumerError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("{0}")]
    Invalid(String),
    #[error("a consumer named '{0}' already exists")]
    DuplicateName(String),
    #[error("no consumer with id or name '{0}'")]
    NotFound(String),
    #[error("could not generate a key: {0}")]
    Random(String),
    #[error("corrupt consumers row: {0}")]
    Corrupt(String),
}

impl From<rusqlite::Error> for ConsumerError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Db(DbError::Sqlite(e))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Consumer {
    pub id: String,
    pub name: String,
    pub scopes: Vec<Scope>,
    pub quota_per_min: u32,
    pub created_at: i64,
    pub revoked_at: Option<i64>,
}

/// A consumer plus the plaintext key, which exists only in this value.
#[derive(Debug)]
pub struct Created {
    pub consumer: Consumer,
    pub key: crate::config::Secret,
}

#[derive(Debug, Clone)]
pub struct NewConsumer {
    pub name: String,
    pub scopes: Vec<Scope>,
    pub quota_per_min: u32,
    /// Import this key instead of generating one (`BOOTSTRAP_ADMIN_KEY`).
    pub key: Option<String>,
}

/// `rpx_` + base64url(24 random bytes): 36 characters, v1's format.
pub fn generate_key() -> Result<String, ConsumerError> {
    let mut bytes = [0u8; 24];
    getrandom::fill(&mut bytes).map_err(|e| ConsumerError::Random(e.to_string()))?;
    Ok(format!(
        "{KEY_PREFIX}{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    ))
}

pub fn hash_key(key: &str) -> [u8; 32] {
    Sha256::digest(key.as_bytes()).into()
}

fn now_ms() -> i64 {
    jiff::Timestamp::now().as_millisecond()
}

fn validate(new: &NewConsumer) -> Result<(), ConsumerError> {
    if new.name.trim().is_empty() {
        return Err(ConsumerError::Invalid("name must not be empty".into()));
    }
    if new.scopes.is_empty() {
        return Err(ConsumerError::Invalid("at least one scope is required".into()));
    }
    if new.quota_per_min == 0 {
        return Err(ConsumerError::Invalid(
            "quota must be at least 1 request per minute".into(),
        ));
    }
    if let Some(key) = &new.key
        && key.len() < 8
    {
        return Err(ConsumerError::Invalid(
            "an imported key must be at least 8 characters".into(),
        ));
    }
    Ok(())
}

fn insert(conn: &Connection, new: &NewConsumer) -> Result<Created, ConsumerError> {
    validate(new)?;
    let key = match &new.key {
        Some(key) => key.clone(),
        None => generate_key()?,
    };
    let mut scopes = new.scopes.clone();
    scopes.sort();
    scopes.dedup();
    let consumer = Consumer {
        id: ulid::Ulid::generate().to_string(),
        name: new.name.trim().to_string(),
        scopes,
        quota_per_min: new.quota_per_min,
        created_at: now_ms(),
        revoked_at: None,
    };
    let scopes_json =
        serde_json::to_string(&consumer.scopes).map_err(|e| ConsumerError::Corrupt(e.to_string()))?;
    let res = conn.execute(
        "INSERT INTO consumers (id, name, key_sha256, scopes, quota_per_min, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![consumer.id, consumer.name, hash_key(&key).as_slice(), scopes_json, consumer.quota_per_min, consumer.created_at],
    );
    match res {
        Ok(_) => Ok(Created {
            consumer,
            key: crate::config::Secret::new(key),
        }),
        Err(rusqlite::Error::SqliteFailure(e, Some(msg)))
            if e.code == rusqlite::ErrorCode::ConstraintViolation && msg.contains("consumers.name") =>
        {
            Err(ConsumerError::DuplicateName(consumer.name))
        }
        Err(rusqlite::Error::SqliteFailure(e, Some(msg)))
            if e.code == rusqlite::ErrorCode::ConstraintViolation && msg.contains("consumers.key_sha256") =>
        {
            Err(ConsumerError::Invalid("that key is already registered".into()))
        }
        Err(e) => Err(e.into()),
    }
}

fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<(Consumer, String)> {
    Ok((
        Consumer {
            id: row.get("id")?,
            name: row.get("name")?,
            scopes: Vec::new(),
            quota_per_min: row.get("quota_per_min")?,
            created_at: row.get("created_at")?,
            revoked_at: row.get("revoked_at")?,
        },
        row.get("scopes")?,
    ))
}

fn with_scopes((mut consumer, scopes): (Consumer, String)) -> Result<Consumer, ConsumerError> {
    consumer.scopes =
        serde_json::from_str(&scopes).map_err(|e| ConsumerError::Corrupt(format!("{}: {e}", consumer.id)))?;
    Ok(consumer)
}

pub async fn create(db: &Db, new: NewConsumer) -> Result<Created, ConsumerError> {
    db.write(move |c| insert(c, &new)).await
}

pub async fn list(db: &Db) -> Result<Vec<Consumer>, ConsumerError> {
    db.read(|c| {
        let mut stmt = c.prepare(
            "SELECT id, name, scopes, quota_per_min, created_at, revoked_at FROM consumers ORDER BY created_at, id",
        )?;
        let rows = stmt.query_map([], from_row)?.collect::<Result<Vec<_>, _>>()?;
        rows.into_iter().map(with_scopes).collect()
    })
    .await
}

/// Soft delete by id or name. The hash stays, so the key can never be reissued
/// (v1 `disableConsumer`). Returns the revoked consumer; revoking twice is an error.
pub async fn revoke(db: &Db, id_or_name: String) -> Result<Consumer, ConsumerError> {
    db.write(move |c| {
        let tx = c.transaction()?;
        let found = tx
            .query_row(
                "SELECT id, name, scopes, quota_per_min, created_at, revoked_at FROM consumers WHERE id = ?1 OR name = ?1",
                [&id_or_name],
                from_row,
            )
            .optional()?;
        let mut consumer = with_scopes(found.ok_or_else(|| ConsumerError::NotFound(id_or_name.clone()))?)?;
        if consumer.revoked_at.is_some() {
            return Err(ConsumerError::Invalid(format!("'{}' is already revoked", consumer.name)));
        }
        let at = now_ms();
        tx.execute("UPDATE consumers SET revoked_at = ?1 WHERE id = ?2", params![at, consumer.id])?;
        tx.commit()?;
        consumer.revoked_at = Some(at);
        Ok(consumer)
    })
    .await
}

/// On first `serve`: when `consumers` is empty, create `bootstrap-admin` with
/// read+admin. Uses `import_key` (`BOOTSTRAP_ADMIN_KEY`) if given, otherwise
/// generates one. Returns `None` when any consumer already exists, so the key is
/// shown at most once. Check and insert share one write, so concurrent boots can't
/// both create one.
pub async fn bootstrap_admin(db: &Db, import_key: Option<String>) -> Result<Option<Created>, ConsumerError> {
    db.write(move |c| {
        let tx = c.transaction()?;
        let existing: i64 = tx.query_row("SELECT COUNT(*) FROM consumers", [], |r| r.get(0))?;
        if existing > 0 {
            return Ok(None);
        }
        let created = insert(
            &tx,
            &NewConsumer {
                name: BOOTSTRAP_NAME.into(),
                scopes: vec![Scope::Read, Scope::Admin],
                quota_per_min: DEFAULT_QUOTA_PER_MIN,
                key: import_key,
            },
        )?;
        tx.commit()?;
        Ok(Some(created))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.db"), 1).unwrap();
        (dir, db)
    }

    fn new(name: &str) -> NewConsumer {
        NewConsumer {
            name: name.into(),
            scopes: vec![Scope::Read],
            quota_per_min: 600,
            key: None,
        }
    }

    #[test]
    fn keys_have_v1_shape_and_are_unique() {
        let a = generate_key().unwrap();
        let b = generate_key().unwrap();
        assert_ne!(a, b);
        assert_eq!(a.len(), 4 + 32);
        assert!(a.starts_with("rpx_"));
        assert!(
            a[4..]
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'),
            "{a}"
        );
    }

    #[tokio::test]
    async fn create_stores_only_the_sha256() {
        let (_dir, db) = db();
        let created = create(&db, new("web")).await.unwrap();
        let key = created.key.expose().to_string();
        let stored: Vec<u8> = db
            .read(move |c| {
                Ok::<_, DbError>(c.query_row("SELECT key_sha256 FROM consumers", [], |r| r.get(0))?)
            })
            .await
            .unwrap();
        assert_eq!(stored, hash_key(&key).to_vec());
        assert_eq!(created.consumer.scopes, vec![Scope::Read]);
        assert_eq!(created.consumer.quota_per_min, 600);
    }

    #[tokio::test]
    async fn names_are_unique_and_input_is_validated() {
        let (_dir, db) = db();
        create(&db, new("web")).await.unwrap();
        assert!(matches!(create(&db, new("web")).await, Err(ConsumerError::DuplicateName(n)) if n == "web"));
        assert!(matches!(
            create(&db, new("  ")).await,
            Err(ConsumerError::Invalid(_))
        ));
        let no_scopes = NewConsumer {
            scopes: vec![],
            ..new("a")
        };
        assert!(matches!(
            create(&db, no_scopes).await,
            Err(ConsumerError::Invalid(_))
        ));
        let zero = NewConsumer {
            quota_per_min: 0,
            ..new("b")
        };
        assert!(matches!(create(&db, zero).await, Err(ConsumerError::Invalid(_))));
        assert!("owner".parse::<Scope>().is_err());
    }

    #[tokio::test]
    async fn list_and_revoke() {
        let (_dir, db) = db();
        let a = create(&db, new("a")).await.unwrap().consumer;
        let admin = NewConsumer {
            scopes: vec![Scope::Admin, Scope::Read, Scope::Admin],
            ..new("b")
        };
        create(&db, admin).await.unwrap();

        let all = list(&db).await.unwrap();
        assert_eq!(
            all.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert_eq!(
            all[1].scopes,
            vec![Scope::Read, Scope::Admin],
            "sorted and deduplicated"
        );

        let revoked = revoke(&db, "b".into()).await.unwrap();
        assert!(revoked.revoked_at.is_some());
        let again = revoke(&db, a.id.clone()).await.unwrap();
        assert_eq!(again.id, a.id, "by id works too");
        assert!(matches!(
            revoke(&db, "b".into()).await,
            Err(ConsumerError::Invalid(_))
        ));
        assert!(matches!(
            revoke(&db, "zzz".into()).await,
            Err(ConsumerError::NotFound(_))
        ));
        assert!(
            list(&db).await.unwrap().iter().all(|c| c.revoked_at.is_some()),
            "rows are kept"
        );
    }

    #[tokio::test]
    async fn bootstrap_happens_once() {
        let (_dir, db) = db();
        let first = bootstrap_admin(&db, None)
            .await
            .unwrap()
            .expect("created on empty table");
        assert_eq!(first.consumer.name, BOOTSTRAP_NAME);
        assert_eq!(first.consumer.scopes, vec![Scope::Read, Scope::Admin]);
        assert!(first.key.expose().starts_with("rpx_"));
        assert!(bootstrap_admin(&db, None).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn bootstrap_skips_when_any_consumer_exists_and_can_import_a_key() {
        let (_dir, db) = db();
        create(&db, new("web")).await.unwrap();
        assert!(bootstrap_admin(&db, None).await.unwrap().is_none());

        let (_dir2, db2) = super::tests::db();
        let created = bootstrap_admin(&db2, Some("rpx_operator_chosen_key".into()))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(created.key.expose(), "rpx_operator_chosen_key");
    }
}
