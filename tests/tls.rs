//! P8-03: `serve` with TLS on, a self-signed certificate standing in for ACME
//! (which needs a real domain): HTTPS answers, plain HTTP on the redirect port
//! answers 308 to HTTPS, and plain HTTP on `PORT` is loopback-only.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::time::Duration;

use riot_proxy::cli::serve::{ServeOptions, serve_with};
use wiremock::MockServer;

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[tokio::test]
async fn serve_with_tls_answers_https_and_redirects_plain_http() {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let cert_pem = cert.cert.pem();
    let key_pem = cert.signing_key.serialize_pem();

    let server = MockServer::start().await;
    let data = tempfile::tempdir().unwrap();
    let (port, https, redirect) = (free_port(), free_port(), free_port());
    let config = common::config(&[
        ("DATA_DIR", data.path().to_str().unwrap()),
        ("PORT", &port.to_string()),
        ("HOST", "127.0.0.1"),
        ("TLS", "true"),
        ("TLS_DOMAIN", "localhost"),
        ("ACME_EMAIL", "ops@example.test"),
        ("TLS_PORT", &https.to_string()),
        ("TLS_REDIRECT_PORT", &redirect.to_string()),
        ("AUTH_DISABLED", "true"),
        ("LOG_LEVEL", "warn"),
    ]);
    let options = ServeOptions {
        riot_base_url: Some(server.uri()),
        skip_tracing_init: true,
        ddragon_urls: Some(riot_proxy::jobs::ddragon::CdnUrls::mock(&server.uri())),
        tls_pem: Some((cert_pem.clone().into_bytes(), key_pem.into_bytes())),
    };
    tokio::spawn(async move { serve_with(config, options).await.unwrap() });

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], https));
    let client = reqwest::Client::builder()
        .add_root_certificate(reqwest::Certificate::from_pem(cert_pem.as_bytes()).unwrap())
        .resolve("localhost", addr)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let base = format!("https://localhost:{https}");
    let mut healthz = None;
    for _ in 0..200 {
        if let Ok(r) = client.get(format!("{base}/healthz")).send().await {
            healthz = Some(r);
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let healthz = healthz.expect("HTTPS never answered");
    assert_eq!(healthz.status(), 200);
    assert!(healthz.headers().contains_key("x-request-id"));

    // Ops endpoints answer loopback peers over HTTPS too.
    let metrics = client.get(format!("{base}/metrics")).send().await.unwrap();
    assert_eq!(metrics.status(), 200);

    // Plain HTTP on the redirect port: 308 to the same path over HTTPS.
    let plain = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let moved = plain
        .get(format!("http://127.0.0.1:{redirect}/v1/static/versions?x=1"))
        .send()
        .await
        .unwrap();
    assert_eq!(moved.status(), 308);
    assert_eq!(
        moved.headers()["location"],
        format!("https://127.0.0.1:{https}/v1/static/versions?x=1").as_str()
    );

    // Plain HTTP on PORT stays up, on loopback, for the healthcheck.
    let local = plain
        .get(format!("http://127.0.0.1:{port}/healthz"))
        .send()
        .await
        .unwrap();
    assert_eq!(local.status(), 200);
}
