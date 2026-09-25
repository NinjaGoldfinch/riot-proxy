//! league-v4 ladder values (v1 `src/riot/ladder.ts`): ranked queues, tiers and
//! divisions. The paged tiers and the apex tiers are separate on purpose: the
//! paged entries route rejects an apex tier, and each apex tier has its own
//! league endpoint.

/// Ranked ladders. TFT queues are a different API and out of scope (v1).
pub const RANKED_QUEUES: [&str; 2] = ["RANKED_SOLO_5x5", "RANKED_FLEX_SR"];
/// Tiers walked page by page, ascending.
pub const PAGED_TIERS: [&str; 7] = [
    "IRON", "BRONZE", "SILVER", "GOLD", "PLATINUM", "EMERALD", "DIAMOND",
];
/// Tiers served whole by their own league endpoint, ascending.
pub const APEX_TIERS: [&str; 3] = ["MASTER", "GRANDMASTER", "CHALLENGER"];
/// Divisions within a tier. Apex tiers report `I` for everyone.
pub const DIVISIONS: [&str; 4] = ["I", "II", "III", "IV"];
/// match-v5 queue ids of the two ranked ladders (v1 `QUEUE_IDS`).
pub const QUEUE_IDS: [(&str, u32); 2] = [("RANKED_SOLO_5x5", 420), ("RANKED_FLEX_SR", 440)];
/// Riot's page size for the paged route, measured by v1.
pub const ENTRIES_PER_PAGE: u32 = 205;

/// Every tier, ascending (v1 `TIERS`).
pub fn tiers() -> impl Iterator<Item = &'static str> {
    PAGED_TIERS.into_iter().chain(APEX_TIERS)
}

/// The league-v4 endpoint serving an apex tier (v1 `APEX_LEAGUES`).
pub fn apex_endpoint(tier: &str) -> Option<&'static str> {
    match tier {
        "MASTER" => Some("league.master"),
        "GRANDMASTER" => Some("league.grandmaster"),
        "CHALLENGER" => Some("league.challenger"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_match_v1() {
        assert_eq!(tiers().collect::<Vec<_>>().len(), 10);
        assert_eq!(tiers().next(), Some("IRON"));
        assert_eq!(tiers().last(), Some("CHALLENGER"));
        for t in APEX_TIERS {
            assert!(apex_endpoint(t).is_some(), "{t}");
        }
        assert_eq!(apex_endpoint("DIAMOND"), None);
        assert_eq!(QUEUE_IDS[0], ("RANKED_SOLO_5x5", 420));
    }
}
