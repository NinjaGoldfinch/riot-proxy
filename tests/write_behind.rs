//! Request-path writes run behind the response (AVL-02, ADR-210): with the one
//! SQLite writer held by a long write, as an `aggregate:analytics` step holds
//! it for minutes on production, a request whose data goes upstream still
//! answers at once, and what it fetched is stored once the writer is free.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use crate::common;

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use bytes::Bytes;
use riot_proxy::archive::SqliteArchive;
use riot_proxy::archive::pack::Kind;
use riot_proxy::cache::keys::KeyScope;
use riot_proxy::consumers::{self, NewConsumer, Scope};
use riot_proxy::db::{Db, DbError};
use riot_proxy::fetcher::Archive;
use riot_proxy::jobs::Queue;
use riot_proxy::jobs::offload::Pending;
use riot_proxy::riot::client::RiotRequest;
use riot_proxy::riot::endpoints::Endpoint;
use riot_proxy::riot::routing::Region;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/replay/cold-lookup");
const PUUID: &str = "NkQRxdiN3U3pEek5MWbWgaxzG_hpH5imJ9Ttch8ql5KM7D6p6Bh-Hbbvn6UoFVdGUBBIvcnEJv72qw";
const MATCH_IDS: [&str; 5] = [
    "KR_8393343196",
    "KR_8393320187",
    "KR_8393276163",
    "KR_8393234454",
    "KR_8393207026",
];
const PROFILE: &str = "/v1/players/by-riot-id/Hide%20on%20bush/KR1/profile?platform=kr";
/// What "answers at once" means here; the hold lasts up to 60 s.
const PROMPT: Duration = Duration::from_secs(1);

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(Path::new(FIXTURES).join(name)).unwrap()
}

/// The writer, busy with one write until `release` (or 60 s).
struct Hold {
    release: std::sync::mpsc::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

impl Hold {
    async fn start(db: &Db) -> Self {
        let (release, gate) = std::sync::mpsc::channel::<()>();
        let held = db.clone();
        let task = tokio::spawn(async move {
            held.write(move |_| {
                let _ = gate.recv_timeout(Duration::from_secs(60));
                Ok::<_, DbError>(())
            })
            .await
            .unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while db.writer_status().busy_since.is_none() {
            assert!(Instant::now() < deadline, "the writer never took the hold");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        Self { release, task }
    }

    async fn end(self) {
        let _ = self.release.send(());
        self.task.await.unwrap();
    }
}

async fn count(db: &Db, sql: &'static str) -> i64 {
    db.read(move |c| Ok::<_, DbError>(c.query_row(sql, [], |r| r.get(0))?))
        .await
        .unwrap()
}

struct Env {
    _dir: tempfile::TempDir,
    state: riot_proxy::app::AppState,
    router: axum::Router,
    key: String,
    _server: MockServer,
}

/// The full app over the cold-lookup recording: a player's profile and match
/// page, and their five matches.
async fn env() -> Env {
    let server = MockServer::start().await;
    let mut routes = vec![
        (
            "/riot/account/v1/accounts/by-riot-id/Hide%20on%20bush/KR1".to_string(),
            fixture("01-account.byRiotId.body"),
        ),
        (
            format!("/lol/summoner/v4/summoners/by-puuid/{PUUID}"),
            fixture("02-summoner.byPuuid.body"),
        ),
        (
            format!("/lol/league/v4/entries/by-puuid/{PUUID}"),
            fixture("03-league.entriesByPuuid.body"),
        ),
        (
            format!("/lol/champion-mastery/v4/champion-masteries/by-puuid/{PUUID}/top"),
            fixture("04-mastery.topByPuuid.body"),
        ),
        (
            format!("/lol/match/v5/matches/by-puuid/{PUUID}/ids"),
            fixture("05-match.idsByPuuid.body"),
        ),
    ];
    for (i, id) in MATCH_IDS.iter().enumerate() {
        routes.push((
            format!("/lol/match/v5/matches/{id}"),
            fixture(&format!("{:02}-match.byId.body", i + 6)),
        ));
    }
    for (p, body) in routes {
        Mock::given(method("GET"))
            .and(path(p))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/json"))
            .mount(&server)
            .await;
    }
    let (dir, state, router) = common::app_with(&[], &server.uri());
    let key = consumers::create(
        &state.db,
        NewConsumer {
            name: "web".into(),
            scopes: vec![Scope::Read],
            quota_per_min: 10_000,
            key: None,
        },
    )
    .await
    .unwrap()
    .key
    .expose()
    .to_string();
    Env {
        _dir: dir,
        state,
        router,
        key,
        _server: server,
    }
}

impl Env {
    /// GET `uri`, failing the test unless it answers within [`PROMPT`].
    async fn get_promptly(&self, uri: &str) -> common::Reply {
        let req = Request::get(uri)
            .header("authorization", format!("Bearer {}", self.key))
            .body(Body::empty())
            .unwrap();
        let started = Instant::now();
        let reply = tokio::time::timeout(PROMPT, common::send(self.router.clone(), req))
            .await
            .unwrap_or_else(|_| panic!("{uri} waited for the held writer"));
        eprintln!("{uri}: {:?} with the writer held", started.elapsed());
        reply
    }

    fn db(&self) -> &Db {
        &self.state.db
    }
}

fn x_cache(r: &common::Reply) -> &str {
    r.headers["x-cache"].to_str().unwrap()
}

/// A MISS profile: the league part goes upstream and its ranks are recorded,
/// and the player is remembered, all behind the response.
#[tokio::test]
async fn a_miss_profile_answers_while_the_writer_is_held() {
    let e = env().await;
    let hold = Hold::start(e.db()).await;

    let r = e.get_promptly(PROFILE).await;
    assert_eq!((r.status, x_cache(&r)), (StatusCode::OK, "MISS"));
    assert_eq!(
        r.json()["league"][0]["tier"],
        "CHALLENGER",
        "the league part came from Riot"
    );
    assert!(e.db().detached_writes() >= 2, "the ranks and the player wait");
    assert_eq!(count(e.db(), "SELECT count(*) FROM player_ranks").await, 0);
    assert_eq!(count(e.db(), "SELECT count(*) FROM players").await, 0);

    hold.end().await;
    e.db().flush_detached().await;
    assert_eq!(e.db().detached_writes(), 0);
    assert_eq!(
        count(
            e.db(),
            "SELECT count(*) FROM player_ranks WHERE tier = 'CHALLENGER' AND queue = 'RANKED_SOLO_5x5'"
        )
        .await,
        1
    );
    assert_eq!(count(e.db(), "SELECT count(*) FROM rank_lookups").await, 1);
    let scope = e.state.fetcher.key_scope().as_str().to_string();
    let player = riot_proxy::players::get(e.db(), &scope, PUUID)
        .await
        .unwrap()
        .expect("the player is remembered");
    assert_eq!(
        (player.game_name.as_deref(), player.tag_line.as_deref()),
        (Some("Hide on bush"), Some("KR1"))
    );
}

/// A MISS match: answered at once, then served from the archive (the body
/// held for the write) before it is stored, and stored with its timeline
/// queued once the writer is free.
#[tokio::test]
async fn a_miss_match_answers_while_the_writer_is_held() {
    let e = env().await;
    let hold = Hold::start(e.db()).await;
    let uri = format!("/v1/lol/matches/asia/{}", MATCH_IDS[0]);

    let first = e.get_promptly(&uri).await;
    assert_eq!((first.status, x_cache(&first)), (StatusCode::OK, "MISS"));
    assert_eq!(first.body, fixture("06-match.byId.body"));
    let second = e.get_promptly(&uri).await;
    assert_eq!(
        (second.status, x_cache(&second), second.body.as_slice()),
        (StatusCode::OK, "ARCHIVE", first.body.as_slice()),
        "the unstored body answers as the archive would"
    );
    assert_eq!(count(e.db(), "SELECT count(*) FROM matches").await, 0);

    hold.end().await;
    e.db().flush_detached().await;
    assert_eq!(count(e.db(), "SELECT count(*) FROM matches").await, 1);
    assert_eq!(count(e.db(), "SELECT count(*) FROM match_facts").await, 10);
    assert_eq!(
        count(
            e.db(),
            "SELECT count(*) FROM jobs WHERE kind = 'archive:match' AND state = 'pending'"
        )
        .await,
        1,
        "the stored match queues its timeline (TL-01)"
    );
    let third = e.get_promptly(&uri).await;
    assert_eq!(x_cache(&third), "ARCHIVE");
}

/// The first page of a lookup: `maybe_backfill` reads the player and finds the
/// walk already queued without the writer; the player row and the page's
/// matches are written behind it.
#[tokio::test]
async fn a_match_page_answers_while_the_writer_is_held() {
    let e = env().await;
    let walk = riot_proxy::jobs::archive::BackfillPlayer {
        puuid: PUUID.into(),
        platform: "kr".into(),
        limit: 500,
        fetch_timeline: None,
        queue_id: None,
        reason: Some("lookup".into()),
    };
    let queued = riot_proxy::jobs::archive::enqueue_backfill(&e.state.jobs, &walk)
        .await
        .unwrap();
    let hold = Hold::start(e.db()).await;

    let r = e
        .get_promptly(&format!("/v1/players/{PUUID}/matches?platform=kr&count=5"))
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let body = r.json();
    assert_eq!(body["matches"].as_array().unwrap().len(), 5);
    assert_eq!(
        (
            body["backfill"]["jobId"].as_str(),
            body["backfill"]["status"].as_str()
        ),
        (Some(queued.id.as_str()), Some("already-queued"))
    );
    assert_eq!(count(e.db(), "SELECT count(*) FROM players").await, 0);

    hold.end().await;
    e.db().flush_detached().await;
    assert_eq!(count(e.db(), "SELECT count(*) FROM players").await, 1);
    assert_eq!(count(e.db(), "SELECT count(*) FROM matches").await, 5);
}

/// A player whose walk has finished queues nothing, and the page still does
/// not wait for the writer.
#[tokio::test]
async fn a_walked_players_page_answers_while_the_writer_is_held() {
    let e = env().await;
    let scope = e.state.fetcher.key_scope().as_str().to_string();
    riot_proxy::players::upsert(
        e.db(),
        &scope,
        riot_proxy::players::Upsert {
            puuid: PUUID,
            platform: "kr",
            ..Default::default()
        },
        1,
    )
    .await
    .unwrap();
    let (s, p) = (scope.clone(), PUUID.to_string());
    e.db()
        .write(move |c| {
            c.execute(
                r#"UPDATE players SET backfill_state = '{"startedAt":1,"doneAt":1,"depth":500}' WHERE key_scope = ?1 AND puuid = ?2"#,
                (s, p),
            )?;
            Ok::<_, DbError>(())
        })
        .await
        .unwrap();
    let hold = Hold::start(e.db()).await;

    let r = e
        .get_promptly(&format!("/v1/players/{PUUID}/matches?platform=kr&count=5"))
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["backfill"], serde_json::Value::Null);

    hold.end().await;
    e.db().flush_detached().await;
    let player = riot_proxy::players::get(e.db(), &scope, PUUID)
        .await
        .unwrap()
        .unwrap();
    assert!(player.updated_at > 1, "the lookup was recorded behind the page");
    assert_eq!(
        count(e.db(), "SELECT count(*) FROM jobs WHERE kind = 'backfill:player'").await,
        0
    );
}

fn request(endpoint: &str, id: &str) -> RiotRequest {
    let e = Endpoint::by_id(endpoint).unwrap();
    RiotRequest::new(e, e.target_for_region(Region::Asia).unwrap(), &[id]).unwrap()
}

/// A match written behind the response still counts towards the offload's
/// next pack and queues its missing timeline (OBJ-03, TL-01).
#[tokio::test]
async fn a_detached_put_feeds_the_offload_and_queues_the_missing_half() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("riot-proxy.db"), 2).unwrap();
    let queue = Queue::new(db.clone());
    let pending = Arc::new(Pending::new(queue.clone(), u64::MAX));
    let archive = SqliteArchive::new(
        db.clone(),
        KeyScope::from_key(&riot_proxy::config::Secret::new("RGAPI-test-key")),
    )
    .queue_missing_halves(queue, true)
    .count_unpacked(Arc::clone(&pending));
    let req = request("match.byId", MATCH_IDS[0]);
    let body = Bytes::from(fixture("06-match.byId.body"));

    let hold = Hold::start(&db).await;
    tokio::time::timeout(PROMPT, archive.store_behind(&req, body.clone()))
        .await
        .expect("store_behind does not wait for the writer");
    assert_eq!(
        archive.get(&req).await,
        Some(body.clone()),
        "served before it is stored"
    );
    assert_eq!(pending.bytes(Kind::Match), 0);

    hold.end().await;
    db.flush_detached().await;
    assert!(pending.bytes(Kind::Match) > 0, "counted towards a pack");
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM jobs WHERE kind = 'archive:match' AND dedupe_key = 'KR_8393343196' AND state = 'pending'"
        )
        .await,
        1
    );
    assert_eq!(
        riot_proxy::archive::matches::get(&db, MATCH_IDS[0])
            .await
            .unwrap(),
        Some(body)
    );
}

async fn insert(db: &Db, n: i64) {
    db.write(move |c| {
        c.execute("INSERT INTO t (n) VALUES (?1)", [n])?;
        Ok::<_, DbError>(())
    })
    .await
    .unwrap();
}

/// Shutdown's flush waits for every detached write still behind a busy
/// writer, so their rows are there once it returns.
#[tokio::test]
async fn the_shutdown_flush_waits_for_every_detached_write() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("riot-proxy.db"), 2).unwrap();
    db.write(|c| Ok::<_, DbError>(c.execute_batch("CREATE TABLE t (n INTEGER)")?))
        .await
        .unwrap();
    let hold = Hold::start(&db).await;
    for n in 0..50 {
        let w = db.clone();
        assert!(db.write_behind("test", 0, async move { insert(&w, n).await }));
    }
    assert_eq!(db.detached_writes(), 50);

    let flushing = tokio::spawn({
        let db = db.clone();
        async move { db.flush_detached().await }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!flushing.is_finished(), "the flush waits for the writer");
    hold.end().await;
    flushing.await.unwrap();
    assert_eq!(db.detached_writes(), 0);
    assert_eq!(count(&db, "SELECT count(*) FROM t").await, 50);
}

/// Past the bound a detached write is dropped and counted, never waited for
/// by the caller; one large write is still admitted on its own.
#[tokio::test]
async fn past_the_bound_a_detached_write_is_dropped_and_counted() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("riot-proxy.db"), 2).unwrap();
    db.write(|c| Ok::<_, DbError>(c.execute_batch("CREATE TABLE t (n INTEGER)")?))
        .await
        .unwrap();
    db.set_detached_limits(2, 1_000);
    let hold = Hold::start(&db).await;
    let behind = |n: i64, bytes: usize| {
        let w = db.clone();
        db.write_behind("test", bytes, async move { insert(&w, n).await })
    };
    assert!(behind(1, 5_000), "the first is admitted however large");
    assert!(!behind(2, 10), "over the bytes");
    hold.end().await;
    db.flush_detached().await;

    let hold = Hold::start(&db).await;
    assert!(behind(3, 10));
    assert!(behind(4, 10));
    assert!(!behind(5, 10), "over the count");
    assert_eq!(db.detached_dropped(), 2);
    hold.end().await;
    db.flush_detached().await;
    assert_eq!(
        count(
            &db,
            "SELECT group_concat(n) = '1,3,4' FROM (SELECT n FROM t ORDER BY n)"
        )
        .await,
        1
    );
}
