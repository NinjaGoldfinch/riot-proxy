//! `riot-proxy migrate-v1 --from <dump>` (plan P8-02, design/08 §Data
//! migration): v1's archive and players into v2's SQLite.
//!
//! The input is v1's Postgres data, either a `pg_dump -Fc` archive (streamed
//! through `pg_restore --data-only -f -`, which must be on `PATH`) or that
//! command's output already written to a file: `COPY … FROM stdin` blocks in
//! Postgres's text format. Nothing is held in memory beyond one batch.
//!
//! - `matches` → v2's archive, through the same path a fetch takes: the body is
//!   zstd-compressed and its facts, bans and remake flag are re-derived, so
//!   `facts_version` starts clean. `timeline` → `timelines`. v1's derived
//!   tables (`match_participants`, `match_bans`, analytics) are not read.
//! - `players` → `players`, `tracked` and the backfill stamps intact.
//! - `consumers` are not migrated: mint new keys (design/08).
//!
//! Facts are filed under one key scope: this deployment's, unless
//! `--key-scope` names v1's. They are the same when v2 runs with v1's Riot key,
//! as the cut-over does. Players keep the key scope v1 stored with them.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use anyhow::Context;
use bytes::Bytes;
use rusqlite::params;
use serde_json::json;

use crate::archive::matches;
use crate::cache::keys::KeyScope;
use crate::config::Config;
use crate::db::{Db, DbError};

/// Matches archived concurrently: compression runs on the blocking pool.
const BATCH: usize = 64;

#[derive(Debug, Clone, clap::Args)]
pub struct Args {
    /// v1's data: a `pg_dump -Fc` archive, or `pg_restore --data-only -f -` output.
    #[arg(long, value_name = "PATH")]
    pub from: PathBuf,
    /// File the match facts under this key scope (8 hex characters) instead of
    /// this deployment's.
    #[arg(long, value_name = "SCOPE")]
    pub key_scope: Option<String>,
}

/// What an import did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    pub matches: usize,
    pub timelines: usize,
    pub players: usize,
    /// Match ids whose body v2 cannot archive, with why.
    pub skipped: Vec<(String, String)>,
    /// Rows of tables that are not migrated, by table.
    pub ignored: BTreeMap<String, usize>,
    /// Key scopes the players carried.
    pub player_scopes: BTreeMap<String, usize>,
}

// ── Postgres COPY text format ───────────────────────────────────────────────

/// One field of a COPY text row: `\N` is NULL, and backslash escapes are
/// decoded (`\t`, `\n`, `\\`, octal `\NNN`, hex `\xHH`, …).
pub fn unescape(field: &[u8]) -> Option<Vec<u8>> {
    if field == b"\\N" {
        return None;
    }
    let mut out = Vec::with_capacity(field.len());
    let mut i = 0;
    while i < field.len() {
        let b = field[i];
        if b != b'\\' || i + 1 == field.len() {
            out.push(b);
            i += 1;
            continue;
        }
        let next = field[i + 1];
        i += 2;
        match next {
            b'b' => out.push(0x08),
            b'f' => out.push(0x0c),
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'v' => out.push(0x0b),
            b'0'..=b'7' => {
                let mut v = u32::from(next - b'0');
                for _ in 0..2 {
                    match field.get(i) {
                        Some(d @ b'0'..=b'7') => {
                            v = v * 8 + u32::from(d - b'0');
                            i += 1;
                        }
                        _ => break,
                    }
                }
                out.push(u8::try_from(v & 0xff).unwrap_or(0));
            }
            b'x' => {
                let mut v = 0u32;
                let mut n = 0;
                while n < 2 {
                    match field.get(i).and_then(|d| (*d as char).to_digit(16)) {
                        Some(d) => {
                            v = v * 16 + d;
                            i += 1;
                            n += 1;
                        }
                        None => break,
                    }
                }
                if n == 0 {
                    out.push(b'x');
                } else {
                    out.push(u8::try_from(v).unwrap_or(0));
                }
            }
            other => out.push(other),
        }
    }
    Some(out)
}

/// A Postgres `timestamptz` as COPY prints it (`2026-09-20 10:00:00.123+00`),
/// in unix ms.
pub fn timestamp_ms(raw: &str) -> Option<i64> {
    let mut s = raw.trim().replacen(' ', "T", 1);
    // `+00` / `-05` → `+00:00`; `+05:30` is already whole.
    let sign = s.rfind(['+', '-']).filter(|i| *i > 10)?;
    if s.len() - sign == 3 {
        s.push_str(":00");
    }
    s.parse::<jiff::Timestamp>()
        .ok()
        .map(jiff::Timestamp::as_millisecond)
}

/// `COPY public.matches (match_id, region, …) FROM stdin;` → (`matches`, columns).
fn copy_header(line: &str) -> Option<(String, Vec<String>)> {
    let rest = line.strip_prefix("COPY ")?.strip_suffix(" FROM stdin;")?;
    let (table, cols) = rest.split_once(" (")?;
    let table = table.rsplit('.').next()?.trim_matches('"').to_string();
    let cols = cols
        .strip_suffix(')')?
        .split(", ")
        .map(|c| c.trim_matches('"').to_string())
        .collect();
    Some((table, cols))
}

// ── Import ──────────────────────────────────────────────────────────────────

struct MatchRow {
    id: String,
    region: String,
    data: Vec<u8>,
    timeline: Option<Vec<u8>>,
    fetched_ms: i64,
}

fn column(cols: &[String], name: &str) -> anyhow::Result<usize> {
    cols.iter()
        .position(|c| c == name)
        .with_context(|| format!("v1 dump has no column {name}"))
}

/// One parsed row, from the parsing thread to the writer.
enum Record {
    Match(MatchRow),
    /// The end of the `matches` block: flush the last batch.
    MatchesDone,
    Player(Vec<String>, Vec<Option<String>>),
    Ignored(String),
}

/// Parse COPY blocks (blocking I/O, on its own thread) into `tx`.
fn parse(input: impl Read, tx: &tokio::sync::mpsc::Sender<anyhow::Result<Record>>) -> anyhow::Result<()> {
    let mut reader = BufReader::with_capacity(1 << 20, input);
    let mut line = Vec::new();
    let mut current: Option<(String, Vec<String>)> = None;
    let send = |r: Record| {
        tx.blocking_send(Ok(r))
            .map_err(|_| anyhow::anyhow!("the import stopped"))
    };
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            return Ok(());
        }
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        let Some((table, cols)) = &current else {
            if let Some(header) = std::str::from_utf8(&line).ok().and_then(copy_header) {
                current = Some(header);
            }
            continue;
        };
        if line == b"\\." {
            if table == "matches" {
                send(Record::MatchesDone)?;
            }
            current = None;
            continue;
        }
        let fields: Vec<Option<Vec<u8>>> = line.split(|b| *b == b'\t').map(unescape).collect();
        let text = |f: &Option<Vec<u8>>| f.as_ref().map(|v| String::from_utf8_lossy(v).into_owned());
        match table.as_str() {
            "matches" => {
                let at = |name: &str| column(cols, name).map(|i| fields.get(i).cloned().flatten());
                let (Some(id), Some(data)) = (at("match_id")?, at("data")?) else {
                    continue;
                };
                send(Record::Match(MatchRow {
                    id: String::from_utf8_lossy(&id).into_owned(),
                    region: at("region")?
                        .map(|r| String::from_utf8_lossy(&r).into_owned())
                        .unwrap_or_default(),
                    data,
                    timeline: at("timeline")?,
                    fetched_ms: at("fetched_at")?
                        .and_then(|t| timestamp_ms(&String::from_utf8_lossy(&t)))
                        .unwrap_or_else(|| crate::clock::Clock::now().unix_ms),
                }))?;
            }
            "players" => send(Record::Player(cols.clone(), fields.iter().map(text).collect()))?,
            other => send(Record::Ignored(other.to_string()))?,
        }
    }
}

/// Read COPY blocks from `input` and import what v2 keeps. The parse runs on
/// a blocking thread; this side only writes.
pub async fn import(db: &Db, input: impl Read + Send + 'static, facts_scope: &str) -> anyhow::Result<Report> {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<anyhow::Result<Record>>(BATCH * 4);
    let parser = tokio::task::spawn_blocking(move || {
        if let Err(e) = parse(input, &tx) {
            let _ = tx.blocking_send(Err(e));
        }
    });
    let mut report = Report::default();
    let mut batch: Vec<MatchRow> = Vec::with_capacity(BATCH);
    while let Some(record) = rx.recv().await {
        match record? {
            Record::Match(row) => {
                batch.push(row);
                if batch.len() == BATCH {
                    flush(db, &mut batch, facts_scope, &mut report).await?;
                }
            }
            Record::MatchesDone => flush(db, &mut batch, facts_scope, &mut report).await?,
            Record::Player(cols, fields) => {
                let scope = import_player(db, &cols, &fields).await?;
                *report.player_scopes.entry(scope).or_default() += 1;
                report.players += 1;
            }
            Record::Ignored(table) => *report.ignored.entry(table).or_default() += 1,
        }
    }
    flush(db, &mut batch, facts_scope, &mut report).await?;
    parser.await?;
    Ok(report)
}

/// Archive a batch concurrently: each match takes the fetch path's own write.
async fn flush(db: &Db, batch: &mut Vec<MatchRow>, scope: &str, report: &mut Report) -> anyhow::Result<()> {
    let rows = std::mem::take(batch);
    let puts = rows.iter().map(|r| {
        matches::put(
            db,
            &r.id,
            &r.region,
            scope,
            Bytes::from(r.data.clone()),
            r.fetched_ms,
        )
    });
    let results = futures_util::future::join_all(puts).await;
    for (row, result) in rows.into_iter().zip(results) {
        match result {
            Ok(_) => {
                report.matches += 1;
                if let Some(t) = row.timeline {
                    matches::put_timeline(db, &row.id, Bytes::from(t)).await?;
                    report.timelines += 1;
                }
            }
            // A body v2 cannot archive (v1 stored it anyway): reported, not fatal.
            Err(matches::ArchiveError::NotAMatch(why)) => report.skipped.push((row.id, why)),
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

/// One v1 player row. v1's three backfill stamps become v2's `backfill_state`
/// (ADR-050): `startedAt`, `doneAt`, `depth`.
async fn import_player(db: &Db, cols: &[String], fields: &[Option<String>]) -> anyhow::Result<String> {
    let text = |i: usize| fields.get(i).cloned().flatten();
    let get = |name: &str| -> anyhow::Result<Option<String>> { Ok(text(column(cols, name)?)) };
    let (Some(puuid), Some(scope)) = (get("puuid")?, get("key_scope")?) else {
        return Ok(String::new());
    };
    let platform = get("platform")?.unwrap_or_default();
    let (name, tag) = (get("game_name")?, get("tag_line")?);
    let tracked = get("tracked")?.as_deref() == Some("t");
    let last_seen = get("last_seen_match_id")?;
    let updated = get("updated_at")?
        .and_then(|t| timestamp_ms(&t))
        .unwrap_or_else(|| crate::clock::Clock::now().unix_ms);
    let stamp = |name: &str| -> Option<i64> {
        column(cols, name)
            .ok()
            .and_then(text)
            .and_then(|t| timestamp_ms(&t))
    };
    let depth = column(cols, "history_backfill_depth")
        .ok()
        .and_then(text)
        .and_then(|d| d.parse::<i64>().ok());
    let backfill = stamp("history_backfill_started_at").map(|started| {
        json!({
            "startedAt": started,
            "doneAt": stamp("history_backfilled_at"),
            "depth": depth.unwrap_or(0),
        })
        .to_string()
    });
    let key_scope = scope.clone();
    db.write(move |c| {
        c.execute(
            "INSERT INTO players (key_scope, puuid, platform, game_name, tag_line, tracked,
               last_seen_match_id, backfill_state, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT (key_scope, puuid) DO UPDATE SET
               platform = excluded.platform, game_name = excluded.game_name, tag_line = excluded.tag_line,
               tracked = excluded.tracked, last_seen_match_id = excluded.last_seen_match_id,
               backfill_state = excluded.backfill_state, updated_at = excluded.updated_at",
            params![
                scope, puuid, platform, name, tag, tracked, last_seen, backfill, updated
            ],
        )?;
        Ok::<_, DbError>(())
    })
    .await?;
    Ok(key_scope)
}

/// The input as COPY text: through `pg_restore` for a custom-format dump.
fn open(path: &Path) -> anyhow::Result<(Box<dyn Read + Send>, Option<std::process::Child>)> {
    let mut magic = [0u8; 5];
    let n = std::fs::File::open(path)
        .with_context(|| format!("opening {}", path.display()))?
        .read(&mut magic)?;
    if n == 5 && &magic == b"PGDMP" {
        let mut child = Command::new("pg_restore")
            .args([
                "--data-only",
                "--table=matches",
                "--table=players",
                "--table=consumers",
                "-f",
                "-",
            ])
            .arg(path)
            .stdout(Stdio::piped())
            .spawn()
            .context("running pg_restore (a custom-format dump needs it on PATH)")?;
        let out = child.stdout.take().context("pg_restore stdout")?;
        return Ok((Box::new(out), Some(child)));
    }
    Ok((Box::new(std::fs::File::open(path)?), None))
}

pub async fn run(config: &Config, args: &Args) -> anyhow::Result<()> {
    let db = super::open_db(config).await?;
    let scope = args
        .key_scope
        .clone()
        .unwrap_or_else(|| KeyScope::from_key(&config.riot_api_key).as_str().to_string());
    let (input, child) = open(&args.from)?;
    let started = Instant::now();
    let report = import(&db, input, &scope).await?;
    if let Some(mut child) = child {
        let status = child.wait()?;
        anyhow::ensure!(status.success(), "pg_restore failed: {status}");
    }
    let secs = started.elapsed().as_secs_f64();
    #[allow(clippy::cast_precision_loss)]
    let rate = report.matches as f64 / secs.max(0.001);
    println!(
        "imported {} matches ({rate:.0}/s), {} timelines, {} players in {secs:.1}s; facts under key scope {scope}",
        report.matches, report.timelines, report.players
    );
    for (id, why) in &report.skipped {
        println!("  skipped {id}: {why}");
    }
    if let Some(n) = report.ignored.get("consumers") {
        println!("  {n} consumers not migrated: mint new keys with `riot-proxy key create`");
    }
    let others: Vec<String> = report
        .player_scopes
        .keys()
        .filter(|s| **s != scope)
        .cloned()
        .collect();
    if !others.is_empty() {
        println!(
            "  note: players carry key scope(s) {} but facts were filed under {scope}; \
             pass --key-scope if v1 used another Riot key",
            others.join(", ")
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_text_fields_unescape_as_postgres_writes_them() {
        assert_eq!(unescape(b"\\N"), None);
        assert_eq!(unescape(b"plain").unwrap(), b"plain");
        assert_eq!(unescape(b"a\\tb\\nc\\\\d").unwrap(), b"a\tb\nc\\d");
        assert_eq!(unescape(b"\\101\\x42\\r").unwrap(), b"AB\r");
        // A JSON string's own escape survives as JSON: `\\t` is `\t` in the text.
        assert_eq!(
            unescape(br#"{"n":"Tab\\tName"}"#).unwrap(),
            br#"{"n":"Tab\tName"}"#
        );
    }

    #[test]
    fn timestamps_read_as_unix_ms() {
        assert_eq!(
            timestamp_ms("2026-09-20 10:00:00.123+00"),
            Some(1_789_898_400_123)
        );
        assert_eq!(timestamp_ms("2026-09-20 12:00:00+02"), Some(1_789_898_400_000));
        assert_eq!(timestamp_ms("2026-09-20 15:30:00+05:30"), Some(1_789_898_400_000));
        assert_eq!(timestamp_ms("not a time"), None);
    }

    #[test]
    fn copy_headers_name_the_table_and_columns() {
        assert_eq!(
            copy_header("COPY public.matches (match_id, region, data) FROM stdin;"),
            Some((
                "matches".into(),
                vec!["match_id".into(), "region".into(), "data".into()]
            ))
        );
        assert_eq!(copy_header("SET client_encoding = 'UTF8';"), None);
    }
}
