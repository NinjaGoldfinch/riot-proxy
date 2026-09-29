//! The overview projection for the composite match page (v1 `match-summary.ts`):
//! the requesting player's line in each game, not the ~100 KB game.
//!
//! v1's two rules hold: Riot's field names and values verbatim (nothing renamed
//! or computed; `perks` is a subset, not a rewrite), and absent means absent,
//! so everything but `matchId` is omitted when it cannot be read.

use serde::Serialize;
use serde_json::{Number, Value};

#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = MatchSummary)]
pub struct MatchSummary {
    pub match_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<f64>)]
    pub queue_id: Option<Number>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub game_mode: Option<String>,
    /// Which patch this was played on: the Data Dragon version to render it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub game_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<f64>)]
    pub game_creation: Option<Number>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<f64>)]
    pub game_end_timestamp: Option<Number>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<f64>)]
    pub game_duration: Option<Number>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_of_game_result: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub player: Option<PlayerSummary>,
}

/// Declares the numeric fields once: name in Riot's JSON, field here.
macro_rules! player_summary {
    ($($num:ident: $key:literal),* $(,)?) => {
        #[derive(Debug, Clone, Default, PartialEq, Serialize, utoipa::ToSchema)]
        #[serde(rename_all = "camelCase")]
        pub struct PlayerSummary {
            #[serde(skip_serializing_if = "Option::is_none")]
            pub puuid: Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            pub win: Option<bool>,
            /// A remake: rendered as neither a win nor a loss.
            #[serde(skip_serializing_if = "Option::is_none")]
            pub game_ended_in_early_surrender: Option<bool>,
            #[serde(skip_serializing_if = "Option::is_none")]
            #[schema(value_type = Option<f64>)]
            pub champion_id: Option<Number>,
            #[serde(skip_serializing_if = "Option::is_none")]
            pub champion_name: Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            #[schema(value_type = Option<f64>)]
            pub champ_level: Option<Number>,
            #[serde(skip_serializing_if = "Option::is_none")]
            #[schema(value_type = Option<f64>)]
            pub team_id: Option<Number>,
            #[serde(skip_serializing_if = "Option::is_none")]
            pub team_position: Option<String>,
            $(
                #[serde(skip_serializing_if = "Option::is_none")]
                #[schema(value_type = Option<f64>)]
                pub $num: Option<Number>,
            )*
            #[serde(skip_serializing_if = "Option::is_none")]
            #[schema(value_type = Option<f64>)]
            pub summoner1_id: Option<Number>,
            #[serde(skip_serializing_if = "Option::is_none")]
            #[schema(value_type = Option<f64>)]
            pub summoner2_id: Option<Number>,
            #[serde(skip_serializing_if = "Option::is_none")]
            pub perks: Option<PerksSummary>,
            /// Arena: there is no win/loss to render, only a placement.
            #[serde(skip_serializing_if = "Option::is_none")]
            #[schema(value_type = Option<f64>)]
            pub placement: Option<Number>,
            #[serde(skip_serializing_if = "Option::is_none")]
            #[schema(value_type = Option<f64>)]
            pub player_subteam_id: Option<Number>,
        }

        fn numbers(p: &Value, s: &mut PlayerSummary) {
            $( s.$num = num(&p[$key]); )*
        }
    };
}

player_summary! {
    kills: "kills",
    deaths: "deaths",
    assists: "assists",
    total_minions_killed: "totalMinionsKilled",
    neutral_minions_killed: "neutralMinionsKilled",
    gold_earned: "goldEarned",
    vision_score: "visionScore",
    total_damage_dealt_to_champions: "totalDamageDealtToChampions",
    item0: "item0",
    item1: "item1",
    item2: "item2",
    item3: "item3",
    item4: "item4",
    item5: "item5",
    item6: "item6",
}

/// Riot's `perks` blob is ~40 lines to convey three icon ids.
#[derive(Debug, Clone, Default, PartialEq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PerksSummary {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<f64>)]
    pub keystone: Option<Number>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<f64>)]
    pub primary_style: Option<Number>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<f64>)]
    pub sub_style: Option<Number>,
}

fn num(v: &Value) -> Option<Number> {
    match v {
        Value::Number(n) => Some(n.clone()),
        _ => None,
    }
}

fn string(v: &Value) -> Option<String> {
    v.as_str().map(str::to_string)
}

/// Project one match to `puuid`'s line in it. `matchId` comes from the payload
/// when present and from the id asked for otherwise, so it is never missing.
/// `None` when the payload names no such participant (or is not a match): the
/// caller reports it in `warnings[]` rather than serving a summary about nobody.
pub fn summarise(body: &[u8], puuid: &str, requested_id: &str) -> Option<MatchSummary> {
    let root: Value = serde_json::from_slice(body).ok()?;
    if !root.is_object() {
        return None;
    }
    let info = &root["info"];
    let player = info["participants"]
        .as_array()?
        .iter()
        .find(|p| p.is_object() && p["puuid"].as_str() == Some(puuid))?;
    Some(MatchSummary {
        match_id: string(&root["metadata"]["matchId"]).unwrap_or_else(|| requested_id.to_string()),
        queue_id: num(&info["queueId"]),
        game_mode: string(&info["gameMode"]),
        game_version: string(&info["gameVersion"]),
        game_creation: num(&info["gameCreation"]),
        game_end_timestamp: num(&info["gameEndTimestamp"]),
        game_duration: num(&info["gameDuration"]),
        end_of_game_result: string(&info["endOfGameResult"]),
        player: Some(summarise_player(player)),
    })
}

fn summarise_player(p: &Value) -> PlayerSummary {
    let mut s = PlayerSummary {
        puuid: string(&p["puuid"]),
        win: p["win"].as_bool(),
        game_ended_in_early_surrender: p["gameEndedInEarlySurrender"].as_bool(),
        champion_id: num(&p["championId"]),
        champion_name: string(&p["championName"]),
        champ_level: num(&p["champLevel"]),
        team_id: num(&p["teamId"]),
        team_position: string(&p["teamPosition"]),
        summoner1_id: num(&p["summoner1Id"]),
        summoner2_id: num(&p["summoner2Id"]),
        perks: perks(&p["perks"]),
        placement: num(&p["placement"]),
        player_subteam_id: num(&p["playerSubteamId"]),
        ..PlayerSummary::default()
    };
    numbers(p, &mut s);
    s
}

/// Keystone and the two style ids. `None` when the shape is not what we expect,
/// or holds none of the three: a missing rune row is not worth guessing at.
fn perks(v: &Value) -> Option<PerksSummary> {
    let styles = v["styles"].as_array()?;
    let by = |d: &str| {
        styles
            .iter()
            .find(|s| s.is_object() && s["description"].as_str() == Some(d))
    };
    let (primary, sub) = (by("primaryStyle"), by("subStyle"));
    let summary = PerksSummary {
        keystone: primary.and_then(|p| num(&p["selections"][0]["perk"])),
        primary_style: primary.and_then(|p| num(&p["style"])),
        sub_style: sub.and_then(|s| num(&s["style"])),
    };
    (summary != PerksSummary::default()).then_some(summary)
}

#[cfg(test)]
mod tests;
