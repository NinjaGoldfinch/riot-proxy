//! Which rate-limit lane a job runs in (SCH-01, design/06 §Claiming): the
//! limiter scope its requests hit (`Target::scope()`: the platform for
//! league/spectator, the region for match-v5) and the endpoint it mainly
//! calls. Derived from the kind and payload at enqueue, so every producer of
//! a kind gets the same answer. Kinds that never call Riot have no lane and
//! can always be claimed.

use serde_json::Value;

use crate::jobs::kinds;
use crate::riot::endpoints::Endpoint;
use crate::riot::ladder::apex_endpoint;
use crate::riot::routing::{Platform, Region};

/// A job's lane and main method.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lane {
    pub lane: &'static str,
    pub method: &'static str,
}

fn str_field<'a>(payload: &'a Value, key: &str) -> Option<&'a str> {
    payload.get(key).and_then(Value::as_str)
}

fn on(method: &'static str, platform: Platform) -> Option<Lane> {
    let endpoint = Endpoint::by_id(method)?;
    Some(Lane {
        lane: endpoint.target_for_platform(platform).scope(),
        method: endpoint.method_scope_key,
    })
}

/// The lane of a `kind` job with this payload; `None` for kinds that make
/// no Riot calls, or a payload too broken to say (the handler fails it).
pub fn of(kind: &str, payload: &Value) -> Option<Lane> {
    let platform = || str_field(payload, "platform").and_then(|p| Platform::parse(p).ok());
    match kind {
        kinds::POLL_LIVE => on("spectator.activeGame", platform()?),
        kinds::POLL_RANK | kinds::RANKS_LOOKUP => on("league.entriesByPuuid", platform()?),
        kinds::POLL_MATCHES | kinds::BACKFILL_PLAYER | kinds::LADDER_COLLECT => {
            on("match.idsByPuuid", platform()?)
        }
        kinds::ARCHIVE_MATCH => on(
            "match.byId",
            Platform::from_match_id(str_field(payload, "matchId")?)?,
        ),
        kinds::LADDER_WALK => on("league.entriesByTier", platform()?),
        kinds::LADDER_APEX => on(apex_endpoint(str_field(payload, "tier")?)?, platform()?),
        _ => None,
    }
}

/// Every lane [`of`] can name: each platform and each region. The claim
/// looks at these lanes (less the blocked ones) and at jobs with no lane.
pub fn all() -> impl Iterator<Item = &'static str> {
    Platform::ALL
        .iter()
        .map(|p| p.as_str())
        .chain(Region::ALL.iter().map(|r| r.as_str()))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn lane(kind: &str, payload: Value) -> Option<(&'static str, &'static str)> {
        of(kind, &payload).map(|l| (l.lane, l.method))
    }

    #[test]
    fn riot_kinds_name_the_scope_their_requests_hit() {
        let p = json!({"puuid": "p", "platform": "na1"});
        assert_eq!(
            lane(kinds::POLL_LIVE, p.clone()),
            Some(("na1", "spectator.activeGame"))
        );
        assert_eq!(
            lane(kinds::POLL_RANK, p.clone()),
            Some(("na1", "league.entriesByPuuid"))
        );
        assert_eq!(
            lane(
                kinds::RANKS_LOOKUP,
                json!({"platform": "oc1", "queue": "RANKED_SOLO_5x5"})
            ),
            Some(("oc1", "league.entriesByPuuid"))
        );
        assert_eq!(
            lane(kinds::POLL_MATCHES, p.clone()),
            Some(("americas", "match.idsByPuuid"))
        );
        assert_eq!(
            lane(kinds::BACKFILL_PLAYER, json!({"puuid": "p", "platform": "KR"})),
            Some(("asia", "match.idsByPuuid"))
        );
        assert_eq!(
            lane(kinds::ARCHIVE_MATCH, json!({"matchId": "OC1_123"})),
            Some(("sea", "match.byId"))
        );
        let leg = json!({"crawlId": "c", "platform": "euw1", "queue": "RANKED_SOLO_5x5", "tier": "DIAMOND", "division": "I"});
        assert_eq!(
            lane(kinds::LADDER_WALK, leg.clone()),
            Some(("euw1", "league.entriesByTier"))
        );
        let mut apex = leg;
        apex["tier"] = json!("GRANDMASTER");
        assert_eq!(
            lane(kinds::LADDER_APEX, apex),
            Some(("euw1", "league.grandmaster"))
        );
        assert_eq!(
            lane(
                kinds::LADDER_COLLECT,
                json!({"crawlId": "c", "platform": "euw1", "queue": "q", "puuids": [], "offset": 0})
            ),
            Some(("europe", "match.idsByPuuid"))
        );
    }

    #[test]
    fn kinds_without_riot_calls_and_broken_payloads_have_no_lane() {
        for kind in [
            kinds::LADDER_CRAWL,
            kinds::LADDER_ARCHIVE,
            kinds::NAMES_BACKFILL,
            kinds::AGGREGATE_ANALYTICS,
            kinds::FACTS_REEXTRACT,
            kinds::MAINTENANCE,
            kinds::DDRAGON_SYNC,
        ] {
            assert_eq!(lane(kind, json!({"platform": "na1"})), None, "{kind}");
        }
        assert_eq!(lane(kinds::POLL_LIVE, json!({"puuid": "p"})), None);
        assert_eq!(lane(kinds::ARCHIVE_MATCH, json!({"matchId": "nope"})), None);
    }

    /// A lane outside `all()` would never be claimed.
    #[test]
    fn every_lane_a_job_can_have_is_one_the_claim_looks_at() {
        let all: Vec<&str> = all().collect();
        for platform in Platform::ALL {
            let p = json!({"platform": platform.as_str(), "tier": "MASTER", "matchId": format!("{}_1", platform.as_str())});
            for kind in [
                kinds::POLL_LIVE,
                kinds::POLL_RANK,
                kinds::POLL_MATCHES,
                kinds::BACKFILL_PLAYER,
                kinds::ARCHIVE_MATCH,
                kinds::LADDER_APEX,
                kinds::LADDER_WALK,
                kinds::LADDER_COLLECT,
                kinds::RANKS_LOOKUP,
            ] {
                let l = of(kind, &p).unwrap_or_else(|| panic!("{kind} on {platform:?}"));
                assert!(all.contains(&l.lane), "{kind}: {}", l.lane);
            }
        }
    }
}
