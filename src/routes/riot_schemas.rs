//! Schemas for the Riot payloads the proxy passes through (SITE-05, ADR-110):
//! documentation only. Nothing is validated or reshaped; the body is Riot's,
//! byte for byte.
//!
//! Fields, types and descriptions are Riot's developer portal's (account-v1
//! `AccountDto`, summoner-v4 `SummonerDTO`, league-v4 `LeagueEntryDTO` and
//! `MiniSeriesDTO`, champion-mastery-v4 `ChampionMasteryDto`,
//! `NextSeasonMilestonesDto` and `RewardConfigDto`, read 2026-10-09). A field
//! is required only if the portal doesn't call it optional and every recorded
//! body (`tests/fixtures/replay`) has it. Riot adds fields, so a body may carry
//! more than these (JSON Schema allows extra properties by default).
#![allow(dead_code)]

use std::collections::HashMap;

use serde::Serialize;
use utoipa::ToSchema;

/// account-v1 `AccountDto`.
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = AccountDto)]
pub struct AccountDto {
    /// Encrypted PUUID. Exact length of 78 characters.
    puuid: String,
    /// This field may be excluded from the response if the account doesn't have a gameName.
    #[schema(nullable = false)]
    game_name: Option<String>,
    /// This field may be excluded from the response if the account doesn't have a tagLine.
    #[schema(nullable = false)]
    tag_line: Option<String>,
}

/// summoner-v4 `SummonerDTO`: represents a summoner.
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = SummonerDTO)]
pub struct SummonerDto {
    /// ID of the summoner icon associated with the summoner.
    profile_icon_id: i32,
    /// Date summoner was last modified specified as epoch milliseconds.
    revision_date: i64,
    /// Encrypted PUUID. Exact length of 78 characters.
    puuid: String,
    /// Summoner level associated with the summoner.
    summoner_level: i64,
}

/// league-v4 `LeagueEntryDTO`: one ranked queue's standing.
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = LeagueEntryDTO)]
pub struct LeagueEntryDto {
    /// Not in every recorded body.
    #[schema(nullable = false)]
    league_id: Option<String>,
    /// Player's encrypted puuid.
    puuid: String,
    /// e.g. `RANKED_SOLO_5x5`.
    queue_type: String,
    tier: String,
    /// The player's division within a tier.
    rank: String,
    league_points: i32,
    /// Winning team on Summoners Rift.
    wins: i32,
    /// Losing team on Summoners Rift.
    losses: i32,
    hot_streak: bool,
    veteran: bool,
    fresh_blood: bool,
    inactive: bool,
    /// Not in every recorded body.
    #[schema(nullable = false)]
    mini_series: Option<MiniSeriesDto>,
}

/// league-v4 `MiniSeriesDTO`.
#[derive(Serialize, ToSchema)]
#[schema(as = MiniSeriesDTO)]
pub struct MiniSeriesDto {
    losses: i32,
    progress: String,
    target: i32,
    wins: i32,
}

/// champion-mastery-v4 `ChampionMasteryDto`: one champion's mastery.
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = ChampionMasteryDto)]
pub struct ChampionMasteryDto {
    /// Player Universal Unique Identifier. Exact length of 78 characters. (Encrypted)
    puuid: String,
    /// Number of points needed to achieve next level. Zero if player reached
    /// maximum champion level for this champion.
    champion_points_until_next_level: i64,
    /// Is chest granted for this champion or not in current season. Not in
    /// every recorded body.
    #[schema(nullable = false)]
    chest_granted: Option<bool>,
    /// Champion ID for this entry.
    champion_id: i64,
    /// Last time this champion was played by this player - in Unix milliseconds time format.
    last_play_time: i64,
    /// Champion level for specified player and champion combination.
    champion_level: i32,
    /// Total number of champion points for this player and champion combination.
    champion_points: i32,
    /// Number of points earned since current level has been achieved.
    champion_points_since_last_level: i64,
    mark_required_for_next_level: i32,
    champion_season_milestone: i32,
    next_season_milestone: NextSeasonMilestonesDto,
    /// The token earned for this champion at the current championLevel.
    tokens_earned: i32,
    #[schema(nullable = false)]
    milestone_grades: Option<Vec<String>>,
}

/// champion-mastery-v4 `NextSeasonMilestonesDto`.
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NextSeasonMilestonesDto {
    require_grade_counts: HashMap<String, i32>,
    /// Reward marks.
    reward_marks: i32,
    /// Bonus.
    bonus: bool,
    /// Reward configuration. Not in every recorded body.
    #[schema(nullable = false)]
    reward_config: Option<RewardConfigDto>,
}

/// champion-mastery-v4 `RewardConfigDto`.
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RewardConfigDto {
    /// Reward value
    reward_value: String,
    /// Reward type
    reward_type: String,
    /// Maximum reward
    maximum_reward: i32,
}
