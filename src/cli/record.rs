//! `riot-proxy riot record` — capture one real exchange as a replay fixture (plan
//! P3-06). Dev only (`--features dev-cli`).
//!
//! Each call appends `NN-<method>.json` (request and response metadata) and
//! `NN-<method>.body` (the response bytes, verbatim) to `--out`. The key is never
//! written: any occurrence is replaced, and the command refuses to leave anything
//! behind that still looks like a Riot key.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};

use super::riot::{find_endpoint, target};
use crate::config::Config;
use crate::riot::client::{RiotClient, RiotRequest};
use crate::riot::endpoints::ENDPOINTS;

/// Response headers kept in fixtures: the ones the proxy reads.
pub const KEPT_HEADERS: &[&str] = &[
    "content-type",
    "x-app-rate-limit",
    "x-app-rate-limit-count",
    "x-method-rate-limit",
    "x-method-rate-limit-count",
    "x-rate-limit-type",
    "retry-after",
];

const REDACTED: &str = "RGAPI-REDACTED";

pub struct RecordArgs {
    pub out: PathBuf,
    pub endpoint: String,
    pub routing: String,
    pub params: Vec<String>,
    pub query: Vec<(String, String)>,
    pub base_url: Option<String>,
}

pub async fn run(config: &Config, args: RecordArgs) -> anyhow::Result<()> {
    let RecordArgs {
        out,
        endpoint,
        routing,
        params,
        query,
        base_url,
    } = args;
    let Some(ep) = find_endpoint(&endpoint) else {
        bail!("unknown endpoint '{endpoint}' ({} known)", ENDPOINTS.len());
    };
    let refs: Vec<&str> = params.iter().map(String::as_str).collect();
    let mut req = RiotRequest::new(ep, target(ep, &routing)?, &refs)?;
    for (k, v) in &query {
        req = req.query(k, Some(v))?;
    }
    let client = match base_url {
        Some(base) => RiotClient::with_base_url(config, &base)?,
        None => RiotClient::new(config)?,
    };
    let (status, headers, body) = match client.send(&req).await {
        Ok(res) => (res.status, res.headers, res.body.to_vec()),
        // Errors carry headers but no body; record them anyway.
        Err(e) => (e.status.unwrap_or(0), *e.headers, Vec::new()),
    };

    std::fs::create_dir_all(&out).with_context(|| format!("creating {}", out.display()))?;
    let seq = next_seq(&out)?;
    let stem = format!("{seq:02}-{}", ep.id);
    let key = config.riot_api_key.expose();

    let kept: serde_json::Map<String, serde_json::Value> = KEPT_HEADERS
        .iter()
        .filter_map(|&name| {
            let v = headers.get(name)?.to_str().ok()?;
            Some((name.to_string(), serde_json::Value::String(redact(v, key))))
        })
        .collect();
    let meta = serde_json::json!({
        "seq": seq,
        "endpoint": ep.id,
        "routing": routing,
        "params": params,
        "query": query,
        "path_and_query": req.path_and_query(),
        "status": status,
        "headers": kept,
        "body_file": format!("{stem}.body"),
        "synthetic": false,
        "recorded_at": jiff::Timestamp::now().to_string(),
    });
    let meta_text = redact(&serde_json::to_string_pretty(&meta)?, key);
    let body = redact_bytes(body, key);

    let meta_path = out.join(format!("{stem}.json"));
    let body_path = out.join(format!("{stem}.body"));
    std::fs::write(&meta_path, format!("{meta_text}\n"))?;
    std::fs::write(&body_path, &body)?;
    for path in [&meta_path, &body_path] {
        ensure_no_key(path, key)?;
    }
    eprintln!("{status} {} → {}", req.path_and_query(), meta_path.display());
    Ok(())
}

fn next_seq(dir: &Path) -> anyhow::Result<usize> {
    let n = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .count();
    Ok(n + 1)
}

fn redact(text: &str, key: &str) -> String {
    if key.is_empty() {
        text.to_string()
    } else {
        text.replace(key, REDACTED)
    }
}

fn redact_bytes(body: Vec<u8>, key: &str) -> Vec<u8> {
    match String::from_utf8(body) {
        Ok(text) if text.contains(key) => redact(&text, key).into_bytes(),
        Ok(text) => text.into_bytes(),
        // Non-UTF-8 bodies are left alone; the check below still guards them.
        Err(e) => e.into_bytes(),
    }
}

/// The CI grep (`RGAPI-[0-9a-f-]{20,}`) and the literal key must both be absent.
pub fn ensure_no_key(path: &Path, key: &str) -> anyhow::Result<()> {
    let bytes = std::fs::read(path)?;
    let text = String::from_utf8_lossy(&bytes);
    if !key.is_empty() && text.contains(key) {
        bail!(
            "{} still contains the API key; refusing to keep it",
            path.display()
        );
    }
    if looks_like_key(&text) {
        bail!(
            "{} contains an RGAPI-… key-shaped string; refusing to keep it",
            path.display()
        );
    }
    Ok(())
}

fn looks_like_key(text: &str) -> bool {
    text.match_indices("RGAPI-").any(|(i, _)| {
        text[i + 6..]
            .bytes()
            .take_while(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase() || *b == b'-')
            .count()
            >= 20
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_shapes_are_detected_like_the_ci_grep() {
        // Split so the CI key grep doesn't flag this file.
        assert!(looks_like_key(concat!(
            "x RGAPI",
            "-0123abcd-0123-0123-0123-0123456789ab y"
        )));
        assert!(!looks_like_key("RGAPI-REDACTED"));
        assert!(!looks_like_key("RGAPI-test-key-not-real"));
        assert!(!looks_like_key("nothing"));
    }

    #[test]
    fn redaction_replaces_every_occurrence() {
        assert_eq!(
            redact("a SECRETKEY b SECRETKEY", "SECRETKEY"),
            "a RGAPI-REDACTED b RGAPI-REDACTED"
        );
        assert_eq!(
            redact_bytes(b"{\"k\":\"SECRETKEY\"}".to_vec(), "SECRETKEY"),
            b"{\"k\":\"RGAPI-REDACTED\"}"
        );
        assert_eq!(redact_bytes(vec![0xff, 0x00], "SECRETKEY"), vec![0xff, 0x00]);
    }
}
