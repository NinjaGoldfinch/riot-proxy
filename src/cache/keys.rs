//! Canonical cache keys and `key_scope` (docs/design/04 §Canonical key, §key_scope).
//!
//! ```text
//! {key_scope}:{method}:{host}:{param}:…[:{query hash}]
//! ```
//!
//! This is design 04's shape (owner decision, ADR-027): path parameters stay
//! readable so an admin purge can target one player. v1 hashed the whole target.
//! Parameters are percent-encoded like `encodeURIComponent`, so a value can never
//! contain a bare `:` and collide with another key. The query is sorted and
//! hashed, so `?start=0&count=20` and `?count=20&start=0` share a key (v1).
//!
//! Negative entries share the key: the cache stores the status with the entry, so
//! a cached 404 is told apart structurally, not by a `neg:` prefix (design 04).

use std::fmt;

use sha2::{Digest, Sha256};

use crate::config::Secret;
use crate::riot::client::RiotRequest;
use crate::riot::endpoints::encode_component;

/// `sha256(RIOT_API_KEY)` as hex, first 8 characters (v1 §7.4 `KEY_SCOPE`).
///
/// Encrypted ids (PUUIDs, summoner ids) are only valid under the key that issued
/// them, so every cache key and stored encrypted id is namespaced by this. Rotating
/// the key changes the scope, which invalidates stale ids instead of silently
/// poisoning lookups.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KeyScope(String);

impl KeyScope {
    pub fn from_key(key: &Secret) -> Self {
        let digest = Sha256::digest(key.expose().as_bytes());
        Self(hex::encode(digest)[..8].to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for KeyScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Sorted `k=v&…` with `encodeURIComponent` on both sides (v1 `canonicalTarget`).
/// Empty values are dropped rather than hashed (v1).
pub fn canonical_query(query: &[(&str, String)]) -> String {
    let mut pairs: Vec<(&str, &str)> = query
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| (*k, v.as_str()))
        .collect();
    pairs.sort();
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", encode_component(k), encode_component(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// 16 hex characters of sha256 over `text`.
fn short_hash(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))[..16].to_string()
}

/// The canonical key for one upstream request.
pub fn cache_key(scope: &KeyScope, req: &RiotRequest) -> String {
    let mut key = format!("{scope}:{}:{}", req.endpoint.id, req.target.host());
    for p in &req.params {
        key.push(':');
        key.push_str(p);
    }
    let query = canonical_query(&req.query);
    if !query.is_empty() {
        key.push(':');
        key.push_str(&short_hash(&query));
    }
    key
}

/// A key for one of the proxy's own derived reads, computed from the archive rather
/// than fetched (v1 `derivedKey`, #113). Scoped for the same reason: derived
/// documents are reached through PUUIDs. `part` names the read so two reads can
/// never collide on the same arguments.
pub fn derived_key(scope: &KeyScope, part: &str, target: &str) -> String {
    format!("{scope}:derived:{part}:{}", short_hash(target))
}

/// Scope an admin purge glob to the current key so one deployment can't wipe
/// another's entries (v1 `scopedPurgePattern`). A pattern that already starts with
/// a key scope (`xxxxxxxx:`) is left alone, as v1 left `c:…` patterns alone.
pub fn scoped_purge_pattern(scope: &KeyScope, pattern: &str) -> String {
    let already_scoped = pattern.len() > 8
        && pattern.as_bytes()[8] == b':'
        && pattern[..8]
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase());
    if already_scoped {
        pattern.to_string()
    } else {
        format!("{scope}:{pattern}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::riot::endpoints::Endpoint;
    use crate::riot::routing::{Platform, Region};

    fn scope() -> KeyScope {
        KeyScope::from_key(&Secret::new("RGAPI-test-key-not-real"))
    }

    fn req(id: &str, platform: Platform, params: &[&str]) -> RiotRequest {
        let e = Endpoint::by_id(id).unwrap();
        RiotRequest::new(e, e.target_for_platform(platform), params).unwrap()
    }

    fn match_ids(query: &[(&str, &str)]) -> RiotRequest {
        let e = Endpoint::by_id("match.idsByPuuid").unwrap();
        let mut r = RiotRequest::new(e, e.target_for_region(Region::Europe).unwrap(), &["P"]).unwrap();
        for (k, v) in query {
            r = r.query(k, Some(*v)).unwrap();
        }
        r
    }

    /// v1 test/config.test.ts "derives KEY_SCOPE as the first 8 hex chars of sha256(key)".
    #[test]
    fn key_scope_is_the_first_8_hex_of_sha256() {
        // sha256("RGAPI-test-key-not-real"), computed independently.
        let full = hex::encode(Sha256::digest(b"RGAPI-test-key-not-real"));
        assert_eq!(scope().as_str(), &full[..8]);
        assert_eq!(scope().as_str().len(), 8);
    }

    #[test]
    fn key_scope_is_stable_per_key_and_differs_across_keys() {
        assert_eq!(
            scope(),
            KeyScope::from_key(&Secret::new("RGAPI-test-key-not-real"))
        );
        assert_ne!(
            scope(),
            KeyScope::from_key(&Secret::new("RGAPI-other-key-not-real"))
        );
    }

    /// v1 "sorts query params so equivalent requests collide".
    #[test]
    fn sorts_query_params_so_equivalent_requests_collide() {
        let a = cache_key(&scope(), &match_ids(&[("start", "0"), ("count", "20")]));
        let b = cache_key(&scope(), &match_ids(&[("count", "20"), ("start", "0")]));
        assert_eq!(a, b);
    }

    /// v1 "drops empty and undefined params rather than hashing them".
    #[test]
    fn drops_empty_params() {
        assert_eq!(canonical_query(&[("a", "1".into()), ("c", String::new())]), "a=1");
        assert_eq!(
            cache_key(&scope(), &match_ids(&[("start", "0"), ("type", "")])),
            cache_key(&scope(), &match_ids(&[("start", "0")]))
        );
        assert_eq!(canonical_query(&[]), "");
    }

    /// v1 "distinguishes different query values".
    #[test]
    fn distinguishes_different_query_values() {
        assert_ne!(
            cache_key(&scope(), &match_ids(&[("count", "20")])),
            cache_key(&scope(), &match_ids(&[("count", "21")]))
        );
    }

    /// v1 "namespaces by key scope, method and host (§7.4)", in design 04's shape.
    #[test]
    fn namespaces_by_scope_method_host_and_params() {
        let key = cache_key(&scope(), &req("summoner.byPuuid", Platform::Euw1, &["PUUID-1"]));
        assert_eq!(
            key,
            format!("{}:summoner.byPuuid:euw1.api.riotgames.com:PUUID-1", scope())
        );

        let key = cache_key(&scope(), &match_ids(&[("start", "0"), ("count", "20")]));
        let (prefix, hash) = key.rsplit_once(':').unwrap();
        assert_eq!(
            prefix,
            format!("{}:match.idsByPuuid:europe.api.riotgames.com:P", scope())
        );
        assert_eq!(hash.len(), 16);
        assert!(hash.bytes().all(|b| b.is_ascii_hexdigit()));
    }

    /// v1 "separates the same path on different hosts".
    #[test]
    fn separates_the_same_path_on_different_hosts() {
        assert_ne!(
            cache_key(&scope(), &req("summoner.byPuuid", Platform::Euw1, &["P"])),
            cache_key(&scope(), &req("summoner.byPuuid", Platform::Na1, &["P"]))
        );
    }

    #[test]
    fn separates_key_scopes() {
        let other = KeyScope::from_key(&Secret::new("RGAPI-rotated-key-not-real"));
        let r = req("summoner.byPuuid", Platform::Euw1, &["P"]);
        assert_ne!(cache_key(&scope(), &r), cache_key(&other, &r));
    }

    /// Readable params must not open a collision: a `:` inside a value is encoded.
    #[test]
    fn colons_and_odd_characters_in_params_cannot_collide() {
        let e = Endpoint::by_id("account.byRiotId").unwrap();
        let t = e.target_for_region(Region::Europe).unwrap();
        let a = cache_key(&scope(), &RiotRequest::new(e, t, &["a:b", "c"]).unwrap());
        let b = cache_key(&scope(), &RiotRequest::new(e, t, &["a", "b:c"]).unwrap());
        assert_ne!(a, b);
        assert!(a.ends_with(":a%3Ab:c"), "{a}");
        let spaced = cache_key(
            &scope(),
            &RiotRequest::new(e, t, &["Hide on bush", "KR1"]).unwrap(),
        );
        assert!(spaced.ends_with(":Hide%20on%20bush:KR1"), "{spaced}");
    }

    /// Account lookups from `sea` share the `asia` entry, because Riot serves them there.
    #[test]
    fn keys_follow_the_resolved_host() {
        let e = Endpoint::by_id("account.byPuuid").unwrap();
        let sea = RiotRequest::new(e, e.target_for_region(Region::Sea).unwrap(), &["P"]).unwrap();
        let asia = RiotRequest::new(e, e.target_for_region(Region::Asia).unwrap(), &["P"]).unwrap();
        assert_eq!(cache_key(&scope(), &sea), cache_key(&scope(), &asia));
    }

    /// v1 "scopes admin purge patterns so one deployment cannot wipe another".
    #[test]
    fn scopes_admin_purge_patterns() {
        let s = scope();
        assert_eq!(scoped_purge_pattern(&s, "summoner.*"), format!("{s}:summoner.*"));
        assert_eq!(scoped_purge_pattern(&s, "0123abcd:*"), "0123abcd:*");
        assert_eq!(
            scoped_purge_pattern(&s, "ABCDEFGH:*"),
            format!("{s}:ABCDEFGH:*"),
            "scopes are lowercase hex"
        );
    }

    #[test]
    fn derived_keys_are_scoped_and_named() {
        let a = derived_key(&scope(), "profile", "euw1:P");
        assert!(a.starts_with(&format!("{}:derived:profile:", scope())));
        assert_ne!(a, derived_key(&scope(), "champions", "euw1:P"));
        assert_ne!(a, derived_key(&scope(), "profile", "euw1:Q"));
    }
}
