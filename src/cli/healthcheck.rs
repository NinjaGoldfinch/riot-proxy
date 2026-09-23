//! `riot-proxy healthcheck`: a `FROM scratch` image has no curl (design/07 §Option A).

use std::time::Duration;

use crate::config::Config;

pub async fn run(config: &Config, timeout_s: u64) -> anyhow::Result<()> {
    let url = healthz_url(config);
    let client = client(Duration::from_secs(timeout_s))?;
    let res = client.get(&url).send().await?;
    anyhow::ensure!(res.status().is_success(), "{url} answered {}", res.status());
    Ok(())
}

/// Plain HTTP to loopback only. An explicitly empty root store stops reqwest from
/// building its platform verifier, which fails in a `FROM scratch` image because
/// there is no system CA store to load (ADR-007).
fn client(timeout: Duration) -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(timeout)
        .tls_certs_only(std::iter::empty())
        .build()
}

/// The server binds `HOST`, which is usually a wildcard; connect via loopback then.
pub fn healthz_url(config: &Config) -> String {
    let host = match config.host.as_str() {
        "0.0.0.0" | "" => "127.0.0.1".to_string(),
        "::" | "[::]" => "[::1]".to_string(),
        h if h.contains(':') && !h.starts_with('[') => format!("[{h}]"),
        h => h.to_string(),
    };
    format!("http://{host}:{}/healthz", config.port)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Sources;

    fn with_host(host: &str) -> Config {
        let env = [
            ("RIOT_API_KEY", "RGAPI-test-key-not-real"),
            ("HOST", host),
            ("PORT", "9000"),
        ];
        Config::from_sources(Sources {
            env: env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            ..Sources::default()
        })
        .unwrap()
    }

    #[test]
    fn client_builds_without_a_system_ca_store() {
        client(Duration::from_secs(1)).expect("no platform verifier involved");
    }

    #[test]
    fn wildcard_binds_are_reached_via_loopback() {
        assert_eq!(
            healthz_url(&with_host("0.0.0.0")),
            "http://127.0.0.1:9000/healthz"
        );
        assert_eq!(healthz_url(&with_host("::")), "http://[::1]:9000/healthz");
        assert_eq!(healthz_url(&with_host("::1")), "http://[::1]:9000/healthz");
        assert_eq!(
            healthz_url(&with_host("10.0.0.5")),
            "http://10.0.0.5:9000/healthz"
        );
    }
}
