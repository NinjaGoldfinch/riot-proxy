//! The endpoint registry: every Riot method the proxy calls, its host family, path,
//! TTLs and cache tier. Ids, paths, host families, TTLs and override keys are v1's
//! (`src/riot/endpoints.ts`, spec §5.3/§8.2); L2 membership is design/04 §TTLs.
//!
//! A method id is Riot's rate-limit method granularity. The limiter buckets on it
//! and the cache key embeds it, so ids must stay stable even if a URL changes.

use std::collections::BTreeMap;
use std::time::Duration;

use crate::riot::routing::{Platform, Region};

/// v1 `STALE_MULTIPLIER`: with stale-while-revalidate on, hard TTL = soft × 4.
pub const STALE_MULTIPLIER: u32 = 4;

/// Which host family serves a method (v1 spec §5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKind {
    /// `{platform}.api.riotgames.com`
    Platform,
    /// `{region}.api.riotgames.com` (match-v5).
    Regional,
    /// Regional, but only americas/asia/europe serve account-v1. A `sea` caller is
    /// sent to `asia` (v1 `accountRegion`).
    Account,
}

/// Where an upstream 404 is negatively cached, and for how long (v1 §8.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NegTtl {
    /// Not negatively cached.
    None,
    /// `NEG_TTL_SECONDS`
    Default,
    /// `NEG_TTL_ACCOUNT_SECONDS`
    Account,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub id: &'static str,
    pub host: HostKind,
    /// `{name}` segments are filled in order by [`Endpoint::path`].
    pub path_template: &'static str,
    /// Query parameters the method accepts (v1 builder signatures).
    pub query: &'static [&'static str],
    /// Limiter method bucket. v1 buckets on the method id itself (§9.2).
    pub method_scope_key: &'static str,
    /// Key for `CACHE_TTL_OVERRIDES`. Several methods may share one.
    pub override_key: &'static str,
    /// `None` = immutable: archived forever, never expires.
    pub soft_ttl: Option<Duration>,
    /// Written through to the SQLite `cache` table (design/04 §TTLs).
    pub persist_l2: bool,
    pub immutable: bool,
    pub negative: NegTtl,
}

const fn secs(s: u64) -> Option<Duration> {
    Some(Duration::from_secs(s))
}

#[allow(clippy::too_many_arguments)]
const fn ep(
    id: &'static str,
    host: HostKind,
    path_template: &'static str,
    query: &'static [&'static str],
    override_key: &'static str,
    soft_ttl: Option<Duration>,
    persist_l2: bool,
    negative: NegTtl,
) -> Endpoint {
    Endpoint {
        id,
        host,
        path_template,
        query,
        method_scope_key: id,
        override_key,
        soft_ttl,
        persist_l2,
        immutable: soft_ttl.is_none(),
        negative,
    }
}

use HostKind::{Account, Platform as OnPlatform, Regional};
use NegTtl::{Account as NegAccount, Default as NegDefault, None as NoNeg};

/// In v1's `METHOD_IDS` order.
pub const ENDPOINTS: &[Endpoint] = &[
    ep(
        "account.byRiotId",
        Account,
        "/riot/account/v1/accounts/by-riot-id/{gameName}/{tagLine}",
        &[],
        "account",
        secs(86_400),
        true,
        NegAccount,
    ),
    ep(
        "account.byPuuid",
        Account,
        "/riot/account/v1/accounts/by-puuid/{puuid}",
        &[],
        "account",
        secs(86_400),
        true,
        NegAccount,
    ),
    ep(
        "summoner.byPuuid",
        OnPlatform,
        "/lol/summoner/v4/summoners/by-puuid/{puuid}",
        &[],
        "summoner",
        secs(3600),
        true,
        NegDefault,
    ),
    ep(
        "league.entriesByPuuid",
        OnPlatform,
        "/lol/league/v4/entries/by-puuid/{puuid}",
        &[],
        "league",
        secs(300),
        false,
        NegDefault,
    ),
    // Ladder reads share one override key and are persisted: a full page walk is
    // thousands of calls to rebuild (design/04).
    ep(
        "league.challenger",
        OnPlatform,
        "/lol/league/v4/challengerleagues/by-queue/{queue}",
        &[],
        "ladder",
        secs(120),
        true,
        NoNeg,
    ),
    ep(
        "league.grandmaster",
        OnPlatform,
        "/lol/league/v4/grandmasterleagues/by-queue/{queue}",
        &[],
        "ladder",
        secs(120),
        true,
        NoNeg,
    ),
    ep(
        "league.master",
        OnPlatform,
        "/lol/league/v4/masterleagues/by-queue/{queue}",
        &[],
        "ladder",
        secs(120),
        true,
        NoNeg,
    ),
    ep(
        "league.entriesByTier",
        OnPlatform,
        "/lol/league/v4/entries/{queue}/{tier}/{division}",
        &["page"],
        "ladder",
        secs(120),
        true,
        NoNeg,
    ),
    ep(
        "match.idsByPuuid",
        Regional,
        "/lol/match/v5/matches/by-puuid/{puuid}/ids",
        &["start", "count", "queue", "type", "startTime", "endTime"],
        "matchIds",
        secs(120),
        false,
        NegDefault,
    ),
    ep(
        "match.byId",
        Regional,
        "/lol/match/v5/matches/{matchId}",
        &[],
        "match",
        None,
        false,
        NegDefault,
    ),
    ep(
        "match.timeline",
        Regional,
        "/lol/match/v5/matches/{matchId}/timeline",
        &[],
        "timeline",
        None,
        false,
        NegDefault,
    ),
    ep(
        "spectator.activeGame",
        OnPlatform,
        "/lol/spectator/v5/active-games/by-summoner/{puuid}",
        &[],
        "spectator",
        secs(30),
        false,
        NegDefault,
    ),
    ep(
        "mastery.byPuuid",
        OnPlatform,
        "/lol/champion-mastery/v4/champion-masteries/by-puuid/{puuid}",
        &[],
        "mastery",
        secs(3600),
        true,
        NegDefault,
    ),
    ep(
        "mastery.topByPuuid",
        OnPlatform,
        "/lol/champion-mastery/v4/champion-masteries/by-puuid/{puuid}/top",
        &["count"],
        "mastery",
        secs(3600),
        true,
        NegDefault,
    ),
    // design/04's table says rotations are not persisted, although its ≥ 1 h rule
    // would include them. The explicit table wins (ADR-016).
    ep(
        "platform.championRotations",
        OnPlatform,
        "/lol/platform/v3/champion-rotations",
        &[],
        "rotations",
        secs(21_600),
        false,
        NoNeg,
    ),
    ep(
        "status.platformData",
        OnPlatform,
        "/lol/status/v4/platform-data",
        &[],
        "status",
        secs(60),
        false,
        NoNeg,
    ),
];

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PathError {
    #[error("{id} takes {expected} path parameters, got {got}")]
    Arity {
        id: &'static str,
        expected: usize,
        got: usize,
    },
}

/// Which host a request goes to, already resolved from the caller's routing value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Platform(Platform),
    Region(Region),
}

impl Target {
    pub fn host(self) -> String {
        match self {
            Self::Platform(p) => p.host(),
            Self::Region(r) => r.host(),
        }
    }

    /// The limiter's app-level scope: the platform or region string on the host (v1).
    pub fn scope(self) -> &'static str {
        match self {
            Self::Platform(p) => p.as_str(),
            Self::Region(r) => r.as_str(),
        }
    }
}

impl Endpoint {
    pub fn by_id(id: &str) -> Option<&'static Endpoint> {
        ENDPOINTS.iter().find(|e| e.id == id)
    }

    /// Names of the `{…}` segments in the template, in order.
    pub fn path_params(&self) -> Vec<&'static str> {
        self.path_template
            .split('/')
            .filter_map(|seg| seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')))
            .collect()
    }

    /// Fill the template, percent-encoding each value like JS `encodeURIComponent`
    /// (Riot IDs legitimately contain spaces and non-ASCII characters; v1 did this).
    pub fn path(&self, values: &[&str]) -> Result<String, PathError> {
        let expected = self.path_params().len();
        if values.len() != expected {
            return Err(PathError::Arity {
                id: self.id,
                expected,
                got: values.len(),
            });
        }
        let mut values = values.iter();
        let segments: Vec<String> = self
            .path_template
            .split('/')
            .map(|seg| {
                if seg.starts_with('{') {
                    values.next().map(|v| encode_component(v)).unwrap_or_default()
                } else {
                    seg.to_string()
                }
            })
            .collect();
        Ok(segments.join("/"))
    }

    /// Resolve the host for a platform. Account and regional methods route through
    /// the platform's region (account-v1 skipping `sea`).
    pub fn target_for_platform(&self, platform: Platform) -> Target {
        match self.host {
            HostKind::Platform => Target::Platform(platform),
            HostKind::Regional => Target::Region(platform.region()),
            HostKind::Account => Target::Region(platform.account_region()),
        }
    }

    /// Resolve the host for a region. `None` for platform-hosted methods, which a
    /// region cannot address. Account methods called with `sea` go to `asia`.
    pub fn target_for_region(&self, region: Region) -> Option<Target> {
        match self.host {
            HostKind::Platform => None,
            HostKind::Regional => Some(Target::Region(region)),
            HostKind::Account => Some(Target::Region(region.account_region())),
        }
    }
}

/// JS `encodeURIComponent`: everything except `A-Z a-z 0-9 - _ . ! ~ * ' ( )`.
pub fn encode_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        if b.is_ascii_alphanumeric()
            || matches!(b, b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')')
        {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// v1 `parseTtlOverrides`: `league=120,spectator=20` → seconds by override key.
/// Malformed pairs, empty keys, and negative or non-numeric values are skipped.
pub fn parse_ttl_overrides(csv: &str) -> BTreeMap<String, u64> {
    let mut out = BTreeMap::new();
    for pair in csv.split(',') {
        let Some((key, value)) = pair.trim().split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        let Ok(seconds) = value.trim().parse::<f64>() else {
            continue;
        };
        if seconds.is_finite() && seconds >= 0.0 {
            // v1 kept fractional seconds; the cache works in whole seconds.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            out.insert(key.to_string(), seconds as u64);
        }
    }
    out
}

/// TTLs for one endpoint under the running config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ttls {
    /// Fresh until. `None` for immutable endpoints.
    pub soft: Option<Duration>,
    /// Servable as `STALE` until. `None` for immutable endpoints.
    pub hard: Option<Duration>,
    /// How long an upstream 404 is cached. `None` = not negatively cached.
    pub negative: Option<Duration>,
}

/// The config-dependent part of the registry: overrides, negative TTLs, SWR.
#[derive(Debug, Clone)]
pub struct TtlPolicy {
    overrides: BTreeMap<String, u64>,
    neg_default: Duration,
    neg_account: Duration,
    stale_while_revalidate: bool,
}

impl TtlPolicy {
    pub fn from_config(config: &crate::config::Config) -> Self {
        Self {
            overrides: parse_ttl_overrides(&config.cache_ttl_overrides),
            neg_default: Duration::from_secs(config.neg_ttl_seconds.into()),
            neg_account: Duration::from_secs(config.neg_ttl_account_seconds.into()),
            stale_while_revalidate: config.stale_while_revalidate,
        }
    }

    pub fn ttls(&self, endpoint: &Endpoint) -> Ttls {
        let soft = endpoint.soft_ttl.map(|ttl| {
            self.overrides
                .get(endpoint.override_key)
                .map_or(ttl, |s| Duration::from_secs(*s))
        });
        let multiplier = if self.stale_while_revalidate {
            STALE_MULTIPLIER
        } else {
            1
        };
        Ttls {
            soft,
            hard: soft.map(|s| s * multiplier),
            negative: match endpoint.negative {
                NegTtl::None => None,
                NegTtl::Default => Some(self.neg_default),
                NegTtl::Account => Some(self.neg_account),
            },
        }
    }

    /// Override keys that change nothing: unknown keys, or keys naming only
    /// immutable endpoints (`match`, `timeline`). Logged at warn on boot.
    pub fn ineffective_overrides(&self) -> Vec<String> {
        self.overrides
            .keys()
            .filter(|k| {
                !ENDPOINTS
                    .iter()
                    .any(|e| e.override_key == k.as_str() && !e.immutable)
            })
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Sources};

    fn policy(extra: &[(&str, &str)]) -> TtlPolicy {
        let mut env = vec![("RIOT_API_KEY".to_string(), "RGAPI-test-key-not-real".to_string())];
        env.extend(extra.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        TtlPolicy::from_config(
            &Config::from_sources(Sources {
                env,
                ..Sources::default()
            })
            .unwrap(),
        )
    }

    fn ep(id: &str) -> &'static Endpoint {
        Endpoint::by_id(id).unwrap()
    }

    fn soft(p: &TtlPolicy, id: &str) -> Option<u64> {
        p.ttls(ep(id)).soft.map(|d| d.as_secs())
    }

    /// `cargo test endpoints::parity`: every v1 method id, and nothing else.
    #[test]
    fn parity() {
        let v1: Vec<&str> = include_str!("../../docs/contract/v1-endpoints.txt")
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect();
        let ours: Vec<&str> = ENDPOINTS.iter().map(|e| e.id).collect();
        assert_eq!(ours, v1);
    }

    /// v1 "uses the documented TTL table", plus design/04 hard TTLs (×4).
    #[test]
    fn documented_ttl_table() {
        let p = policy(&[]);
        let table: &[(&str, u64, u64)] = &[
            ("account.byRiotId", 86_400, 345_600),
            ("account.byPuuid", 86_400, 345_600),
            ("summoner.byPuuid", 3600, 14_400),
            ("league.entriesByPuuid", 300, 1200),
            ("league.challenger", 120, 480),
            ("league.grandmaster", 120, 480),
            ("league.master", 120, 480),
            ("league.entriesByTier", 120, 480),
            ("match.idsByPuuid", 120, 480),
            ("spectator.activeGame", 30, 120),
            ("mastery.byPuuid", 3600, 14_400),
            ("mastery.topByPuuid", 3600, 14_400),
            ("platform.championRotations", 21_600, 86_400),
            ("status.platformData", 60, 240),
        ];
        for &(id, s, h) in table {
            let t = p.ttls(ep(id));
            assert_eq!(t.soft, Some(Duration::from_secs(s)), "{id} soft");
            assert_eq!(t.hard, Some(Duration::from_secs(h)), "{id} hard");
        }
        assert_eq!(table.len() + 2, ENDPOINTS.len(), "plus the two immutable ones");
    }

    /// v1 "treats matches and timelines as immutable".
    #[test]
    fn matches_and_timelines_are_immutable() {
        let p = policy(&[]);
        for id in ["match.byId", "match.timeline"] {
            assert!(ep(id).immutable);
            assert_eq!(p.ttls(ep(id)).soft, None);
            assert_eq!(p.ttls(ep(id)).hard, None);
        }
        assert_eq!(ENDPOINTS.iter().filter(|e| e.immutable).count(), 2);
    }

    /// v1 "routes account-v1 and match-v5 regionally, everything else per platform".
    #[test]
    fn host_families() {
        let mut regional: Vec<&str> = ENDPOINTS
            .iter()
            .filter(|e| e.host != HostKind::Platform)
            .map(|e| e.id)
            .collect();
        regional.sort();
        assert_eq!(
            regional,
            [
                "account.byPuuid",
                "account.byRiotId",
                "match.byId",
                "match.idsByPuuid",
                "match.timeline"
            ]
        );
        for e in ENDPOINTS {
            assert_eq!(
                e.host == HostKind::Account,
                e.id.starts_with("account."),
                "{}",
                e.id
            );
        }
    }

    #[test]
    fn l2_membership_matches_design_04() {
        let mut persisted: Vec<&str> = ENDPOINTS.iter().filter(|e| e.persist_l2).map(|e| e.id).collect();
        persisted.sort();
        assert_eq!(
            persisted,
            [
                "account.byPuuid",
                "account.byRiotId",
                "league.challenger",
                "league.entriesByTier",
                "league.grandmaster",
                "league.master",
                "mastery.byPuuid",
                "mastery.topByPuuid",
                "summoner.byPuuid",
            ]
        );
    }

    #[test]
    fn negative_ttls_follow_v1() {
        let p = policy(&[("NEG_TTL_SECONDS", "31"), ("NEG_TTL_ACCOUNT_SECONDS", "301")]);
        let neg = |id| p.ttls(ep(id)).negative.map(|d| d.as_secs());
        assert_eq!(neg("account.byRiotId"), Some(301));
        assert_eq!(neg("account.byPuuid"), Some(301));
        assert_eq!(neg("spectator.activeGame"), Some(31));
        assert_eq!(neg("summoner.byPuuid"), Some(31));
        assert_eq!(neg("match.byId"), Some(31));
        for id in [
            "league.challenger",
            "league.entriesByTier",
            "platform.championRotations",
            "status.platformData",
        ] {
            assert_eq!(neg(id), None, "{id}");
        }
    }

    /// v1 "gives every ladder read one override key, distinct from per-player league entries".
    #[test]
    fn ladder_reads_share_one_override_key() {
        for id in [
            "league.challenger",
            "league.grandmaster",
            "league.master",
            "league.entriesByTier",
        ] {
            assert_eq!(ep(id).override_key, "ladder", "{id}");
            assert_eq!(ep(id).host, HostKind::Platform);
            assert!(!ep(id).immutable);
        }
        assert_eq!(ep("league.entriesByPuuid").override_key, "league");
    }

    /// v1 test/config.test.ts "parses the CACHE_TTL_OVERRIDES CSV format".
    #[test]
    fn parses_overrides_like_v1() {
        let m = |pairs: &[(&str, u64)]| {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), *v))
                .collect::<BTreeMap<_, _>>()
        };
        assert_eq!(
            parse_ttl_overrides("league=120,spectator=20"),
            m(&[("league", 120), ("spectator", 20)])
        );
        assert_eq!(parse_ttl_overrides(" league = 120 "), m(&[("league", 120)]));
        assert_eq!(parse_ttl_overrides(""), m(&[]));
        assert_eq!(parse_ttl_overrides("garbage,league=abc,=5,x=-1"), m(&[]));
    }

    #[test]
    fn overrides_change_soft_and_hard_for_every_method_sharing_the_key() {
        let p = policy(&[(
            "CACHE_TTL_OVERRIDES",
            "league=120,spectator=20,ladder=60,match=5,typo=1",
        )]);
        assert_eq!(soft(&p, "league.entriesByPuuid"), Some(120));
        assert_eq!(soft(&p, "spectator.activeGame"), Some(20));
        assert_eq!(
            p.ttls(ep("spectator.activeGame")).hard,
            Some(Duration::from_secs(80))
        );
        for id in ["league.challenger", "league.entriesByTier"] {
            assert_eq!(soft(&p, id), Some(60), "{id}");
        }
        assert_eq!(soft(&p, "match.byId"), None, "immutable ignores overrides");
        assert_eq!(p.ineffective_overrides(), ["match", "typo"]);
    }

    #[test]
    fn without_swr_hard_equals_soft() {
        let p = policy(&[("STALE_WHILE_REVALIDATE", "false")]);
        assert_eq!(
            p.ttls(ep("summoner.byPuuid")).hard,
            Some(Duration::from_secs(3600))
        );
    }

    #[test]
    fn paths_fill_and_encode_like_encode_uri_component() {
        assert_eq!(
            ep("account.byRiotId").path(&["Faker", "KR1"]).unwrap(),
            "/riot/account/v1/accounts/by-riot-id/Faker/KR1"
        );
        // v1 "encodes Riot IDs containing spaces and non-ASCII characters".
        assert_eq!(
            ep("account.byRiotId").path(&["Hide on bush", "KR1"]).unwrap(),
            "/riot/account/v1/accounts/by-riot-id/Hide%20on%20bush/KR1"
        );
        assert_eq!(encode_component("Ünïcode/#?&"), "%C3%9Cn%C3%AFcode%2F%23%3F%26");
        assert_eq!(encode_component("a-b_c.d!e~f*g'h(i)"), "a-b_c.d!e~f*g'h(i)");
        assert_eq!(
            ep("league.challenger").path(&["RANKED_SOLO_5x5"]).unwrap(),
            "/lol/league/v4/challengerleagues/by-queue/RANKED_SOLO_5x5"
        );
        assert_eq!(
            ep("league.entriesByTier")
                .path(&["RANKED_SOLO_5x5", "DIAMOND", "I"])
                .unwrap(),
            "/lol/league/v4/entries/RANKED_SOLO_5x5/DIAMOND/I"
        );
        assert_eq!(
            ep("platform.championRotations").path(&[]).unwrap(),
            "/lol/platform/v3/champion-rotations"
        );
        assert_eq!(
            ep("match.byId").path(&[]),
            Err(PathError::Arity {
                id: "match.byId",
                expected: 1,
                got: 0
            })
        );
    }

    /// v1 "builds regional URLs", "only accepts account hosts, so a sea caller must
    /// convert first", "still routes sea match-v5 to the sea host", "builds platform URLs".
    #[test]
    fn targets_resolve_to_the_right_host() {
        let t = ep("account.byRiotId").target_for_region(Region::Europe).unwrap();
        assert_eq!(
            (t.host(), t.scope()),
            ("europe.api.riotgames.com".to_string(), "europe")
        );

        let t = ep("account.byRiotId").target_for_region(Region::Sea).unwrap();
        assert_eq!(
            (t.host(), t.scope()),
            ("asia.api.riotgames.com".to_string(), "asia")
        );
        let t = ep("account.byPuuid").target_for_platform(Platform::Oc1);
        assert_eq!(t.scope(), "asia");

        assert_eq!(
            ep("match.byId").target_for_region(Region::Sea).unwrap().host(),
            "sea.api.riotgames.com"
        );
        assert_eq!(ep("match.byId").target_for_platform(Platform::Oc1).scope(), "sea");

        let t = ep("summoner.byPuuid").target_for_platform(Platform::Euw1);
        assert_eq!(
            (t.host(), t.scope()),
            ("euw1.api.riotgames.com".to_string(), "euw1")
        );
        assert_eq!(ep("summoner.byPuuid").target_for_region(Region::Europe), None);
    }

    #[test]
    fn method_scope_key_is_the_method_id_and_ids_are_unique() {
        let mut ids: Vec<&str> = ENDPOINTS.iter().map(|e| e.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), ENDPOINTS.len());
        assert!(ENDPOINTS.iter().all(|e| e.method_scope_key == e.id));
    }

    /// P1 exit check: every endpoint, in all nine groups, resolves to the right host
    /// from one platform per region. Platform-hosted groups stay on the platform;
    /// match-v5 goes to the platform's region; account-v1 does too, except SEA → asia.
    #[test]
    fn every_endpoint_group_resolves_to_the_right_host() {
        let groups = [
            ("account", "account-v1"),
            ("summoner", "summoner-v4"),
            ("league", "league-v4"),
            ("match", "match-v5"),
            ("spectator", "spectator-v5"),
            ("mastery", "champion-mastery-v4"),
            ("platform", "champion rotations"),
            ("status", "lol-status-v4"),
        ];
        // Ladder reads are the ninth group, prefixed "league." like per-player entries.
        assert_eq!(ENDPOINTS.iter().filter(|e| e.override_key == "ladder").count(), 4);
        for e in ENDPOINTS {
            let group = e.id.split('.').next().unwrap();
            assert!(groups.iter().any(|(g, _)| *g == group), "{} has no group", e.id);
        }

        let cases = [
            (Platform::Na1, "na1", "americas", "americas"),
            (Platform::Euw1, "euw1", "europe", "europe"),
            (Platform::Kr, "kr", "asia", "asia"),
            (Platform::Oc1, "oc1", "sea", "asia"),
            (Platform::Vn2, "vn2", "sea", "asia"),
        ];
        for e in ENDPOINTS {
            for (platform, plat, match_region, account_region) in cases {
                let expected = match e.id.split('.').next().unwrap() {
                    "account" => account_region,
                    "match" => match_region,
                    _ => plat,
                };
                assert_eq!(
                    e.target_for_platform(platform).host(),
                    format!("{expected}.api.riotgames.com"),
                    "{} from {plat}",
                    e.id
                );
            }
        }
    }
}
