//! The poll handlers against a wiremock Riot (plan P6-05): every transition
//! publishes exactly one event and a steady state publishes none; the match
//! cursor advances and a gap deeper than the catch-up limit becomes a backfill.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::Arc;

use riot_proxy::archive::SqliteArchive;
use riot_proxy::cache::keys::KeyScope;
use riot_proxy::db::{Db, DbError};
use riot_proxy::jobs::poll::PollContext;
use riot_proxy::jobs::{Job, Queue};
use riot_proxy::players::{self, Upsert};
use riot_proxy::riot::limiter::Limiter;
use riot_proxy::ws::protocol::FIREHOSE;
use riot_proxy::ws::{Hub, Topic};
use serde_json::{Value, json};
use tokio::sync::broadcast;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const P: &str = "NkQRxdiN3U3pEek5MWbWgaxzG_hpH5imJ9Ttch8ql5KM7D6p6Bh-Hbbvn6UoFVdGUBBIvcnEJv72qw";

struct Env {
    _dir: tempfile::TempDir,
    server: MockServer,
    ctx: PollContext,
    db: Db,
    scope: String,
    events: broadcast::Receiver<axum::extract::ws::Utf8Bytes>,
}

async fn env(catchup_limit: u32) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("riot-proxy.db"), 2).unwrap();
    let server = MockServer::start().await;
    let config = common::config(&[]);
    let scope = KeyScope::from_key(&config.riot_api_key);
    let archive = Arc::new(SqliteArchive::new(db.clone(), scope.clone(), false));
    let fetcher = common::fetcher(&config, &server.uri(), Arc::new(Limiter::new(0.8)), Some(archive));
    let hub = Hub::new();
    let events = hub.subscribe(&Topic::named(FIREHOSE));
    let scope = scope.as_str().to_string();
    players::upsert(
        &db,
        &scope,
        Upsert {
            puuid: P,
            platform: "kr",
            tracked: Some(true),
            ..Upsert::default()
        },
        1,
    )
    .await
    .unwrap();
    let ctx = PollContext {
        fetcher,
        queue: Queue::new(db.clone()),
        hub,
        key_scope: scope.clone(),
        catchup_limit,
        backfill_limit: 10_000,
        archive_timelines: false,
    };
    Env {
        _dir: dir,
        server,
        ctx,
        db,
        scope,
        events,
    }
}

fn job(kind: &str) -> Job {
    Job {
        id: "j".into(),
        kind: kind.into(),
        dedupe_key: Some(P.into()),
        priority: 10_000,
        payload: json!({"puuid": P, "platform": "kr"}).to_string(),
        attempts: 1,
        run_after: 0,
    }
}

impl Env {
    /// Answer `p` with `status` and `body` from now on, and forget cached answers
    /// (a poll a tick later would find them expired).
    async fn riot(&self, p: &str, status: u16, body: Value) {
        self.server.reset().await;
        Mock::given(method("GET"))
            .and(path(p))
            .respond_with(ResponseTemplate::new(status).set_body_json(body))
            .mount(&self.server)
            .await;
        self.ctx.fetcher.cache().l1.invalidate_where(|_| true).await;
    }

    /// Events published since the last call, as `(name, data)`.
    fn published(&mut self) -> Vec<(String, Value)> {
        let mut out = Vec::new();
        while let Ok(frame) = self.events.try_recv() {
            let v: Value = serde_json::from_str(frame.as_str()).unwrap();
            out.push((v["event"].as_str().unwrap().to_string(), v["data"].clone()));
        }
        out
    }

    async fn player(&self) -> players::Player {
        players::get(&self.db, &self.scope, P).await.unwrap().unwrap()
    }

    async fn jobs(&self) -> Vec<(String, String, i64, String)> {
        self.db
            .read(|c| {
                let mut s = c.prepare(
                    "SELECT kind, dedupe_key, priority, payload FROM jobs ORDER BY priority, dedupe_key",
                )?;
                let r = s.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
                r.collect::<Result<Vec<_>, _>>().map_err(DbError::from)
            })
            .await
            .unwrap()
    }
}

const SPECTATOR: &str = "/lol/spectator/v5/active-games/by-summoner/NkQRxdiN3U3pEek5MWbWgaxzG_hpH5imJ9Ttch8ql5KM7D6p6Bh-Hbbvn6UoFVdGUBBIvcnEJv72qw";
const LEAGUE: &str = "/lol/league/v4/entries/by-puuid/NkQRxdiN3U3pEek5MWbWgaxzG_hpH5imJ9Ttch8ql5KM7D6p6Bh-Hbbvn6UoFVdGUBBIvcnEJv72qw";
const IDS: &str = "/lol/match/v5/matches/by-puuid/NkQRxdiN3U3pEek5MWbWgaxzG_hpH5imJ9Ttch8ql5KM7D6p6Bh-Hbbvn6UoFVdGUBBIvcnEJv72qw/ids";

fn game(id: i64) -> Value {
    json!({"gameId": id, "gameQueueConfigId": 420, "participants": [{"puuid": P, "championId": 134}, {"puuid": "other", "championId": 1}]})
}

#[tokio::test]
async fn live_polls_publish_one_event_per_transition() {
    let mut e = env(500).await;
    let live = job("poll:live");

    e.riot(SPECTATOR, 404, json!({"status": {"status_code": 404}}))
        .await;
    e.ctx.poll_live(&live).await.unwrap();
    assert!(e.published().is_empty(), "not in game, and never was");

    e.riot(SPECTATOR, 200, game(7)).await;
    e.ctx.poll_live(&live).await.unwrap();
    assert_eq!(
        e.published(),
        [(
            "game.started".into(),
            json!({"puuid": P, "platform": "kr", "gameId": 7, "queueId": 420, "championId": 134})
        )]
    );
    assert_eq!(e.player().await.in_game_id, Some(7));

    e.ctx.poll_live(&live).await.unwrap();
    assert!(e.published().is_empty(), "the same game again is no transition");

    e.riot(SPECTATOR, 404, json!({})).await;
    e.ctx.poll_live(&live).await.unwrap();
    assert_eq!(
        e.published(),
        [(
            "game.ended".into(),
            json!({"puuid": P, "platform": "kr", "gameId": 7})
        )]
    );
    assert_eq!(e.player().await.in_game_id, None);
    let jobs = e.jobs().await;
    assert_eq!(jobs.len(), 1, "the match poll is nudged");
    assert_eq!((jobs[0].0.as_str(), jobs[0].1.as_str()), ("poll:matches", P));

    e.ctx.poll_live(&live).await.unwrap();
    assert!(e.published().is_empty());

    // Straight from one game into the next: v1 reported only the new start.
    e.riot(SPECTATOR, 200, game(8)).await;
    e.ctx.poll_live(&live).await.unwrap();
    e.riot(SPECTATOR, 200, game(9)).await;
    e.ctx.poll_live(&live).await.unwrap();
    let names: Vec<String> = e.published().into_iter().map(|p| p.0).collect();
    assert_eq!(names, ["game.started", "game.started"]);
}

#[tokio::test]
async fn a_failing_spectator_call_is_retried_not_misread_as_offline() {
    let e = env(500).await;
    e.riot(SPECTATOR, 403, json!({})).await;
    let err = e.ctx.poll_live(&job("poll:live")).await.unwrap_err();
    assert!(matches!(err, riot_proxy::jobs::JobError::Retry(_)), "{err:?}");
}

fn entry(queue: &str, tier: &str, lp: i64) -> Value {
    json!({"queueType": queue, "tier": tier, "rank": "I", "leaguePoints": lp, "wins": 1, "losses": 1})
}

#[tokio::test]
async fn rank_polls_publish_one_event_per_queue_that_moved() {
    let mut e = env(500).await;
    let rank = job("poll:rank");

    e.riot(LEAGUE, 200, json!([entry("RANKED_SOLO_5x5", "CHALLENGER", 2178)]))
        .await;
    e.ctx.poll_rank(&rank).await.unwrap();
    assert!(e.published().is_empty(), "the first observation is a baseline");
    e.ctx.poll_rank(&rank).await.unwrap();
    assert!(e.published().is_empty(), "no movement, no event");

    e.riot(
        LEAGUE,
        200,
        json!([
            entry("RANKED_SOLO_5x5", "CHALLENGER", 2199),
            entry("RANKED_FLEX_SR", "MASTER", 10)
        ]),
    )
    .await;
    e.ctx.poll_rank(&rank).await.unwrap();
    assert_eq!(
        e.published(),
        [
            (
                "rank.changed".into(),
                json!({"puuid": P, "queue": "RANKED_FLEX_SR", "before": null, "after": {"tier": "MASTER", "rank": "I", "lp": 10}})
            ),
            (
                "rank.changed".into(),
                json!({"puuid": P, "queue": "RANKED_SOLO_5x5",
                       "before": {"tier": "CHALLENGER", "rank": "I", "lp": 2178},
                       "after": {"tier": "CHALLENGER", "rank": "I", "lp": 2199}})
            ),
        ]
    );
    e.ctx.poll_rank(&rank).await.unwrap();
    assert!(e.published().is_empty());
}

fn ids(range: std::ops::Range<u32>) -> Value {
    json!(range.map(|n| format!("KR_{}", 9000 - n)).collect::<Vec<_>>())
}

async fn ids_page(e: &Env, start: u32, count: u32, body: Value) {
    Mock::given(method("GET"))
        .and(path(IDS))
        .and(query_param("start", start.to_string()))
        .and(query_param("count", count.to_string()))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&e.server)
        .await;
}

#[tokio::test]
async fn match_polls_queue_whats_new_and_advance_the_cursor() {
    let e = env(500).await;
    let poll = job("poll:matches");

    // Never polled: one page of five; the newest becomes the cursor.
    e.riot(IDS, 200, ids(0..5)).await;
    e.ctx.poll_matches(&poll).await.unwrap();
    let queued = e.jobs().await;
    assert_eq!(queued.len(), 5);
    assert!(
        queued.iter().all(|j| j.0 == "archive:match" && j.2 == 0),
        "a just-finished game is top priority"
    );
    assert_eq!(
        queued[0].3,
        json!({"matchId": "KR_8996", "puuid": P, "fetchTimeline": false}).to_string()
    );
    assert_eq!(e.player().await.last_seen_match_id.as_deref(), Some("KR_9000"));

    // Two new games on top: exactly those two, cursor moves up.
    e.riot(
        IDS,
        200,
        json!(["KR_9002", "KR_9001", "KR_9000", "KR_8999", "KR_8998"]),
    )
    .await;
    e.ctx.poll_matches(&poll).await.unwrap();
    assert_eq!(e.jobs().await.len(), 7);
    assert_eq!(e.player().await.last_seen_match_id.as_deref(), Some("KR_9002"));

    // Nothing new: no jobs, cursor unchanged.
    e.ctx.fetcher.cache().l1.invalidate_where(|_| true).await;
    e.riot(IDS, 200, json!(["KR_9002", "KR_9001"])).await;
    e.ctx.poll_matches(&poll).await.unwrap();
    assert_eq!(e.jobs().await.len(), 7);
    assert_eq!(e.player().await.last_seen_match_id.as_deref(), Some("KR_9002"));
}

#[tokio::test]
async fn already_archived_new_games_still_move_the_cursor() {
    let e = env(500).await;
    players::set_poll_state(
        &e.db,
        &e.scope,
        P,
        players::PollState::LastSeenMatch("KR_1".into()),
    )
    .await
    .unwrap();
    // KR_2 was archived by a lookup before the poll saw it.
    e.db.write(|c| {
        c.execute(
            "INSERT INTO matches (match_id, region, patch, queue_id, game_end_ms, body_zstd, body_size, archived_at) VALUES ('KR_2', 'asia', '16.19', 420, 1, x'00', 1, 1)",
            [],
        )
        .map_err(DbError::from)
    })
    .await
    .unwrap();
    e.riot(IDS, 200, json!(["KR_2", "KR_1"])).await;
    e.ctx.poll_matches(&job("poll:matches")).await.unwrap();
    assert!(e.jobs().await.is_empty(), "nothing to archive");
    assert_eq!(
        e.player().await.last_seen_match_id.as_deref(),
        Some("KR_2"),
        "but the cursor moved (v1 did not)"
    );
}

#[tokio::test]
async fn a_poll_that_falls_behind_pages_back_then_hands_over_to_a_backfill() {
    let e = env(205).await;
    players::set_poll_state(
        &e.db,
        &e.scope,
        P,
        players::PollState::LastSeenMatch("KR_1".into()),
    )
    .await
    .unwrap();
    e.server.reset().await;
    ids_page(&e, 0, 5, ids(0..5)).await;
    ids_page(&e, 5, 100, ids(5..105)).await;
    ids_page(&e, 105, 100, ids(105..205)).await;
    e.ctx.poll_matches(&job("poll:matches")).await.unwrap();

    let jobs = e.jobs().await;
    let archives: Vec<_> = jobs.iter().filter(|j| j.0 == "archive:match").collect();
    assert_eq!(archives.len(), 205, "everything read is queued");
    let deepest = archives.iter().map(|j| j.2).max().unwrap();
    assert_eq!(deepest, 100 + 204 / 10, "deep matches rank by depth block");
    let backfill = jobs.iter().find(|j| j.0 == "backfill:player").unwrap();
    assert_eq!(backfill.2, 20_000);
    assert_eq!(
        backfill.3,
        json!({"puuid": P, "platform": "kr", "limit": 10_000, "reason": "catchup"}).to_string()
    );
    assert_eq!(e.player().await.last_seen_match_id.as_deref(), Some("KR_9000"));
}

#[tokio::test]
async fn catching_up_stops_at_the_cursor_within_the_limit() {
    let e = env(500).await;
    players::set_poll_state(
        &e.db,
        &e.scope,
        P,
        players::PollState::LastSeenMatch("KR_8950".into()),
    )
    .await
    .unwrap();
    e.server.reset().await;
    ids_page(&e, 0, 5, ids(0..5)).await;
    ids_page(&e, 5, 100, ids(5..105)).await;
    e.ctx.poll_matches(&job("poll:matches")).await.unwrap();
    let jobs = e.jobs().await;
    assert_eq!(jobs.len(), 50, "KR_9000 down to KR_8951");
    assert!(jobs.iter().all(|j| j.0 == "archive:match"), "no backfill needed");
}

#[test]
fn archive_priorities_follow_design_06() {
    use riot_proxy::jobs::poll::archive_priority;
    assert_eq!((archive_priority(0), archive_priority(4)), (0, 0));
    assert_eq!(
        (archive_priority(5), archive_priority(19), archive_priority(20)),
        (100, 101, 102)
    );
}
