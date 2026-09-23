//! The real binary: subcommands, key storage, bootstrap-once, healthcheck.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_riot-proxy");
const KEY: &str = "RGAPI-test-key-not-real";

fn cmd(data_dir: &Path) -> Command {
    let mut c = Command::new(BIN);
    c.env_clear()
        .env("RIOT_API_KEY", KEY)
        .env("DATA_DIR", data_dir)
        .env("LOG_FORMAT", "json")
        .current_dir(data_dir.parent().unwrap());
    c
}

fn run(data_dir: &Path, args: &[&str]) -> Output {
    let out = cmd(data_dir).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "{args:?} failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn stdout(out: &Output) -> String {
    String::from_utf8(out.stdout.clone()).unwrap()
}

fn extract_key(text: &str) -> String {
    text.split_whitespace()
        .find(|w| w.starts_with("rpx_"))
        .expect("an rpx_ key")
        .to_string()
}

/// Every byte of every file in the data dir (db, -wal, -shm).
fn all_bytes(dir: &Path) -> Vec<u8> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            out.extend(std::fs::read(path).unwrap());
        }
    }
    out
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn every_subcommand_has_help() {
    for args in [
        vec!["--help"],
        vec!["serve", "--help"],
        vec!["migrate", "--help"],
        vec!["key", "--help"],
        vec!["key", "create", "--help"],
        vec!["key", "list", "--help"],
        vec!["key", "revoke", "--help"],
        vec!["healthcheck", "--help"],
        vec!["spec", "--help"],
    ] {
        let out = Command::new(BIN).args(&args).output().unwrap();
        assert!(out.status.success(), "{args:?}");
        let text = stdout(&out);
        assert!(text.contains("Usage:"), "{args:?}: {text}");
    }
    let out = Command::new(BIN).output().unwrap();
    assert!(!out.status.success(), "no subcommand prints help and fails");
}

#[test]
fn key_create_stores_only_the_sha256() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let out = run(&data, &["key", "create", "--name", "test"]);
    let text = stdout(&out);
    let key = extract_key(&text);
    assert_eq!(key.len(), 36, "{key}");
    assert!(text.contains("scopes    read"), "{text}");
    assert!(text.contains("quota     600/min"), "{text}");

    let conn = rusqlite::Connection::open(data.join("riot-proxy.db")).unwrap();
    let (name, hash, scopes): (String, Vec<u8>, String) = conn
        .query_row("SELECT name, key_sha256, scopes FROM consumers", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap();
    drop(conn);
    assert_eq!(name, "test");
    assert_eq!(scopes, r#"["read"]"#);
    let expected: [u8; 32] = riot_proxy::consumers::hash_key(&key);
    assert_eq!(hash, expected.to_vec());

    let bytes = all_bytes(&data);
    assert!(!contains(&bytes, key.as_bytes()), "plaintext key found on disk");
    assert!(!contains(&bytes, &key.as_bytes()[4..]), "key body found on disk");
}

#[test]
fn key_list_and_revoke() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    run(
        &data,
        &[
            "key",
            "create",
            "--name",
            "web",
            "--scopes",
            "read,admin",
            "--quota",
            "120",
        ],
    );
    let list = stdout(&run(&data, &["key", "list"]));
    assert!(
        list.contains("web")
            && list.contains("read,admin")
            && list.contains("120")
            && list.contains("active"),
        "{list}"
    );
    assert!(!list.contains("rpx_"), "list never shows keys: {list}");

    run(&data, &["key", "revoke", "web"]);
    assert!(stdout(&run(&data, &["key", "list"])).contains("revoked"));

    let dup = cmd(&data)
        .args(["key", "create", "--name", "web"])
        .output()
        .unwrap();
    assert!(!dup.status.success());
    assert!(String::from_utf8_lossy(&dup.stderr).contains("already exists"));

    let bad = cmd(&data)
        .args(["key", "create", "--name", "x", "--scopes", "owner"])
        .output()
        .unwrap();
    assert!(!bad.status.success());
}

#[test]
fn migrate_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    assert!(stdout(&run(&data, &["migrate"])).contains("applied V1__init"));
    assert!(stdout(&run(&data, &["migrate"])).contains("up to date"));
}

#[test]
fn spec_prints_a_json_document() {
    let out = Command::new(BIN).arg("spec").output().unwrap();
    assert!(out.status.success());
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(doc["openapi"], "3.1.0");
}

#[test]
fn missing_riot_key_is_a_clear_error() {
    let tmp = tempfile::tempdir().unwrap();
    let out = Command::new(BIN)
        .env_clear()
        .current_dir(tmp.path())
        .args(["key", "list"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("RIOT_API_KEY: is required"));
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Start `serve`, wait for "listening", and collect stdout+stderr until exit.
struct Server {
    child: Child,
    stderr: std::sync::mpsc::Receiver<String>,
    seen: Vec<String>,
}

impl Server {
    fn start(data: &Path, port: u16) -> Self {
        let mut child = cmd(data)
            .args(["serve", "--host", "127.0.0.1", "--port", &port.to_string()])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // Logs go to stdout, the bootstrap banner to stderr; merge them.
        let (tx, rx) = std::sync::mpsc::channel();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let tx2 = tx.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let _ = tx2.send(line.unwrap_or_default());
            }
        });
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                let _ = tx.send(line.unwrap_or_default());
            }
        });
        let mut server = Self {
            child,
            stderr: rx,
            seen: Vec::new(),
        };
        server.seen = server.wait_for("\"listening\"");
        server
    }

    fn wait_for(&self, needle: &str) -> Vec<String> {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut seen = Vec::new();
        while Instant::now() < deadline {
            if let Ok(line) = self.stderr.recv_timeout(Duration::from_millis(100)) {
                let hit = line.contains(needle);
                seen.push(line);
                if hit {
                    return seen;
                }
            }
        }
        panic!("never saw {needle:?}; got:\n{}", seen.join("\n"));
    }

    /// Lines already received (start() consumed up to "listening"; keep them).
    fn wait_for_nothing(&self) -> Vec<String> {
        self.stderr.try_iter().collect()
    }

    fn stop(mut self, before: Vec<String>) -> String {
        Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status()
            .unwrap();
        let status = self.child.wait().unwrap();
        assert!(status.success(), "serve exits 0 on SIGTERM");
        let mut all = std::mem::take(&mut self.seen);
        all.extend(before);
        all.extend(self.stderr.try_iter());
        all.join("\n")
    }
}

#[test]
fn serve_prints_the_bootstrap_key_exactly_once_and_healthcheck_passes() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let port = free_port();

    let first = Server::start(&data, port);
    let booted = first.wait_for_nothing();
    let hc = cmd(&data)
        .args(["healthcheck"])
        .env("PORT", port.to_string())
        .env("HOST", "0.0.0.0")
        .output()
        .unwrap();
    assert!(
        hc.status.success(),
        "healthcheck: {}",
        String::from_utf8_lossy(&hc.stderr)
    );
    let log = first.stop(booted);
    assert_eq!(
        log.matches("bootstrap admin key (shown once)").count(),
        1,
        "{log}"
    );
    let key = extract_key(log.lines().find(|l| l.contains("shown once")).unwrap());
    // The key goes to stderr as plain text, never into a JSON log line.
    assert!(
        log.lines()
            .filter(|l| l.starts_with('{'))
            .all(|l| !l.contains(&key)),
        "{log}"
    );
    assert!(!contains(&all_bytes(&data), key.as_bytes()));

    let second = Server::start(&data, port);
    let log = second.stop(Vec::new());
    assert!(!log.contains("shown once"), "second boot prints nothing: {log}");
    assert!(!log.contains("rpx_"), "{log}");

    let list = stdout(&run(&data, &["key", "list"]));
    assert!(
        list.contains("bootstrap-admin") && list.contains("read,admin"),
        "{list}"
    );
}

#[test]
fn healthcheck_fails_when_nothing_listens() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let out = cmd(&data)
        .args(["healthcheck", "--timeout", "1"])
        .env("PORT", free_port().to_string())
        .output()
        .unwrap();
    assert!(!out.status.success());
}

#[cfg(feature = "dev-cli")]
#[tokio::test(flavor = "multi_thread")]
async fn riot_get_prints_the_raw_body() {
    use wiremock::matchers::{header, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(path("/riot/account/v1/accounts/by-riot-id/Hide%20on%20bush/KR1"))
        .and(header("x-riot-token", KEY))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(r#"{"puuid":"P","gameName":"Hide on bush"}"#),
        )
        .mount(&server)
        .await;
    Mock::given(path("/lol/champion-mastery/v4/champion-masteries/by-puuid/P/top"))
        .and(query_param("count", "3"))
        .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
        .mount(&server)
        .await;
    Mock::given(path("/lol/summoner/v4/summoners/by-puuid/missing"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let uri = server.uri();
    let data2 = data.clone();
    let out = tokio::task::spawn_blocking(move || {
        run(
            &data2,
            &[
                "riot",
                "get",
                "account/by-riot-id",
                "europe",
                "Hide on bush",
                "KR1",
                "--base-url",
                &uri,
            ],
        )
    })
    .await
    .unwrap();
    assert_eq!(stdout(&out).trim(), r#"{"puuid":"P","gameName":"Hide on bush"}"#);
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .starts_with("200 /riot/account/v1/accounts/by-riot-id/Hide%20on%20bush/KR1")
    );

    let uri = server.uri();
    let data2 = data.clone();
    let out = tokio::task::spawn_blocking(move || {
        run(
            &data2,
            &[
                "riot",
                "get",
                "mastery.topByPuuid",
                "kr",
                "P",
                "-q",
                "count=3",
                "--base-url",
                &uri,
            ],
        )
    })
    .await
    .unwrap();
    assert_eq!(stdout(&out).trim(), "[]");

    let uri = server.uri();
    let out = tokio::task::spawn_blocking(move || {
        cmd(&data)
            .args([
                "riot",
                "get",
                "summoner/by-puuid",
                "euw1",
                "missing",
                "--base-url",
                &uri,
            ])
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("NotFound"));
}
