//! Ported from v1 `test/player-champions.test.ts` (`listPlayerChampions`).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

const ME: &str = "me";

fn db() -> (tempfile::TempDir, Db) {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("riot-proxy.db"), 2).unwrap();
    (dir, db)
}

/// One archived match, with facts rows `(puuid, champion, win, kda)`.
#[allow(clippy::type_complexity)]
async fn game(
    db: &Db,
    id: &str,
    queue: i64,
    patch: &str,
    end: i64,
    rows: &[(&str, i64, bool, Option<(i64, i64, i64)>)],
) {
    let (id, patch) = (id.to_string(), patch.to_string());
    let rows: Vec<_> = rows
        .iter()
        .map(|(p, c, w, k)| (p.to_string(), *c, *w, *k))
        .collect();
    db.write(move |c| {
        c.execute(
            "INSERT INTO matches VALUES (?1, 'europe', ?2, ?3, ?4, x'00', 1, 0)",
            rusqlite::params![id, patch, queue, end],
        )?;
        for (puuid, champ, win, kda) in rows {
            c.execute(
                "INSERT INTO match_facts (match_id, key_scope, puuid, team_id, champion_id, win, kills, deaths, assists, facts_version)
                 VALUES (?1, 's1', ?2, 100, ?3, ?4, ?5, ?6, ?7, 1)",
                rusqlite::params![id, puuid, champ, win, kda.map(|k| k.0), kda.map(|k| k.1), kda.map(|k| k.2)],
            )?;
        }
        Ok::<_, DbError>(())
    })
    .await
    .unwrap();
}

fn all() -> Filter {
    Filter {
        limit: 200,
        ..Filter::default()
    }
}

async fn ids(db: &Db, f: Filter) -> Vec<(i64, i64)> {
    champions(db, "s1", ME, f)
        .await
        .unwrap()
        .iter()
        .map(|r| (r.champion_id, r.games))
        .collect()
}

#[tokio::test]
async fn groups_one_players_games_by_champion_most_played_first() {
    let (_d, db) = db();
    game(
        &db,
        "EUW1_1",
        420,
        "14.18",
        1,
        &[(ME, 64, true, None), ("them", 1, false, None)],
    )
    .await;
    game(&db, "EUW1_2", 420, "14.18", 2, &[(ME, 11, true, None)]).await;
    game(
        &db,
        "EUW1_3",
        420,
        "14.18",
        3,
        &[(ME, 64, false, None), ("them", 64, true, None)],
    )
    .await;
    assert_eq!(
        ids(&db, all()).await,
        [(64, 2), (11, 1)],
        "only the player asked about is counted"
    );
    let rows = champions(&db, "s2", ME, all()).await.unwrap();
    assert!(rows.is_empty(), "another key scope sees nothing");
}

#[tokio::test]
async fn narrows_by_platform_prefix_queue_and_patch() {
    let (_d, db) = db();
    game(&db, "EUW1_1", 420, "14.18", 1, &[(ME, 1, true, None)]).await;
    game(&db, "EUN1_2", 420, "14.18", 2, &[(ME, 2, true, None)]).await;
    game(&db, "EUW1_3", 440, "14.18", 3, &[(ME, 3, true, None)]).await;
    game(&db, "EUW1_4", 420, "14.19", 4, &[(ME, 4, true, None)]).await;
    let f = |platform: Option<&str>, queue: Option<i64>, patch: Option<&str>| Filter {
        platform: platform.map(str::to_string),
        queue_id: queue,
        patch: patch.map(str::to_string),
        limit: 200,
    };
    assert_eq!(ids(&db, f(Some("eun1"), None, None)).await, [(2, 1)]);
    assert_eq!(
        ids(&db, f(Some("euw1"), Some(420), Some("14.18"))).await,
        [(1, 1)]
    );
    assert_eq!(ids(&db, f(None, Some(440), None)).await, [(3, 1)]);
    assert_eq!(ids(&db, f(None, None, Some("14.19"))).await, [(4, 1)]);
    assert_eq!(ids(&db, Filter { limit: 2, ..all() }).await.len(), 2);
}

#[tokio::test]
async fn sums_the_facts_and_counts_the_stated_rows_separately() {
    let (_d, db) = db();
    game(
        &db,
        "EUW1_1",
        420,
        "14.18",
        10,
        &[(ME, 64, true, Some((5, 2, 7)))],
    )
    .await;
    game(
        &db,
        "EUW1_2",
        420,
        "14.18",
        30,
        &[(ME, 64, false, Some((1, 4, 3)))],
    )
    .await;
    game(&db, "EUW1_3", 420, "14.18", 20, &[(ME, 64, true, None)]).await;
    let row = &champions(&db, "s1", ME, all()).await.unwrap()[0];
    assert_eq!(
        row,
        &ChampionRow {
            champion_id: 64,
            games: 3,
            wins: 2,
            stated_games: 2,
            kills: 6,
            deaths: 6,
            assists: 10,
            last_played_ms: Some(30),
        }
    );
}
