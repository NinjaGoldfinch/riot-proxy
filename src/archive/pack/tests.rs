use super::*;
use crate::db::Db;

/// 2026-10-13T10:00:00Z.
const OCT_13: u64 = 1_791_885_600_000;

fn id() -> Ulid {
    Ulid::from_parts(OCT_13, 42)
}

fn bodies(ids: &[&str]) -> Vec<(String, Bytes)> {
    ids.iter()
        .map(|id| {
            let body =
                zstd::encode_all(format!("{{\"metadata\":{{\"matchId\":\"{id}\"}}}}").as_bytes(), 3).unwrap();
            (id.to_string(), Bytes::from(body))
        })
        .collect()
}

#[test]
fn a_pack_round_trips_through_writer_and_reader() {
    let input = bodies(&["OC1_3", "KR_10", "OC1_1", "KR_2"]);
    let pack = build(id(), Kind::Match, input.clone()).unwrap();
    let ids: Vec<&str> = pack.entries.iter().map(|e| e.match_id.as_str()).collect();
    assert_eq!(
        ids,
        ["KR_10", "KR_2", "OC1_1", "OC1_3"],
        "sorted by match id, as text"
    );

    // Back to back from offset 0, each body byte for byte.
    let mut at = 0;
    for e in &pack.entries {
        assert_eq!(e.offset, at);
        let body = &input.iter().find(|(id, _)| *id == e.match_id).unwrap().1;
        assert_eq!(
            &pack.bytes[e.offset as usize..(e.offset + e.len) as usize],
            body.as_ref()
        );
        assert_eq!(e.sha256, sha256(body));
        at += e.len;
    }
    assert_eq!(&pack.bytes[pack.bytes.len() - 8..], MAGIC);
    assert_eq!(pack.sha256, sha256(&pack.bytes));

    let read = read(Kind::Match, &pack.bytes).unwrap();
    assert_eq!(read, pack.entries, "the index alone gives every location");
    assert!(verify(&pack.bytes, &read));
}

#[test]
fn keys_and_metadata_follow_design_12() {
    let pack = build(id(), Kind::Match, bodies(&["KR_1", "KR_2"])).unwrap();
    assert_eq!(pack.key(), format!("packs/matches/2026/10/{}.pack", id()));
    let jan = Ulid::from_parts(1_767_225_600_000, 1);
    assert_eq!(
        key(Kind::Timeline, jan),
        format!("packs/timelines/2026/01/{jan}.pack"),
        "2026-01-01T00:00:00Z, UTC"
    );
    let [(k1, sha), (k2, n)] = pack.metadata();
    assert_eq!((k1, sha.as_str()), ("sha256", hex::encode(pack.sha256).as_str()));
    assert_eq!((k2, n.as_str()), ("bodies", "2"));
}

#[test]
fn the_index_is_compressed_json_lines() {
    let pack = build(id(), Kind::Timeline, bodies(&["KR_1"])).unwrap();
    let range = index_range(pack.bytes.len() as u64, &pack.bytes[pack.bytes.len() - 16..]).unwrap();
    let text = zstd::decode_all(&pack.bytes[range.start as usize..range.end as usize]).unwrap();
    let line: serde_json::Value = serde_json::from_slice(text.trim_ascii_end()).unwrap();
    assert_eq!(
        line,
        serde_json::json!({
            "kind": "timeline", "matchId": "KR_1", "offset": 0,
            "len": pack.entries[0].len, "sha256": hex::encode(pack.entries[0].sha256),
        })
    );
}

#[test]
fn bad_inputs_are_refused() {
    assert!(matches!(
        build(id(), Kind::Match, Vec::new()),
        Err(PackError::Empty)
    ));
    let mut twice = bodies(&["KR_1", "KR_2"]);
    twice.push(twice[0].clone());
    assert!(matches!(build(id(), Kind::Match, twice), Err(PackError::Duplicate(m)) if m == "KR_1"));
    let empty = vec![("KR_1".to_string(), Bytes::new())];
    assert!(matches!(build(id(), Kind::Match, empty), Err(PackError::EmptyBody(m)) if m == "KR_1"));
}

#[test]
fn a_truncated_or_foreign_object_is_not_a_pack() {
    let pack = build(id(), Kind::Match, bodies(&["KR_1", "KR_2", "KR_3"])).unwrap();
    let b = &pack.bytes;
    let n = b.len();

    for cut in [1, 8, 16, 40] {
        assert!(read(Kind::Match, &b[..n - cut]).is_err(), "cut {cut}");
    }
    assert!(read(Kind::Match, &b[..10]).is_err());
    assert!(read(Kind::Match, b"").is_err());

    let mut magic = b.to_vec();
    magic[n - 1] = b'2';
    assert!(matches!(read(Kind::Match, &magic), Err(PackError::NotAPack(_))));

    let mut long = b.to_vec();
    long[n - 16..n - 8].copy_from_slice(&(n as u64).to_le_bytes());
    assert!(matches!(read(Kind::Match, &long), Err(PackError::NotAPack(_))));

    assert!(
        matches!(read(Kind::Timeline, b), Err(PackError::Index(_))),
        "a match pack read as timelines"
    );
}

#[test]
fn a_damaged_index_is_refused() {
    let line = |id: &str, offset: u64, len: u64| {
        format!(
            r#"{{"kind":"match","matchId":"{id}","offset":{offset},"len":{len},"sha256":"{}"}}"#,
            "00".repeat(32)
        )
    };
    let parse = |lines: &[String], end: u64| {
        let z = zstd::encode_all(lines.join("\n").as_bytes(), 3).unwrap();
        parse_index(Kind::Match, &z, end)
    };
    assert!(parse(&[line("KR_1", 0, 10), line("KR_2", 10, 5)], 15).is_ok());
    assert!(
        parse(&[line("KR_1", 0, 10), line("KR_2", 10, 6)], 15).is_err(),
        "past the bodies"
    );
    assert!(
        parse(&[line("KR_2", 0, 10), line("KR_1", 10, 5)], 15).is_err(),
        "out of id order"
    );
    assert!(
        parse(&[line("KR_1", 0, 10), line("KR_2", 5, 5)], 15).is_err(),
        "overlapping"
    );
    assert!(parse(&[line("KR_1", 0, 0)], 15).is_err(), "empty body");
    assert!(parse(&[], 15).is_err(), "no entries");
    assert!(parse(&["{\"kind\":\"match\"}".to_string()], 15).is_err());
    assert!(parse_index(Kind::Match, b"not zstd", 15).is_err());
}

#[test]
fn a_changed_body_fails_verification() {
    let pack = build(id(), Kind::Match, bodies(&["KR_1", "KR_2"])).unwrap();
    let mut bytes = pack.bytes.to_vec();
    bytes[pack.entries[1].offset as usize] ^= 1;
    assert!(!verify(&bytes, &pack.entries));
    assert!(verify(&pack.bytes, &pack.entries));
}

fn seeded_db() -> (tempfile::TempDir, Db) {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("riot-proxy.db"), 1).unwrap();
    (dir, db)
}

async fn rows(db: &Db, sql: &'static str) -> Vec<String> {
    db.read(move |c| {
        let mut stmt = c.prepare(sql)?;
        let out = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok::<_, crate::db::DbError>(out)
    })
    .await
    .unwrap()
}

async fn record(db: &Db, packs: Vec<(Pack, i64)>) {
    db.write(move |c| {
        let tx = c.transaction()?;
        for (p, at) in &packs {
            record_on(
                &tx,
                p.id,
                p.kind,
                p.bytes.len() as u64,
                &p.sha256,
                &p.entries,
                *at,
            )?;
        }
        tx.commit()?;
        Ok::<_, crate::db::DbError>(())
    })
    .await
    .unwrap();
}

/// design/12 §Recovery: a pack's own index is enough to rebuild its rows.
#[tokio::test]
async fn recording_from_the_index_alone_locates_every_body_and_marks_it_packed() {
    let (_dir, db) = seeded_db();
    let input = bodies(&["KR_1", "KR_2"]);
    let seed = input.clone();
    db.write(move |c| {
        for (id, body) in &seed {
            c.execute(
                "INSERT INTO timelines (match_id, body_zstd) VALUES (?1, ?2)",
                rusqlite::params![id, body.as_ref()],
            )?;
        }
        Ok::<_, crate::db::DbError>(())
    })
    .await
    .unwrap();
    let pack = build(id(), Kind::Timeline, input).unwrap();
    // Only what the object itself says: the index read back from its bytes.
    let from_index = Pack {
        entries: read(Kind::Timeline, &pack.bytes).unwrap(),
        ..pack.clone()
    };
    record(&db, vec![(from_index, 7)]).await;

    let size = pack.bytes.len();
    let live: u64 = pack.entries.iter().map(|e| e.len).sum();
    assert_eq!(
        rows(
            &db,
            "SELECT pack_id || ' ' || kind || ' ' || bytes || ' ' || bodies || ' ' || live_bytes || ' '
                    || hex(sha256) || ' ' || created_at FROM body_packs"
        )
        .await,
        [format!(
            "{} timeline {size} 2 {live} {} 7",
            id(),
            hex::encode_upper(pack.sha256)
        )]
    );
    let want: Vec<String> = pack
        .entries
        .iter()
        .map(|e| {
            format!(
                "{} timeline {} {} {} {}",
                e.match_id,
                id(),
                e.offset,
                e.len,
                hex::encode_upper(e.sha256)
            )
        })
        .collect();
    assert_eq!(
        rows(
            &db,
            "SELECT match_id || ' ' || kind || ' ' || pack_id || ' ' || offset || ' ' || len || ' ' || hex(sha256)
               FROM body_locations ORDER BY match_id"
        )
        .await,
        want
    );
    assert_eq!(
        rows(
            &db,
            "SELECT match_id || ' ' || packed_at || ' ' || coalesce(offloaded_at, '-') || ' ' || length(body_zstd)
               FROM timelines ORDER BY match_id"
        )
        .await,
        pack.entries
            .iter()
            .map(|e| format!("{} 7 - {}", e.match_id, e.len))
            .collect::<Vec<_>>(),
        "packed, local copy kept"
    );
}

/// A body packed again (compaction, OBJ-06) moves; its old pack's live bytes drop.
#[tokio::test]
async fn a_body_packed_again_moves_and_its_old_pack_loses_its_bytes() {
    let (_dir, db) = seeded_db();
    let first = build(id(), Kind::Match, bodies(&["KR_1", "KR_2"])).unwrap();
    let second_id = Ulid::from_parts(OCT_13 + 1, 1);
    let second = build(second_id, Kind::Match, bodies(&["KR_2"])).unwrap();
    record(&db, vec![(first.clone(), 1), (second.clone(), 2)]).await;
    assert_eq!(
        rows(
            &db,
            "SELECT pack_id || ' ' || live_bytes FROM body_packs ORDER BY pack_id"
        )
        .await,
        [
            format!("{} {}", id(), first.entries[0].len),
            format!("{second_id} {}", second.entries[0].len)
        ]
    );
    assert_eq!(
        rows(
            &db,
            "SELECT match_id || ' ' || pack_id FROM body_locations ORDER BY match_id"
        )
        .await,
        [format!("KR_1 {}", id()), format!("KR_2 {second_id}")]
    );
}
