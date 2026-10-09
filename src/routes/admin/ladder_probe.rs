//! `POST /v1/admin/ladder/probe` (LAD-03): asks Riot, on demand, the questions
//! that found the apex list cap (IMPLEMENTATION.md §Post-release — LAD), so
//! the owner can re-run them from `/dev` on any shard instead of by hand.
//!
//! Three checks, each answering `confirmed` (Riot still does what was seen on
//! 2026-10-09), `not-seen` (it doesn't apply here, e.g. a shard with fewer
//! Master players than the cap), `changed` (Riot now answers differently) or
//! `error`:
//!
//! - `master-capped`: `masterleagues` lists exactly [`RIOT_APEX_LIST_CAP`].
//! - `exp-same-list`: league-exp-v4's Master pages hold nobody `masterleagues`
//!   lacks, beyond what the ladder moving under the walk explains.
//! - `paged-refuses-apex`: `entries/{queue}/MASTER/I` is refused, so the paged
//!   walk can't reach the players either.
//!
//! The analysis is a pure function of what Riot answered ([`analyse`]); only
//! [`probe`] calls Riot. Every call skips the cache read, so the answer is
//! Riot's now, and goes through the limiter at interactive priority.

use std::collections::HashSet;
use std::time::Instant;

use axum::Extension;
use axum::body::Bytes;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use super::{Who, body_enum, default_ladder_queue, ok};
use crate::app::AppState;
use crate::fetcher::{FetchOptions, Fetcher};
use crate::http::body::Body;
use crate::http::{ApiError, validate};
use crate::riot::endpoints::Endpoint;
use crate::riot::ladder::{APEX_TIERS, RANKED_QUEUES, RIOT_APEX_LIST_CAP, apex_endpoint};
use crate::riot::limiter::Priority;
use crate::riot::routing::Platform;
use crate::routes::passthrough::{LocalErrors, request};

/// league-exp pages walked at most: 20,500 entries, twice the cap, so a list
/// longer than the cap still shows as one.
pub const MAX_EXP_PAGES: u32 = 100;
/// league-exp pages asked for at once.
pub const EXP_BATCH: u32 = 10;
/// Players the two Master lists may differ by because the ladder moves while
/// league-exp is paged. The owner's check saw 1 and 2 of 10,000 (kr, euw1).
pub const CHURN_TOLERANCE: usize = 10;

/// One player in a list, as much as the probe needs.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Seen {
    pub puuid: Option<String>,
    #[serde(default)]
    pub league_points: i64,
}

#[derive(Debug, Deserialize)]
struct LeagueList {
    #[serde(default)]
    entries: Vec<Seen>,
}

/// What league-exp answered for MASTER/I, page by page.
#[derive(Debug, Default)]
pub struct ExpWalk {
    /// Entries per non-empty page, in order.
    pub pages: Vec<usize>,
    pub players: Vec<Seen>,
    /// Stopped at [`MAX_EXP_PAGES`] before an empty page.
    pub truncated: bool,
    /// The page that failed, after the pages before it.
    pub error: Option<String>,
}

/// Everything Riot answered, before any judgement.
#[derive(Debug)]
pub struct Answers {
    /// Per apex tier, ascending.
    pub lists: Vec<(&'static str, Result<Vec<Seen>, String>)>,
    pub exp: ExpWalk,
    /// Entries on `entries/{queue}/MASTER/I?page=1`, or why it was refused.
    pub paged_master: Result<usize, String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ListReport {
    pub tier: &'static str,
    pub entries: Option<usize>,
    pub lowest_lp: Option<i64>,
    /// At least [`RIOT_APEX_LIST_CAP`] entries: Riot may have left players out.
    pub capped: bool,
    pub error: Option<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExpReport {
    pub pages: usize,
    pub entries: usize,
    pub last_page_size: Option<usize>,
    pub distinct: usize,
    /// Set only when `masterleagues` answered too.
    pub in_both: Option<usize>,
    pub only_in_exp: Option<usize>,
    pub only_in_master: Option<usize>,
    pub truncated: bool,
    pub error: Option<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PagedReport {
    /// `refused` or `answered`.
    pub status: &'static str,
    pub entries: Option<usize>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    Confirmed,
    NotSeen,
    Changed,
    Error,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Check {
    pub id: &'static str,
    pub title: &'static str,
    pub verdict: Verdict,
    pub detail: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub cap: usize,
    pub churn_tolerance: usize,
    pub summary: String,
    pub checks: Vec<Check>,
    pub lists: Vec<ListReport>,
    pub exp: ExpReport,
    pub paged_master: PagedReport,
}

fn puuids(players: &[Seen]) -> HashSet<&str> {
    players.iter().filter_map(|p| p.puuid.as_deref()).collect()
}

/// Judge Riot's answers against the cap (pure; the unit tests drive it).
#[allow(clippy::too_many_lines)]
pub fn analyse(a: &Answers, cap: usize, tolerance: usize) -> Report {
    let lists: Vec<ListReport> = a
        .lists
        .iter()
        .map(|(tier, r)| match r {
            Ok(players) => ListReport {
                tier,
                entries: Some(players.len()),
                lowest_lp: players.iter().map(|p| p.league_points).min(),
                capped: players.len() >= cap,
                error: None,
            },
            Err(e) => ListReport {
                tier,
                entries: None,
                lowest_lp: None,
                capped: false,
                error: Some(e.clone()),
            },
        })
        .collect();
    let master = a
        .lists
        .iter()
        .find(|(t, _)| *t == "MASTER")
        .map(|(_, r)| r.as_ref());

    let master_check = match master {
        Some(Ok(players)) => {
            let n = players.len();
            let lowest = players.iter().map(|p| p.league_points).min().unwrap_or(0);
            let (verdict, detail) = match n.cmp(&cap) {
                std::cmp::Ordering::Equal => (
                    Verdict::Confirmed,
                    format!(
                        "masterleagues lists exactly {n} players, the lowest on {lowest} LP. Master players below that are not returned."
                    ),
                ),
                std::cmp::Ordering::Less => (
                    Verdict::NotSeen,
                    format!(
                        "masterleagues lists {n} players, under the cap of {cap}: this Master list is complete."
                    ),
                ),
                std::cmp::Ordering::Greater => (
                    Verdict::Changed,
                    format!(
                        "masterleagues lists {n} players, more than the cap of {cap}: Riot may have raised it."
                    ),
                ),
            };
            Check {
                id: "master-capped",
                title: "masterleagues stops at the cap",
                verdict,
                detail,
            }
        }
        Some(Err(e)) => Check {
            id: "master-capped",
            title: "masterleagues stops at the cap",
            verdict: Verdict::Error,
            detail: format!("masterleagues failed: {e}"),
        },
        None => Check {
            id: "master-capped",
            title: "masterleagues stops at the cap",
            verdict: Verdict::Error,
            detail: "masterleagues was not asked".into(),
        },
    };

    let exp_ids = puuids(&a.exp.players);
    let master_ids = master.and_then(Result::ok).map(|p| puuids(p));
    let overlap = master_ids.as_ref().map(|m| {
        let both = exp_ids.intersection(m).count();
        (both, exp_ids.len() - both, m.len() - both)
    });
    let exp = ExpReport {
        pages: a.exp.pages.len(),
        entries: a.exp.pages.iter().sum(),
        last_page_size: a.exp.pages.last().copied(),
        distinct: exp_ids.len(),
        in_both: overlap.map(|o| o.0),
        only_in_exp: overlap.map(|o| o.1),
        only_in_master: overlap.map(|o| o.2),
        truncated: a.exp.truncated,
        error: a.exp.error.clone(),
    };
    let exp_title = "league-exp lists nobody masterleagues lacks";
    let exp_check = match (&a.exp.error, overlap) {
        (Some(e), _) => Check {
            id: "exp-same-list",
            title: exp_title,
            verdict: Verdict::Error,
            detail: format!("league-exp page {} failed: {e}", a.exp.pages.len() + 1),
        },
        (None, None) => Check {
            id: "exp-same-list",
            title: exp_title,
            verdict: Verdict::Error,
            detail: format!(
                "league-exp paged {} players, but masterleagues failed, so there is nothing to compare with.",
                exp.distinct
            ),
        },
        (None, Some((both, only_exp, only_master))) => {
            let pages = format!(
                "{} pages ({} on the last), {} players, {both} in both lists",
                exp.pages,
                exp.last_page_size.unwrap_or(0),
                exp.distinct
            );
            let more = if a.exp.truncated {
                format!(" It was still going after {MAX_EXP_PAGES} pages.")
            } else {
                String::new()
            };
            if only_exp > tolerance || a.exp.truncated {
                Check {
                    id: "exp-same-list",
                    title: exp_title,
                    verdict: Verdict::Changed,
                    detail: format!(
                        "{pages}: {only_exp} are only in league-exp, more than the ladder moving explains (up to {tolerance}).{more}"
                    ),
                }
            } else {
                Check {
                    id: "exp-same-list",
                    title: exp_title,
                    verdict: Verdict::Confirmed,
                    detail: format!(
                        "{pages}. {only_exp} only in league-exp and {only_master} only in masterleagues: the ladder moving while it is paged."
                    ),
                }
            }
        }
    };

    let (paged_master, paged_check) = match &a.paged_master {
        Err(e) => (
            PagedReport {
                status: "refused",
                entries: None,
                error: Some(e.clone()),
            },
            Check {
                id: "paged-refuses-apex",
                title: "entries/…/MASTER/I is refused",
                verdict: Verdict::Confirmed,
                detail: format!(
                    "Refused ({e}); on 2026-10-09 Riot answered 400. The paged walk can't reach Master."
                ),
            },
        ),
        Ok(n) => (
            PagedReport {
                status: "answered",
                entries: Some(*n),
                error: None,
            },
            Check {
                id: "paged-refuses-apex",
                title: "entries/…/MASTER/I is refused",
                verdict: Verdict::Changed,
                detail: format!(
                    "Answered 200 with {n} entries on page 1: Riot now pages Master on this route."
                ),
            },
        ),
    };

    let checks = vec![master_check, exp_check, paged_check];
    let summary = if checks.iter().any(|c| c.verdict == Verdict::Changed) {
        "Riot answers differently from 2026-10-09: see the checks marked changed.".to_string()
    } else if checks.iter().any(|c| c.verdict == Verdict::Error) {
        "Some checks failed to run: see the errors below.".to_string()
    } else if checks[0].verdict == Verdict::Confirmed {
        format!("Capped: Riot returns the top {cap} Master players only, and no route here lists the rest.")
    } else {
        "Not capped: every Master player on this shard is listed.".to_string()
    };
    Report {
        cap,
        churn_tolerance: tolerance,
        summary,
        checks,
        lists,
        exp,
        paged_master,
    }
}

const FRESH: FetchOptions = FetchOptions {
    priority: Priority::Interactive,
    bypass: true,
    job: false,
};

async fn get(
    fetcher: &Fetcher,
    id: &'static str,
    platform: Platform,
    params: &[&str],
    page: Option<u32>,
) -> Result<Bytes, String> {
    let target = Endpoint::by_id(id).map(|e| e.target_for_platform(platform));
    let query = [("page", page.map(|p| p.to_string()))];
    let req =
        request(id, target, params, if page.is_some() { &query } else { &[] }).map_err(|e| e.message)?;
    fetcher
        .fetch(req, FRESH)
        .await
        .map(|r| r.body)
        .map_err(|e| format!("{} {}", e.api.code.as_str(), e.api.message))
}

/// Ask Riot everything [`analyse`] judges, on one platform and queue.
pub async fn probe(fetcher: &Fetcher, platform: Platform, queue: &str) -> Answers {
    let mut lists = Vec::new();
    for tier in APEX_TIERS {
        let Some(id) = apex_endpoint(tier) else { continue };
        let list = get(fetcher, id, platform, &[queue], None).await.and_then(|b| {
            serde_json::from_slice::<LeagueList>(&b)
                .map(|l| l.entries)
                .map_err(|e| format!("unreadable list: {e}"))
        });
        lists.push((tier, list));
    }
    let mut exp = ExpWalk::default();
    // Riot takes about a second a page, so pages go out in batches and are
    // read in order; a batch may ask for a few pages past the end.
    'walk: for first in (1..=MAX_EXP_PAGES).step_by(EXP_BATCH as usize) {
        let pages = first..=(first + EXP_BATCH - 1).min(MAX_EXP_PAGES);
        let batch = futures_util::future::join_all(pages.map(|page| async move {
            let got = get(
                fetcher,
                "league.expEntries",
                platform,
                &[queue, "MASTER", "I"],
                Some(page),
            )
            .await
            .and_then(|b| {
                serde_json::from_slice::<Vec<Seen>>(&b).map_err(|e| format!("unreadable page: {e}"))
            });
            (page, got)
        }))
        .await;
        for (page, got) in batch {
            match got {
                Ok(entries) if entries.is_empty() => break 'walk,
                Ok(entries) => {
                    exp.pages.push(entries.len());
                    exp.players.extend(entries);
                    exp.truncated = page == MAX_EXP_PAGES;
                }
                Err(e) => {
                    exp.error = Some(e);
                    break 'walk;
                }
            }
        }
    }
    let paged_master = get(
        fetcher,
        "league.entriesByTier",
        platform,
        &[queue, "MASTER", "I"],
        Some(1),
    )
    .await
    .and_then(|b| {
        serde_json::from_slice::<Vec<Seen>>(&b)
            .map(|e| e.len())
            .map_err(|e| format!("unreadable page: {e}"))
    });
    Answers {
        lists,
        exp,
        paged_master,
    }
}

/// `POST /v1/admin/ladder/probe` body.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct ProbeBody {
    /// Platform routing value.
    platform: String,
    /// `RANKED_SOLO_5x5` or `RANKED_FLEX_SR`; default the first of `LADDER_QUEUES`.
    queue: Option<String>,
}

#[utoipa::path(
    post, path = "/v1/admin/ladder/probe", tag = "admin",
    summary = "Check Riot's apex list cap on one ladder",
    description = "Asks Riot now, skipping the cache: the three apex leagues, league-exp-v4's MASTER/I pages, ten \
        at a time, until an empty one (at most 100), and `entries/{queue}/MASTER/I`. Judges each against the \
        10,000-entry cap seen on 2026-10-09: `confirmed`, `not-seen`, `changed` or `error`. About 55 Riot \
        calls on a capped shard, at interactive priority. Stores nothing.",
    request_body = ProbeBody,
    responses((status = 200, description = "`{platform, queue, tookMs, cap, churnTolerance, summary, checks, lists, exp, pagedMaster}`",
        body = serde_json::Value), LocalErrors),
)]
pub async fn ladder_probe(State(state): State<AppState>, Extension(_c): Who, bytes: Bytes) -> Response {
    let parsed = (|| {
        let b = Body::parse(&bytes)?;
        b.required(&["platform"])?;
        let platform = b
            .with("platform", validate::platform_at)?
            .ok_or_else(ApiError::internal)?;
        let queue = b.with("queue", |loc, v| body_enum(loc, "queue", v, &RANKED_QUEUES))?;
        Ok::<_, ApiError>((
            platform,
            queue.map_or_else(|| default_ladder_queue(&state), str::to_string),
        ))
    })();
    let (platform, queue) = match parsed {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };
    let started = Instant::now();
    let answers = probe(&state.fetcher, platform, &queue).await;
    let report = analyse(&answers, RIOT_APEX_LIST_CAP, CHURN_TOLERANCE);
    tracing::info!(platform = platform.as_str(), queue = %queue, summary = %report.summary, "ladder probe");
    let mut body = serde_json::to_value(&report).unwrap_or_default();
    if let Some(o) = body.as_object_mut() {
        o.insert("platform".into(), platform.as_str().into());
        o.insert("queue".into(), queue.into());
        o.insert(
            "tookMs".into(),
            u64::try_from(started.elapsed().as_millis())
                .unwrap_or(u64::MAX)
                .into(),
        );
    }
    ok(&body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn players(prefix: &str, n: usize) -> Vec<Seen> {
        (0..n)
            .map(|i| Seen {
                puuid: Some(format!("{prefix}{i}")),
                league_points: i64::try_from(1000 - i).unwrap_or(0),
            })
            .collect()
    }

    fn exp_of(players: Vec<Seen>, per_page: usize) -> ExpWalk {
        let pages = players.chunks(per_page).map(<[Seen]>::len).collect();
        ExpWalk {
            pages,
            players,
            truncated: false,
            error: None,
        }
    }

    fn answers(master: usize, exp: ExpWalk, paged: Result<usize, String>) -> Answers {
        Answers {
            lists: vec![
                ("MASTER", Ok(players("m", master))),
                ("GRANDMASTER", Ok(players("g", 7))),
                ("CHALLENGER", Ok(players("c", 3))),
            ],
            exp,
            paged_master: paged,
        }
    }

    fn verdicts(r: &Report) -> Vec<(&str, Verdict)> {
        r.checks.iter().map(|c| (c.id, c.verdict)).collect()
    }

    const REFUSED: Result<usize, String> = Err(String::new());

    #[test]
    fn a_capped_shard_confirms_all_three() {
        // As on euw1: league-exp pages the same list, one player moved meanwhile.
        let mut exp = players("m", 100);
        exp[99].puuid = Some("moved-in".into());
        let r = analyse(&answers(100, exp_of(exp, 21), REFUSED), 100, 2);
        assert_eq!(
            verdicts(&r),
            [
                ("master-capped", Verdict::Confirmed),
                ("exp-same-list", Verdict::Confirmed),
                ("paged-refuses-apex", Verdict::Confirmed),
            ]
        );
        assert!(r.summary.starts_with("Capped"), "{}", r.summary);
        assert_eq!(
            (
                r.exp.pages,
                r.exp.last_page_size,
                r.exp.in_both,
                r.exp.only_in_exp,
                r.exp.only_in_master
            ),
            (5, Some(16), Some(99), Some(1), Some(1))
        );
        assert_eq!(r.lists[0].lowest_lp, Some(901));
        assert_eq!(
            r.lists.iter().map(|l| l.capped).collect::<Vec<_>>(),
            [true, false, false],
            "only a list at the cap is capped"
        );
    }

    #[test]
    fn a_list_under_the_cap_is_complete() {
        let r = analyse(&answers(99, exp_of(players("m", 99), 21), REFUSED), 100, 2);
        assert_eq!(r.checks[0].verdict, Verdict::NotSeen);
        assert!(!r.lists[0].capped);
        assert!(r.summary.starts_with("Not capped"), "{}", r.summary);
    }

    #[test]
    fn more_than_the_cap_or_extra_exp_players_or_a_paged_master_is_a_change() {
        let over = analyse(&answers(101, exp_of(players("m", 101), 21), REFUSED), 100, 2);
        assert_eq!(over.checks[0].verdict, Verdict::Changed);

        let mut extra = players("m", 100);
        extra.extend(players("x", 3));
        let more = analyse(&answers(100, exp_of(extra, 21), REFUSED), 100, 2);
        assert_eq!(more.checks[1].verdict, Verdict::Changed);
        assert_eq!(more.exp.only_in_exp, Some(3));

        let mut long = exp_of(players("m", 100), 21);
        long.truncated = true;
        let truncated = analyse(&answers(100, long, REFUSED), 100, 2);
        assert_eq!(
            truncated.checks[1].verdict,
            Verdict::Changed,
            "still going at the page limit"
        );

        let paged = analyse(&answers(100, exp_of(players("m", 100), 21), Ok(205)), 100, 2);
        assert_eq!(paged.checks[2].verdict, Verdict::Changed);
        assert_eq!(paged.paged_master.status, "answered");
        assert!(paged.summary.contains("changed"), "{}", paged.summary);
    }

    #[test]
    fn a_failed_call_is_an_error_not_a_verdict() {
        let mut a = answers(100, exp_of(players("m", 100), 21), REFUSED);
        a.lists[0].1 = Err("UPSTREAM_ERROR".into());
        let r = analyse(&a, 100, 2);
        assert_eq!(
            (r.checks[0].verdict, r.checks[1].verdict),
            (Verdict::Error, Verdict::Error),
            "nothing to compare league-exp with"
        );
        assert_eq!(r.exp.only_in_exp, None);
        assert_eq!(r.lists[0].error.as_deref(), Some("UPSTREAM_ERROR"));
        assert!(r.summary.starts_with("Some checks failed"), "{}", r.summary);

        let mut b = answers(100, exp_of(players("m", 40), 20), REFUSED);
        b.exp.error = Some("RATE_LIMITED".into());
        let r = analyse(&b, 100, 2);
        assert_eq!(r.checks[1].verdict, Verdict::Error);
        assert!(r.checks[1].detail.contains("page 3"), "{}", r.checks[1].detail);
    }
}
