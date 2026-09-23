//! Typed configuration from `.env`, the process environment and CLI flags.
//!
//! Precedence, lowest to highest: built-in defaults < `.env` < environment < flags.
//! Variable names are v1's wherever the concept survives (docs/design/07 §Configuration).
//!
//! figment merges the three layers as string maps; the typed pass below parses and
//! validates the merged map itself (ADR-008). figment's `Env` provider can't be used
//! directly: it turns numeric-looking strings into numbers, which then fail against
//! `String` fields such as `RIOT_API_KEY`.

use std::collections::BTreeMap;
use std::fmt;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use figment::Figment;
use figment::providers::Serialized;

/// Every variable the config reads. `.env.example` documents each one.
pub const VARS: &[&str] = &[
    "RIOT_API_KEY",
    "RIOT_USER_AGENT",
    "ENV",
    "HOST",
    "PORT",
    "LOG_LEVEL",
    "LOG_FORMAT",
    "DATA_DIR",
    "DATABASE_URL",
    "ROLE",
    "JOB_CONCURRENCY",
    "TLS",
    "TLS_DOMAIN",
    "ACME_EMAIL",
    "DEFAULT_PLATFORM",
    "CACHE_TTL_OVERRIDES",
    "NEG_TTL_SECONDS",
    "NEG_TTL_ACCOUNT_SECONDS",
    "CLIENT_WAIT_BUDGET_MS",
    "BULK_USAGE_CEILING",
    "STALE_WHILE_REVALIDATE",
    "METRICS_INTERVAL_S",
    "METRICS_HISTORY_INTERVAL_S",
    "TRACK_POLL_LIVE_S",
    "TRACK_POLL_RANK_S",
    "TRACK_POLL_MATCH_S",
    "DDRAGON_SYNC_S",
    "ARCHIVE_TIMELINES",
    "LOOKUP_BACKFILL_LIMIT",
    "TRACK_CATCHUP_LIMIT",
    "LADDER_CRAWL_S",
    "LADDER_QUEUES",
    "LADDER_PLATFORMS",
    "LADDER_TIER_FLOOR",
    "LADDER_BACKFILL_LIMIT",
    "FACTS_REEXTRACT_BATCH",
    "AGGREGATE_MIN_GAMES",
    "AGGREGATE_PATCH_LIMIT",
    "AGGREGATE_INTERVAL_S",
    "DDRAGON_DIR",
    "DDRAGON_LOCALE",
    "ADMIN_IP_ALLOWLIST",
    "BOOTSTRAP_ADMIN_KEY",
    "AUTH_DISABLED",
    "DEV_UI",
    "DOCS_UI",
    "DASHBOARD_UI",
];

/// v1's name for `ENV`. Read only as a fallback when `ENV` is unset, so a ported
/// `.env` with `NODE_ENV=production` still boots as production (ADR-008).
const LEGACY_ENV_VAR: &str = "NODE_ENV";

pub const DEFAULT_USER_AGENT: &str = "riot-proxy/2.0 (+https://github.com/NinjaGoldfinch/riot-proxy)";
pub const DB_FILE_NAME: &str = "riot-proxy.db";

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("Invalid environment configuration:\n{}", .0.iter().map(|e| format!("  {e}")).collect::<Vec<_>>().join("\n"))]
    Invalid(Vec<String>),
    #[error("could not read {path}: {source}")]
    DotEnv { path: PathBuf, source: dotenvy::Error },
    #[error("could not merge configuration layers: {0}")]
    Merge(#[from] Box<figment::Error>),
    #[error("could not create DATA_DIR {path}: {source}")]
    DataDir { path: PathBuf, source: std::io::Error },
}

/// A value that must never reach a log line. `Debug` and `Display` print `[REDACTED]`.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Environment {
    Development,
    Test,
    Production,
}

impl FromStr for Environment {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "development" => Ok(Self::Development),
            "test" => Ok(Self::Test),
            "production" => Ok(Self::Production),
            _ => Err("expected one of development, test, production".into()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    All,
    Api,
    Worker,
}

impl FromStr for Role {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "all" => Ok(Self::All),
            "api" => Ok(Self::Api),
            "worker" => Ok(Self::Worker),
            _ => Err("expected one of all, api, worker".into()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    Json,
    Pretty,
}

impl FromStr for LogFormat {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "json" => Ok(Self::Json),
            "pretty" => Ok(Self::Pretty),
            _ => Err("expected one of json, pretty".into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Database {
    Sqlite(PathBuf),
    /// Only usable with the `postgres` feature (P8-04). The URL may hold a password.
    Postgres(Secret),
}

#[derive(Debug, Clone)]
pub struct Config {
    pub riot_api_key: Secret,
    pub riot_user_agent: String,
    pub env: Environment,
    pub host: String,
    pub port: u16,
    pub log_level: String,
    pub log_format: LogFormat,
    pub data_dir: PathBuf,
    pub database: Database,
    pub role: Role,
    pub job_concurrency: u32,
    pub tls: bool,
    pub tls_domain: Option<String>,
    pub acme_email: Option<String>,

    // Values below are validated against Riot enums by the modules that own them
    // (routing in P1-01, endpoints in P1-02, ladder in P7-02).
    pub default_platform: String,
    pub cache_ttl_overrides: String,
    pub neg_ttl_seconds: u32,
    pub neg_ttl_account_seconds: u32,
    pub client_wait_budget_ms: u64,
    pub bulk_usage_ceiling: f64,
    pub stale_while_revalidate: bool,
    pub metrics_interval_s: u32,
    pub metrics_history_interval_s: u32,
    pub track_poll_live_s: u32,
    pub track_poll_rank_s: u32,
    pub track_poll_match_s: u32,
    pub ddragon_sync_s: u32,
    pub archive_timelines: bool,
    pub lookup_backfill_limit: u32,
    pub track_catchup_limit: u32,
    pub ladder_crawl_s: u32,
    pub ladder_queues: Vec<String>,
    /// Empty `LADDER_PLATFORMS` resolves to `[DEFAULT_PLATFORM]`, as in v1.
    pub ladder_platforms: Vec<String>,
    pub ladder_tier_floor: String,
    pub ladder_backfill_limit: u32,
    pub facts_reextract_batch: u32,
    pub aggregate_min_games: u32,
    pub aggregate_patch_limit: u32,
    pub aggregate_interval_s: u32,
    pub ddragon_dir: PathBuf,
    pub ddragon_locale: String,
    pub admin_ip_allowlist: Vec<String>,
    pub bootstrap_admin_key: Option<Secret>,
    pub auth_disabled: bool,
    pub dev_ui: bool,
    pub docs_ui: bool,
    pub dashboard_ui: bool,
}

/// Flags that override any variable. Global: accepted before or after any subcommand.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct ConfigArgs {
    /// Address to bind [env: HOST]
    #[arg(long, global = true)]
    pub host: Option<String>,
    /// Port to bind [env: PORT]
    #[arg(long, global = true)]
    pub port: Option<u16>,
    /// Directory for the database, ddragon mirror and backups [env: DATA_DIR]
    #[arg(long, global = true)]
    pub data_dir: Option<PathBuf>,
    /// sqlite://path (default) or postgres://… with the postgres feature [env: DATABASE_URL]
    #[arg(long, global = true)]
    pub database_url: Option<String>,
    /// development | test | production [env: ENV]
    #[arg(long = "env", global = true)]
    pub environment: Option<String>,
    /// all | api | worker [env: ROLE]
    #[arg(long, global = true)]
    pub role: Option<String>,
    /// tracing filter, e.g. info or riot_proxy=debug [env: LOG_LEVEL]
    #[arg(long, global = true)]
    pub log_level: Option<String>,
    /// json | pretty [env: LOG_FORMAT]
    #[arg(long, global = true)]
    pub log_format: Option<String>,
    /// Terminate TLS in-process via ACME [env: TLS]
    #[arg(long, global = true)]
    pub tls: bool,
    /// Domain to request a certificate for [env: TLS_DOMAIN]
    #[arg(long, global = true)]
    pub domain: Option<String>,
    /// ACME account contact [env: ACME_EMAIL]
    #[arg(long, global = true)]
    pub acme_email: Option<String>,
}

impl ConfigArgs {
    fn overrides(&self) -> BTreeMap<String, String> {
        let pairs = [
            ("HOST", self.host.clone()),
            ("PORT", self.port.map(|p| p.to_string())),
            (
                "DATA_DIR",
                self.data_dir.as_ref().map(|p| p.display().to_string()),
            ),
            ("DATABASE_URL", self.database_url.clone()),
            ("ENV", self.environment.clone()),
            ("ROLE", self.role.clone()),
            ("LOG_LEVEL", self.log_level.clone()),
            ("LOG_FORMAT", self.log_format.clone()),
            ("TLS", self.tls.then(|| "true".to_string())),
            ("TLS_DOMAIN", self.domain.clone()),
            ("ACME_EMAIL", self.acme_email.clone()),
        ];
        pairs
            .into_iter()
            .filter_map(|(k, v)| v.map(|v| (k.to_string(), v)))
            .collect()
    }
}

/// The raw inputs to [`Config::from_sources`], kept separate so tests never touch
/// the real process environment.
#[derive(Debug, Default)]
pub struct Sources {
    pub dotenv: Vec<(String, String)>,
    pub env: Vec<(String, String)>,
    pub args: ConfigArgs,
    /// Whether stdout is a terminal; picks the `LOG_FORMAT` default.
    pub stdout_is_tty: bool,
}

impl Config {
    /// Load from `./.env`, the process environment and `args`, then create `DATA_DIR`.
    pub fn load(args: ConfigArgs) -> Result<Self, ConfigError> {
        Self::load_with(Some(Path::new(".env")), std::env::vars(), args)
    }

    /// [`Config::load`] with the `.env` path and environment supplied by the caller.
    /// A missing `.env` file is not an error.
    pub fn load_with(
        dotenv_path: Option<&Path>,
        env: impl IntoIterator<Item = (String, String)>,
        args: ConfigArgs,
    ) -> Result<Self, ConfigError> {
        let dotenv = match dotenv_path {
            Some(path) if path.exists() => read_dotenv(path)?,
            _ => Vec::new(),
        };
        let config = Self::from_sources(Sources {
            dotenv,
            env: env.into_iter().collect(),
            args,
            stdout_is_tty: std::io::stdout().is_terminal(),
        })?;
        config.ensure_data_dir()?;
        Ok(config)
    }

    /// Merge and validate. Pure: no filesystem or environment access.
    pub fn from_sources(sources: Sources) -> Result<Self, ConfigError> {
        let merged: BTreeMap<String, String> = Figment::new()
            .merge(Serialized::defaults(known(sources.dotenv)))
            .merge(Serialized::defaults(known(sources.env)))
            .merge(Serialized::defaults(sources.args.overrides()))
            .extract()
            .map_err(Box::new)?;
        Self::from_map(merged, sources.stdout_is_tty)
    }

    fn from_map(map: BTreeMap<String, String>, stdout_is_tty: bool) -> Result<Self, ConfigError> {
        let mut v = Vars {
            map,
            errors: Vec::new(),
        };

        let riot_api_key = match v.opt_string("RIOT_API_KEY") {
            Some(key) if key.len() >= 8 => key,
            Some(_) => v.error("RIOT_API_KEY", "must be at least 8 characters"),
            None => v.error("RIOT_API_KEY", "is required"),
        };

        let env = match (v.opt_string("ENV"), v.opt_string(LEGACY_ENV_VAR)) {
            (Some(raw), _) => v.parse_or("ENV", &raw, Environment::Development),
            (None, Some(raw)) => v.parse_or(LEGACY_ENV_VAR, &raw, Environment::Development),
            (None, None) => Environment::Development,
        };

        let data_dir = PathBuf::from(v.string("DATA_DIR", "./data"));
        let database = match v.opt_string("DATABASE_URL") {
            None => Database::Sqlite(data_dir.join(DB_FILE_NAME)),
            Some(url) => {
                if let Some(path) = url.strip_prefix("sqlite://") {
                    Database::Sqlite(PathBuf::from(path))
                } else if url.starts_with("postgres://") || url.starts_with("postgresql://") {
                    v.push(
                        "DATABASE_URL",
                        "postgres:// needs a build with the `postgres` feature (not available yet)",
                    );
                    Database::Postgres(Secret::new(url))
                } else {
                    v.push("DATABASE_URL", "must start with sqlite:// or postgres://");
                    Database::Sqlite(data_dir.join(DB_FILE_NAME))
                }
            }
        };

        let role = v.parsed("ROLE", Role::All);
        if role != Role::All && !matches!(database, Database::Postgres(_)) {
            v.push(
                "ROLE",
                "api and worker need DATABASE_URL=postgres://; SQLite supports only all",
            );
        }

        let log_format = v.parsed(
            "LOG_FORMAT",
            if stdout_is_tty {
                LogFormat::Pretty
            } else {
                LogFormat::Json
            },
        );

        let auth_disabled = v.bool("AUTH_DISABLED", false);
        if auth_disabled && env == Environment::Production {
            v.errors
                .push("AUTH_DISABLED cannot be enabled when ENV=production".into());
        }

        let default_platform = v.string("DEFAULT_PLATFORM", "euw1");
        let ladder_platforms = match csv(&v.string("LADDER_PLATFORMS", "")) {
            empty if empty.is_empty() => vec![default_platform.clone()],
            list => list.into_iter().map(|p| p.to_ascii_lowercase()).collect(),
        };

        let config = Config {
            riot_api_key: Secret::new(riot_api_key),
            riot_user_agent: v.string("RIOT_USER_AGENT", DEFAULT_USER_AGENT),
            env,
            host: v.string("HOST", "0.0.0.0"),
            port: v.int("PORT", 8080, 1, u16::MAX),
            log_level: v.string("LOG_LEVEL", "info"),
            log_format,
            ddragon_dir: v
                .opt_string("DDRAGON_DIR")
                .map_or_else(|| data_dir.join("ddragon"), PathBuf::from),
            data_dir,
            database,
            role,
            job_concurrency: v.int("JOB_CONCURRENCY", 8, 1, u32::MAX),
            tls: v.bool("TLS", false),
            tls_domain: v.opt_string("TLS_DOMAIN"),
            acme_email: v.opt_string("ACME_EMAIL"),
            default_platform,
            cache_ttl_overrides: v.string("CACHE_TTL_OVERRIDES", ""),
            neg_ttl_seconds: v.int("NEG_TTL_SECONDS", 30, 1, u32::MAX),
            neg_ttl_account_seconds: v.int("NEG_TTL_ACCOUNT_SECONDS", 300, 1, u32::MAX),
            client_wait_budget_ms: v.int("CLIENT_WAIT_BUDGET_MS", 2000, 0, u64::MAX),
            bulk_usage_ceiling: v.float("BULK_USAGE_CEILING", 0.8, 0.0, 1.0),
            stale_while_revalidate: v.bool("STALE_WHILE_REVALIDATE", true),
            metrics_interval_s: v.int("METRICS_INTERVAL_S", 5, 1, 300),
            metrics_history_interval_s: v.int("METRICS_HISTORY_INTERVAL_S", 60, 10, 3600),
            track_poll_live_s: v.int("TRACK_POLL_LIVE_S", 60, 10, u32::MAX),
            track_poll_rank_s: v.int("TRACK_POLL_RANK_S", 600, 30, u32::MAX),
            track_poll_match_s: v.int("TRACK_POLL_MATCH_S", 300, 30, u32::MAX),
            ddragon_sync_s: v.int("DDRAGON_SYNC_S", 3600, 60, u32::MAX),
            archive_timelines: v.bool("ARCHIVE_TIMELINES", false),
            lookup_backfill_limit: v.int("LOOKUP_BACKFILL_LIMIT", 10_000, 0, 10_000),
            track_catchup_limit: v.int("TRACK_CATCHUP_LIMIT", 500, 0, 10_000),
            ladder_crawl_s: v.int("LADDER_CRAWL_S", 0, 0, 604_800),
            ladder_queues: csv(&v.string("LADDER_QUEUES", "RANKED_SOLO_5x5")),
            ladder_platforms,
            ladder_tier_floor: v.string("LADDER_TIER_FLOOR", "MASTER").to_ascii_uppercase(),
            ladder_backfill_limit: v.int("LADDER_BACKFILL_LIMIT", 100, 0, 10_000),
            facts_reextract_batch: v.int("FACTS_REEXTRACT_BATCH", 500, 1, 10_000),
            aggregate_min_games: v.int("AGGREGATE_MIN_GAMES", 10, 0, 10_000),
            aggregate_patch_limit: v.int("AGGREGATE_PATCH_LIMIT", 4, 0, 100),
            aggregate_interval_s: v.int("AGGREGATE_INTERVAL_S", 0, 0, 604_800),
            ddragon_locale: v.string("DDRAGON_LOCALE", "en_US"),
            admin_ip_allowlist: csv(&v.string("ADMIN_IP_ALLOWLIST", "")),
            bootstrap_admin_key: v.opt_string("BOOTSTRAP_ADMIN_KEY").map(Secret::new),
            auth_disabled,
            dev_ui: v.opt_bool("DEV_UI").unwrap_or(env != Environment::Production),
            docs_ui: v.bool("DOCS_UI", true),
            dashboard_ui: v.bool("DASHBOARD_UI", true),
        };

        if v.errors.is_empty() {
            Ok(config)
        } else {
            Err(ConfigError::Invalid(v.errors))
        }
    }

    /// Create `DATA_DIR` (and parents) if missing.
    pub fn ensure_data_dir(&self) -> Result<(), ConfigError> {
        std::fs::create_dir_all(&self.data_dir).map_err(|source| ConfigError::DataDir {
            path: self.data_dir.clone(),
            source,
        })
    }

    pub fn is_production(&self) -> bool {
        self.env == Environment::Production
    }
}

fn read_dotenv(path: &Path) -> Result<Vec<(String, String)>, ConfigError> {
    let err = |source| ConfigError::DotEnv {
        path: path.to_path_buf(),
        source,
    };
    dotenvy::from_path_iter(path)
        .map_err(err)?
        .map(|item| item.map_err(err))
        .collect()
}

/// Keep only variables the config reads, and drop empty values so `FOO=` in a
/// `.env` file falls back to the default rather than overriding it (v1 behaviour).
fn known(pairs: Vec<(String, String)>) -> BTreeMap<String, String> {
    pairs
        .into_iter()
        .filter(|(k, v)| !v.is_empty() && (VARS.contains(&k.as_str()) || k == LEGACY_ENV_VAR))
        .collect()
}

fn csv(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

/// The merged string map plus every validation error seen so far, so one boot
/// reports every bad variable at once.
struct Vars {
    map: BTreeMap<String, String>,
    errors: Vec<String>,
}

impl Vars {
    fn push(&mut self, name: &str, message: &str) {
        self.errors.push(format!("{name}: {message}"));
    }

    /// Record an error and return a placeholder; the config is discarded anyway.
    fn error<T: Default>(&mut self, name: &str, message: &str) -> T {
        self.push(name, message);
        T::default()
    }

    fn opt_string(&self, name: &str) -> Option<String> {
        self.map.get(name).cloned()
    }

    fn string(&self, name: &str, default: &str) -> String {
        self.opt_string(name).unwrap_or_else(|| default.to_string())
    }

    fn parse_or<T: FromStr<Err = String>>(&mut self, name: &str, raw: &str, fallback: T) -> T {
        raw.parse().unwrap_or_else(|e: String| {
            self.push(name, &format!("'{raw}': {e}"));
            fallback
        })
    }

    fn parsed<T: FromStr<Err = String>>(&mut self, name: &str, default: T) -> T {
        match self.opt_string(name) {
            Some(raw) => self.parse_or(name, &raw, default),
            None => default,
        }
    }

    fn int<T>(&mut self, name: &str, default: T, min: T, max: T) -> T
    where
        T: FromStr + PartialOrd + fmt::Display + Copy,
    {
        let Some(raw) = self.opt_string(name) else {
            return default;
        };
        match raw.trim().parse::<T>() {
            Ok(n) if n >= min && n <= max => n,
            Ok(_) => self.range_error(name, &raw, min, max, default),
            Err(_) => {
                self.push(name, &format!("'{raw}' is not a whole number"));
                default
            }
        }
    }

    fn float(&mut self, name: &str, default: f64, min: f64, max: f64) -> f64 {
        let Some(raw) = self.opt_string(name) else {
            return default;
        };
        match raw.trim().parse::<f64>() {
            Ok(n) if n.is_finite() && n >= min && n <= max => n,
            Ok(_) => self.range_error(name, &raw, min, max, default),
            Err(_) => {
                self.push(name, &format!("'{raw}' is not a number"));
                default
            }
        }
    }

    fn range_error<T: fmt::Display>(&mut self, name: &str, raw: &str, min: T, max: T, default: T) -> T {
        self.push(name, &format!("'{raw}' is outside {min}..={max}"));
        default
    }

    fn opt_bool(&mut self, name: &str) -> Option<bool> {
        let raw = self.opt_string(name)?;
        match raw.trim().to_ascii_lowercase().as_str() {
            "true" | "1" => Some(true),
            "false" | "0" => Some(false),
            _ => {
                self.push(name, &format!("'{raw}' is not a boolean (true/false/1/0)"));
                None
            }
        }
    }

    fn bool(&mut self, name: &str, default: bool) -> bool {
        self.opt_bool(name).unwrap_or(default)
    }
}

#[cfg(test)]
mod tests;
