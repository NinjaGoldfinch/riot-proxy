//! Host routing (v1 spec §5.1, v1 `src/riot/routing.ts`). Platform and regional host
//! values are confirmed against developer.riotgames.com/docs/lol#routing-values
//! (2026-09-24). The platform→region grouping and the account-v1 rule are v1's.
//! `me1` is not in v1 nor in that table: the owner confirmed its host answers
//! league-v4 and quoted Riot's match-v5 routing list (EUROPE serves EUNE, EUW,
//! ME1, TR and RU) on 2026-11-09 (ME-02, ADR-216).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::http::ApiError;

/// Platform hosts serve summoner, league, spectator, champion-mastery and status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Na1,
    Br1,
    La1,
    La2,
    Euw1,
    Eun1,
    Tr1,
    Ru,
    Me1,
    Kr,
    Jp1,
    Oc1,
    Ph2,
    Sg2,
    Th2,
    Tw2,
    Vn2,
}

/// Regional hosts serve account-v1 and match-v5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Region {
    Americas,
    Europe,
    Asia,
    Sea,
}

impl Platform {
    /// In v1's order, with `me1` (ME-02) after the other europe platforms.
    pub const ALL: [Platform; 17] = [
        Self::Na1,
        Self::Br1,
        Self::La1,
        Self::La2,
        Self::Euw1,
        Self::Eun1,
        Self::Tr1,
        Self::Ru,
        Self::Me1,
        Self::Kr,
        Self::Jp1,
        Self::Oc1,
        Self::Ph2,
        Self::Sg2,
        Self::Th2,
        Self::Tw2,
        Self::Vn2,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Na1 => "na1",
            Self::Br1 => "br1",
            Self::La1 => "la1",
            Self::La2 => "la2",
            Self::Euw1 => "euw1",
            Self::Eun1 => "eun1",
            Self::Tr1 => "tr1",
            Self::Ru => "ru",
            Self::Me1 => "me1",
            Self::Kr => "kr",
            Self::Jp1 => "jp1",
            Self::Oc1 => "oc1",
            Self::Ph2 => "ph2",
            Self::Sg2 => "sg2",
            Self::Th2 => "th2",
            Self::Tw2 => "tw2",
            Self::Vn2 => "vn2",
        }
    }

    /// v1 `PLATFORM_LABELS`; `me1`'s is the owner's name for the shard (ME-01, ME-02).
    pub fn label(self) -> &'static str {
        match self {
            Self::Na1 => "North America",
            Self::Br1 => "Brazil",
            Self::La1 => "LATAM North",
            Self::La2 => "LATAM South",
            Self::Euw1 => "EU West",
            Self::Eun1 => "EU Nordic & East",
            Self::Tr1 => "Türkiye",
            Self::Ru => "Russia",
            Self::Me1 => "Middle East",
            Self::Kr => "Korea",
            Self::Jp1 => "Japan",
            Self::Oc1 => "Oceania",
            Self::Ph2 => "Philippines",
            Self::Sg2 => "Singapore",
            Self::Th2 => "Thailand",
            Self::Tw2 => "Taiwan",
            Self::Vn2 => "Vietnam",
        }
    }

    /// The regional host that serves match-v5 for this platform.
    pub fn region(self) -> Region {
        match self {
            Self::Na1 | Self::Br1 | Self::La1 | Self::La2 => Region::Americas,
            Self::Euw1 | Self::Eun1 | Self::Tr1 | Self::Ru | Self::Me1 => Region::Europe,
            Self::Kr | Self::Jp1 => Region::Asia,
            Self::Oc1 | Self::Ph2 | Self::Sg2 | Self::Th2 | Self::Tw2 | Self::Vn2 => Region::Sea,
        }
    }

    /// The account-v1 host for this platform, skipping the invalid `sea` hop.
    pub fn account_region(self) -> Region {
        self.region().account_region()
    }

    pub fn host(self) -> String {
        format!("{}.api.riotgames.com", self.as_str())
    }

    /// Case-insensitive parse that fails with v1's `BAD_REGION` envelope.
    pub fn parse(value: &str) -> Result<Self, ApiError> {
        value.parse()
    }

    /// Match ids are prefixed with the hosting platform, e.g. `EUW1_7381937461`.
    pub fn from_match_id(match_id: &str) -> Option<Self> {
        let (prefix, _) = match_id.split_once('_')?;
        prefix.parse().ok()
    }
}

/// Match id prefixes that are not a platform this proxy serves, and the
/// match-v5 host that answers for them (ME-01, ADR-143). Empty since `ME1`
/// became a platform (ME-02, ADR-216); kept so a future prefix Riot serves
/// before its platform is known needs one line, with a live check as its source.
pub const MATCH_ONLY_PREFIXES: [(&str, Region); 0] = [];

/// The region `list` gives a match id's prefix, any case.
fn match_only_region(match_id: &str, list: &[(&str, Region)]) -> Option<Region> {
    let (prefix, _) = match_id.split_once('_')?;
    list.iter()
        .find(|(p, _)| p.eq_ignore_ascii_case(prefix))
        .map(|&(_, region)| region)
}

impl Region {
    pub const ALL: [Region; 4] = [Self::Americas, Self::Europe, Self::Asia, Self::Sea];

    /// Regions that actually serve account-v1 (v1 `ACCOUNT_REGIONS`).
    pub const ACCOUNT: [Region; 3] = [Self::Americas, Self::Asia, Self::Europe];

    /// The order the proxy tries account-v1 clusters in when the caller names
    /// none (owner, RC-02): Riot serves every account from each of them.
    pub const ACCOUNT_PICK: [Region; 3] = [Self::Asia, Self::Americas, Self::Europe];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Americas => "americas",
            Self::Europe => "europe",
            Self::Asia => "asia",
            Self::Sea => "sea",
        }
    }

    /// v1 `REGION_LABELS`.
    pub fn label(self) -> &'static str {
        match self {
            Self::Americas => "Americas",
            Self::Europe => "Europe",
            Self::Asia => "Asia",
            Self::Sea => "Southeast Asia",
        }
    }

    /// Riot routes account-v1 only to americas, asia and europe; `sea` is a match-v5
    /// host and 404s for account paths, so SEA borrows `asia` (v1 behaviour).
    pub fn account_region(self) -> Region {
        match self {
            Self::Sea => Self::Asia,
            other => other,
        }
    }

    /// The match-v5 host a match id routes to: its platform's region, or for a
    /// prefix in [`MATCH_ONLY_PREFIXES`], the region listed there (ME-01).
    pub fn from_match_id(match_id: &str) -> Option<Region> {
        Platform::from_match_id(match_id)
            .map(Platform::region)
            .or_else(|| match_only_region(match_id, &MATCH_ONLY_PREFIXES))
    }

    pub fn platforms(self) -> impl Iterator<Item = Platform> {
        Platform::ALL.into_iter().filter(move |p| p.region() == self)
    }

    pub fn host(self) -> String {
        format!("{}.api.riotgames.com", self.as_str())
    }

    pub fn parse(value: &str) -> Result<Self, ApiError> {
        value.parse()
    }
}

fn list<T: Copy>(all: &[T], name: impl Fn(T) -> &'static str) -> String {
    all.iter().map(|&v| name(v)).collect::<Vec<_>>().join(", ")
}

impl FromStr for Platform {
    type Err = ApiError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let lower = value.to_ascii_lowercase();
        Self::ALL
            .into_iter()
            .find(|p| p.as_str() == lower)
            .ok_or_else(|| {
                ApiError::bad_region(format!(
                    "Unknown platform '{value}'. Expected one of: {}",
                    list(&Self::ALL, Self::as_str)
                ))
            })
    }
}

impl FromStr for Region {
    type Err = ApiError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let lower = value.to_ascii_lowercase();
        Self::ALL
            .into_iter()
            .find(|r| r.as_str() == lower)
            .ok_or_else(|| {
                ApiError::bad_region(format!(
                    "Unknown region '{value}'. Expected one of: {}",
                    list(&Self::ALL, Self::as_str)
                ))
            })
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Display for Region {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::ErrorCode;

    /// Every platform, its region and its account-v1 host. v1 spec §5.1 and
    /// `routing.ts`; the host names match developer.riotgames.com routing values.
    const TABLE: &[(&str, Region, Region)] = &[
        ("na1", Region::Americas, Region::Americas),
        ("br1", Region::Americas, Region::Americas),
        ("la1", Region::Americas, Region::Americas),
        ("la2", Region::Americas, Region::Americas),
        ("euw1", Region::Europe, Region::Europe),
        ("eun1", Region::Europe, Region::Europe),
        ("tr1", Region::Europe, Region::Europe),
        ("ru", Region::Europe, Region::Europe),
        ("me1", Region::Europe, Region::Europe),
        ("kr", Region::Asia, Region::Asia),
        ("jp1", Region::Asia, Region::Asia),
        ("oc1", Region::Sea, Region::Asia),
        ("ph2", Region::Sea, Region::Asia),
        ("sg2", Region::Sea, Region::Asia),
        ("th2", Region::Sea, Region::Asia),
        ("tw2", Region::Sea, Region::Asia),
        ("vn2", Region::Sea, Region::Asia),
    ];

    #[test]
    fn every_platform_routes_per_the_table() {
        assert_eq!(TABLE.len(), Platform::ALL.len(), "table covers every platform");
        for &(name, region, account) in TABLE {
            let p: Platform = name.parse().unwrap();
            assert_eq!(p.as_str(), name);
            assert_eq!(p.region(), region, "{name}");
            assert_eq!(p.account_region(), account, "{name} account-v1");
            assert_eq!(p.host(), format!("{name}.api.riotgames.com"));
        }
    }

    /// v1 "covers every platform across the four regions with no overlap" and
    /// "matches the documented region groupings".
    #[test]
    fn regions_partition_the_platforms() {
        let names = |r: Region| {
            let mut v: Vec<_> = r.platforms().map(Platform::as_str).collect();
            v.sort();
            v
        };
        assert_eq!(names(Region::Americas), ["br1", "la1", "la2", "na1"]);
        assert_eq!(names(Region::Europe), ["eun1", "euw1", "me1", "ru", "tr1"]);
        assert_eq!(names(Region::Asia), ["jp1", "kr"]);
        assert_eq!(names(Region::Sea), ["oc1", "ph2", "sg2", "th2", "tw2", "vn2"]);
        let total: usize = Region::ALL.iter().map(|r| r.platforms().count()).sum();
        assert_eq!(total, Platform::ALL.len());
    }

    /// v1 "never routes account-v1 to sea" and "leaves the three real account hosts alone".
    #[test]
    fn account_v1_never_routes_to_sea() {
        for p in Platform::ALL {
            assert!(Region::ACCOUNT.contains(&p.account_region()), "{p}");
        }
        assert_eq!(Region::Sea.account_region(), Region::Asia);
        for r in Region::ACCOUNT {
            assert_eq!(r.account_region(), r);
        }
    }

    #[test]
    fn builds_both_host_families() {
        assert_eq!(Platform::Euw1.host(), "euw1.api.riotgames.com");
        assert_eq!(Region::Europe.host(), "europe.api.riotgames.com");
        assert_eq!(Region::Sea.host(), "sea.api.riotgames.com");
    }

    /// v1 "accepts case-insensitive input and rejects unknown values with BAD_REGION".
    #[test]
    fn parsing_is_case_insensitive_and_unknowns_are_bad_region() {
        assert_eq!(Platform::parse("EUW1").unwrap(), Platform::Euw1);
        assert_eq!(Region::parse("EUROPE").unwrap(), Region::Europe);
        for bad in ["euw", "eu", "", "na"] {
            let err = Platform::parse(bad).unwrap_err();
            assert_eq!(err.code, ErrorCode::BadRegion);
            assert_eq!(err.status.as_u16(), 400);
        }
        let err = Region::parse("eu").unwrap_err();
        assert_eq!(err.code, ErrorCode::BadRegion);
        assert_eq!(
            err.message,
            "Unknown region 'eu'. Expected one of: americas, europe, asia, sea"
        );
        assert!(
            Platform::parse("xx")
                .unwrap_err()
                .message
                .starts_with("Unknown platform 'xx'. Expected one of: na1, br1,")
        );
    }

    #[test]
    fn derives_platform_and_region_from_a_match_id() {
        assert_eq!(Platform::from_match_id("EUW1_7381937461"), Some(Platform::Euw1));
        assert_eq!(
            Platform::from_match_id("KR_1234567890").map(Platform::region),
            Some(Region::Asia)
        );
        assert_eq!(Platform::from_match_id("NOPE_1"), None);
        assert_eq!(Platform::from_match_id("EUW17381937461"), None);
    }

    #[test]
    fn a_match_id_routes_to_its_region() {
        assert_eq!(Region::from_match_id("EUW1_7381937461"), Some(Region::Europe));
        assert_eq!(Region::from_match_id("OC1_1"), Some(Region::Sea));
        // ME1 is a platform (ME-02), still served by europe as ME-01 checked.
        assert_eq!(Platform::from_match_id("ME1_17228556"), Some(Platform::Me1));
        assert_eq!(Platform::from_match_id("me1_17228556"), Some(Platform::Me1));
        assert_eq!(Region::from_match_id("ME1_17228556"), Some(Region::Europe));
        assert_eq!(Region::from_match_id("me1_17228556"), Some(Region::Europe));
        assert_eq!(Region::from_match_id("NOPE_1"), None);
        assert_eq!(Region::from_match_id("ME117228556"), None);
    }

    /// `me1` (ME-02, ADR-216): its own platform host, europe for match-v5
    /// (Riot's routing list) and so for account-v1, as the europe platforms.
    #[test]
    fn me1_is_a_europe_platform() {
        let p = Platform::parse("me1").unwrap();
        assert_eq!(p, Platform::Me1);
        assert_eq!(Platform::parse("ME1").unwrap(), Platform::Me1);
        assert_eq!(p.host(), "me1.api.riotgames.com");
        assert_eq!(p.region(), Region::Europe);
        assert_eq!(p.account_region(), Region::Europe);
        assert_eq!(p.account_region(), Platform::Euw1.account_region());
        assert_eq!(p.label(), "Middle East");
        assert_eq!(serde_json::to_string(&p).unwrap(), "\"me1\"");
        assert!(Region::Europe.platforms().any(|q| q == Platform::Me1));
    }

    /// No prefix is match-only today; the fallback still works for one listed.
    #[test]
    fn a_match_only_prefix_falls_back_to_its_listed_region() {
        assert!(MATCH_ONLY_PREFIXES.is_empty(), "ME1 is a platform since ME-02");
        let list = [("XX9", Region::Asia)];
        assert_eq!(match_only_region("XX9_1", &list), Some(Region::Asia));
        assert_eq!(match_only_region("xx9_1", &list), Some(Region::Asia));
        assert_eq!(match_only_region("XX8_1", &list), None);
        assert_eq!(match_only_region("XX91", &list), None);
        assert_eq!(match_only_region("XX9_1", &MATCH_ONLY_PREFIXES), None);
    }

    #[test]
    fn serde_uses_lowercase_values() {
        assert_eq!(serde_json::to_string(&Platform::Oc1).unwrap(), "\"oc1\"");
        assert_eq!(serde_json::from_str::<Region>("\"sea\"").unwrap(), Region::Sea);
        assert_eq!(Platform::Tr1.label(), "Türkiye");
        assert_eq!(Region::Sea.label(), "Southeast Asia");
    }
}
