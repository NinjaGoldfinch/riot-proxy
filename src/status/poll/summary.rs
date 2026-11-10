//! `GET /v1/lol/status`'s body (SITE-14, ADR-220): every polled platform's
//! status at once, from what the status poller stored in `platform_status`
//! ([`super::stored`]), never from Riot.
//!
//! Riot's field names come only from the portal's `PlatformDataDto` and
//! `StatusDto` (ADR-217): `incidents[]`, `maintenances[]`,
//! `maintenance_status` (`scheduled`, `in_progress`, `complete`) and
//! `incident_severity` (`info`, `warning`, `critical`). A body without them is
//! read as listing nothing, never a failure.

use std::collections::BTreeSet;
use std::sync::{Mutex, PoisonError};

use serde::Serialize;
use serde_json::Value;
use utoipa::ToSchema;

use super::PlatformStatus;
use crate::riot::routing::Platform;

/// What an entry's `error` says when the poller has stored nothing for it.
pub const NOT_READ_YET: &str = "not read yet";

/// An incident's `incident_severity`, in the site's order: `critical` over
/// `warning` over `info`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warning,
    Critical,
}

impl Severity {
    /// The portal's three values; anything else is `None`.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "info" => Some(Self::Info),
            "warning" => Some(Self::Warning),
            "critical" => Some(Self::Critical),
            _ => None,
        }
    }
}

/// What one stored body lists.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Counts {
    /// Entries in `incidents[]`.
    pub incidents: u32,
    /// Entries in `maintenances[]` whose `maintenance_status` is not `complete`.
    pub maintenances: u32,
    /// The worst `incident_severity` of `incidents[]`.
    pub worst_severity: Option<Severity>,
    /// Whether a maintenance is `in_progress`.
    pub maintenance_in_progress: bool,
    /// `incident_severity` values outside the portal's three, as found; not
    /// counted in `worst_severity`.
    pub unknown_severities: Vec<String>,
}

/// The entries of `body[field]`; none when it is missing or not a list.
fn list<'a>(body: &'a Value, field: &str) -> &'a [Value] {
    body.get(field)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

/// Count what `body` (a `PlatformDataDto`) lists. A missing or malformed
/// field counts as an empty list; a missing or `null` severity or status is
/// no value.
pub fn counts(body: &Value) -> Counts {
    let incidents = list(body, "incidents");
    let maintenances = list(body, "maintenances");
    fn status(m: &Value) -> Option<&str> {
        m.get("maintenance_status").and_then(Value::as_str)
    }
    let mut out = Counts {
        incidents: u32::try_from(incidents.len()).unwrap_or(u32::MAX),
        maintenances: u32::try_from(
            maintenances
                .iter()
                .filter(|m| status(m) != Some("complete"))
                .count(),
        )
        .unwrap_or(u32::MAX),
        maintenance_in_progress: maintenances.iter().any(|m| status(m) == Some("in_progress")),
        ..Counts::default()
    };
    for incident in incidents {
        match incident.get("incident_severity") {
            None | Some(Value::Null) => {}
            Some(Value::String(s)) => match Severity::parse(s) {
                Some(sev) => out.worst_severity = out.worst_severity.max(Some(sev)),
                None => out.unknown_severities.push(s.clone()),
            },
            Some(other) => out.unknown_severities.push(other.to_string()),
        }
    }
    out
}

/// One platform in `GET /v1/lol/status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlatformSummary {
    /// The platform routing value, e.g. `euw1`.
    #[schema(example = "euw1")]
    pub platform: String,
    /// Whether the counts below describe the platform's current status: the
    /// poller has stored a 200 from Riot for it and its last read since has
    /// not failed. `false` with `error` saying why; the counts are then `null`.
    pub available: bool,
    /// Why the entry is unavailable: the last read's error (the proxy's
    /// error code and message, e.g. `NOT_FOUND: Resource not found upstream`),
    /// or `not read yet`. `null` when available.
    #[schema(required = true)]
    pub error: Option<String>,
    /// How many incidents Riot lists (`incidents[]`). `null` when unavailable.
    #[schema(required = true)]
    pub incidents: Option<u32>,
    /// How many maintenances Riot lists that are not `complete`
    /// (`maintenances[]` without `maintenance_status: "complete"`). `null`
    /// when unavailable.
    #[schema(required = true)]
    pub maintenances: Option<u32>,
    /// The worst `incident_severity` among the incidents: `critical` over
    /// `warning` over `info`. `null` with no incident that has one of those
    /// three, or when unavailable. A value outside the three is not counted.
    #[schema(required = true)]
    pub worst_severity: Option<Severity>,
    /// Whether a maintenance is `in_progress`. `null` when unavailable.
    #[schema(required = true)]
    pub maintenance_in_progress: Option<bool>,
    /// Seconds since the poller last read a 200 for this platform, whether or
    /// not a read has failed since; `null` before any.
    #[schema(required = true)]
    pub fetched_age_seconds: Option<u64>,
}

/// `GET /v1/lol/status`: every platform the status poller reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct StatusSummary {
    /// One entry per platform in `STATUS_POLL_PLATFORMS` (every platform by
    /// default), in the proxy's platform order, each at most one poll
    /// interval old when available.
    pub platforms: Vec<PlatformSummary>,
}

impl StatusSummary {
    /// The stalest available entry's `fetchedAgeSeconds`, for
    /// `X-Cache-Fetched-Age`; `None` when no entry is available.
    pub fn stalest_available_age(&self) -> Option<u64> {
        self.platforms
            .iter()
            .filter(|p| p.available)
            .filter_map(|p| p.fetched_age_seconds)
            .max()
    }
}

/// Whole seconds from `then_ms` to `now_ms`, rounded as `X-Cache-Age` is, and
/// never negative (a stamp ahead of this clock is 0).
fn age_seconds(now_ms: i64, then_ms: i64) -> u64 {
    let ms = u64::try_from(now_ms.saturating_sub(then_ms)).unwrap_or(0);
    ms.saturating_add(500) / 1000
}

/// The entry for `platform` from its stored row, if any.
pub fn entry(platform: Platform, row: Option<&PlatformStatus>, now_ms: i64) -> PlatformSummary {
    let fetched_at = row.and_then(|r| r.fetched_at_ms);
    let failed_since = match (row.and_then(|r| r.failed_at_ms), fetched_at) {
        (Some(failed), Some(fetched)) => failed > fetched,
        (Some(_), None) => true,
        (None, _) => false,
    };
    let mut out = PlatformSummary {
        platform: platform.as_str().to_string(),
        available: false,
        error: None,
        incidents: None,
        maintenances: None,
        worst_severity: None,
        maintenance_in_progress: None,
        fetched_age_seconds: fetched_at.map(|at| age_seconds(now_ms, at)),
    };
    let body = row
        .and_then(|r| r.body.as_deref())
        .filter(|_| fetched_at.is_some());
    if failed_since || body.is_none() {
        out.error = Some(
            row.and_then(|r| r.error.clone())
                .filter(|_| failed_since)
                .unwrap_or_else(|| NOT_READ_YET.to_string()),
        );
        return out;
    }
    let Some(value) = body.and_then(|b| serde_json::from_str::<Value>(b).ok()) else {
        // The poller stores only JSON, so this is a damaged row.
        out.error = Some("the stored body is not JSON".to_string());
        return out;
    };
    let c = counts(&value);
    for value in &c.unknown_severities {
        log_unknown_severity(platform, value);
    }
    out.available = true;
    out.incidents = Some(c.incidents);
    out.maintenances = Some(c.maintenances);
    out.worst_severity = c.worst_severity;
    out.maintenance_in_progress = Some(c.maintenance_in_progress);
    out
}

/// Every platform in `listed`, in [`Platform::ALL`] order, from `rows`
/// ([`super::stored`]) at `now_ms`. A row for a platform not listed is
/// left out; a listed platform without a row is `not read yet`.
pub fn summarize(listed: &[Platform], rows: &[PlatformStatus], now_ms: i64) -> StatusSummary {
    StatusSummary {
        platforms: Platform::ALL
            .into_iter()
            .filter(|p| listed.contains(p))
            .map(|p| entry(p, rows.iter().find(|r| r.platform == p), now_ms))
            .collect(),
    }
}

/// An `incident_severity` outside the portal's three is logged once per
/// platform and value for the life of the process: the route is read on
/// every page view, and the value stays in the body until Riot changes it.
fn log_unknown_severity(platform: Platform, value: &str) {
    static SEEN: Mutex<BTreeSet<(Platform, String)>> = Mutex::new(BTreeSet::new());
    let first = SEEN
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert((platform, value.to_string()));
    if first {
        tracing::warn!(
            platform = platform.as_str(),
            value,
            "lol-status-v4 incident_severity outside info, warning and critical; not counted"
        );
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn incident(severity: Value) -> Value {
        json!({"id": 1, "incident_severity": severity, "titles": [], "updates": []})
    }

    fn maintenance(status: &str) -> Value {
        json!({"id": 2, "maintenance_status": status, "titles": [], "updates": []})
    }

    #[test]
    fn counts_incidents_and_open_maintenances() {
        let body = json!({
            "id": "EUW1", "name": "EU West", "locales": ["en_US"],
            "incidents": [incident(json!("info")), incident(json!("warning"))],
            "maintenances": [maintenance("scheduled"), maintenance("in_progress"), maintenance("complete")],
        });
        let c = counts(&body);
        assert_eq!(c.incidents, 2);
        assert_eq!(c.maintenances, 2, "`complete` is left out");
        assert_eq!(c.worst_severity, Some(Severity::Warning));
        assert!(c.maintenance_in_progress);
        assert!(c.unknown_severities.is_empty());
    }

    #[test]
    fn only_complete_maintenances_is_none_open_and_none_in_progress() {
        let c = counts(&json!({"incidents": [], "maintenances": [maintenance("complete")]}));
        assert_eq!((c.maintenances, c.maintenance_in_progress), (0, false));
        let c = counts(&json!({"incidents": [], "maintenances": [maintenance("scheduled")]}));
        assert_eq!((c.maintenances, c.maintenance_in_progress), (1, false));
    }

    #[test]
    fn worst_severity_is_critical_over_warning_over_info() {
        let worst = |sevs: &[&str]| {
            let incidents: Vec<Value> = sevs.iter().map(|s| incident(json!(s))).collect();
            counts(&json!({"incidents": incidents, "maintenances": []})).worst_severity
        };
        assert_eq!(worst(&[]), None);
        assert_eq!(worst(&["info"]), Some(Severity::Info));
        assert_eq!(worst(&["info", "warning"]), Some(Severity::Warning));
        assert_eq!(worst(&["warning", "critical", "info"]), Some(Severity::Critical));
        assert_eq!(worst(&["critical", "warning"]), Some(Severity::Critical));
        assert!(Severity::Critical > Severity::Warning && Severity::Warning > Severity::Info);
    }

    #[test]
    fn an_unknown_severity_is_not_counted_but_reported() {
        let body =
            json!({"incidents": [incident(json!("severe")), incident(json!("info")), incident(json!(3))]});
        let c = counts(&body);
        assert_eq!(c.incidents, 3, "the incident still counts");
        assert_eq!(c.worst_severity, Some(Severity::Info), "the unknown value is not");
        assert_eq!(c.unknown_severities, ["severe", "3"]);
        // Case matters: the portal's values are lower case.
        let c = counts(&json!({"incidents": [incident(json!("CRITICAL"))]}));
        assert_eq!(
            (c.worst_severity, c.unknown_severities),
            (None, vec!["CRITICAL".to_string()])
        );
    }

    #[test]
    fn missing_fields_are_empty_lists_not_a_failure() {
        assert_eq!(counts(&json!({"id": "OC1"})), Counts::default());
        assert_eq!(counts(&json!([])), Counts::default());
        assert_eq!(
            counts(&json!({"incidents": "x", "maintenances": {}})),
            Counts::default()
        );
        // An incident without a severity, a maintenance without a status.
        let c =
            counts(&json!({"incidents": [{"id": 1}, incident(Value::Null)], "maintenances": [{"id": 2}]}));
        assert_eq!(c.incidents, 2);
        assert_eq!(c.worst_severity, None);
        assert!(
            c.unknown_severities.is_empty(),
            "no value is not an unknown value"
        );
        assert_eq!((c.maintenances, c.maintenance_in_progress), (1, false));
    }

    fn row(
        platform: Platform,
        body: Option<&str>,
        fetched: Option<i64>,
        failed: Option<(i64, &str)>,
    ) -> PlatformStatus {
        PlatformStatus {
            platform,
            body: body.map(str::to_string),
            fetched_at_ms: fetched,
            failed_at_ms: failed.map(|f| f.0),
            error: failed.map(|f| f.1.to_string()),
        }
    }

    const NOW: i64 = 1_800_000_000_000;
    const BODY: &str = r#"{"incidents":[{"id":1,"incident_severity":"critical"}],"maintenances":[]}"#;

    #[test]
    fn an_entry_is_available_after_a_200_not_followed_by_a_failure() {
        let e = entry(
            Platform::Oc1,
            Some(&row(Platform::Oc1, Some(BODY), Some(NOW - 61_600), None)),
            NOW,
        );
        assert_eq!(
            e,
            PlatformSummary {
                platform: "oc1".into(),
                available: true,
                error: None,
                incidents: Some(1),
                maintenances: Some(0),
                worst_severity: Some(Severity::Critical),
                maintenance_in_progress: Some(false),
                fetched_age_seconds: Some(62),
            }
        );
        // A failure before the last 200 does not count.
        let e = entry(
            Platform::Oc1,
            Some(&row(
                Platform::Oc1,
                Some(BODY),
                Some(NOW - 1_000),
                Some((NOW - 2_000, "x")),
            )),
            NOW,
        );
        assert!(e.available);
        assert_eq!(e.error, None);
    }

    #[test]
    fn a_failure_after_the_last_200_makes_it_unavailable_with_null_counts() {
        let e = entry(
            Platform::Kr,
            Some(&row(
                Platform::Kr,
                Some(BODY),
                Some(NOW - 600_000),
                Some((NOW - 1_000, "UPSTREAM_ERROR: Upstream request failed")),
            )),
            NOW,
        );
        assert!(!e.available);
        assert_eq!(
            e.error.as_deref(),
            Some("UPSTREAM_ERROR: Upstream request failed")
        );
        assert_eq!(
            (
                e.incidents,
                e.maintenances,
                e.worst_severity,
                e.maintenance_in_progress
            ),
            (None, None, None, None)
        );
        assert_eq!(
            e.fetched_age_seconds,
            Some(600),
            "the age of the last 200 is still given"
        );
    }

    #[test]
    fn no_200_yet_is_unavailable() {
        let e = entry(Platform::Tw2, None, NOW);
        assert_eq!(
            (e.available, e.error.as_deref(), e.fetched_age_seconds),
            (false, Some(NOT_READ_YET), None)
        );
        let e = entry(
            Platform::Tw2,
            Some(&row(
                Platform::Tw2,
                None,
                None,
                Some((NOW - 5, "NOT_FOUND: Resource not found upstream")),
            )),
            NOW,
        );
        assert_eq!(e.error.as_deref(), Some("NOT_FOUND: Resource not found upstream"));
        assert_eq!(e.fetched_age_seconds, None);
    }

    #[test]
    fn a_damaged_body_is_unavailable_not_a_failure() {
        let e = entry(
            Platform::Oc1,
            Some(&row(Platform::Oc1, Some("{"), Some(NOW), None)),
            NOW,
        );
        assert!(!e.available);
        assert_eq!(e.error.as_deref(), Some("the stored body is not JSON"));
    }

    #[test]
    fn summarize_lists_the_polled_platforms_in_platform_order() {
        let rows = [
            row(Platform::Kr, Some(BODY), Some(NOW - 10_000), None),
            row(Platform::Euw1, Some(BODY), Some(NOW - 20_000), None),
            // Polled before, no longer listed.
            row(Platform::Na1, Some(BODY), Some(NOW), None),
        ];
        let s = summarize(&[Platform::Oc1, Platform::Kr, Platform::Euw1], &rows, NOW);
        let names: Vec<&str> = s.platforms.iter().map(|p| p.platform.as_str()).collect();
        assert_eq!(names, ["euw1", "kr", "oc1"]);
        assert_eq!(s.platforms[2].error.as_deref(), Some(NOT_READ_YET));
        assert_eq!(s.stalest_available_age(), Some(20), "the stalest available entry");
        let none = summarize(&[Platform::Oc1], &rows, NOW);
        assert_eq!(none.stalest_available_age(), None);
    }

    #[test]
    fn the_body_serialises_with_the_documented_names() {
        let s = summarize(
            &[Platform::Oc1, Platform::Tw2],
            &[row(Platform::Oc1, Some(BODY), Some(NOW), None)],
            NOW,
        );
        assert_eq!(
            serde_json::to_value(&s).unwrap(),
            json!({"platforms": [
                {"platform": "oc1", "available": true, "error": null, "incidents": 1, "maintenances": 0,
                 "worstSeverity": "critical", "maintenanceInProgress": false, "fetchedAgeSeconds": 0},
                {"platform": "tw2", "available": false, "error": "not read yet", "incidents": null, "maintenances": null,
                 "worstSeverity": null, "maintenanceInProgress": null, "fetchedAgeSeconds": null},
            ]})
        );
    }
}
