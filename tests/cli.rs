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
        vec!["backup", "--help"],
        vec!["packs", "--help"],
        vec!["packs", "reindex", "--help"],
        vec!["packs", "restore-index", "--help"],
        vec!["packs", "snapshot", "--help"],
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
        Self::start_with(data, port, &[])
    }

    fn start_with(data: &Path, port: u16, env: &[(&str, &str)]) -> Self {
        let mut child = cmd(data)
            .envs(env.iter().copied())
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

/// AVL-01 (ADR-209): "listening" comes before the boot catch-up, which logs
/// its duration as it finishes and only then starts the job workers.
#[test]
fn serve_listens_before_the_boot_catch_up_and_logs_its_duration() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let server = Server::start(&data, free_port());
    assert!(
        !server.seen.iter().any(|l| l.contains("boot catch-up done")),
        "{:?}",
        server.seen
    );
    let after = server.wait_for("starting job workers");
    let done = after
        .iter()
        .position(|l| l.contains("boot catch-up done"))
        .unwrap_or_else(|| panic!("{after:?}"));
    assert!(after[done].contains("\"ms\":"), "{}", after[done]);
    assert!(
        done < after.len() - 1,
        "the catch-up finishes before the workers start"
    );
    server.stop(after);
}

/// OBJ-01: boot names the bucket and endpoint, never the keys, and makes no
/// request to it (the endpoint here does not resolve).
#[test]
fn serve_logs_the_object_store_without_its_keys() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let server = Server::start_with(
        &data,
        free_port(),
        &[
            ("ARCHIVE_OBJECT_URL", "s3://riot-proxy-archive"),
            ("ARCHIVE_OBJECT_ENDPOINT", "https://s3.invalid"),
            ("ARCHIVE_OBJECT_ACCESS_KEY", "AKIDBOOTLOGTEST"),
            ("ARCHIVE_OBJECT_SECRET_KEY", "boot-log-secret-value"),
        ],
    );
    let log = server.stop(Vec::new());
    let line = log
        .lines()
        .find(|l| l.contains("object store configured"))
        .unwrap_or_else(|| panic!("{log}"));
    assert!(
        line.contains("s3://riot-proxy-archive at https://s3.invalid"),
        "{line}"
    );
    assert!(
        !log.contains("AKIDBOOTLOGTEST") && !log.contains("boot-log-secret"),
        "{log}"
    );
}

/// OBJ-03: with a bucket, boot queues `archive:offload` and a worker runs it;
/// without one, the kind does not exist and nothing is queued.
#[test]
fn serve_with_a_bucket_runs_the_offload_at_boot() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let bucket = tmp.path().join("bucket");
    let url = format!("file://{}", bucket.display());
    let server = Server::start_with(&data, free_port(), &[("ARCHIVE_OBJECT_URL", &url)]);
    let seen = server.wait_for("offload done");
    let log = server.stop(seen);
    assert!(log.contains("queued archive:offload"), "{log}");
    assert!(bucket.is_dir(), "the local bucket is created");

    let plain = Server::start(&tmp.path().join("plain"), free_port());
    let log = plain.stop(Vec::new());
    assert!(
        !log.contains("queued archive:offload") && !log.contains("offload done"),
        "{log}"
    );
}

/// STG-02: a read-only bucket gets no offload at boot, so a local body is
/// neither packed nor freed and nothing is written to the bucket; `packs
/// snapshot` is refused.
#[tokio::test]
async fn serve_with_a_read_only_bucket_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let bucket = tmp.path().join("bucket");
    let url = format!("file://{}", bucket.display());
    run(&data, &["migrate"]);
    let body = zstd::encode_all(&b"{\"metadata\":{}}"[..], 3).unwrap();
    rusqlite::Connection::open(data.join("riot-proxy.db"))
        .unwrap()
        .execute(
            "INSERT INTO matches (match_id, region, patch, queue_id, game_end_ms, body_zstd, body_size, archived_at)
             VALUES ('OC1_1', 'sea', '16.19', 420, 1, ?1, 1, 1)",
            [body],
        )
        .unwrap();
    let env = [
        ("ARCHIVE_OBJECT_URL", url.as_str()),
        ("ARCHIVE_OBJECT_READ_ONLY", "true"),
        ("ARCHIVE_BODY_LOCAL_DAYS", "0"),
        ("ARCHIVE_PACK_TARGET_BYTES", "1048576"),
    ];
    let server = Server::start_with(&data, free_port(), &env);
    std::thread::sleep(Duration::from_secs(2));
    let log = server.stop(Vec::new());
    assert!(log.contains("(read-only)"), "{log}");
    assert!(
        !log.contains("queued archive:offload") && !log.contains("offload done"),
        "{log}"
    );
    let files = walk(&bucket);
    assert!(files.is_empty(), "nothing written to the bucket: {files:?}");
    let local: Option<Vec<u8>> = rusqlite::Connection::open(data.join("riot-proxy.db"))
        .unwrap()
        .query_row("SELECT body_zstd FROM matches", [], |r| r.get(0))
        .unwrap();
    assert!(local.is_some(), "the local body is kept");

    let out = cmd(&data)
        .envs(env.iter().copied())
        .args(["packs", "snapshot"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("ARCHIVE_OBJECT_READ_ONLY"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(walk(&bucket).is_empty());
}

/// Every file under `dir`, or none if it does not exist.
fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .flat_map(|e| {
            let p = e.path();
            if p.is_dir() { walk(&p) } else { vec![p] }
        })
        .collect()
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

#[test]
fn backup_writes_a_copy_that_opens_and_never_overwrites() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    run(&data, &["key", "create", "--name", "kept"]);
    let out = tmp.path().join("copies").join("before-upgrade.db");
    let text = stdout(&run(&data, &["backup", out.to_str().unwrap()]));
    assert!(
        text.starts_with(out.to_str().unwrap()) && text.contains("bytes"),
        "{text}"
    );
    let conn = rusqlite::Connection::open(&out).unwrap();
    let name: String = conn
        .query_row("SELECT name FROM consumers", [], |r| r.get(0))
        .unwrap();
    assert_eq!(name, "kept");

    let again = cmd(&data)
        .args(["backup", out.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!again.status.success());
    assert!(
        String::from_utf8_lossy(&again.stderr).contains("already exists"),
        "{}",
        String::from_utf8_lossy(&again.stderr)
    );
}

/// OBJ-04: `packs snapshot` writes today's index snapshot, `packs
/// restore-index` brings an emptied map back from it, and `packs reindex`
/// rebuilds it from the packs; all three need a bucket.
#[tokio::test]
async fn packs_snapshot_restore_and_reindex_rebuild_the_map() {
    use riot_proxy::archive::objects::Objects;
    use riot_proxy::archive::pack::Kind;

    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let bucket = tmp.path().join("bucket");
    let url = format!("file://{}", bucket.display());
    let packs = |args: &[&str]| {
        let out = cmd(&data)
            .env("ARCHIVE_OBJECT_URL", &url)
            .arg("packs")
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        stdout(&out)
    };

    run(&data, &["migrate"]);
    let db = riot_proxy::db::Db::open(&data.join("riot-proxy.db"), 1).unwrap();
    std::fs::create_dir_all(&bucket).unwrap();
    let objects = Objects::from_store(
        std::sync::Arc::new(object_store::local::LocalFileSystem::new_with_prefix(&bucket).unwrap()),
        false,
        "file://bucket",
    );
    let body = zstd::encode_all(&b"{\"metadata\":{}}"[..], 3).unwrap();
    db.write(move |c| {
        c.execute(
            "INSERT INTO matches (match_id, region, patch, queue_id, game_end_ms, body_zstd, body_size, archived_at)
             VALUES ('OC1_1', 'sea', '16.19', 420, 1, ?1, 1, 1)",
            [body],
        )?;
        Ok::<_, riot_proxy::db::DbError>(())
    })
    .await
    .unwrap();
    riot_proxy::archive::offload::write_pack(
        &db,
        &objects,
        Kind::Match,
        &["OC1_1".to_string()],
        1_791_000_000_000,
    )
    .await
    .unwrap()
    .unwrap();
    let before = riot_proxy::archive::recovery::tables(&db).await.unwrap();
    let locations = || {
        rusqlite::Connection::open(data.join("riot-proxy.db"))
            .unwrap()
            .query_row("SELECT count(*) FROM body_locations", [], |r| r.get::<_, i64>(0))
            .unwrap()
    };
    let empty = || {
        rusqlite::Connection::open(data.join("riot-proxy.db"))
            .unwrap()
            .execute_batch("DELETE FROM body_locations; DELETE FROM body_packs;")
            .unwrap();
    };

    let text = packs(&["snapshot"]);
    assert!(text.contains("1 packs, 1 locations"), "{text}");
    empty();
    let text = packs(&["restore-index"]);
    assert!(
        text.contains("restored the snapshot of") && text.contains("reindexed 0 newer packs"),
        "{text}"
    );
    assert_eq!(locations(), 1);
    assert_eq!(riot_proxy::archive::recovery::tables(&db).await.unwrap(), before);

    empty();
    let text = packs(&["reindex"]);
    assert!(text.contains("reindexed 1 packs, 1 bodies"), "{text}");
    assert_eq!(riot_proxy::archive::recovery::tables(&db).await.unwrap(), before);
    let since = &before.packs[0].pack_id;
    let text = packs(&["reindex", "--since", since]);
    assert!(text.contains("reindexed 0 packs"), "{text}");

    let out = cmd(&data).args(["packs", "reindex"]).output().unwrap();
    assert!(!out.status.success(), "no bucket, no reindex");
    assert!(String::from_utf8_lossy(&out.stderr).contains("ARCHIVE_OBJECT_URL"));
}

// ─── AVL-05: a rebuild killed mid-way resumes (ADR-215) ─────────────────────

/// The key scope the rebuild tests pin, so the seeded facts are the server's.
const REBUILD_SCOPE: &str = "a05a05a0";
/// The patches seeded, newest first: the order the rebuild walks them in.
const REBUILD_PATCHES: [&str; 8] = [
    "14.18", "14.17", "14.16", "14.15", "14.14", "14.13", "14.12", "14.11",
];
/// The rebuild's job row, queued before the server boots.
const REBUILD_JOB: &str = "avl05-rebuild";
/// One table each of the rebuild steps that write rows from these facts, by
/// step index (the sets step reads `match_builds`, which these have none of).
const STEP_TABLES: [(usize, &str); 5] = [
    (0, "champion_stats"),
    (1, "champion_matchups"),
    (2, "champion_items"),
    (3, "champion_runes"),
    (4, "champion_spells"),
];

/// A migrated data dir holding 12 KR solo matches a patch over
/// [`REBUILD_PATCHES`], every fact stamped with a tier, and one queued
/// `aggregate:analytics` for the ladder, which no rebuild has stamped.
fn seed_rebuild(data: &Path) -> std::path::PathBuf {
    std::fs::create_dir_all(data).unwrap();
    run(data, &["migrate"]);
    let path = data.join("riot-proxy.db");
    let mut c = rusqlite::Connection::open(&path).unwrap();
    let tx = c.transaction().unwrap();
    let positions = ["TOP", "JUNGLE", "MIDDLE", "BOTTOM", "UTILITY"];
    let tiers = ["MASTER", "GRANDMASTER", "DIAMOND", "UNKNOWN"];
    let mut n = 0_i64;
    for patch in REBUILD_PATCHES {
        for _ in 0..12 {
            n += 1;
            let id = format!("KR_{}", 7_000_000 + n);
            tx.execute(
                "INSERT INTO matches (match_id, region, patch, queue_id, game_end_ms, body_zstd, body_size,
                   archived_at, game_duration, remake, facts_version)
                 VALUES (?1, 'asia', ?2, 420, ?3, x'00', 1, ?3, ?4, 0, ?5)",
                rusqlite::params![
                    id,
                    patch,
                    1_700_000_000_000 + n,
                    900 + n * 7 % 1500,
                    riot_proxy::archive::facts::FACTS_VERSION
                ],
            )
            .unwrap();
            for p in 0..10_i64 {
                let team = if p < 5 { 100 } else { 200 };
                let champion = 1 + (n * 3 + p * 7) % 11;
                tx.execute(
                    "INSERT INTO match_facts (match_id, key_scope, puuid, team_id, position, champion_id, win,
                       kills, deaths, assists, cs, gold, damage, vision, items, runes, summoners, facts_version, tier)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11 * 50, ?11 * 100, ?11 / 10,
                       ?12, '[8000,8112,8300,[],null]', ?13, ?14, ?15)",
                    rusqlite::params![
                        id,
                        REBUILD_SCOPE,
                        format!("P{}", (n + p) % 37),
                        team,
                        positions[usize::try_from(p % 5).unwrap()],
                        champion,
                        (team == 100) == (n % 2 == 0),
                        n % 9,
                        p % 7,
                        (n + p) % 11,
                        100 + (n * p) % 200,
                        format!("[3157,{},0,0,0,0]", 3000 + (n + p) % 5),
                        if p % 2 == 0 { "[4,14]" } else { "[4,12]" },
                        riot_proxy::archive::facts::FACTS_VERSION,
                        tiers[usize::try_from((n + p) % 4).unwrap()],
                    ],
                )
                .unwrap();
            }
        }
    }
    tx.execute(
        "INSERT INTO jobs (id, kind, dedupe_key, priority, payload, state, attempts, run_after)
         VALUES (?1, 'aggregate:analytics', 'kr:RANKED_SOLO_5x5', 30000,
                 '{\"platform\":\"kr\",\"queue\":\"RANKED_SOLO_5x5\"}', 'pending', 0, 0)",
        [REBUILD_JOB],
    )
    .unwrap();
    tx.commit().unwrap();
    path
}

/// The rebuild server: the seeded scope, every patch rebuilt, nothing that
/// leaves the box.
fn start_rebuild_server(data: &Path) -> Server {
    Server::start_with(
        data,
        free_port(),
        &[
            ("KEY_SCOPE", REBUILD_SCOPE),
            ("AGGREGATE_PATCH_LIMIT", "0"),
            ("RIOT_BASE_URL", "http://127.0.0.1:9"),
            ("DDRAGON_BASE_URL", "http://127.0.0.1:9"),
        ],
    )
}

/// The rebuild job's state and payload.
fn rebuild_row(db: &Path) -> (String, serde_json::Value) {
    let c = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let (state, payload): (String, String) = c
        .query_row(
            "SELECT state, payload FROM jobs WHERE id = ?1",
            [REBUILD_JOB],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    (state, serde_json::from_str(&payload).unwrap())
}

/// Wait until the rebuild job is `done`.
fn wait_rebuilt(db: &Path) {
    let deadline = Instant::now() + Duration::from_secs(120);
    while rebuild_row(db).0 != "done" {
        assert!(Instant::now() < deadline, "never rebuilt: {:?}", rebuild_row(db));
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// `computed_at` of each step table's rows, by (step, patch): one value each,
/// since a step writes a patch's rows in one transaction.
fn computed_at(db: &Path) -> std::collections::BTreeMap<(usize, String), i64> {
    let c = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let mut out = std::collections::BTreeMap::new();
    for (step, table) in STEP_TABLES {
        let mut stmt = c
            .prepare(&format!(
                "SELECT patch, min(computed_at), max(computed_at) FROM {table} GROUP BY patch"
            ))
            .unwrap();
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?))
            })
            .unwrap();
        for row in rows {
            let (patch, lo, hi) = row.unwrap();
            assert_eq!(lo, hi, "{table} {patch}: one transaction");
            out.insert((step, patch), lo);
        }
    }
    out
}

/// Every row of every analytics table and every stamp, `computed_at` left
/// out, as text: what `diff` would compare.
fn analytics_rows(db: &Path) -> Vec<String> {
    let c = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let tables: Vec<String> = c
        .prepare(
            "SELECT name FROM sqlite_master WHERE type = 'table'
               AND (name LIKE 'champion_%' OR name LIKE 'analytics_%') AND name <> 'analytics_runs'
             ORDER BY name",
        )
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let mut out = vec![];
    for t in tables {
        let cols: Vec<String> = c
            .prepare(&format!(
                "SELECT name FROM pragma_table_info('{t}') WHERE name NOT IN ('computed_at', 'stamped_at') ORDER BY cid"
            ))
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let row = cols
            .iter()
            .map(|c| format!("quote({c})"))
            .collect::<Vec<_>>()
            .join(" || '|' || ");
        let rows: Vec<String> = c
            .prepare(&format!("SELECT '{t}|' || {row} FROM {t} ORDER BY 1"))
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        out.extend(rows);
    }
    out
}

/// AVL-05 (ADR-215): a SIGKILL in the middle of a rebuild leaves its row
/// `running` with the progress its last commit saved; the next boot
/// re-queues it and resumes at that patch and step. What was committed
/// before the kill is not written again, everything after it is, and the
/// analytics tables and stamps come out as a rebuild run straight through
/// leaves them.
#[test]
fn a_rebuild_killed_mid_way_resumes_at_its_recorded_patch_and_step() {
    let tmp = tempfile::tempdir().unwrap();

    // A rebuild run straight through: the reference.
    let clean = tmp.path().join("clean");
    let clean_db = seed_rebuild(&clean);
    let server = start_rebuild_server(&clean);
    wait_rebuilt(&clean_db);
    server.stop(Vec::new());

    // The same, killed once it is past its third patch's first step.
    let data = tmp.path().join("data");
    let db = seed_rebuild(&data);
    let mut server = start_rebuild_server(&data);
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let (state, payload) = rebuild_row(&db);
        assert_ne!(state, "done", "finished before the kill: make the rebuild longer");
        if payload["rebuild"]["patch"].as_u64().is_some_and(|p| p >= 2) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "never got to the third patch: {payload}"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    server.child.kill().unwrap(); // SIGKILL
    server.child.wait().unwrap();

    // What the kill left: the row as its last commit saved it.
    let (state, payload) = rebuild_row(&db);
    assert_eq!(state, "running", "{payload}");
    let progress = &payload["rebuild"];
    let at = (
        usize::try_from(progress["patch"].as_u64().unwrap()).unwrap(),
        usize::try_from(progress["step"].as_u64().unwrap()).unwrap(),
    );
    assert!(at.0 >= 2 && at.0 < REBUILD_PATCHES.len(), "{payload}");
    assert_eq!(progress["pruned"], true, "{payload}");
    let before = computed_at(&db);
    let committed = |step: usize, patch: usize| (patch, step) < at;
    for (i, patch) in REBUILD_PATCHES.iter().enumerate() {
        for (step, _) in STEP_TABLES {
            assert_eq!(
                before.contains_key(&(step, (*patch).to_string())),
                committed(step, i),
                "step {step} of {patch} against the recorded {at:?}"
            );
        }
    }
    let killed_at = before.values().copied().max().unwrap();

    // The next boot resumes it.
    let server = start_rebuild_server(&data);
    wait_rebuilt(&db);
    server.stop(Vec::new());
    let after = computed_at(&db);
    for (i, patch) in REBUILD_PATCHES.iter().enumerate() {
        for (step, table) in STEP_TABLES {
            let key = (step, (*patch).to_string());
            let now = after[&key];
            if committed(step, i) {
                assert_eq!(now, before[&key], "{table} {patch} was written again");
            } else {
                assert!(
                    now > killed_at,
                    "{table} {patch} was not written after the restart"
                );
            }
        }
    }
    assert_eq!(analytics_rows(&db), analytics_rows(&clean_db));
    let c = rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let (status, stamped): (String, Option<i64>) = c
        .query_row(
            "SELECT r.status, s.stamped_at FROM analytics_runs r
               LEFT JOIN analytics_stamped s USING (key_scope, platform, queue)
              WHERE r.key_scope = ?1 AND r.platform = 'kr' AND r.kind = 'rebuild'",
            [REBUILD_SCOPE],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(status, "completed");
    assert!(stamped.is_some(), "a rebuild of every patch stamps the ladder");
}
