//! The `players` table (docs/design/04 §Schema): every player this key scope has
//! looked up or tracked. Rows are keyed by `(key_scope, puuid)` because PUUIDs
//! are encrypted per API key (v1 `db/players.ts`).

use rusqlite::OptionalExtension;

use crate::db::{Db, DbError};

/// What a caller knows about a player. `None` leaves a column as it is, so the
/// lookup path (which knows only a PUUID and a platform) never blanks the Riot
/// ID an admin track stored (v1 `upsertPlayer`).
#[derive(Debug, Clone, Default)]
pub struct Upsert<'a> {
    pub puuid: &'a str,
    pub platform: &'a str,
    pub game_name: Option<&'a str>,
    pub tag_line: Option<&'a str>,
    pub tracked: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Player {
    pub puuid: String,
    pub platform: String,
    pub game_name: Option<String>,
    pub tag_line: Option<String>,
    pub tracked: bool,
    pub last_seen_match_id: Option<String>,
    /// JSON `{done, cursor, limit}` (v1 #44); `None` until a backfill starts.
    pub backfill_state: Option<String>,
    pub updated_at: i64,
}

const COLUMNS: &str =
    "puuid, platform, game_name, tag_line, tracked, last_seen_match_id, backfill_state, updated_at";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Player> {
    Ok(Player {
        puuid: r.get(0)?,
        platform: r.get(1)?,
        game_name: r.get(2)?,
        tag_line: r.get(3)?,
        tracked: r.get(4)?,
        last_seen_match_id: r.get(5)?,
        backfill_state: r.get(6)?,
        updated_at: r.get(7)?,
    })
}

/// Insert or update a player and return the stored row.
pub async fn upsert(db: &Db, key_scope: &str, p: Upsert<'_>, now_ms: i64) -> Result<Player, DbError> {
    let (scope, puuid, platform) = (key_scope.to_string(), p.puuid.to_string(), p.platform.to_string());
    let (name, tag) = (p.game_name.map(str::to_string), p.tag_line.map(str::to_string));
    let tracked = p.tracked;
    db.write(move |c| {
        c.query_row(
            &format!(
                "INSERT INTO players (key_scope, puuid, platform, game_name, tag_line, tracked, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, coalesce(?6, 0), ?7)
                 ON CONFLICT (key_scope, puuid) DO UPDATE SET
                   platform = excluded.platform,
                   game_name = coalesce(?4, game_name),
                   tag_line = coalesce(?5, tag_line),
                   tracked = coalesce(?6, tracked),
                   updated_at = excluded.updated_at
                 RETURNING {COLUMNS}"
            ),
            rusqlite::params![scope, puuid, platform, name, tag, tracked, now_ms],
            row,
        )
        .map_err(DbError::from)
    })
    .await
}

pub async fn get(db: &Db, key_scope: &str, puuid: &str) -> Result<Option<Player>, DbError> {
    let (scope, puuid) = (key_scope.to_string(), puuid.to_string());
    db.read(move |c| {
        c.query_row(
            &format!("SELECT {COLUMNS} FROM players WHERE key_scope = ?1 AND puuid = ?2"),
            (scope, puuid),
            row,
        )
        .optional()
        .map_err(DbError::from)
    })
    .await
}

/// Every player row for this key scope, tracked or not (v1 `listPlayers`),
/// most recently updated first.
pub async fn list(db: &Db, key_scope: &str) -> Result<Vec<Player>, DbError> {
    let scope = key_scope.to_string();
    db.read(move |c| {
        let mut stmt = c.prepare(&format!(
            "SELECT {COLUMNS} FROM players WHERE key_scope = ?1 ORDER BY updated_at DESC, puuid"
        ))?;
        let rows = stmt.query_map([scope], row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(DbError::from)
    })
    .await
}

/// The tracked players of this key scope (v1 `listTrackedPlayers`), what every
/// poll tick fans out over.
pub async fn list_tracked(db: &Db, key_scope: &str) -> Result<Vec<Player>, DbError> {
    let scope = key_scope.to_string();
    db.read(move |c| {
        let mut stmt = c.prepare(&format!(
            "SELECT {COLUMNS} FROM players WHERE key_scope = ?1 AND tracked = 1 ORDER BY puuid"
        ))?;
        let rows = stmt.query_map([scope], row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(DbError::from)
    })
    .await
}

/// Set `tracked` on an existing row; `false` if there is no such player.
pub async fn set_tracked(
    db: &Db,
    key_scope: &str,
    puuid: &str,
    tracked: bool,
    now_ms: i64,
) -> Result<bool, DbError> {
    let (scope, puuid) = (key_scope.to_string(), puuid.to_string());
    db.write(move |c| {
        let n = c.execute(
            "UPDATE players SET tracked = ?1, updated_at = ?2 WHERE key_scope = ?3 AND puuid = ?4",
            rusqlite::params![tracked, now_ms, scope, puuid],
        )?;
        Ok::<_, DbError>(n > 0)
    })
    .await
}

/// `(known, tracked)` players for this key scope.
pub async fn counts(db: &Db, key_scope: &str) -> Result<(i64, i64), DbError> {
    let scope = key_scope.to_string();
    db.read(move |c| {
        c.query_row(
            "SELECT count(*), coalesce(sum(tracked), 0) FROM players WHERE key_scope = ?1",
            [scope],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(DbError::from)
    })
    .await
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn db() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("riot-proxy.db"), 2).unwrap();
        (dir, db)
    }

    fn at<'a>(puuid: &'a str, platform: &'a str) -> Upsert<'a> {
        Upsert {
            puuid,
            platform,
            ..Upsert::default()
        }
    }

    #[tokio::test]
    async fn a_lookup_never_blanks_what_a_track_stored() {
        let (_d, db) = db();
        let first = Upsert {
            game_name: Some("Hide on bush"),
            tag_line: Some("KR1"),
            tracked: Some(true),
            ..at("p1", "kr")
        };
        upsert(&db, "s1", first, 1).await.unwrap();
        let again = upsert(&db, "s1", at("p1", "kr"), 2).await.unwrap();
        assert_eq!(
            (
                again.game_name.as_deref(),
                again.tag_line.as_deref(),
                again.tracked,
                again.updated_at
            ),
            (Some("Hide on bush"), Some("KR1"), true, 2)
        );
    }

    #[tokio::test]
    async fn rows_are_scoped_by_key() {
        let (_d, db) = db();
        upsert(&db, "s1", at("p1", "kr"), 1).await.unwrap();
        assert!(get(&db, "s1", "p1").await.unwrap().is_some());
        assert!(get(&db, "s2", "p1").await.unwrap().is_none());
        let fresh = upsert(&db, "s2", at("p1", "euw1"), 1).await.unwrap();
        assert_eq!((fresh.platform.as_str(), fresh.tracked), ("euw1", false));
    }

    #[tokio::test]
    async fn list_untrack_and_count() {
        let (_d, db) = db();
        upsert(
            &db,
            "s1",
            Upsert {
                tracked: Some(true),
                ..at("p1", "kr")
            },
            1,
        )
        .await
        .unwrap();
        upsert(&db, "s1", at("p2", "kr"), 2).await.unwrap();
        upsert(&db, "s2", at("p3", "kr"), 3).await.unwrap();
        let names: Vec<_> = list(&db, "s1")
            .await
            .unwrap()
            .into_iter()
            .map(|p| p.puuid)
            .collect();
        assert_eq!(names, ["p2", "p1"], "own scope, newest first");
        assert_eq!(counts(&db, "s1").await.unwrap(), (2, 1));
        assert!(set_tracked(&db, "s1", "p1", false, 4).await.unwrap());
        assert!(
            !set_tracked(&db, "s1", "p3", false, 4).await.unwrap(),
            "another scope's row"
        );
        assert_eq!(counts(&db, "s1").await.unwrap(), (2, 0));
    }

    #[tokio::test]
    async fn the_platform_follows_the_latest_lookup() {
        let (_d, db) = db();
        upsert(&db, "s1", at("p1", "kr"), 1).await.unwrap();
        assert_eq!(
            upsert(&db, "s1", at("p1", "euw1"), 2).await.unwrap().platform,
            "euw1"
        );
    }
}
