//! `riot-proxy riot get …` — one raw upstream call, for development. Compiled only
//! with `--features dev-cli`. It bypasses the cache and the limiter, so keep it to
//! occasional manual calls.

use std::io::Write;

use anyhow::{Context, bail};
use clap::Subcommand;

use crate::config::Config;
use crate::riot::client::{RiotClient, RiotRequest};
use crate::riot::endpoints::{ENDPOINTS, Endpoint, Target};
use crate::riot::routing::{Platform, Region};

#[derive(Debug, Subcommand)]
pub enum RiotCommand {
    /// GET one endpoint and print Riot's raw body to stdout (status to stderr).
    ///
    /// Example: riot-proxy riot get account/by-riot-id europe Faker KR1
    Get {
        /// Method id (account.byRiotId) or its slug (account/by-riot-id)
        endpoint: String,
        /// Platform (euw1) or region (europe)
        routing: String,
        /// Path parameters, in template order
        params: Vec<String>,
        /// Query parameter as key=value; repeatable
        #[arg(long = "query", short = 'q', value_parser = parse_kv)]
        query: Vec<(String, String)>,
        /// Send to this base URL instead of Riot (tests)
        #[arg(long, hide = true)]
        base_url: Option<String>,
    },
    /// Call one endpoint and save the exchange as a replay fixture in --out
    /// (NN-<method>.json + .body). The key is redacted.
    ///
    /// Example: riot-proxy riot record --out tests/fixtures/replay/cold-lookup account/by-riot-id europe 'Hide on bush' KR1
    Record {
        /// Fixture directory (created if missing)
        #[arg(long)]
        out: std::path::PathBuf,
        /// Method id or slug
        endpoint: String,
        /// Platform or region
        routing: String,
        /// Path parameters, in template order
        params: Vec<String>,
        /// Query parameter as key=value; repeatable
        #[arg(long = "query", short = 'q', value_parser = parse_kv)]
        query: Vec<(String, String)>,
        /// Send to this base URL instead of Riot (tests)
        #[arg(long, hide = true)]
        base_url: Option<String>,
    },
}

fn parse_kv(raw: &str) -> Result<(String, String), String> {
    raw.split_once('=')
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .ok_or_else(|| format!("expected key=value, got '{raw}'"))
}

/// `account.byRiotId` → `account/by-riot-id`.
pub fn slug(id: &str) -> String {
    let mut out = String::with_capacity(id.len() + 4);
    for c in id.chars() {
        match c {
            '.' => out.push('/'),
            c if c.is_ascii_uppercase() => {
                out.push('-');
                out.push(c.to_ascii_lowercase());
            }
            c => out.push(c),
        }
    }
    out
}

pub(crate) fn find_endpoint(name: &str) -> Option<&'static Endpoint> {
    Endpoint::by_id(name).or_else(|| ENDPOINTS.iter().find(|e| slug(e.id) == name.to_ascii_lowercase()))
}

pub(crate) fn target(endpoint: &Endpoint, routing: &str) -> anyhow::Result<Target> {
    if let Ok(platform) = Platform::parse(routing) {
        return Ok(endpoint.target_for_platform(platform));
    }
    let region = Region::parse(routing).map_err(|e| anyhow::anyhow!(e.message))?;
    endpoint.target_for_region(region).with_context(|| {
        format!(
            "{} is served per platform; pass a platform, not a region",
            endpoint.id
        )
    })
}

pub async fn run(config: &Config, cmd: RiotCommand) -> anyhow::Result<()> {
    let (endpoint, routing, params, query, base_url) = match cmd {
        RiotCommand::Get {
            endpoint,
            routing,
            params,
            query,
            base_url,
        } => (endpoint, routing, params, query, base_url),
        RiotCommand::Record {
            out,
            endpoint,
            routing,
            params,
            query,
            base_url,
        } => {
            return super::record::run(
                config,
                super::record::RecordArgs {
                    out,
                    endpoint,
                    routing,
                    params,
                    query,
                    base_url,
                },
            )
            .await;
        }
    };
    let Some(ep) = find_endpoint(&endpoint) else {
        let known: Vec<String> = ENDPOINTS
            .iter()
            .map(|e| format!("{} ({})", e.id, slug(e.id)))
            .collect();
        bail!("unknown endpoint '{endpoint}'. Known:\n  {}", known.join("\n  "));
    };
    let params: Vec<&str> = params.iter().map(String::as_str).collect();
    let mut req = RiotRequest::new(ep, target(ep, &routing)?, &params)?;
    for (k, v) in &query {
        req = req.query(k, Some(v))?;
    }
    let client = match base_url {
        Some(base) => RiotClient::with_base_url(config, &base)?,
        None => RiotClient::new(config)?,
    };
    match client.send(&req).await {
        Ok(res) => {
            eprintln!("{} {} ({} ms)", res.status, req.path_and_query(), res.upstream_ms);
            let mut out = std::io::stdout().lock();
            out.write_all(&res.body)?;
            out.write_all(b"\n")?;
            Ok(())
        }
        Err(e) => bail!("{} → {:?} (status {:?})", req.path_and_query(), e.kind, e.status),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_derived_from_ids() {
        assert_eq!(slug("account.byRiotId"), "account/by-riot-id");
        assert_eq!(slug("platform.championRotations"), "platform/champion-rotations");
        assert_eq!(
            find_endpoint("account/by-riot-id").map(|e| e.id),
            Some("account.byRiotId")
        );
        assert_eq!(
            find_endpoint("Account/By-Riot-Id").map(|e| e.id),
            Some("account.byRiotId")
        );
        assert_eq!(find_endpoint("match.byId").map(|e| e.id), Some("match.byId"));
        assert!(find_endpoint("nope").is_none());
    }

    #[test]
    fn every_slug_is_unique() {
        let mut slugs: Vec<String> = ENDPOINTS.iter().map(|e| slug(e.id)).collect();
        slugs.sort();
        slugs.dedup();
        assert_eq!(slugs.len(), ENDPOINTS.len());
    }

    #[test]
    fn routing_picks_platform_or_region() {
        let acct = find_endpoint("account.byRiotId").unwrap();
        assert_eq!(target(acct, "europe").unwrap().scope(), "europe");
        assert_eq!(target(acct, "oc1").unwrap().scope(), "asia");
        let summ = find_endpoint("summoner.byPuuid").unwrap();
        assert_eq!(target(summ, "EUW1").unwrap().scope(), "euw1");
        assert!(target(summ, "europe").is_err());
        assert!(target(summ, "mars").is_err());
    }
}
