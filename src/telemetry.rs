//! Logging (tracing) and the Prometheus recorder behind `/metrics`.

use std::sync::Mutex;
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::get;
use metrics_exporter_prometheus::{BuildError, Matcher, PrometheusBuilder, PrometheusHandle};
use tracing::Subscriber;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;

use crate::config::{Config, LogFormat};
use crate::metrics::{CATALOGUE, describe_all};

/// Prometheus text exposition format, as prom-client served it in v1.
pub const METRICS_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

/// How often histogram buckets are drained into the render buffer.
const UPKEEP_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum TelemetryError {
    #[error("invalid LOG_LEVEL filter: {0}")]
    Filter(#[from] tracing_subscriber::filter::ParseError),
    #[error("could not install the tracing subscriber: {0}")]
    Subscriber(#[from] tracing_subscriber::util::TryInitError),
    #[error("could not install the metrics recorder: {0}")]
    Metrics(#[from] BuildError),
}

/// Install the global tracing subscriber: JSON or pretty per `LOG_FORMAT`,
/// filtered by `LOG_LEVEL` (a `tracing` filter directive such as `info`).
pub fn init_tracing(config: &Config) -> Result<(), TelemetryError> {
    let filter = EnvFilter::builder().parse(&config.log_level)?;
    let registry = tracing_subscriber::registry().with(filter);
    match config.log_format {
        LogFormat::Json => registry.with(json_layer(std::io::stdout)).try_init()?,
        LogFormat::Pretty => registry
            .with(tracing_subscriber::fmt::layer().pretty())
            .try_init()?,
    }
    Ok(())
}

/// One JSON object per event, event fields at the top level, and the innermost
/// span's fields under `span` — which is how `request_id` reaches every line.
pub fn json_layer<S, W>(writer: W) -> impl Layer<S>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    W: for<'w> MakeWriter<'w> + Send + Sync + 'static,
{
    tracing_subscriber::fmt::layer()
        .json()
        .flatten_event(true)
        .with_current_span(true)
        .with_span_list(false)
        .with_writer(writer)
}

static HANDLE: Mutex<Option<PrometheusHandle>> = Mutex::new(None);

/// Install the global Prometheus recorder once, with v1's histogram buckets, and
/// describe the catalogue. Later calls return the same handle.
pub fn metrics_handle() -> Result<PrometheusHandle, TelemetryError> {
    // A poisoned lock only means another caller panicked mid-install; the Option
    // inside is still either None or a fully installed handle.
    let mut slot = HANDLE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(handle) = slot.as_ref() {
        return Ok(handle.clone());
    }
    let mut builder = PrometheusBuilder::new();
    for m in CATALOGUE.iter().filter(|m| !m.buckets.is_empty()) {
        builder = builder.set_buckets_for_metric(Matcher::Full(m.name.to_string()), m.buckets)?;
    }
    let handle = builder.install_recorder()?;
    describe_all();
    *slot = Some(handle.clone());
    Ok(handle)
}

/// Drain histograms periodically; the exporter leaves this to the caller when
/// it isn't running its own HTTP listener.
pub fn spawn_upkeep(handle: PrometheusHandle) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(UPKEEP_INTERVAL);
        loop {
            tick.tick().await;
            handle.run_upkeep();
        }
    })
}

/// `GET /metrics`. Merged into the app router by `routes::health` (P0-05).
pub fn metrics_router(handle: PrometheusHandle) -> Router {
    Router::new()
        .route("/metrics", get(render_metrics))
        .with_state(handle)
}

/// Prometheus text exposition, documented in the OpenAPI document (v1 `/metrics`).
#[utoipa::path(
    get,
    path = "/metrics",
    tag = "ops",
    summary = "Prometheus metrics",
    description = "Prometheus text exposition format, not JSON. Behind a reverse proxy, restrict this path to \
                   private ranges.",
    security(()),
    responses((status = 200, description = "Prometheus text exposition format", content_type = "text/plain", body = String)),
)]
pub async fn render_metrics(State(handle): State<PrometheusHandle>) -> impl IntoResponse {
    ([(header::CONTENT_TYPE, METRICS_CONTENT_TYPE)], handle.render())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Sources};

    fn config(level: &str) -> Config {
        let env = [("RIOT_API_KEY", "RGAPI-test-key-not-real"), ("LOG_LEVEL", level)];
        Config::from_sources(Sources {
            env: env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            ..Sources::default()
        })
        .expect("config")
    }

    #[test]
    fn invalid_log_level_is_reported_not_panicked() {
        let err = init_tracing(&config("info,[[[")).expect_err("bad filter");
        assert!(matches!(err, TelemetryError::Filter(_)), "{err:?}");
    }

    #[derive(Clone, Default)]
    struct Buf(std::sync::Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Buf {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn json_lines_carry_the_request_span_fields() {
        let buf = Buf::default();
        let writer = buf.clone();
        let subscriber = tracing_subscriber::registry().with(json_layer(move || writer.clone()));
        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("request", request_id = "01TESTID");
            let _entered = span.enter();
            tracing::info!(upstream_ms = 12, "served");
        });
        let out = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        let line: serde_json::Value = serde_json::from_str(out.trim()).expect(&out);
        assert_eq!(line["message"], "served");
        assert_eq!(line["upstream_ms"], 12);
        assert_eq!(line["level"], "INFO");
        assert_eq!(line["span"]["request_id"], "01TESTID");
    }

    #[test]
    fn tracing_installs_once() {
        init_tracing(&config("riot_proxy=debug,info")).expect("first install");
        let err = init_tracing(&config("info")).expect_err("second install");
        assert!(matches!(err, TelemetryError::Subscriber(_)), "{err:?}");
    }
}

/// A counter's current values by label set, read from the exporter's own text
/// rendering: what the dashboard's snapshot reports (v1 read prom-client's
/// registry the same way).
pub fn counter_values(
    handle: &PrometheusHandle,
    name: &str,
) -> Vec<(std::collections::BTreeMap<String, String>, f64)> {
    parse_counter(&handle.render(), name)
}

/// One metric's series by label set, read from a rendering already taken:
/// `/status` reads several metrics from one `render()` (STS-02). Label values
/// are free text (a consumer's name), so the label block is read as the
/// Prometheus text format writes it: `key="value"` pairs, with `\"`, `\\`
/// and `\n` escaped inside the quotes, closed by the first `}` outside them.
pub fn parse_counter(text: &str, name: &str) -> Vec<(std::collections::BTreeMap<String, String>, f64)> {
    text.lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|line| {
            let rest = line.strip_prefix(name)?;
            let (labels, value) = match rest.strip_prefix('{') {
                Some(r) => parse_labels(r)?,
                None if rest.starts_with(' ') => (std::collections::BTreeMap::new(), rest),
                None => return None,
            };
            // The value, then an optional timestamp the exporter never writes.
            Some((labels, value.split_whitespace().next()?.parse().ok()?))
        })
        .collect()
}

/// The pairs of a label block whose `{` is already consumed, and what follows
/// its closing `}`. `None` when the block is not well formed.
fn parse_labels(mut s: &str) -> Option<(std::collections::BTreeMap<String, String>, &str)> {
    let mut labels = std::collections::BTreeMap::new();
    loop {
        s = s.trim_start();
        if let Some(after) = s.strip_prefix('}') {
            return Some((labels, after));
        }
        let (key, after) = s.split_once('=')?;
        let after = after.trim_start().strip_prefix('"')?;
        let mut value = String::new();
        let mut chars = after.char_indices();
        let end = loop {
            match chars.next()? {
                (i, '"') => break i,
                (_, '\\') => match chars.next()? {
                    (_, 'n') => value.push('\n'),
                    (_, c @ ('"' | '\\')) => value.push(c),
                    (_, c) => {
                        value.push('\\');
                        value.push(c);
                    }
                },
                (_, c) => value.push(c),
            }
        };
        labels.insert(key.trim().to_string(), value);
        s = after[end + 1..].trim_start();
        // A trailing comma before `}` is allowed by the format.
        s = s.strip_prefix(',').unwrap_or(s);
    }
}

#[cfg(test)]
mod counter_tests {
    use super::parse_counter;

    #[test]
    fn counters_are_read_back_from_the_rendering() {
        let text = "# TYPE proxy_cache_reads_total counter\n\
            proxy_cache_reads_total{state=\"hit\"} 12\n\
            proxy_cache_reads_total{state=\"miss\"} 3\n\
            proxy_cache_reads_total_other 9\n\
            proxy_refresh_claims_total{part=\"summoner\",outcome=\"claimed\"} 2\n";
        let hits = parse_counter(text, "proxy_cache_reads_total");
        assert_eq!(hits.len(), 2);
        assert_eq!((hits[0].0["state"].as_str(), hits[0].1), ("hit", 12.0));
        let claims = parse_counter(text, "proxy_refresh_claims_total");
        assert_eq!(
            (claims[0].0["part"].as_str(), claims[0].0["outcome"].as_str()),
            ("summoner", "claimed")
        );
    }

    /// Label values are free text: a consumer's name may hold any of the
    /// characters the format uses for structure (STS-02).
    #[test]
    fn label_values_may_hold_the_format_s_own_characters() {
        let text = concat!(
            r#"consumer_requests_total{consumer="Acme, Inc",group="riot",class="2xx"} 4"#,
            "\n",
            r#"consumer_requests_total{consumer="team}x",group="ops",class="5xx"} 2"#,
            "\n",
            r#"consumer_requests_total{consumer="say \"hi\" a=b",group="lol",class="2xx"} 3"#,
            "\n",
            r#"consumer_requests_total{consumer="back\\slash\nline",group="ui",class="4xx",} 1"#,
            "\n",
        );
        let got: Vec<_> = parse_counter(text, "consumer_requests_total")
            .into_iter()
            .map(|(l, v)| (l["consumer"].clone(), l["group"].clone(), l["class"].clone(), v))
            .collect();
        let want = [
            ("Acme, Inc", "riot", "2xx", 4.0),
            ("team}x", "ops", "5xx", 2.0),
            ("say \"hi\" a=b", "lol", "2xx", 3.0),
            ("back\\slash\nline", "ui", "4xx", 1.0),
        ];
        assert_eq!(got.len(), want.len(), "{got:?}");
        for (g, w) in got.iter().zip(want) {
            assert_eq!((g.0.as_str(), g.1.as_str(), g.2.as_str(), g.3), w);
        }
    }

    /// A line that is not a well-formed series is skipped, not misread.
    #[test]
    fn a_broken_label_block_is_skipped() {
        let text = "m{a=\"unterminated} 1\nm{a=b} 2\nm{a=\"ok\"} 3\nm 4\nm{} 5\n";
        let got: Vec<f64> = parse_counter(text, "m").into_iter().map(|(_, v)| v).collect();
        assert_eq!(got, [3.0, 4.0, 5.0]);
    }
}
