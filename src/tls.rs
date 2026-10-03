//! Built-in TLS (plan P8-03, design/07 §Option B): `serve --tls --domain …
//! --acme-email …` obtains and renews a Let's Encrypt certificate with
//! `rustls-acme` into `$DATA_DIR/acme/`, serves HTTPS on `TLS_PORT` (443) and
//! redirects plain HTTP on `TLS_REDIRECT_PORT` (80) to it. The ACME challenge
//! is TLS-ALPN-01, answered on the HTTPS port itself.
//!
//! A PEM certificate and key is the other source: what tests use (ACME needs a
//! real domain), and what an operator with their own certificate would.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::extract::{ConnectInfo, Request};
use axum::http::{HeaderValue, StatusCode, Uri, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum_server::Handle;
use tokio::sync::watch;
use tokio_stream::StreamExt;

use crate::app::SHUTDOWN_GRACE;
use crate::http::{ApiError, ErrorCode};

/// Where the certificate comes from.
pub enum Source {
    /// Let's Encrypt, cached in `cache` (`$DATA_DIR/acme`).
    Acme {
        domain: String,
        email: String,
        cache: PathBuf,
        /// Let's Encrypt's production directory; staging otherwise.
        production: bool,
    },
    /// A certificate chain and private key, PEM.
    Pem { cert: Vec<u8>, key: Vec<u8> },
}

#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    #[error("reading the certificate: {0}")]
    Pem(String),
    #[error(transparent)]
    Rustls(#[from] rustls::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Whether `ip` is loopback or private: what may read `/metrics` and `/readyz`
/// when the proxy faces the internet itself (design/07).
pub fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_private(IpAddr::V4(v4)),
            None => v6.is_loopback() || v6.is_unique_local() || v6.is_unicast_link_local(),
        },
    }
}

/// Refuse `/metrics` and `/readyz` to public addresses, with the allowlist's
/// 403 envelope (design/07: "the same middleware that enforces
/// ADMIN_IP_ALLOWLIST").
pub async fn private_ops(req: Request, next: Next) -> Response {
    let path = req.uri().path();
    if path == "/metrics" || path == "/readyz" {
        let peer = req
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0.ip());
        if !peer.is_some_and(is_private) {
            return ApiError::new(
                ErrorCode::Forbidden,
                "This endpoint is only served to private addresses",
            )
            .into_response();
        }
    }
    next.run(req).await
}

/// `http://host/path` → 308 `https://host[:port]/path`.
fn redirect_target(host: Option<&str>, uri: &Uri, https_port: u16) -> Option<String> {
    let host = host?;
    // Drop any port the client used for plain HTTP; a bracketed IPv6 keeps its brackets.
    let bare = match host.rfind(':') {
        Some(i) if !host[i..].contains(']') => &host[..i],
        _ => host,
    };
    let port = if https_port == 443 {
        String::new()
    } else {
        format!(":{https_port}")
    };
    let path = uri.path_and_query().map_or("/", |p| p.as_str());
    Some(format!("https://{bare}{port}{path}"))
}

/// The plain-HTTP side: every request is redirected to HTTPS.
pub fn redirect_router(https_port: u16) -> Router {
    Router::new().fallback(move |req: Request| async move {
        let host = req
            .headers()
            .get(header::HOST)
            .and_then(|h| h.to_str().ok())
            .map(str::to_string);
        match redirect_target(host.as_deref(), req.uri(), https_port)
            .and_then(|t| HeaderValue::from_str(&t).ok())
        {
            Some(location) => {
                (StatusCode::PERMANENT_REDIRECT, [(header::LOCATION, location)]).into_response()
            }
            None => ApiError::new(ErrorCode::Validation, "A Host header is required").into_response(),
        }
    })
}

/// rustls with this crate's provider (aws-lc-rs, as reqwest's).
fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::aws_lc_rs::default_provider())
}

fn pem_config(cert: &[u8], key: &[u8]) -> Result<rustls::ServerConfig, TlsError> {
    use rustls::pki_types::pem::PemObject;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    let chain = CertificateDer::pem_slice_iter(cert)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| TlsError::Pem(e.to_string()))?;
    let key = PrivateKeyDer::from_pem_slice(key).map_err(|e| TlsError::Pem(e.to_string()))?;
    let mut config = rustls::ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(chain, key)?;
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(config)
}

/// Serve `app` over HTTPS on `https`, and redirect plain HTTP on `redirect`
/// (if any), until `stop` turns true; then drain like `app::serve`.
pub async fn serve(
    app: Router,
    https: SocketAddr,
    redirect: Option<SocketAddr>,
    source: Source,
    stop: watch::Receiver<bool>,
) -> Result<(), TlsError> {
    let stopped = |mut rx: watch::Receiver<bool>| async move {
        let _ = rx.wait_for(|s| *s).await;
    };
    let redirect_task = match redirect {
        Some(addr) => {
            let listener = tokio::net::TcpListener::bind(addr).await?;
            tracing::info!(addr = %listener.local_addr()?, "redirecting HTTP to HTTPS");
            let stop = stopped(stop.clone());
            Some(tokio::spawn(async move {
                axum::serve(listener, redirect_router(https.port()))
                    .with_graceful_shutdown(stop)
                    .await
            }))
        }
        None => None,
    };
    let handle = Handle::<SocketAddr>::new();
    let stopper = handle.clone();
    let stop_https = stopped(stop);
    tokio::spawn(async move {
        stop_https.await;
        tracing::info!(
            grace_s = SHUTDOWN_GRACE.as_secs(),
            "shutdown signal received; draining HTTPS"
        );
        stopper.graceful_shutdown(Some(SHUTDOWN_GRACE));
    });
    let service = app.into_make_service_with_connect_info::<SocketAddr>();
    tracing::info!(addr = %https, "listening (HTTPS)");
    match source {
        Source::Pem { cert, key } => {
            let config =
                axum_server::tls_rustls::RustlsConfig::from_config(Arc::new(pem_config(&cert, &key)?));
            axum_server::bind_rustls(https, config)
                .handle(handle)
                .serve(service)
                .await?;
        }
        Source::Acme {
            domain,
            email,
            cache,
            production,
        } => {
            let mut state = rustls_acme::AcmeConfig::new([domain.clone()])
                .contact_push(format!("mailto:{email}"))
                .cache(rustls_acme::caches::DirCache::new(cache))
                .directory_lets_encrypt(production)
                .state();
            let acceptor = state.axum_acceptor(state.default_rustls_config());
            // The certificate's life: obtain, renew, report.
            tokio::spawn(async move {
                while let Some(event) = state.next().await {
                    match event {
                        Ok(ok) => tracing::info!(%domain, event = ?ok, "acme"),
                        Err(err) => tracing::error!(%domain, error = ?err, "acme"),
                    }
                }
            });
            axum_server::bind(https)
                .handle(handle)
                .acceptor(acceptor)
                .serve(service)
                .await?;
        }
    }
    if let Some(task) = redirect_task {
        let _ = task.await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_means_loopback_rfc1918_link_local_and_ula() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.9",
            "192.168.1.1",
            "169.254.1.1",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:10.0.0.1",
        ] {
            assert!(is_private(ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "8.8.8.8",
            "172.32.0.1",
            "100.64.0.1",
            "2001:4860::8888",
            "::ffff:1.1.1.1",
        ] {
            assert!(!is_private(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn redirects_keep_host_and_path_and_drop_the_plain_port() {
        let uri: Uri = "/v1/static/versions?x=1".parse().unwrap();
        assert_eq!(
            redirect_target(Some("api.example.nz"), &uri, 443).as_deref(),
            Some("https://api.example.nz/v1/static/versions?x=1")
        );
        assert_eq!(
            redirect_target(Some("api.example.nz:80"), &uri, 8443).as_deref(),
            Some("https://api.example.nz:8443/v1/static/versions?x=1")
        );
        assert_eq!(
            redirect_target(Some("[::1]:8080"), &"/".parse().unwrap(), 443).as_deref(),
            Some("https://[::1]/")
        );
        assert_eq!(redirect_target(None, &uri, 443), None);
    }

    #[tokio::test]
    async fn ops_endpoints_are_refused_to_public_peers_only() {
        use tower::ServiceExt;
        let app = Router::new()
            .route("/metrics", axum::routing::get(|| async { "ok" }))
            .route("/readyz", axum::routing::get(|| async { "ok" }))
            .route("/healthz", axum::routing::get(|| async { "ok" }))
            .layer(axum::middleware::from_fn(private_ops));
        let status = |path: &'static str, peer: &'static str| {
            let app = app.clone();
            async move {
                let mut req = Request::builder()
                    .uri(path)
                    .body(axum::body::Body::empty())
                    .unwrap();
                req.extensions_mut()
                    .insert(ConnectInfo::<SocketAddr>(peer.parse().unwrap()));
                app.oneshot(req).await.unwrap().status()
            }
        };
        assert_eq!(status("/metrics", "8.8.8.8:5000").await, StatusCode::FORBIDDEN);
        assert_eq!(
            status("/readyz", "[2001:4860::8888]:5000").await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(status("/healthz", "8.8.8.8:5000").await, StatusCode::OK);
        assert_eq!(status("/metrics", "10.0.0.2:5000").await, StatusCode::OK);
        assert_eq!(status("/readyz", "127.0.0.1:5000").await, StatusCode::OK);
    }
}
