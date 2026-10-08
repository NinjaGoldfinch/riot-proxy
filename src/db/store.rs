//! The engine seam (design/04 §Postgres compatibility, plan P8-04). Queries are
//! plain SQL both engines accept; what differs goes through [`Store`]. Today
//! that is the schema migration and the job claim, which on Postgres needs
//! `FOR UPDATE SKIP LOCKED` (design/06 §Claiming).
//!
//! Only SQLite is implemented. [`PgStore`] exists behind the `postgres`
//! feature so the trait keeps compiling against a second engine; every call
//! fails, and the config still refuses `postgres://` URLs (ADR-062).

use futures_util::future::BoxFuture;
use rusqlite::OptionalExtension;

use super::{Db, DbError};
use crate::jobs::scheduler::{self, Job};

/// Which database engine a [`Store`] talks to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    Sqlite,
    Postgres,
}

/// The engine-specific operations; everything else is shared SQL.
pub trait Store: Send + Sync + 'static {
    fn engine(&self) -> Engine;

    /// Apply pending migrations. Idempotent.
    fn migrate(&self) -> BoxFuture<'_, Result<(), DbError>>;

    /// Atomically move the highest-priority ready job to `running` and return it.
    fn claim_job(&self, now_ms: i64) -> BoxFuture<'_, Result<Option<Job>, DbError>>;
}

/// The job claim (design/06 §Claiming). `$1` binds the same value at every
/// use on both engines; Postgres also locks the chosen row and skips rows
/// another worker holds.
pub fn claim_sql(engine: Engine) -> String {
    let lock = match engine {
        Engine::Sqlite => "",
        Engine::Postgres => "\n                                FOR UPDATE SKIP LOCKED",
    };
    format!(
        "UPDATE jobs SET state = 'running', claimed_at = $1, attempts = attempts + 1
          WHERE id = (SELECT id FROM jobs
                       WHERE state = 'pending' AND run_after <= $1
                       ORDER BY {order}
                       LIMIT 1{lock})
          RETURNING {}",
        scheduler::COLUMNS,
        order = scheduler::CLAIM_ORDER,
    )
}

/// SQLite through [`Db`]'s single writer.
#[derive(Debug, Clone)]
pub struct SqliteStore {
    db: Db,
}

impl SqliteStore {
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

impl Store for SqliteStore {
    fn engine(&self) -> Engine {
        Engine::Sqlite
    }

    fn migrate(&self) -> BoxFuture<'_, Result<(), DbError>> {
        Box::pin(self.db.write(|c| super::migrate(c).map(drop)))
    }

    fn claim_job(&self, now_ms: i64) -> BoxFuture<'_, Result<Option<Job>, DbError>> {
        Box::pin(self.db.write(move |c| {
            c.query_row(&claim_sql(Engine::Sqlite), [now_ms], scheduler::row)
                .optional()
                .map_err(DbError::from)
        }))
    }
}

/// Postgres: a stub so the [`Store`] seam keeps a second implementation.
/// Every call fails with [`DbError::PostgresUnimplemented`].
#[cfg(feature = "postgres")]
#[derive(Debug)]
pub struct PgStore {
    url: crate::config::Secret,
}

#[cfg(feature = "postgres")]
impl PgStore {
    pub fn new(url: crate::config::Secret) -> Self {
        Self { url }
    }

    fn unimplemented<T: Send + 'static>(&self) -> BoxFuture<'_, Result<T, DbError>> {
        // The URL is held for the real implementation; it is never printed.
        let _ = &self.url;
        Box::pin(async { Err(DbError::PostgresUnimplemented) })
    }
}

#[cfg(feature = "postgres")]
impl Store for PgStore {
    fn engine(&self) -> Engine {
        Engine::Postgres
    }

    fn migrate(&self) -> BoxFuture<'_, Result<(), DbError>> {
        self.unimplemented()
    }

    fn claim_job(&self, _now_ms: i64) -> BoxFuture<'_, Result<Option<Job>, DbError>> {
        self.unimplemented()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_engines_differ_only_by_the_row_lock() {
        let sqlite = claim_sql(Engine::Sqlite);
        let postgres = claim_sql(Engine::Postgres);
        assert!(!sqlite.contains("SKIP LOCKED"));
        assert!(postgres.contains("LIMIT 1\n                                FOR UPDATE SKIP LOCKED)"));
        assert_eq!(
            postgres.replace("\n                                FOR UPDATE SKIP LOCKED", ""),
            sqlite
        );
    }

    #[tokio::test]
    async fn sqlite_claims_through_the_trait_and_migrates_idempotently() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_async(dir.path().join("t.db"), 1).await.unwrap();
        let store: Box<dyn Store> = Box::new(SqliteStore::new(db.clone()));
        assert_eq!(store.engine(), Engine::Sqlite);
        store.migrate().await.unwrap();
        assert!(store.claim_job(1_000).await.unwrap().is_none());
        db.write(|c| {
            c.execute(
                "INSERT INTO jobs (id, kind, priority, payload, run_after)
                 VALUES ('01A', 'k', 5, '{}', 500), ('01B', 'k', 1, '{}', 2000)",
                [],
            )
            .map_err(DbError::from)
        })
        .await
        .unwrap();
        // 01B outranks 01A but is not ready until 2 000.
        let job = store.claim_job(1_000).await.unwrap().unwrap();
        assert_eq!((job.id.as_str(), job.attempts), ("01A", 1));
        assert!(store.claim_job(1_000).await.unwrap().is_none());
        assert_eq!(store.claim_job(2_000).await.unwrap().unwrap().id, "01B");
    }

    #[cfg(feature = "postgres")]
    #[tokio::test]
    async fn postgres_is_a_stub_that_refuses_every_call() {
        let store: Box<dyn Store> = Box::new(PgStore::new(crate::config::Secret::new(
            "postgres://u:p@localhost/db",
        )));
        assert_eq!(store.engine(), Engine::Postgres);
        assert!(matches!(
            store.migrate().await,
            Err(DbError::PostgresUnimplemented)
        ));
        assert!(matches!(
            store.claim_job(0).await,
            Err(DbError::PostgresUnimplemented)
        ));
    }
}
