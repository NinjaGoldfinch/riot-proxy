//! `builds:extract` end to end (BLD-01): a recorded ranked game and its
//! timeline archived for real, the mirrored item.json on disk, and
//! `match_builds` read back.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::Arc;

use riot_proxy::archive::builds::BUILDS_VERSION;
use riot_proxy::archive::matches;
use riot_proxy::db::DbError;
use riot_proxy::jobs::analytics::AnalyticsContext;
use riot_proxy::jobs::{Job, Queue};
use serde_json::{Value, json};

const MATCH_ID: &str = "OC1_711969250";
const MATCH: &[u8] = include_bytes!("fixtures/builds/OC1_711969250.match.json");
const TIMELINE: &[u8] = include_bytes!("fixtures/builds/OC1_711969250.timeline.json");
const ITEMS: &[u8] = include_bytes!("fixtures/builds/item-16.19.1.json");
const EXPECTED: &[u8] = include_bytes!("fixtures/builds/OC1_711969250.expected.json");
/// The facts belong to this scope, not the context's, as after a key rotation.
const OWNER: &str = "owner-scope";

struct Env {
    _dir: tempfile::TempDir,
    state: riot_proxy::app::AppState,
}

async fn env() -> Env {
    let (dir, state, _router) = common::app_with(&[], "http://127.0.0.1:9");
    Env { _dir: dir, state }
}

fn job(kind: &str, payload: Value) -> Job {
    Job {
        id: "j".into(),
        kind: kind.into(),
        dedupe_key: None,
        priority: 30_000,
        payload: payload.to_string(),
        attempts: 1,
        run_after: 0,
    }
}

/// `(puuid, key_scope, starter, boots, items, skills, skill_order, builds_version)`.
type Row = (
    String,
    String,
    String,
    Option<i64>,
    String,
    String,
    Option<String>,
    i64,
);

impl Env {
    fn ctx(&self) -> AnalyticsContext {
        AnalyticsContext {
            queue: Queue::new(self.state.db.clone()),
            hub: self.state.hub.clone(),
            key_scope: self.state.fetcher.key_scope().as_str().to_string(),
            patch_limit: 4,
            reextract_batch: 500,
            mirror: Arc::clone(&self.state.ddragon),
        }
    }

    /// The match and its timeline, archived as the fetcher archives them.
    async fn archive(&self) {
        matches::put(&self.state.db, MATCH_ID, "sea", OWNER, MATCH.to_vec().into(), 1)
            .await
            .unwrap();
        matches::put_timeline(&self.state.db, MATCH_ID, TIMELINE.to_vec().into())
            .await
            .unwrap();
    }

    /// Patch 16.19.1 fully mirrored: its item.json, then versions.json last.
    fn mirror(&self) {
        let dir = self.state.ddragon.dir().join("16.19.1");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("item.json"), ITEMS).unwrap();
        std::fs::write(dir.join("versions.json"), r#"["16.19.1"]"#).unwrap();
    }

    async fn sql(&self, sql: &'static str) {
        self.state
            .db
            .write(move |c| {
                c.execute_batch(sql)?;
                Ok::<_, DbError>(())
            })
            .await
            .unwrap();
    }

    async fn rows(&self) -> Vec<Row> {
        self.state
            .db
            .read(|c| {
                let mut stmt = c.prepare(
                    "SELECT puuid, key_scope, starter, boots, items, skills, skill_order, builds_version
                       FROM match_builds ORDER BY puuid",
                )?;
                let rows = stmt
                    .query_map([], |r| {
                        Ok((
                            r.get(0)?,
                            r.get(1)?,
                            r.get(2)?,
                            r.get(3)?,
                            r.get(4)?,
                            r.get(5)?,
                            r.get(6)?,
                            r.get(7)?,
                        ))
                    })?
                    .collect::<Result<Vec<Row>, _>>()?;
                Ok::<_, DbError>(rows)
            })
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn extraction_fills_match_builds_for_the_scope_that_owns_the_facts() {
    let e = env().await;
    e.archive().await;
    e.mirror();
    assert_eq!(e.ctx().extract_builds().await.unwrap(), 1);

    let rows = e.rows().await;
    assert_eq!(rows.len(), 10);
    let expected: Value = serde_json::from_slice(EXPECTED).unwrap();
    for (puuid, scope, starter, boots, items, skills, order, version) in &rows {
        let want = &expected[puuid];
        assert_eq!(scope, OWNER);
        assert_eq!(serde_json::from_str::<Value>(starter).unwrap(), want["starter"]);
        assert_eq!(json!(boots), want["boots"]);
        assert_eq!(serde_json::from_str::<Value>(items).unwrap(), want["items"]);
        assert_eq!(json!(skills), want["skills"]);
        assert_eq!(json!(order), want["skillOrder"]);
        assert_eq!(*version, BUILDS_VERSION);
    }

    // Done: the next sweep reads nothing and the rows stay as they were.
    assert_eq!(e.ctx().extract_builds().await.unwrap(), 0);
    assert_eq!(e.rows().await, rows);
}

#[tokio::test]
async fn rows_from_an_older_version_are_rewritten() {
    let e = env().await;
    e.archive().await;
    e.mirror();
    e.ctx().extract_builds().await.unwrap();
    let fresh = e.rows().await;
    e.sql(
        "UPDATE match_builds SET builds_version = 0, items = '[]';
           DELETE FROM match_builds WHERE puuid = 'bld-fixture-puuid-01';",
    )
    .await;

    assert_eq!(e.ctx().extract_builds().await.unwrap(), 1);
    assert_eq!(e.rows().await, fresh);
}

#[tokio::test]
async fn a_timeline_whose_match_has_no_facts_is_skipped() {
    let e = env().await;
    e.archive().await;
    e.mirror();
    e.sql("DELETE FROM match_facts").await;
    assert_eq!(e.ctx().extract_builds().await.unwrap(), 0);
    assert!(e.rows().await.is_empty());
}

#[tokio::test]
async fn without_a_mirrored_item_json_nothing_is_extracted() {
    let e = env().await;
    e.archive().await;
    assert_eq!(e.ctx().extract_builds().await.unwrap(), 0);
    assert!(e.rows().await.is_empty());
    // The builds:extract job does the same and succeeds.
    let registry = riot_proxy::jobs::analytics::BuildsHandler(Arc::new(e.ctx()));
    riot_proxy::jobs::Handler::run(&registry, &job("builds:extract", json!({})))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_recompute_extracts_builds_first() {
    let e = env().await;
    e.archive().await;
    e.mirror();
    e.ctx()
        .aggregate(&job(
            "aggregate:analytics",
            json!({"platform": "oc1", "queue": "RANKED_SOLO_5x5"}),
        ))
        .await
        .unwrap();
    assert_eq!(e.rows().await.len(), 10);
}

#[tokio::test]
async fn deleting_a_match_deletes_its_builds() {
    let e = env().await;
    e.archive().await;
    e.mirror();
    e.ctx().extract_builds().await.unwrap();
    e.sql("DELETE FROM timelines; DELETE FROM matches;").await;
    assert!(e.rows().await.is_empty());
}

/// BLD-05: the job passes each player's champion from `match_facts`, so
/// Viego's possession isn't counted as his skill points.
#[tokio::test]
async fn extraction_drops_viegos_possessed_level_ups() {
    const ID: &str = "OC1_712417978";
    let e = env().await;
    matches::put(
        &e.state.db,
        ID,
        "sea",
        OWNER,
        include_bytes!("fixtures/builds/OC1_712417978.match.json")
            .to_vec()
            .into(),
        1,
    )
    .await
    .unwrap();
    matches::put_timeline(
        &e.state.db,
        ID,
        include_bytes!("fixtures/builds/OC1_712417978.timeline.json")
            .to_vec()
            .into(),
    )
    .await
    .unwrap();
    let dir = e.state.ddragon.dir().join("16.20.1");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("item.json"),
        include_bytes!("fixtures/builds/item-16.20.1.json"),
    )
    .unwrap();
    std::fs::write(dir.join("versions.json"), r#"["16.20.1"]"#).unwrap();
    assert_eq!(e.ctx().extract_builds().await.unwrap(), 1);

    let expected: Value =
        serde_json::from_slice(include_bytes!("fixtures/builds/OC1_712417978.expected.json")).unwrap();
    let rows = e.rows().await;
    assert_eq!(rows.len(), 10);
    for (puuid, _, _, _, _, skills, order, _) in &rows {
        assert_eq!(json!(skills), expected[puuid]["skills"], "{puuid}");
        assert_eq!(json!(order), expected[puuid]["skillOrder"], "{puuid}");
    }
    let viego = rows.iter().find(|r| r.0 == "bld-fixture-puuid-02").unwrap();
    assert_eq!(viego.5, "QWQEQRQEQEREE");
}
