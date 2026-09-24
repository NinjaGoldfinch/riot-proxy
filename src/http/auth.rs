//! Consumer authentication and authorisation (v1 `src/auth/plugin.ts`, §7/§12.1).
//!
//! - The key comes from `Authorization: Bearer rpx_…`, or `?token=` (WebSocket
//!   handshakes can't set headers in a browser; v1 accepts it everywhere).
//! - It is sha256'd and looked up among active consumers, cached in memory for
//!   60 s (unknown keys for 30 s, so a guessing flood can't hammer SQLite).
//! - Routes need `read` or `admin`. Admin routes are also IP-allowlisted.
//! - `AUTH_DISABLED=true` (never in production) runs every request as the synthetic
//!   `dev-local` consumer with read+admin and no allowlist.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::connect_info::MockConnectInfo;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, Uri, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use moka::Expiry;
use moka::future::Cache;

use crate::app::AppState;
use crate::config::Config;
use crate::consumers::{self, Scope, hash_key};
use crate::db::Db;
use crate::http::{ApiError, ErrorCode};

/// Plan P4-01: consumer lookups are cached for 60 s (ADR-033).
pub const AUTH_CACHE_TTL: Duration = Duration::from_secs(60);
/// v1 `NEGATIVE_AUTH_TTL`.
pub const NEGATIVE_AUTH_TTL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Consumer {
    pub id: String,
    pub name: String,
    pub scopes: Vec<Scope>,
    pub quota_per_min: u32,
}

impl Consumer {
    /// v1 `DEV_CONSUMER`: used for every request when `AUTH_DISABLED=true`.
    /// Never persisted; its quota is deliberately generous.
    pub fn dev_local() -> Self {
        Self {
            id: "dev-local".into(),
            name: "dev-local".into(),
            scopes: vec![Scope::Read, Scope::Admin],
            quota_per_min: 100_000,
        }
    }

    pub fn has(&self, scope: Scope) -> bool {
        self.scopes.contains(&scope)
    }
}

/// One `ADMIN_IP_ALLOWLIST` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AllowEntry {
    Addr(IpAddr),
    Cidr(IpAddr, u8),
}

impl AllowEntry {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().split_once('/') {
            None => raw.trim().parse().ok().map(|a| Self::Addr(normalise(a))),
            Some((base, bits)) => {
                let base: IpAddr = base.trim().parse().ok()?;
                let bits: u8 = bits.trim().parse().ok()?;
                let max = if base.is_ipv4() { 32 } else { 128 };
                (bits <= max).then(|| Self::Cidr(normalise(base), bits))
            }
        }
    }

    fn matches(&self, addr: IpAddr) -> bool {
        match *self {
            Self::Addr(a) => a == addr,
            Self::Cidr(IpAddr::V4(base), bits) => match addr {
                IpAddr::V4(a) => {
                    let mask = if bits == 0 {
                        0
                    } else {
                        u32::MAX << (32 - u32::from(bits))
                    };
                    u32::from(a) & mask == u32::from(base) & mask
                }
                IpAddr::V6(_) => false,
            },
            Self::Cidr(IpAddr::V6(base), bits) => match addr {
                IpAddr::V6(a) => {
                    let mask = if bits == 0 {
                        0
                    } else {
                        u128::MAX << (128 - u32::from(bits))
                    };
                    u128::from(a) & mask == u128::from(base) & mask
                }
                IpAddr::V4(_) => false,
            },
        }
    }
}

/// `::ffff:127.0.0.1` and `127.0.0.1` are the same host (v1).
fn normalise(addr: IpAddr) -> IpAddr {
    match addr {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(addr, IpAddr::V4),
        v4 => v4,
    }
}

/// v1 `ipAllowed`: an empty allowlist allows everyone; otherwise an unknown source
/// is refused.
pub fn ip_allowed(remote: Option<IpAddr>, allowlist: &[AllowEntry]) -> bool {
    if allowlist.is_empty() {
        return true;
    }
    let Some(addr) = remote.map(normalise) else {
        return false;
    };
    allowlist.iter().any(|e| e.matches(addr))
}

/// The client address. v1 ran Fastify with `trustProxy: true`, so the leftmost
/// `X-Forwarded-For` entry wins when present (ADR-033); otherwise the peer.
pub fn client_ip(headers: &HeaderMap, peer: Option<SocketAddr>) -> Option<IpAddr> {
    let forwarded = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .and_then(|first| first.trim().parse::<IpAddr>().ok());
    forwarded.or(peer.map(|p| p.ip()))
}

/// v1 `bearerFrom`: the `Authorization: Bearer` header, else `?token=`.
pub fn bearer(headers: &HeaderMap, uri: &Uri) -> Option<String> {
    if let Some(value) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok())
        && let Some((scheme, token)) = value.split_once(' ')
        && scheme.eq_ignore_ascii_case("bearer")
        && !token.trim().is_empty()
    {
        return Some(token.trim().to_string());
    }
    uri.query()?
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(k, _)| *k == "token")
        .map(|(_, v)| v.to_string())
        .filter(|v| !v.is_empty())
}

type Cached = Option<Arc<Consumer>>;

struct AuthTtl;

impl Expiry<[u8; 32], Cached> for AuthTtl {
    fn expire_after_create(&self, _: &[u8; 32], value: &Cached, _: std::time::Instant) -> Option<Duration> {
        Some(if value.is_some() {
            AUTH_CACHE_TTL
        } else {
            NEGATIVE_AUTH_TTL
        })
    }
}

/// Resolves keys to consumers and holds the auth policy. Lives in `AppState`.
pub struct Auth {
    db: Db,
    cache: Cache<[u8; 32], Cached>,
    disabled: bool,
    allowlist: Vec<AllowEntry>,
}

impl std::fmt::Debug for Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Auth")
            .field("disabled", &self.disabled)
            .field("allowlist", &self.allowlist)
            .finish()
    }
}

impl Auth {
    pub fn new(config: &Config, db: Db) -> Self {
        let mut allowlist = Vec::new();
        for raw in &config.admin_ip_allowlist {
            match AllowEntry::parse(raw) {
                Some(e) => allowlist.push(e),
                None => {
                    tracing::warn!(entry = %raw, "ADMIN_IP_ALLOWLIST entry is not an IP or CIDR; ignored")
                }
            }
        }
        if config.auth_disabled {
            tracing::warn!(
                "AUTH_DISABLED=true: every request runs as the synthetic dev-local consumer (read+admin)"
            );
        }
        let cache = Cache::builder()
            .max_capacity(10_000)
            .expire_after(AuthTtl)
            .build();
        Self {
            db,
            cache,
            disabled: config.auth_disabled,
            allowlist,
        }
    }

    /// The active consumer for `key`, through the cache.
    pub async fn resolve(&self, key: &str) -> Result<Cached, ApiError> {
        let hash = hash_key(key);
        if let Some(hit) = self.cache.get(&hash).await {
            return Ok(hit);
        }
        let found = consumers::find_active_by_hash(&self.db, hash)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "consumer lookup failed");
                ApiError::internal()
            })?;
        let cached = found.map(|c| {
            Arc::new(Consumer {
                id: c.id,
                name: c.name,
                scopes: c.scopes,
                quota_per_min: c.quota_per_min,
            })
        });
        self.cache.insert(hash, cached.clone()).await;
        Ok(cached)
    }

    /// Drop a cached lookup so a revocation takes effect at once (admin API, P5-05).
    pub async fn invalidate(&self, key_hash: [u8; 32]) {
        self.cache.invalidate(&key_hash).await;
    }

    async fn authorise(&self, ask: Ask, required: Scope) -> Result<Arc<Consumer>, ApiError> {
        if self.disabled {
            return Ok(Arc::new(Consumer::dev_local()));
        }
        let token = ask.token.ok_or_else(ApiError::unauthorized)?;
        let Some(consumer) = self.resolve(&token).await? else {
            tracing::warn!(path = %ask.path, "rejected unknown key");
            return Err(ApiError::unauthorized());
        };
        if !consumer.has(required) {
            return Err(ApiError::new(
                ErrorCode::Forbidden,
                format!("This key lacks the '{required}' scope"),
            ));
        }
        if required == Scope::Admin && !ip_allowed(ask.ip, &self.allowlist) {
            tracing::warn!(ip = ?ask.ip, consumer = %consumer.name, "admin request from disallowed IP");
            return Err(ApiError::new(
                ErrorCode::Forbidden,
                "Admin access is not permitted from this address",
            ));
        }
        Ok(consumer)
    }
}

/// What authorisation needs from a request, taken out before any await (a
/// `&Request` is not `Send`).
struct Ask {
    token: Option<String>,
    ip: Option<IpAddr>,
    path: String,
}

impl Ask {
    fn from(req: &Request) -> Self {
        // What the `ConnectInfo` extractor does: the real peer, or the address
        // `MockConnectInfo` supplies in tests.
        let peer = req
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0)
            .or_else(|| req.extensions().get::<MockConnectInfo<SocketAddr>>().map(|c| c.0));
        Self {
            token: bearer(req.headers(), req.uri()),
            ip: client_ip(req.headers(), peer),
            path: req.uri().path().to_string(),
        }
    }
}

/// Authenticate, authorise and meter one request.
async fn guard(state: AppState, required: Scope, mut req: Request, next: Next) -> Response {
    let ask = Ask::from(&req);
    let consumer = match state.auth.authorise(ask, required).await {
        Ok(consumer) => consumer,
        Err(e) => return e.into_response(),
    };
    tracing::Span::current().record("consumer", consumer.name.as_str());
    // The consumer quota runs after authentication, keyed by consumer (P4-02).
    match state.quotas.check(&consumer) {
        Ok(quota) => {
            req.extensions_mut().insert(consumer);
            let mut res = next.run(req).await;
            quota.apply(res.headers_mut());
            res
        }
        Err((err, quota)) => {
            let mut res = err.into_response();
            quota.apply(res.headers_mut());
            res
        }
    }
}

/// `route_layer(from_fn_with_state(state, require_read))`
pub async fn require_read(State(state): State<AppState>, req: Request, next: Next) -> Response {
    guard(state, Scope::Read, req, next).await
}

/// `route_layer(from_fn_with_state(state, require_admin))`
pub async fn require_admin(State(state): State<AppState>, req: Request, next: Next) -> Response {
    guard(state, Scope::Admin, req, next).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allow(list: &[&str]) -> Vec<AllowEntry> {
        list.iter().map(|e| AllowEntry::parse(e).unwrap()).collect()
    }

    fn ip(s: &str) -> Option<IpAddr> {
        Some(s.parse().unwrap())
    }

    /// v1 "permits everything when the allowlist is empty".
    #[test]
    fn empty_allowlist_permits_everything() {
        assert!(ip_allowed(ip("203.0.113.9"), &[]));
        assert!(ip_allowed(None, &[]));
    }

    /// v1 "matches exact addresses and normalises IPv4-mapped IPv6".
    #[test]
    fn exact_addresses_and_mapped_ipv6() {
        assert!(ip_allowed(ip("127.0.0.1"), &allow(&["127.0.0.1"])));
        assert!(ip_allowed(ip("::ffff:127.0.0.1"), &allow(&["127.0.0.1"])));
        assert!(ip_allowed(ip("::1"), &allow(&["::1"])));
        assert!(!ip_allowed(ip("203.0.113.9"), &allow(&["127.0.0.1"])));
    }

    /// v1 "matches CIDR blocks".
    #[test]
    fn cidr_blocks() {
        assert!(ip_allowed(ip("10.1.2.3"), &allow(&["10.0.0.0/8"])));
        assert!(!ip_allowed(ip("11.1.2.3"), &allow(&["10.0.0.0/8"])));
        assert!(ip_allowed(ip("192.168.1.50"), &allow(&["192.168.1.0/24"])));
        assert!(!ip_allowed(ip("192.168.2.50"), &allow(&["192.168.1.0/24"])));
        assert!(ip_allowed(ip("8.8.8.8"), &allow(&["0.0.0.0/0"])));
        // IPv6 ranges too (v1 required listing IPv6 hosts one by one).
        assert!(ip_allowed(ip("fd00::5"), &allow(&["fd00::/8"])));
        assert!(!ip_allowed(ip("10.0.0.1"), &allow(&["fd00::/8"])));
    }

    /// v1 "rejects an unknown source when an allowlist is configured".
    #[test]
    fn unknown_source_is_rejected() {
        assert!(!ip_allowed(None, &allow(&["127.0.0.1"])));
    }

    #[test]
    fn bad_entries_do_not_parse() {
        for bad in ["", "localhost", "10.0.0.0/33", "10.0.0.0/x", "::/129"] {
            assert_eq!(AllowEntry::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn bearer_from_header_or_token_query() {
        let mut h = HeaderMap::new();
        h.insert(header::AUTHORIZATION, "Bearer rpx_abc".parse().unwrap());
        assert_eq!(bearer(&h, &"/x".parse().unwrap()).as_deref(), Some("rpx_abc"));
        h.insert(header::AUTHORIZATION, "bearer   rpx_abc  ".parse().unwrap());
        assert_eq!(bearer(&h, &"/x".parse().unwrap()).as_deref(), Some("rpx_abc"));
        h.insert(header::AUTHORIZATION, "Basic abc".parse().unwrap());
        assert_eq!(bearer(&h, &"/x".parse().unwrap()), None);
        let empty = HeaderMap::new();
        assert_eq!(
            bearer(&empty, &"/x?a=1&token=rpx_q".parse().unwrap()).as_deref(),
            Some("rpx_q")
        );
        assert_eq!(bearer(&empty, &"/x?token=".parse().unwrap()), None);
    }

    #[test]
    fn client_ip_prefers_forwarded_for() {
        let peer: SocketAddr = "10.0.0.9:5555".parse().unwrap();
        let mut h = HeaderMap::new();
        assert_eq!(client_ip(&h, Some(peer)), ip("10.0.0.9"));
        h.insert("x-forwarded-for", "203.0.113.7, 10.0.0.1".parse().unwrap());
        assert_eq!(client_ip(&h, Some(peer)), ip("203.0.113.7"));
        h.insert("x-forwarded-for", "garbage".parse().unwrap());
        assert_eq!(client_ip(&h, Some(peer)), ip("10.0.0.9"));
        assert_eq!(client_ip(&HeaderMap::new(), None), None);
    }
}
