//! Request validation with v1's rules and messages (`src/routes/schemas.ts`).
//!
//! v1 validated with ajv and turned failures into `VALIDATION` (400), or
//! `BAD_REGION` when the message named a platform or region. The messages here
//! follow ajv's wording (`params/gameName must NOT have more than 16 characters`)
//! so clients that surface them see the same text.

use crate::http::{ApiError, ErrorCode};
use crate::riot::routing::{Platform, Region};

/// v1 §12.3: Riot's documented Riot ID maxima; minima are only "non-empty".
pub const GAME_NAME_MAX: usize = 16;
pub const TAG_LINE_MAX: usize = 5;
/// PUUIDs: 60–128 url-safe characters (length varies by key era).
pub const PUUID_MIN: usize = 60;
pub const PUUID_MAX: usize = 128;
/// Match ids: `PLATFORM_123`, 6–40 characters.
pub const MATCH_ID_MIN: usize = 6;
pub const MATCH_ID_MAX: usize = 40;

pub(crate) fn invalid(location: &str, name: &str, rule: &str) -> ApiError {
    let message = format!("{location}/{name} {rule}");
    // v1: a platform/region failure is BAD_REGION so consumers can branch on it.
    let lower = message.to_ascii_lowercase();
    let code = if lower.contains("platform") || lower.contains("region") {
        ErrorCode::BadRegion
    } else {
        ErrorCode::Validation
    };
    ApiError::new(code, message)
}

fn length(location: &str, name: &str, value: &str, min: usize, max: usize) -> Result<(), ApiError> {
    // ajv counts Unicode code points.
    let n = value.chars().count();
    if n < min {
        return Err(invalid(
            location,
            name,
            &format!("must NOT have fewer than {min} characters"),
        ));
    }
    if n > max {
        return Err(invalid(
            location,
            name,
            &format!("must NOT have more than {max} characters"),
        ));
    }
    Ok(())
}

fn one_of<T: Copy>(
    location: &str,
    name: &str,
    value: &str,
    all: &[T],
    as_str: impl Fn(T) -> &'static str,
) -> Result<T, ApiError> {
    // ajv enums are case-sensitive, so `EUW1` is refused at the route (v1).
    all.iter()
        .copied()
        .find(|v| as_str(*v) == value)
        .ok_or_else(|| invalid(location, name, "must be equal to one of the allowed values"))
}

pub fn region(value: &str) -> Result<Region, ApiError> {
    one_of("params", "region", value, &Region::ALL, Region::as_str)
}

pub fn platform(value: &str) -> Result<Platform, ApiError> {
    platform_at("params", value)
}

/// A platform in `location` (`params`, `body`, …).
pub fn platform_at(location: &str, value: &str) -> Result<Platform, ApiError> {
    one_of(location, "platform", value, &Platform::ALL, Platform::as_str)
}

/// `params/scope`: a rate-limit bucket, a platform or a region (v1 `ScopeParam`).
pub fn limiter_scope(value: &str) -> Result<&'static str, ApiError> {
    Platform::ALL
        .iter()
        .map(|p| p.as_str())
        .chain(Region::ALL.iter().map(|r| r.as_str()))
        .find(|s| *s == value)
        .ok_or_else(|| invalid("params", "scope", "must be equal to one of the allowed values"))
}

/// A `params/<name>` value from a closed set of strings (ladder enums).
pub fn one_of_str(name: &str, value: &str, all: &[&'static str]) -> Result<&'static str, ApiError> {
    all.iter()
        .copied()
        .find(|v| *v == value)
        .ok_or_else(|| invalid("params", name, "must be equal to one of the allowed values"))
}

/// A `querystring/<name>` value from a closed set of strings.
pub fn query_one_of(
    name: &str,
    raw: Option<&str>,
    all: &[&'static str],
) -> Result<Option<&'static str>, ApiError> {
    raw.map(|v| {
        all.iter()
            .copied()
            .find(|a| *a == v)
            .ok_or_else(|| invalid("querystring", name, "must be equal to one of the allowed values"))
    })
    .transpose()
}

pub fn game_name(value: &str) -> Result<(), ApiError> {
    game_name_at("params", value)
}

pub fn game_name_at(location: &str, value: &str) -> Result<(), ApiError> {
    length(location, "gameName", value, 1, GAME_NAME_MAX)
}

pub fn tag_line(value: &str) -> Result<(), ApiError> {
    tag_line_at("params", value)
}

pub fn tag_line_at(location: &str, value: &str) -> Result<(), ApiError> {
    length(location, "tagLine", value, 1, TAG_LINE_MAX)
}

pub fn puuid(value: &str) -> Result<(), ApiError> {
    puuid_at("params", value)
}

pub fn puuid_at(location: &str, value: &str) -> Result<(), ApiError> {
    length(location, "puuid", value, PUUID_MIN, PUUID_MAX)?;
    if value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        Ok(())
    } else {
        Err(invalid(
            location,
            "puuid",
            "must match pattern \"^[A-Za-z0-9_-]+$\"",
        ))
    }
}

pub fn match_id(value: &str) -> Result<(), ApiError> {
    length("params", "matchId", value, MATCH_ID_MIN, MATCH_ID_MAX)?;
    let ok = value.split_once('_').is_some_and(|(p, n)| {
        !p.is_empty()
            && p.bytes().all(|b| b.is_ascii_alphanumeric())
            && !n.is_empty()
            && n.bytes().all(|b| b.is_ascii_digit())
    });
    if ok {
        Ok(())
    } else {
        Err(invalid(
            "params",
            "matchId",
            "must match pattern \"^[A-Za-z0-9]+_[0-9]+$\"",
        ))
    }
}

/// An integer query parameter within `[min, max]`.
pub fn int_query(name: &str, raw: Option<&str>, min: i64, max: i64) -> Result<Option<i64>, ApiError> {
    let Some(raw) = raw else { return Ok(None) };
    let n: i64 = raw
        .parse()
        .map_err(|_| invalid("querystring", name, "must be integer"))?;
    if n < min {
        return Err(invalid("querystring", name, &format!("must be >= {min}")));
    }
    if n > max {
        return Err(invalid("querystring", name, &format!("must be <= {max}")));
    }
    Ok(Some(n))
}

/// A boolean query parameter. ajv coerced only `true` and `false` from a query
/// string; anything else is refused (v1).
pub fn bool_query(name: &str, raw: Option<&str>) -> Result<Option<bool>, ApiError> {
    match raw {
        None => Ok(None),
        Some("true") => Ok(Some(true)),
        Some("false") => Ok(Some(false)),
        Some(_) => Err(invalid("querystring", name, "must be boolean")),
    }
}

/// `?platform=`: a platform, case-sensitive, `BAD_REGION` otherwise (v1 `PlatformParam`).
pub fn query_platform(raw: Option<&str>) -> Result<Option<Platform>, ApiError> {
    raw.map(|v| one_of("querystring", "platform", v, &Platform::ALL, Platform::as_str))
        .transpose()
}

/// `?patch=`: `major.minor`, 3–8 characters (v1 `PlayerChampionsQuery`).
pub fn patch_query(raw: Option<&str>) -> Result<Option<&str>, ApiError> {
    let Some(v) = raw else { return Ok(None) };
    length("querystring", "patch", v, 3, 8)?;
    let ok = v.split_once('.').is_some_and(|(a, b)| {
        !a.is_empty()
            && !b.is_empty()
            && a.bytes().all(|c| c.is_ascii_digit())
            && b.bytes().all(|c| c.is_ascii_digit())
    });
    if ok {
        Ok(Some(v))
    } else {
        Err(invalid(
            "querystring",
            "patch",
            "must match pattern \"^[0-9]+\\.[0-9]+$\"",
        ))
    }
}

/// `?version=`: a Data Dragon patch, at most 20 characters (v1 `/v1/static/{file}`).
pub fn version_query(raw: Option<&str>) -> Result<Option<&str>, ApiError> {
    let Some(v) = raw else { return Ok(None) };
    length("querystring", "version", v, 0, 20)?;
    if crate::r#static::is_version(v) {
        Ok(Some(v))
    } else {
        Err(invalid(
            "querystring",
            "version",
            "must match pattern \"^[0-9]+(\\.[0-9]+)*$\"",
        ))
    }
}

/// `params/id`: a consumer id, a ULID (v2 ids; v1's were UUIDs, ADR-044).
pub fn consumer_id(value: &str) -> Result<(), ApiError> {
    if value.parse::<ulid::Ulid>().is_ok() && value.len() == 26 {
        Ok(())
    } else {
        Err(invalid("params", "id", "must match format \"ulid\""))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_locations_scopes_and_ids() {
        assert_eq!(
            msg(puuid_at("body", "short")).1,
            "body/puuid must NOT have fewer than 60 characters"
        );
        assert_eq!(msg(platform_at("body", "EUW1")).0, ErrorCode::BadRegion);
        assert_eq!(limiter_scope("europe").unwrap(), "europe");
        assert_eq!(limiter_scope("euw1").unwrap(), "euw1");
        assert_eq!(
            msg(limiter_scope("mars")),
            (
                ErrorCode::Validation,
                "params/scope must be equal to one of the allowed values".into()
            )
        );
        assert!(consumer_id(&ulid::Ulid::generate().to_string()).is_ok());
        assert_eq!(
            msg(consumer_id("not-an-id")).1,
            "params/id must match format \"ulid\""
        );
    }

    #[test]
    fn query_booleans_platforms_and_patches() {
        assert_eq!(bool_query("refresh", None).unwrap(), None);
        assert_eq!(bool_query("refresh", Some("true")).unwrap(), Some(true));
        assert_eq!(bool_query("refresh", Some("false")).unwrap(), Some(false));
        assert_eq!(
            msg(bool_query("refresh", Some("1"))),
            (
                ErrorCode::Validation,
                "querystring/refresh must be boolean".into()
            )
        );
        assert_eq!(query_platform(Some("kr")).unwrap(), Some(Platform::Kr));
        assert_eq!(
            msg(query_platform(Some("KR"))),
            (
                ErrorCode::BadRegion,
                "querystring/platform must be equal to one of the allowed values".into()
            )
        );
        assert_eq!(patch_query(Some("14.18")).unwrap(), Some("14.18"));
        assert_eq!(
            msg(patch_query(Some("14.x"))).1,
            "querystring/patch must match pattern \"^[0-9]+\\.[0-9]+$\""
        );
        assert_eq!(
            msg(patch_query(Some("1"))).1,
            "querystring/patch must NOT have fewer than 3 characters"
        );
    }

    fn msg(r: Result<impl std::fmt::Debug, ApiError>) -> (ErrorCode, String) {
        let e = r.unwrap_err();
        (e.code, e.message)
    }

    #[test]
    fn regions_and_platforms_are_case_sensitive_bad_region() {
        assert_eq!(region("europe").unwrap(), Region::Europe);
        assert_eq!(region("sea").unwrap(), Region::Sea);
        assert_eq!(
            msg(region("EUROPE")),
            (
                ErrorCode::BadRegion,
                "params/region must be equal to one of the allowed values".into()
            )
        );
        assert_eq!(platform("euw1").unwrap(), Platform::Euw1);
        assert_eq!(msg(platform("euw")).0, ErrorCode::BadRegion);
    }

    /// v1 §12.3: maxima are Riot's caps; minima are only non-empty (riot-proxy#11).
    #[test]
    fn riot_id_bounds() {
        assert!(game_name("a").is_ok());
        assert!(game_name(&"x".repeat(16)).is_ok());
        assert!(game_name("Ünïcödé Nämé 16c").is_ok(), "code points, not bytes");
        assert_eq!(
            msg(game_name(&"x".repeat(17))),
            (
                ErrorCode::Validation,
                "params/gameName must NOT have more than 16 characters".into()
            )
        );
        assert_eq!(
            msg(game_name("")).1,
            "params/gameName must NOT have fewer than 1 characters"
        );
        assert!(tag_line("KR1").is_ok());
        assert!(tag_line("NA").is_ok(), "2-character legacy tag lines are real");
        assert_eq!(
            msg(tag_line("TOOLONG")).1,
            "params/tagLine must NOT have more than 5 characters"
        );
    }

    #[test]
    fn puuid_rules() {
        let good = "a".repeat(78);
        assert!(puuid(&good).is_ok());
        assert!(
            puuid("NkQRxdiN3U3pEek5MWbWgaxzG_hpH5imJ9Ttch8ql5KM7D6p6Bh-Hbbvn6UoFVdGUBBIvcnEJv72qw").is_ok()
        );
        assert_eq!(
            msg(puuid("short")).1,
            "params/puuid must NOT have fewer than 60 characters"
        );
        assert_eq!(
            msg(puuid(&"a".repeat(129))).1,
            "params/puuid must NOT have more than 128 characters"
        );
        let bad = format!("{}!", "a".repeat(70));
        assert_eq!(
            msg(puuid(&bad)).1,
            "params/puuid must match pattern \"^[A-Za-z0-9_-]+$\""
        );
    }

    #[test]
    fn match_id_rules() {
        assert!(match_id("EUW1_7381937461").is_ok());
        assert!(match_id("KR_8393343196").is_ok());
        for bad in ["EUW17381937461", "EUW1_", "_123", "EUW1_12a", "EU-W_123"] {
            assert_eq!(msg(match_id(bad)).0, ErrorCode::Validation, "{bad}");
        }
        assert_eq!(
            msg(match_id("A_1")).1,
            "params/matchId must NOT have fewer than 6 characters"
        );
    }

    #[test]
    fn integer_queries() {
        assert_eq!(int_query("count", None, 1, 100).unwrap(), None);
        assert_eq!(int_query("count", Some("20"), 1, 100).unwrap(), Some(20));
        assert_eq!(
            msg(int_query("count", Some("0"), 1, 100)).1,
            "querystring/count must be >= 1"
        );
        assert_eq!(
            msg(int_query("count", Some("101"), 1, 100)).1,
            "querystring/count must be <= 100"
        );
        assert_eq!(
            msg(int_query("count", Some("x"), 1, 100)).1,
            "querystring/count must be integer"
        );
    }
}
