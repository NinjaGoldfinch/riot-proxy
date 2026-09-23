//! A real SIGTERM lets an in-flight request finish before the server exits.
//! Its own test binary: it installs a process-wide SIGTERM handler.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::{Duration, Instant};

use axum::Router;
use axum::routing::get;
use riot_proxy::app;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

async fn raw_get(addr: std::net::SocketAddr, path: &str) -> String {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let req = format!("GET {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    stream.read_to_string(&mut out).await.unwrap();
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sigterm_drains_in_flight_requests() {
    let router = Router::new().route(
        "/slow",
        get(|| async {
            tokio::time::sleep(Duration::from_millis(500)).await;
            "done"
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let shutdown = app::shutdown_signal().expect("signal handlers");
    let server = tokio::spawn(app::serve(listener, router, shutdown));

    let in_flight = tokio::spawn(raw_get(addr, "/slow"));
    tokio::time::sleep(Duration::from_millis(100)).await; // request is now inside the handler

    let started = Instant::now();
    let status = std::process::Command::new("kill")
        .args(["-TERM", &std::process::id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());

    let response = in_flight.await.unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.ends_with("done"), "{response}");

    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("server exits after draining")
        .unwrap()
        .unwrap();
    assert!(started.elapsed() < app::SHUTDOWN_GRACE, "drained, not timed out");

    // No longer accepting.
    assert!(TcpStream::connect(addr).await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drain_is_bounded_by_the_grace_period() {
    let router = Router::new().route(
        "/stuck",
        get(|| async {
            tokio::time::sleep(Duration::from_secs(60)).await;
            "never"
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(app::serve_with_grace(
        listener,
        router,
        async {
            let _ = rx.await;
        },
        Duration::from_millis(300),
    ));
    let _stuck = tokio::spawn(raw_get(addr, "/stuck"));
    tokio::time::sleep(Duration::from_millis(100)).await;

    let started = Instant::now();
    tx.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("exits")
        .unwrap()
        .unwrap();
    let took = started.elapsed();
    assert!(
        took >= Duration::from_millis(300) && took < Duration::from_secs(2),
        "{took:?}"
    );
}
