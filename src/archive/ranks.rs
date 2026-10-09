//! Players' ranks from league-v4 lookups (V0010, ADR-105): every
//! `league.entriesByPuuid` body the fetcher reads from Riot is recorded here,
//! so analytics can place a participant the ladder does not hold.

use rusqlite::params;
use serde::Deserialize;

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

/// Replace one player's ranks on one platform with what `body` says. A queue
/// the body has no entry for is one the player is unranked in, so its row
/// goes. A body that is not a list of entries changes nothing.
pub async fn record(
    db: &Db,
    key_scope: &str,
    platform: &str,
    puuid: &str,
    body: &[u8],
    now: i64,
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
        }
        tx.commit()?;
        Ok(ranks.len())
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
        assert_eq!(record(&db, "s", "oc1", "P", body, 10).await.unwrap(), 2);
        // Another platform's row is a different account's standing and stays.
        let other = br#"[{"queueType":"RANKED_SOLO_5x5","tier":"IRON","rank":"IV","leaguePoints":1}]"#;
        record(&db, "s", "euw1", "P", other, 11).await.unwrap();
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
        record(&db, "s", "oc1", "P", later, 20).await.unwrap();
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
        assert_eq!(record(&db, "s", "euw1", "P", odd, 30).await.unwrap(), 0);
        assert_eq!(
            record(&db, "s", "oc1", "P", br#"{"status":{}}"#, 30)
                .await
                .unwrap(),
            0
        );
        assert_eq!(ranks(&db).await, ["oc1 RANKED_SOLO_5x5 P DIAMOND IV 0 20"]);
    }
}
