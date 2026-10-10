//! Players' ranks from league-v4 lookups (V0010, ADR-105): every
//! `league.entriesByPuuid` body the fetcher reads from Riot is recorded here,
//! so analytics can place a participant the ladder does not hold. Plus the
//! list `ranks:lookup` works through to place the rest (V0011, ADR-111).

use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;

use crate::archive::tiers;
use crate::db::{Db, DbError};

/// The fields of league-v4's `LeagueEntryDTO` a rank needs (the same ones
/// `poll:rank` reads).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    queue_type: Option<String>,
    tier: Option<String>,
    rank: Option<String>,
    league_points: Option<i64>,
}

/// Replace one player's ranks on one platform with what `body` says, and stamp
/// the lookup. A queue the body has no entry for is one the player is unranked
/// in, so its row goes. A body that is not a list of entries changes nothing.
/// Each rank also places the player's `UNKNOWN` tier stamps on matches that
/// ended at or after `late_since` (THR-02, [`tiers::late_stamp`]).
pub async fn record(
    db: &Db,
    key_scope: &str,
    platform: &str,
    puuid: &str,
    body: &[u8],
    now: i64,
    late_since: i64,
) -> Result<usize, DbError> {
    let Ok(entries) = serde_json::from_slice::<Vec<Entry>>(body) else {
        return Ok(0);
    };
    let ranks: Vec<(String, String, String, i64)> = entries
        .into_iter()
        .filter_map(|e| {
            let tier = e.tier.filter(|t| crate::riot::ladder::is_tier(t))?;
            Some((
                e.queue_type?,
                tier,
                e.rank.unwrap_or_default(),
                e.league_points.unwrap_or(0),
            ))
        })
        .collect();
    let (scope, platform, puuid) = (key_scope.to_string(), platform.to_string(), puuid.to_string());
    db.write(move |c| {
        let tx = c.transaction()?;
        tx.execute(
            "DELETE FROM player_ranks WHERE key_scope = ?1 AND platform = ?2 AND puuid = ?3",
            params![scope, platform, puuid],
        )?;
        for (queue, tier, division, lp) in &ranks {
            tx.execute(
                "INSERT INTO player_ranks (key_scope, platform, queue, puuid, tier, division, league_points, fetched_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![scope, platform, queue, puuid, tier, division, lp, now],
            )?;
            tiers::late_stamp(&tx, &scope, &platform, queue, &puuid, tier, late_since, now)?;
        }
        stamp_on(&tx, &scope, &platform, &puuid, now)?;
        tx.commit()?;
        Ok(ranks.len())
    })
    .await
}

fn stamp_on(c: &Connection, key_scope: &str, platform: &str, puuid: &str, now: i64) -> Result<(), DbError> {
    c.execute(
        "INSERT INTO rank_lookups (key_scope, platform, puuid, looked_up_at) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (key_scope, platform, puuid) DO UPDATE SET looked_up_at = excluded.looked_up_at",
        params![key_scope, platform, puuid, now],
    )?;
    Ok(())
}

/// Stamp a lookup that read no entries: Riot does not know the player on this
/// platform (a 404), so asking again before the recheck is no use.
pub async fn stamp(db: &Db, key_scope: &str, platform: &str, puuid: &str, now: i64) -> Result<(), DbError> {
    let (scope, platform, puuid) = (key_scope.to_string(), platform.to_string(), puuid.to_string());
    db.write(move |c| stamp_on(c, &scope, &platform, &puuid, now))
        .await
}

/// One (platform, queue)'s lookup list.
#[derive(Debug, Clone)]
pub struct Ladder {
    pub key_scope: String,
    pub platform: String,
    /// league-v4's queue, `RANKED_SOLO_5x5` or `RANKED_FLEX_SR`.
    pub queue: String,
    /// match-v5's id for `queue` (420 / 440).
    pub queue_id: i64,
}

/// Players still on the list.
pub async fn waiting(db: &Db, l: &Ladder) -> Result<i64, DbError> {
    let l = l.clone();
    db.read(move |c| {
        Ok::<_, DbError>(c.query_row(
            "SELECT count(*) FROM rank_lookup_queue WHERE key_scope = ?1 AND platform = ?2 AND queue = ?3",
            params![l.key_scope, l.platform, l.queue],
            |r| r.get(0),
        )?)
    })
    .await
}

/// Fill the list with up to `limit` players to look up: participants of the
/// platform's archived matches in this queue that analytics would count
/// (ADR-105) but the ladder does not hold for it and nobody has looked up
/// since `recheck_after`. Most archived games first, since each lookup places
/// all of a player's games. Returns how many were added.
pub async fn plan(db: &Db, l: &Ladder, limit: u32, recheck_after: i64) -> Result<usize, DbError> {
    let read = l.clone();
    // Read on a reader: a large archive takes a while to count, and the
    // writer must not wait on it. Only the list is written.
    let players: Vec<(String, i64)> = db
        .read(move |c| {
            let mut stmt = c.prepare(PLAN)?;
            let rows = stmt
                .query_map(
                    params![
                        read.key_scope,
                        read.platform,
                        read.queue,
                        read.queue_id,
                        recheck_after,
                        limit
                    ],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, DbError>(rows)
        })
        .await?;
    let l = l.clone();
    db.write(move |c| {
        let tx = c.transaction()?;
        let mut added = 0;
        for (puuid, games) in &players {
            added += tx.execute(
                "INSERT OR IGNORE INTO rank_lookup_queue (key_scope, platform, queue, puuid, games)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![l.key_scope, l.platform, l.queue, puuid, games],
            )?;
        }
        tx.commit()?;
        Ok::<_, DbError>(added)
    })
    .await
}

/// [`plan`]'s read. `CROSS JOIN` keeps the platform's id range (`OC1_…`, on
/// the primary key) the outer loop, so other platforms' facts are never read,
/// and `+` keeps `facts_player` (key scope first) from being picked over the
/// facts' own key. The ladder and the stamps are checked once per player,
/// after the count.
/// Binds `?1` key scope, `?2` platform, `?3` queue, `?4` queue id, `?5` the
/// recheck point, `?6` the limit.
const PLAN: &str = "SELECT puuid, games FROM (
       SELECT f.puuid AS puuid, count(*) AS games
         FROM matches m
        CROSS JOIN match_facts f ON f.match_id = m.match_id
        WHERE m.match_id > upper(?2) || '_' AND m.match_id < upper(?2) || '`'
          AND m.queue_id = ?4 AND m.patch IS NOT NULL AND +f.key_scope = ?1
        GROUP BY f.puuid
     ) p
     WHERE NOT EXISTS (SELECT 1 FROM ladder_entries le WHERE le.key_scope = ?1
             AND le.platform = ?2 AND le.queue = ?3 AND le.puuid = p.puuid)
       AND NOT EXISTS (SELECT 1 FROM rank_lookups rl WHERE rl.key_scope = ?1
             AND rl.platform = ?2 AND rl.puuid = p.puuid AND rl.looked_up_at > ?5)
     ORDER BY games DESC, puuid
     LIMIT ?6";

/// Take the next player off the list, most games first, with their games.
/// Taken before the lookup, so a player Riot keeps failing on cannot hold up
/// the rest: they are planned again next time.
pub async fn take(db: &Db, l: &Ladder) -> Result<Option<(String, i64)>, DbError> {
    let l = l.clone();
    db.write(move |c| {
        let tx = c.transaction()?;
        let next: Option<(String, i64)> = tx
            .query_row(
                "SELECT puuid, games FROM rank_lookup_queue WHERE key_scope = ?1 AND platform = ?2 AND queue = ?3
                  ORDER BY games DESC, puuid LIMIT 1",
                params![l.key_scope, l.platform, l.queue],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((puuid, _)) = &next {
            tx.execute(
                "DELETE FROM rank_lookup_queue WHERE key_scope = ?1 AND platform = ?2 AND queue = ?3 AND puuid = ?4",
                params![l.key_scope, l.platform, l.queue, puuid],
            )?;
        }
        tx.commit()?;
        Ok::<_, DbError>(next)
    })
    .await
}

/// Return a taken player to the list: the limiter had no room for them.
pub async fn put_back(db: &Db, l: &Ladder, puuid: &str, games: i64) -> Result<(), DbError> {
    let (l, puuid) = (l.clone(), puuid.to_string());
    db.write(move |c| {
        c.execute(
            "INSERT OR IGNORE INTO rank_lookup_queue (key_scope, platform, queue, puuid, games)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![l.key_scope, l.platform, l.queue, puuid, games],
        )?;
        Ok::<_, DbError>(())
    })
    .await
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    async fn ranks(db: &Db) -> Vec<String> {
        db.read(|c| {
            let mut stmt = c.prepare(
                "SELECT platform || ' ' || queue || ' ' || puuid || ' ' || tier || ' ' || division || ' '
                        || league_points || ' ' || fetched_at FROM player_ranks ORDER BY platform, queue",
            )?;
            let rows = stmt
                .query_map([], |r| r.get(0))?
                .collect::<Result<Vec<String>, _>>()?;
            Ok::<_, DbError>(rows)
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn a_lookup_replaces_the_players_ranks_on_its_platform() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.db"), 1).unwrap();
        let body = br#"[
            {"queueType":"RANKED_SOLO_5x5","tier":"EMERALD","rank":"II","leaguePoints":41,"puuid":"P","wins":1},
            {"queueType":"RANKED_FLEX_SR","tier":"GOLD","rank":"I","leaguePoints":0}
        ]"#;
        assert_eq!(record(&db, "s", "oc1", "P", body, 10, i64::MAX).await.unwrap(), 2);
        // Another platform's row is a different account's standing and stays.
        let other = br#"[{"queueType":"RANKED_SOLO_5x5","tier":"IRON","rank":"IV","leaguePoints":1}]"#;
        record(&db, "s", "euw1", "P", other, 11, i64::MAX).await.unwrap();
        assert_eq!(
            ranks(&db).await,
            [
                "euw1 RANKED_SOLO_5x5 P IRON IV 1 11",
                "oc1 RANKED_FLEX_SR P GOLD I 0 10",
                "oc1 RANKED_SOLO_5x5 P EMERALD II 41 10",
            ]
        );

        // Unranked in flex now: its row goes; solo is updated.
        let later = br#"[{"queueType":"RANKED_SOLO_5x5","tier":"DIAMOND","rank":"IV","leaguePoints":0}]"#;
        record(&db, "s", "oc1", "P", later, 20, i64::MAX).await.unwrap();
        assert_eq!(
            ranks(&db).await,
            [
                "euw1 RANKED_SOLO_5x5 P IRON IV 1 11",
                "oc1 RANKED_SOLO_5x5 P DIAMOND IV 0 20"
            ]
        );

        // An entry with a tier we don't know counts as unranked (euw1's row
        // goes); a body that isn't a list changes nothing.
        let odd = br#"[{"queueType":"RANKED_SOLO_5x5","tier":"WOOD","rank":"I","leaguePoints":0}]"#;
        assert_eq!(record(&db, "s", "euw1", "P", odd, 30, i64::MAX).await.unwrap(), 0);
        assert_eq!(
            record(&db, "s", "oc1", "P", br#"{"status":{}}"#, 30, i64::MAX)
                .await
                .unwrap(),
            0
        );
        assert_eq!(ranks(&db).await, ["oc1 RANKED_SOLO_5x5 P DIAMOND IV 0 20"]);
    }

    /// Match `id` in `queue_id` with these participants, as the archive
    /// stores them (only the columns the plan reads matter).
    async fn game(db: &Db, id: &'static str, queue_id: i64, players: &'static [&'static str]) {
        db.write(move |c| {
            c.execute(
                "INSERT INTO matches (match_id, region, patch, queue_id, game_end_ms, body_zstd, body_size, archived_at)
                 VALUES (?1, 'sea', '16.19', ?2, 0, x'', 0, 0)",
                params![id, queue_id],
            )?;
            for (i, p) in players.iter().enumerate() {
                c.execute(
                    "INSERT INTO match_facts (match_id, key_scope, puuid, team_id, champion_id, win, facts_version)
                     VALUES (?1, 's', ?2, ?3, 1, 1, 1)",
                    params![id, p, if i < 5 { 100 } else { 200 }],
                )?;
            }
            Ok::<_, DbError>(())
        })
        .await
        .unwrap();
    }

    async fn on_ladder(db: &Db, queue: &'static str, puuid: &'static str) {
        db.write(move |c| {
            c.execute(
                "INSERT INTO ladder_entries (key_scope, platform, queue, puuid, tier, division, league_points,
                    wins, losses, first_seen_crawl_id, last_seen_crawl_id, updated_at)
                 VALUES ('s', 'oc1', ?1, ?2, 'MASTER', 'I', 0, 0, 0, 'c', 'c', 0)",
                params![queue, puuid],
            )?;
            Ok::<_, DbError>(())
        })
        .await
        .unwrap();
    }

    fn solo() -> Ladder {
        Ladder {
            key_scope: "s".into(),
            platform: "oc1".into(),
            queue: "RANKED_SOLO_5x5".into(),
            queue_id: 420,
        }
    }

    async fn drain(db: &Db, l: &Ladder) -> Vec<(String, i64)> {
        let mut out = Vec::new();
        while let Some(next) = take(db, l).await.unwrap() {
            out.push(next);
        }
        out
    }

    /// The plan lists the players analytics count under UNKNOWN: in this
    /// platform's matches of this queue, not on its ladder for the queue, not
    /// looked up since the recheck. Most games first, up to the limit.
    #[tokio::test]
    async fn the_plan_lists_unplaced_players_most_games_first() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.db"), 1).unwrap();
        game(&db, "OC1_1", 420, &["A", "B", "C", "L"]).await;
        game(&db, "OC1_2", 420, &["A", "B", "R"]).await;
        game(&db, "OC1_3", 420, &["A", "OLD"]).await;
        // Flex, another platform, and a platform whose id shares the prefix
        // letters: none of them are this ladder's games.
        game(&db, "OC1_4", 440, &["FLEX", "FLEX2"]).await;
        game(&db, "NA1_5", 420, &["NA"]).await;
        game(&db, "OC1X_6", 420, &["ODD"]).await;
        // L is on the solo ladder; on the flex ladder only, B still counts.
        on_ladder(&db, "RANKED_SOLO_5x5", "L").await;
        on_ladder(&db, "RANKED_FLEX_SR", "B").await;
        // R was looked up after the recheck point, OLD before it; a lookup
        // on another platform is another account's.
        record(&db, "s", "oc1", "R", b"[]", 500, i64::MAX).await.unwrap();
        record(&db, "s", "oc1", "OLD", b"[]", 50, i64::MAX).await.unwrap();
        stamp(&db, "s", "na1", "C", 500).await.unwrap();

        assert_eq!(plan(&db, &solo(), 50_000, 100).await.unwrap(), 4);
        assert_eq!(waiting(&db, &solo()).await.unwrap(), 4);
        assert_eq!(
            drain(&db, &solo()).await,
            [
                ("A".into(), 3),
                ("B".into(), 2),
                ("C".into(), 1),
                ("OLD".into(), 1)
            ]
        );
        assert_eq!(waiting(&db, &solo()).await.unwrap(), 0);

        // The limit keeps the players with the most games.
        assert_eq!(plan(&db, &solo(), 2, 100).await.unwrap(), 2);
        assert_eq!(drain(&db, &solo()).await, [("A".into(), 3), ("B".into(), 2)]);

        // A player put back is taken again, in their place.
        plan(&db, &solo(), 50_000, 100).await.unwrap();
        let (first, games) = take(&db, &solo()).await.unwrap().unwrap();
        put_back(&db, &solo(), &first, games).await.unwrap();
        assert_eq!(take(&db, &solo()).await.unwrap(), Some(("A".into(), 3)));
    }

    /// The plan reads the platform's id range on the primary key and each
    /// fact by its key, so another platform's archive costs it nothing.
    #[tokio::test]
    async fn the_plan_reads_only_the_platforms_matches() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.db"), 1).unwrap();
        let plan = db
            .read(|c| {
                let mut stmt = c.prepare(&format!("EXPLAIN QUERY PLAN {PLAN}"))?;
                let rows = stmt
                    .query_map(params!["s", "oc1", "RANKED_SOLO_5x5", 420, 0, 10], |r| {
                        r.get::<_, String>(3)
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok::<_, DbError>(rows)
            })
            .await
            .unwrap();
        let text = plan.join("\n");
        assert!(
            text.contains("SEARCH m USING INDEX sqlite_autoindex_matches_1 (match_id>? AND match_id<?)"),
            "{text}"
        );
        assert!(
            text.contains("SEARCH f USING INDEX sqlite_autoindex_match_facts_1 (match_id=?)"),
            "{text}"
        );
        assert!(!text.contains("SCAN m") && !text.contains("SCAN f"), "{text}");
    }

    /// Every lookup is stamped, an unranked one too (it leaves no rank row);
    /// a body that is not a list is no lookup.
    #[tokio::test]
    async fn a_lookup_is_stamped_ranked_or_not() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.db"), 1).unwrap();
        record(&db, "s", "oc1", "U", b"[]", 10, i64::MAX).await.unwrap();
        record(&db, "s", "oc1", "U", b"[]", 20, i64::MAX).await.unwrap();
        record(&db, "s", "oc1", "X", br#"{"status":{}}"#, 30, i64::MAX)
            .await
            .unwrap();
        stamp(&db, "s", "oc1", "GONE", 40).await.unwrap();
        let stamps = db
            .read(|c| {
                let mut stmt =
                    c.prepare("SELECT puuid || ' ' || looked_up_at FROM rank_lookups ORDER BY puuid")?;
                let rows = stmt
                    .query_map([], |r| r.get(0))?
                    .collect::<Result<Vec<String>, _>>()?;
                Ok::<_, DbError>(rows)
            })
            .await
            .unwrap();
        assert_eq!(stamps, ["GONE 40", "U 20"]);
        assert!(ranks(&db).await.is_empty());
    }
}
