//! `match_facts`: one row per participant, a pure derivation of an archived
//! match body (docs/design/04 §Schema). What player history and the analytics
//! aggregates read instead of opening match blobs.
//!
//! The extraction never panics on a strange body: a missing optional field is a
//! `NULL`, and a participant missing a `NOT NULL` column (puuid, team, champion,
//! win) is left out rather than given an invented value. Remakes are rows like
//! any other; [`extras`] says whether Riot flagged the match one, and analytics
//! keep the two apart (ADR-056). Bump [`FACTS_VERSION`] whenever the output of
//! [`extract`] or [`extras`] changes; `facts:reextract` re-derives older rows.

use serde::{Deserialize, Serialize};

/// The version of [`extract`] that wrote a row (`match_facts.facts_version`).
///
/// 2: `cs` (P5-04, ADR-043).
/// 3: `gold`, `damage`, `vision`, `match_bans` and `matches.remake` (P7-04, ADR-056).
pub const FACTS_VERSION: i64 = 3;

/// One `match_facts` row, minus the `match_id` and `key_scope` the caller owns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Fact {
    pub puuid: String,
    pub team_id: i64,
    /// `teamPosition`; `None` where Riot sends none or `""` (Arena, ARAM, remakes).
    pub position: Option<String>,
    pub champion_id: i64,
    pub win: bool,
    pub kills: Option<i64>,
    pub deaths: Option<i64>,
    pub assists: Option<i64>,
    /// `totalMinionsKilled + neutralMinionsKilled`; `None` only when both are
    /// absent (v1 `extractCs`).
    pub cs: Option<i64>,
    /// `goldEarned`.
    pub gold: Option<i64>,
    /// `totalDamageDealtToChampions`.
    pub damage: Option<i64>,
    /// `visionScore`.
    pub vision: Option<i64>,
    /// `item0`–`item5` in slot order, `0` for an empty slot. The trinket
    /// (`item6`) is left out, as v1's item stats left it out.
    pub items: Vec<i64>,
    pub runes: Option<Runes>,
    /// `[summoner1Id, summoner2Id]` in slot order; `None` unless both are present.
    pub summoners: Option<[i64; 2]>,
}

/// The rune page: what v1's rune stats grouped on (keystone, sub-style tree)
/// plus the rest of the page, so build views need no facts re-extraction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Runes {
    pub primary_style: Option<i64>,
    /// The primary style's first selection.
    pub keystone: Option<i64>,
    pub sub_style: Option<i64>,
    /// Every selected perk, primary style first, in Riot's order.
    pub perks: Vec<i64>,
    /// `[offense, flex, defense]`.
    pub stat_perks: Option<[i64; 3]>,
}

impl Fact {
    /// `items` as stored: a JSON array.
    pub fn items_json(&self) -> String {
        serde_json::to_string(&self.items).unwrap_or_else(|_| "[]".into())
    }

    pub fn runes_json(&self) -> Option<String> {
        self.runes.as_ref().and_then(|r| serde_json::to_string(r).ok())
    }

    pub fn summoners_json(&self) -> Option<String> {
        self.summoners.and_then(|s| serde_json::to_string(&s).ok())
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FactsError {
    #[error("not a match-v5 body: {0}")]
    NotAMatch(String),
}

#[derive(Deserialize)]
struct Body {
    info: Info,
}

#[derive(Deserialize)]
struct Info {
    #[serde(default)]
    participants: Vec<Participant>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Participant {
    puuid: Option<String>,
    team_id: Option<i64>,
    team_position: Option<String>,
    champion_id: Option<i64>,
    win: Option<bool>,
    kills: Option<i64>,
    deaths: Option<i64>,
    assists: Option<i64>,
    total_minions_killed: Option<i64>,
    neutral_minions_killed: Option<i64>,
    gold_earned: Option<i64>,
    total_damage_dealt_to_champions: Option<i64>,
    vision_score: Option<i64>,
    item0: Option<i64>,
    item1: Option<i64>,
    item2: Option<i64>,
    item3: Option<i64>,
    item4: Option<i64>,
    item5: Option<i64>,
    summoner1_id: Option<i64>,
    summoner2_id: Option<i64>,
    perks: Option<Perks>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Perks {
    stat_perks: Option<StatPerks>,
    #[serde(default)]
    styles: Vec<Style>,
}

#[derive(Deserialize)]
struct StatPerks {
    offense: Option<i64>,
    flex: Option<i64>,
    defense: Option<i64>,
}

#[derive(Deserialize)]
struct Style {
    description: Option<String>,
    style: Option<i64>,
    #[serde(default)]
    selections: Vec<Selection>,
}

#[derive(Deserialize)]
struct Selection {
    perk: Option<i64>,
}

/// The facts for every participant of a match body, in Riot's participant
/// order. A puuid seen twice in one match keeps its first row only: the table's
/// key is `(match_id, puuid)`.
pub fn extract(body: &[u8]) -> Result<Vec<Fact>, FactsError> {
    let body: Body = serde_json::from_slice(body).map_err(|e| FactsError::NotAMatch(e.to_string()))?;
    let mut seen = std::collections::HashSet::new();
    let mut facts = Vec::with_capacity(body.info.participants.len());
    for p in body.info.participants {
        let (Some(puuid), Some(team_id), Some(champion_id), Some(win)) =
            (p.puuid, p.team_id, p.champion_id, p.win)
        else {
            continue;
        };
        if puuid.is_empty() || !seen.insert(puuid.clone()) {
            continue;
        }
        let items = [p.item0, p.item1, p.item2, p.item3, p.item4, p.item5]
            .into_iter()
            .map(|i| i.unwrap_or(0))
            .collect();
        facts.push(Fact {
            puuid,
            team_id,
            position: p.team_position.filter(|s| !s.is_empty()),
            champion_id,
            win,
            kills: p.kills,
            deaths: p.deaths,
            assists: p.assists,
            cs: match (p.total_minions_killed, p.neutral_minions_killed) {
                (None, None) => None,
                (lane, jungle) => Some(lane.unwrap_or(0) + jungle.unwrap_or(0)),
            },
            gold: p.gold_earned,
            damage: p.total_damage_dealt_to_champions,
            vision: p.vision_score,
            items,
            runes: p.perks.map(runes),
            summoners: p.summoner1_id.zip(p.summoner2_id).map(|(a, b)| [a, b]),
        });
    }
    Ok(facts)
}

/// What a match says about itself rather than one participant.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Extras {
    /// Any participant has `gameEndedInEarlySurrender` (Riot's remake flag).
    pub remake: bool,
    pub bans: Vec<Ban>,
}

/// One `info.teams[].bans[]` entry (v1 `extractBans`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ban {
    pub team_id: i64,
    pub pick_turn: i64,
    pub champion_id: i64,
}

#[derive(Deserialize)]
struct ExtrasBody {
    info: ExtrasInfo,
}

#[derive(Deserialize)]
struct ExtrasInfo {
    #[serde(default)]
    participants: Vec<SurrenderFlag>,
    #[serde(default)]
    teams: Vec<Team>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SurrenderFlag {
    game_ended_in_early_surrender: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Team {
    team_id: Option<i64>,
    #[serde(default)]
    bans: Vec<RawBan>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawBan {
    champion_id: Option<i64>,
    pick_turn: Option<i64>,
}

/// The remake flag and the bans. A ban of `-1` (no ban) or one without a team
/// or pick turn is left out, as v1 left it out.
pub fn extras(body: &[u8]) -> Result<Extras, FactsError> {
    let body: ExtrasBody = serde_json::from_slice(body).map_err(|e| FactsError::NotAMatch(e.to_string()))?;
    let remake = body
        .info
        .participants
        .iter()
        .any(|p| p.game_ended_in_early_surrender == Some(true));
    let bans = body
        .info
        .teams
        .iter()
        .flat_map(|t| {
            t.bans.iter().filter_map(move |b| {
                let champion_id = b.champion_id.filter(|c| *c != -1)?;
                Some(Ban {
                    team_id: t.team_id?,
                    pick_turn: b.pick_turn?,
                    champion_id,
                })
            })
        })
        .collect();
    Ok(Extras { remake, bans })
}

/// Replace a match's bans.
pub fn write_bans(conn: &rusqlite::Connection, match_id: &str, bans: &[Ban]) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM match_bans WHERE match_id = ?1", [match_id])?;
    let mut stmt = conn.prepare_cached(
        "INSERT OR IGNORE INTO match_bans (match_id, team_id, pick_turn, champion_id) VALUES (?1, ?2, ?3, ?4)",
    )?;
    for b in bans {
        stmt.execute(rusqlite::params![match_id, b.team_id, b.pick_turn, b.champion_id])?;
    }
    Ok(())
}

fn runes(perks: Perks) -> Runes {
    let by = |d: &str| perks.styles.iter().find(|s| s.description.as_deref() == Some(d));
    let (primary, sub) = (by("primaryStyle"), by("subStyle"));
    let perk_ids = |s: Option<&Style>| {
        s.map(|s| s.selections.iter().filter_map(|x| x.perk).collect::<Vec<_>>())
            .unwrap_or_default()
    };
    let mut all = perk_ids(primary);
    all.extend(perk_ids(sub));
    Runes {
        primary_style: primary.and_then(|s| s.style),
        keystone: primary.and_then(|s| s.selections.first()).and_then(|x| x.perk),
        sub_style: sub.and_then(|s| s.style),
        perks: all,
        stat_perks: perks
            .stat_perks
            .as_ref()
            .and_then(|s| Some([s.offense?, s.flex?, s.defense?])),
    }
}

/// Replace this key scope's facts for one match with `facts`, inside the
/// caller's transaction. Other key scopes' rows are left alone.
pub fn write(
    conn: &rusqlite::Connection,
    match_id: &str,
    key_scope: &str,
    facts: &[Fact],
) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM match_facts WHERE match_id = ?1 AND key_scope = ?2",
        (match_id, key_scope),
    )?;
    let mut stmt = conn.prepare_cached(
        "INSERT INTO match_facts (match_id, key_scope, puuid, team_id, position, champion_id, win,
           kills, deaths, assists, items, runes, summoners, facts_version, cs, gold, damage, vision)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
         ON CONFLICT (match_id, puuid) DO UPDATE SET
           key_scope = excluded.key_scope, team_id = excluded.team_id, position = excluded.position,
           champion_id = excluded.champion_id, win = excluded.win, kills = excluded.kills,
           deaths = excluded.deaths, assists = excluded.assists, items = excluded.items,
           runes = excluded.runes, summoners = excluded.summoners, facts_version = excluded.facts_version,
           cs = excluded.cs, gold = excluded.gold, damage = excluded.damage, vision = excluded.vision",
    )?;
    for f in facts {
        stmt.execute(rusqlite::params![
            match_id,
            key_scope,
            f.puuid,
            f.team_id,
            f.position,
            f.champion_id,
            f.win,
            f.kills,
            f.deaths,
            f.assists,
            f.items_json(),
            f.runes_json(),
            f.summoners_json(),
            FACTS_VERSION,
            f.cs,
            f.gold,
            f.damage,
            f.vision,
        ])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
