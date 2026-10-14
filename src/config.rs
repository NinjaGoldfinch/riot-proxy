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

use crate::riot::routing::Platform;
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
    "JOB_KIND_LIMITS",
    "JOB_YIELD_BUDGET_MS",
    "CACHE_TTL_OVERRIDES",
    "CACHE_L1_MAX_MB",
    "NEG_TTL_SECONDS",
    "NEG_TTL_ACCOUNT_SECONDS",
    "CLIENT_WAIT_BUDGET_MS",
    "BULK_USAGE_CEILING",
    "BULK_IDLE_CEILING",
    "BULK_IDLE_AFTER_S",
    "STALE_WHILE_REVALIDATE",
    "METRICS_INTERVAL_S",
    "METRICS_HISTORY_INTERVAL_S",
    "TRACK_POLL_LIVE_S",
    "TRACK_POLL_RANK_S",
    "TRACK_POLL_MATCH_S",
    "DDRAGON_SYNC_S",
    "ARCHIVE_TIMELINES",
    "TIMELINE_BACKFILL_PATCHES",
    "BACKUP_KEEP",
    "ARCHIVE_OBJECT_URL",
    "ARCHIVE_OBJECT_ENDPOINT",
    "ARCHIVE_OBJECT_REGION",
    "ARCHIVE_OBJECT_ACCESS_KEY",
    "ARCHIVE_OBJECT_SECRET_KEY",
    "ARCHIVE_OBJECT_CONCURRENCY",
    "ARCHIVE_BODY_LOCAL_DAYS",
    "ARCHIVE_PACK_TARGET_BYTES",
    "ARCHIVE_OFFLOAD_PACE_MS",
    "LOOKUP_BACKFILL_LIMIT",
    "TRACK_CATCHUP_LIMIT",
    "LADDER_CRAWL_S",
    "LADDER_QUEUES",
    "LADDER_PLATFORMS",
    "LADDER_TIER_FLOOR",
    "LADDER_BACKFILL_LIMIT",
    "RANK_LOOKUP_LIMIT",
    "RANK_LOOKUP_RECHECK_S",
    "TIER_LATE_STAMP_DAYS",
    "FACTS_REEXTRACT_BATCH",
    "AGGREGATE_PATCH_LIMIT",
    "ANALYTICS_CACHE_ENTRIES",
    "AGGREGATE_INTERVAL_S",
    "DDRAGON_DIR",
    "DDRAGON_LOCALE",
    "ADMIN_IP_ALLOWLIST",
    "TRUST_PROXY",
    "ADMIN_CORS_ORIGINS",
    "BOOTSTRAP_ADMIN_KEY",
    "AUTH_DISABLED",
    "DEV_UI",
    "DOCS_UI",
    "DASHBOARD_UI",
    "RIOT_BASE_URL",
    "DDRAGON_BASE_URL",
];

/// Read only to refuse: built-in TLS was removed (ADR-067), and `TLS=true` must
/// not quietly come up as plain HTTP.
const RETIRED_TLS_VAR: &str = "TLS";

/// v1's name for `ENV`. Read only as a fallback when `ENV` is unset, so a ported
/// `.env` with `NODE_ENV=production` still boots as production (ADR-008).
const LEGACY_ENV_VAR: &str = "NODE_ENV";

pub const DEFAULT_USER_AGENT: &str = "riot-proxy/2.0 (+https://github.com/NinjaGoldfinch/riot-proxy)";
pub const DB_FILE_NAME: &str = "riot-proxy.db";
/// Bulk's ceiling while nobody is using the app (THR-06a). Below 1.0 so the gap
/// absorbs drift between our windows and Riot's.
pub const DEFAULT_BULK_IDLE_CEILING: f64 = 0.95;

/// One analytics rebuild at a time (OPS-06): rebuilds for several ladders
/// started together run one after another instead of sharing the CPU and
/// SQLite's write lock.
/// `archive:offload` packs one kind at a time and must never race itself
/// (OBJ-03).
pub const DEFAULT_JOB_KIND_LIMITS: &str = "aggregate:analytics=1,archive:offload=1";

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("Invalid environment configuration:\n{}", .0.iter().map(|e| format!("  {e}")).collect::<Vec<_>>().join("\n"))]
    Invalid(Vec<String>),
    #[error("could not read {path}: {source}")]
    DotEnvIo { path: PathBuf, source: std::io::Error },
    #[error("{path} line {line}: {message}")]
    DotEnv {
        path: PathBuf,
        line: usize,
        message: String,
    },
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

impl Environment {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Development => "development",
            Self::Test => "test",
            Self::Production => "production",
        }
    }
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

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Api => "api",
            Self::Worker => "worker",
        }
    }
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

impl LogFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Pretty => "pretty",
        }
    }
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

/// Where packed bodies are kept (`ARCHIVE_OBJECT_URL`, design/12, OBJ-01).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObjectUrl {
    /// `s3://<bucket>`: an S3-compatible bucket at `ARCHIVE_OBJECT_ENDPOINT`.
    S3 { bucket: String },
    /// `file:///<dir>`: a directory standing in for a bucket (development and
    /// tests). Refused in production.
    File(PathBuf),
}

impl fmt::Display for ObjectUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::S3 { bucket } => write!(f, "s3://{bucket}"),
            Self::File(dir) => write!(f, "file://{}", dir.display()),
        }
    }
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
    /// The most jobs of a kind running at once, for kinds with no rate-limit
    /// lane (OPS-06). Kinds not named have no cap beyond the worker count.
    pub job_kind_limits: BTreeMap<String, u32>,
    /// How long a job's fetch waits for the limiter before the job gives its
    /// worker back (SCH-01).
    pub job_yield_budget_ms: u64,

    // Validated against the endpoint registry by its owner (P1-02).
    pub cache_ttl_overrides: String,
    /// L1 weight budget in MiB (design/07 §Sizing, default 128).
    pub cache_l1_max_mb: u32,
    pub neg_ttl_seconds: u32,
    pub neg_ttl_account_seconds: u32,
    pub client_wait_budget_ms: u64,
    pub bulk_usage_ceiling: f64,
    /// Bulk's ceiling on a scope with no interactive request in the last
    /// `bulk_idle_after_s` (THR-06a). Never below `bulk_usage_ceiling`.
    pub bulk_idle_ceiling: f64,
    pub bulk_idle_after_s: u32,
    pub stale_while_revalidate: bool,
    pub metrics_interval_s: u32,
    pub metrics_history_interval_s: u32,
    pub track_poll_live_s: u32,
    pub track_poll_rank_s: u32,
    pub track_poll_match_s: u32,
    pub ddragon_sync_s: u32,
    pub archive_timelines: bool,
    /// `TIMELINE_BACKFILL_PATCHES` (TL-02): `timelines:backfill` fetches the
    /// missing timelines of ranked matches in this many of the newest
    /// patches; 0 turns it off.
    pub timeline_backfill_patches: u32,
    /// `BACKUP_KEEP` (DSK-02): daily `VACUUM INTO` backups kept in
    /// `$DATA_DIR/backups`; 0 takes none.
    pub backup_keep: u32,
    /// `ARCHIVE_OBJECT_URL`: unset keeps every body in SQLite and registers
    /// no offload (design/12).
    pub archive_object_url: Option<ObjectUrl>,
    /// `ARCHIVE_OBJECT_ENDPOINT`: `scheme://host[:port]`, required with an
    /// `s3://` URL. A host that starts with `<bucket>.` is addressed
    /// virtual-hosted style, any other path style.
    pub archive_object_endpoint: Option<String>,
    pub archive_object_region: String,
    pub archive_object_access_key: Option<Secret>,
    pub archive_object_secret_key: Option<Secret>,
    /// Ranged GETs `get_many` runs at once against the bucket.
    pub archive_object_concurrency: u32,
    /// How long SQLite keeps its own copy of a body already in the bucket,
    /// counted from when its match was archived; 0 frees every packed body.
    pub archive_body_local_days: u32,
    /// A pack closes at this many bytes of bodies.
    pub archive_pack_target_bytes: u64,
    /// Pause between offload turns.
    pub archive_offload_pace_ms: u64,
    pub lookup_backfill_limit: u32,
    pub track_catchup_limit: u32,
    pub ladder_crawl_s: u32,
    pub ladder_queues: Vec<String>,
    /// Empty `LADDER_PLATFORMS` schedules no crawl: there is no default platform (ADR-065).
    pub ladder_platforms: Vec<Platform>,
    pub ladder_tier_floor: String,
    pub ladder_backfill_limit: u32,
    /// Players a `ranks:lookup` run looks up at most; 0 turns it off (ADR-111).
    pub rank_lookup_limit: u32,
    /// How long a lookup keeps a player off the next run's list (ADR-111).
    pub rank_lookup_recheck_s: u32,
    /// How far back a rank places a player's `UNKNOWN` tier stamps, in days;
    /// 0 places none (THR-02, ADR-127).
    pub tier_late_stamp_days: u32,
    pub facts_reextract_batch: u32,
    pub aggregate_patch_limit: u32,
    /// Analytics responses kept in memory until the next rebuild (PERF-03,
    /// ADR-169); 0 keeps none.
    pub analytics_cache_entries: u32,
    pub aggregate_interval_s: u32,
    pub ddragon_dir: PathBuf,
    pub ddragon_locale: String,
    pub admin_ip_allowlist: Vec<String>,
    /// Take the client address from `X-Forwarded-For` (a trusted proxy in front).
    /// Off: the TCP peer, which a caller cannot forge (ADR-068).
    pub trust_proxy: bool,
    /// Exact origins (`https://host[:port]`) allowed to call the admin API,
    /// `/v1/ws` and the health routes from a browser; empty sends no CORS
    /// headers at all (ADM-07, ADR-014's deferral closed).
    pub admin_cors_origins: Vec<String>,
    pub bootstrap_admin_key: Option<Secret>,
    pub auth_disabled: bool,
    /// `/dev`: never in production, whatever `DEV_UI` says (ADR-071).
    pub dev_ui: bool,
    /// `DEV_UI=true` was set with `ENV=production` and ignored; `serve` warns.
    pub dev_ui_ignored: bool,
    pub docs_ui: bool,
    pub dashboard_ui: bool,
    /// Send every Riot request here instead of Riot's hosts (a mock; the
    /// acceptance suite). Refused in production.
    pub riot_base_url: Option<String>,
    /// Fetch Data Dragon and the queue table from here instead. Refused in production.
    pub ddragon_base_url: Option<String>,
}

/// What a secret reads as in [`Config::redacted`].
pub const REDACTED: &str = "[REDACTED]";

impl Config {
    /// Every variable in [`VARS`] but `RIOT_API_KEY`, with the value this
    /// process runs with, written as it would be in `.env` (ADM-03,
    /// `GET /v1/admin/info`). Secrets (`BOOTSTRAP_ADMIN_KEY`, a Postgres
    /// `DATABASE_URL`) read [`REDACTED`]; an optional value left unset is `None`.
    /// `RIOT_API_KEY` is left out entirely: not even its length is shown.
    pub fn redacted(&self) -> BTreeMap<&'static str, Option<String>> {
        fn s(v: impl ToString) -> Option<String> {
            Some(v.to_string())
        }
        let path = |p: &Path| s(p.display());
        let kind_limits = if self.job_kind_limits.is_empty() {
            "none".to_string()
        } else {
            self.job_kind_limits
                .iter()
                .map(|(k, n)| format!("{k}={n}"))
                .collect::<Vec<_>>()
                .join(",")
        };
        let database = match &self.database {
            Database::Sqlite(p) => format!("sqlite://{}", p.display()),
            Database::Postgres(_) => REDACTED.to_string(),
        };
        let platforms: Vec<&str> = self.ladder_platforms.iter().map(|p| p.as_str()).collect();
        BTreeMap::from([
            ("RIOT_USER_AGENT", s(&self.riot_user_agent)),
            ("ENV", s(self.env.as_str())),
            ("HOST", s(&self.host)),
            ("PORT", s(self.port)),
            ("LOG_LEVEL", s(&self.log_level)),
            ("LOG_FORMAT", s(self.log_format.as_str())),
            ("DATA_DIR", path(&self.data_dir)),
            ("DATABASE_URL", Some(database)),
            ("ROLE", s(self.role.as_str())),
            ("JOB_CONCURRENCY", s(self.job_concurrency)),
            ("JOB_KIND_LIMITS", Some(kind_limits)),
            ("JOB_YIELD_BUDGET_MS", s(self.job_yield_budget_ms)),
            ("CACHE_TTL_OVERRIDES", s(&self.cache_ttl_overrides)),
            ("CACHE_L1_MAX_MB", s(self.cache_l1_max_mb)),
            ("NEG_TTL_SECONDS", s(self.neg_ttl_seconds)),
            ("NEG_TTL_ACCOUNT_SECONDS", s(self.neg_ttl_account_seconds)),
            ("CLIENT_WAIT_BUDGET_MS", s(self.client_wait_budget_ms)),
            ("BULK_USAGE_CEILING", s(self.bulk_usage_ceiling)),
            ("BULK_IDLE_CEILING", s(self.bulk_idle_ceiling)),
            ("BULK_IDLE_AFTER_S", s(self.bulk_idle_after_s)),
            ("STALE_WHILE_REVALIDATE", s(self.stale_while_revalidate)),
            ("METRICS_INTERVAL_S", s(self.metrics_interval_s)),
            ("METRICS_HISTORY_INTERVAL_S", s(self.metrics_history_interval_s)),
            ("TRACK_POLL_LIVE_S", s(self.track_poll_live_s)),
            ("TRACK_POLL_RANK_S", s(self.track_poll_rank_s)),
            ("TRACK_POLL_MATCH_S", s(self.track_poll_match_s)),
            ("DDRAGON_SYNC_S", s(self.ddragon_sync_s)),
            ("ARCHIVE_TIMELINES", s(self.archive_timelines)),
            ("TIMELINE_BACKFILL_PATCHES", s(self.timeline_backfill_patches)),
            ("BACKUP_KEEP", s(self.backup_keep)),
            (
                "ARCHIVE_OBJECT_URL",
                self.archive_object_url.as_ref().map(ToString::to_string),
            ),
            ("ARCHIVE_OBJECT_ENDPOINT", self.archive_object_endpoint.clone()),
            ("ARCHIVE_OBJECT_REGION", s(&self.archive_object_region)),
            (
                "ARCHIVE_OBJECT_ACCESS_KEY",
                self.archive_object_access_key
                    .as_ref()
                    .map(|_| REDACTED.to_string()),
            ),
            (
                "ARCHIVE_OBJECT_SECRET_KEY",
                self.archive_object_secret_key
                    .as_ref()
                    .map(|_| REDACTED.to_string()),
            ),
            ("ARCHIVE_OBJECT_CONCURRENCY", s(self.archive_object_concurrency)),
            ("ARCHIVE_BODY_LOCAL_DAYS", s(self.archive_body_local_days)),
            ("ARCHIVE_PACK_TARGET_BYTES", s(self.archive_pack_target_bytes)),
            ("ARCHIVE_OFFLOAD_PACE_MS", s(self.archive_offload_pace_ms)),
            ("LOOKUP_BACKFILL_LIMIT", s(self.lookup_backfill_limit)),
            ("TRACK_CATCHUP_LIMIT", s(self.track_catchup_limit)),
            ("LADDER_CRAWL_S", s(self.ladder_crawl_s)),
            ("LADDER_QUEUES", s(self.ladder_queues.join(","))),
            ("LADDER_PLATFORMS", s(platforms.join(","))),
            ("LADDER_TIER_FLOOR", s(&self.ladder_tier_floor)),
            ("LADDER_BACKFILL_LIMIT", s(self.ladder_backfill_limit)),
            ("RANK_LOOKUP_LIMIT", s(self.rank_lookup_limit)),
            ("RANK_LOOKUP_RECHECK_S", s(self.rank_lookup_recheck_s)),
            ("TIER_LATE_STAMP_DAYS", s(self.tier_late_stamp_days)),
            ("FACTS_REEXTRACT_BATCH", s(self.facts_reextract_batch)),
            ("AGGREGATE_PATCH_LIMIT", s(self.aggregate_patch_limit)),
            ("ANALYTICS_CACHE_ENTRIES", s(self.analytics_cache_entries)),
            ("AGGREGATE_INTERVAL_S", s(self.aggregate_interval_s)),
            ("DDRAGON_DIR", path(&self.ddragon_dir)),
            ("DDRAGON_LOCALE", s(&self.ddragon_locale)),
            ("ADMIN_IP_ALLOWLIST", s(self.admin_ip_allowlist.join(","))),
            ("TRUST_PROXY", s(self.trust_proxy)),
            ("ADMIN_CORS_ORIGINS", s(self.admin_cors_origins.join(","))),
            (
                "BOOTSTRAP_ADMIN_KEY",
                self.bootstrap_admin_key.as_ref().map(|_| REDACTED.to_string()),
            ),
            ("AUTH_DISABLED", s(self.auth_disabled)),
            ("DEV_UI", s(self.dev_ui)),
            ("DOCS_UI", s(self.docs_ui)),
            ("DASHBOARD_UI", s(self.dashboard_ui)),
            ("RIOT_BASE_URL", self.riot_base_url.clone()),
            ("DDRAGON_BASE_URL", self.ddragon_base_url.clone()),
        ])
    }
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
    /// Background jobs run at once [env: JOB_CONCURRENCY]
    #[arg(long, global = true)]
    pub job_concurrency: Option<u32>,
    /// Analytics responses kept in memory, 0 none [env: ANALYTICS_CACHE_ENTRIES]
    #[arg(long, global = true)]
    pub analytics_cache_entries: Option<u32>,
    /// tracing filter, e.g. info or riot_proxy=debug [env: LOG_LEVEL]
    #[arg(long, global = true)]
    pub log_level: Option<String>,
    /// json | pretty [env: LOG_FORMAT]
    #[arg(long, global = true)]
    pub log_format: Option<String>,
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
            ("JOB_CONCURRENCY", self.job_concurrency.map(|n| n.to_string())),
            (
                "ANALYTICS_CACHE_ENTRIES",
                self.analytics_cache_entries.map(|n| n.to_string()),
            ),
            ("LOG_LEVEL", self.log_level.clone()),
            ("LOG_FORMAT", self.log_format.clone()),
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

        let dev_ui_flag = v.opt_bool("DEV_UI");
        let dev_ui_set = dev_ui_flag.is_some();
        let dev_ui = dev_ui_flag.unwrap_or(true);

        if v.bool(RETIRED_TLS_VAR, false) {
            v.push(
                RETIRED_TLS_VAR,
                "built-in TLS was removed; terminate HTTPS in a reverse proxy",
            );
        }

        let riot_base_url = v.opt_string("RIOT_BASE_URL");
        let ddragon_base_url = v.opt_string("DDRAGON_BASE_URL");
        for (name, set) in [
            ("RIOT_BASE_URL", riot_base_url.is_some()),
            ("DDRAGON_BASE_URL", ddragon_base_url.is_some()),
        ] {
            if set && env == Environment::Production {
                v.errors.push(format!("{name} cannot be set when ENV=production"));
            }
        }

        let ladder_platforms = csv(&v.string("LADDER_PLATFORMS", ""))
            .iter()
            .map(|p| v.platform("LADDER_PLATFORMS", p))
            .collect();

        let objects = v.object_store(env);

        let bulk_idle_ceiling_set = v.opt_string("BULK_IDLE_CEILING").is_some();

        let mut config = Config {
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
            job_kind_limits: v.kind_limits("JOB_KIND_LIMITS", DEFAULT_JOB_KIND_LIMITS),
            job_yield_budget_ms: v.int("JOB_YIELD_BUDGET_MS", 1000, 0, 900_000),
            cache_ttl_overrides: v.string("CACHE_TTL_OVERRIDES", ""),
            cache_l1_max_mb: v.int("CACHE_L1_MAX_MB", 128, 1, 1_048_576),
            neg_ttl_seconds: v.int("NEG_TTL_SECONDS", 30, 1, u32::MAX),
            neg_ttl_account_seconds: v.int("NEG_TTL_ACCOUNT_SECONDS", 300, 1, u32::MAX),
            client_wait_budget_ms: v.int("CLIENT_WAIT_BUDGET_MS", 2000, 0, u64::MAX),
            bulk_usage_ceiling: v.float("BULK_USAGE_CEILING", 0.8, 0.0, 1.0),
            bulk_idle_ceiling: v.float("BULK_IDLE_CEILING", DEFAULT_BULK_IDLE_CEILING, 0.0, 1.0),
            bulk_idle_after_s: v.int("BULK_IDLE_AFTER_S", 30, 0, 86_400),
            stale_while_revalidate: v.bool("STALE_WHILE_REVALIDATE", true),
            metrics_interval_s: v.int("METRICS_INTERVAL_S", 5, 1, 300),
            metrics_history_interval_s: v.int("METRICS_HISTORY_INTERVAL_S", 60, 10, 3600),
            track_poll_live_s: v.int("TRACK_POLL_LIVE_S", 60, 10, u32::MAX),
            track_poll_rank_s: v.int("TRACK_POLL_RANK_S", 600, 30, u32::MAX),
            track_poll_match_s: v.int("TRACK_POLL_MATCH_S", 300, 30, u32::MAX),
            ddragon_sync_s: v.int("DDRAGON_SYNC_S", 3600, 60, u32::MAX),
            archive_timelines: v.bool("ARCHIVE_TIMELINES", true),
            timeline_backfill_patches: v.int("TIMELINE_BACKFILL_PATCHES", 0, 0, 1000),
            backup_keep: v.int("BACKUP_KEEP", 14, 0, 365),
            archive_object_url: objects.url,
            archive_object_endpoint: objects.endpoint,
            archive_object_region: v.string("ARCHIVE_OBJECT_REGION", "auto"),
            archive_object_access_key: objects.access_key,
            archive_object_secret_key: objects.secret_key,
            archive_object_concurrency: v.int("ARCHIVE_OBJECT_CONCURRENCY", 16, 1, 256),
            archive_body_local_days: v.int("ARCHIVE_BODY_LOCAL_DAYS", 14, 0, 365),
            archive_pack_target_bytes: v.int("ARCHIVE_PACK_TARGET_BYTES", 64 << 20, 1 << 20, 1 << 30),
            archive_offload_pace_ms: v.int("ARCHIVE_OFFLOAD_PACE_MS", 50, 0, 60_000),
            lookup_backfill_limit: v.int("LOOKUP_BACKFILL_LIMIT", u32::MAX, 0, u32::MAX),
            track_catchup_limit: v.int("TRACK_CATCHUP_LIMIT", 500, 0, 10_000),
            ladder_crawl_s: v.int("LADDER_CRAWL_S", 0, 0, 604_800),
            ladder_queues: csv(&v.string("LADDER_QUEUES", "RANKED_SOLO_5x5"))
                .into_iter()
                .filter(|q| v.ranked_queue("LADDER_QUEUES", q))
                .collect(),
            ladder_platforms,
            ladder_tier_floor: v.tier("LADDER_TIER_FLOOR", "MASTER"),
            ladder_backfill_limit: v.int("LADDER_BACKFILL_LIMIT", 100, 0, 10_000),
            rank_lookup_limit: v.int("RANK_LOOKUP_LIMIT", 50_000, 0, 10_000_000),
            rank_lookup_recheck_s: v.int("RANK_LOOKUP_RECHECK_S", 604_800, 0, 31_536_000),
            tier_late_stamp_days: v.int("TIER_LATE_STAMP_DAYS", 14, 0, 365),
            facts_reextract_batch: v.int("FACTS_REEXTRACT_BATCH", 500, 1, 10_000),
            aggregate_patch_limit: v.int("AGGREGATE_PATCH_LIMIT", 4, 0, 100),
            analytics_cache_entries: v.int("ANALYTICS_CACHE_ENTRIES", 512, 0, 4096),
            aggregate_interval_s: v.int("AGGREGATE_INTERVAL_S", 0, 0, 604_800),
            ddragon_locale: v.string("DDRAGON_LOCALE", "en_US"),
            admin_ip_allowlist: csv(&v.string("ADMIN_IP_ALLOWLIST", "")),
            trust_proxy: v.bool("TRUST_PROXY", false),
            admin_cors_origins: v.origins("ADMIN_CORS_ORIGINS"),
            bootstrap_admin_key: v.opt_string("BOOTSTRAP_ADMIN_KEY").map(Secret::new),
            auth_disabled,
            dev_ui: dev_ui && env != Environment::Production,
            dev_ui_ignored: dev_ui && env == Environment::Production && dev_ui_set,
            docs_ui: v.bool("DOCS_UI", true),
            dashboard_ui: v.bool("DASHBOARD_UI", true),
            riot_base_url,
            ddragon_base_url,
        };

        let (idle, busy) = (config.bulk_idle_ceiling, config.bulk_usage_ceiling);
        if !bulk_idle_ceiling_set {
            // Unset, it never sits below a raised usage ceiling.
            config.bulk_idle_ceiling = idle.max(busy);
        } else if idle >= 1.0 {
            // The local windows and Riot's are never exactly in step; the gap
            // is what prevents 429s.
            v.push("BULK_IDLE_CEILING", &format!("'{idle}' must be below 1"));
        } else if idle < busy {
            v.push(
                "BULK_IDLE_CEILING",
                &format!("'{idle}' is below BULK_USAGE_CEILING ({busy})"),
            );
        }

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
    let text = std::fs::read_to_string(path).map_err(|source| ConfigError::DotEnvIo {
        path: path.to_path_buf(),
        source,
    })?;
    parse_dotenv(&text).map_err(|(line, message)| ConfigError::DotEnv {
        path: path.to_path_buf(),
        line,
        message,
    })
}

/// `.env` parsing with node `dotenv`'s rules, so v1 files port unchanged (ADR-020).
/// v1's own `.env.example` has `RIOT_USER_AGENT=riot-proxy/1.0 (+https://…)`
/// unquoted, which stricter parsers such as `dotenvy` reject.
///
/// - Blank lines and lines starting with `#` are skipped; `export KEY=…` is allowed.
/// - Unquoted values run to the end of the line, stop at ` #` (inline comment), and are trimmed.
/// - Values in `'…'`, `"…"` or `` `…` `` are taken literally, except that `"…"`
///   expands `\n`. Multi-line quoted values are not supported.
/// - Any other line, e.g. one without `=`, is an error naming its line number.
pub fn parse_dotenv(text: &str) -> Result<Vec<(String, String)>, (usize, String)> {
    let mut out = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line_no = i + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").map_or(line, str::trim_start);
        let Some((key, value)) = line.split_once('=') else {
            return Err((line_no, "expected KEY=VALUE".into()));
        };
        let key = key.trim();
        if key.is_empty()
            || !key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b'-')
        {
            return Err((line_no, format!("invalid variable name '{key}'")));
        }
        let value = value.trim();
        let parsed = match value.chars().next() {
            Some(q @ ('"' | '\'' | '`')) => {
                let inner = &value[1..];
                let Some(end) = inner.find(q) else {
                    return Err((line_no, format!("unterminated {q}-quoted value")));
                };
                let rest = inner[end + 1..].trim();
                if !(rest.is_empty() || rest.starts_with('#')) {
                    return Err((line_no, "unexpected text after the closing quote".into()));
                }
                let body = &inner[..end];
                if q == '"' {
                    body.replace("\\n", "\n")
                } else {
                    body.to_string()
                }
            }
            _ => value.split(" #").next().unwrap_or_default().trim().to_string(),
        };
        out.push((key.to_string(), parsed));
    }
    Ok(out)
}

/// Keep only variables the config reads, and drop empty values so `FOO=` in a
/// `.env` file falls back to the default rather than overriding it (v1 behaviour).
fn known(pairs: Vec<(String, String)>) -> BTreeMap<String, String> {
    pairs
        .into_iter()
        .filter(|(k, v)| {
            !v.is_empty() && (VARS.contains(&k.as_str()) || k == LEGACY_ENV_VAR || k == RETIRED_TLS_VAR)
        })
        .collect()
}

/// `s3://<bucket>` (S3 bucket naming: 3–63 of `a-z0-9.-`, starting and ending
/// with a letter or digit, no path) or `file:///<absolute dir>`.
fn parse_object_url(raw: &str) -> Result<ObjectUrl, &'static str> {
    if let Some(bucket) = raw.strip_prefix("s3://") {
        let bucket = bucket.strip_suffix('/').unwrap_or(bucket);
        let edge = |c: Option<char>| c.is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
        let ok = (3..=63).contains(&bucket.len())
            && bucket
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-')
            && edge(bucket.chars().next())
            && edge(bucket.chars().last());
        return if ok {
            Ok(ObjectUrl::S3 {
                bucket: bucket.to_string(),
            })
        } else {
            Err("is not s3://<bucket> with a valid bucket name and no path")
        };
    }
    if let Some(path) = raw.strip_prefix("file://") {
        return if path.starts_with('/') {
            Ok(ObjectUrl::File(PathBuf::from(path)))
        } else {
            Err("needs an absolute path, like file:///var/lib/riot-proxy/objects")
        };
    }
    Err("must start with s3:// or file://")
}

/// `https://host[:port]`, or `http://` outside production; no path. A
/// trailing `/` is dropped.
fn check_endpoint(raw: &str, production: bool) -> Result<String, String> {
    let trimmed = raw.strip_suffix('/').unwrap_or(raw);
    let host = match (trimmed.strip_prefix("https://"), trimmed.strip_prefix("http://")) {
        (Some(h), _) => h,
        (None, Some(_)) if production => return Err("must be https:// when ENV=production".into()),
        (None, Some(h)) => h,
        (None, None) => "",
    };
    if host.is_empty() || !host.is_ascii() || host.contains(['/', '?', '#', ' ', '@']) {
        return Err(format!("'{raw}' is not an endpoint like https://s3.example.com"));
    }
    Ok(trimmed.to_string())
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

/// The `ARCHIVE_OBJECT_*` values that are checked together.
struct ObjectVars {
    url: Option<ObjectUrl>,
    endpoint: Option<String>,
    access_key: Option<Secret>,
    secret_key: Option<Secret>,
}

impl Vars {
    fn push(&mut self, name: &str, message: &str) {
        self.errors.push(format!("{name}: {message}"));
    }

    /// `ARCHIVE_OBJECT_URL`, its endpoint and keys (OBJ-01). The keys come
    /// as a pair; production refuses a URL without them, and a `file://` one.
    fn object_store(&mut self, env: Environment) -> ObjectVars {
        const URL: &str = "ARCHIVE_OBJECT_URL";
        const ENDPOINT: &str = "ARCHIVE_OBJECT_ENDPOINT";
        let production = env == Environment::Production;
        let url = self.opt_string(URL).and_then(|raw| {
            let parsed = parse_object_url(&raw);
            if let Err(message) = &parsed {
                self.push(URL, &format!("'{raw}' {message}"));
            }
            parsed.ok()
        });
        let endpoint_raw = self.opt_string(ENDPOINT);
        let endpoint = endpoint_raw
            .clone()
            .and_then(|raw| match check_endpoint(&raw, production) {
                Ok(endpoint) => Some(endpoint),
                Err(message) => {
                    self.push(ENDPOINT, &message);
                    None
                }
            });
        let access_key = self.opt_string("ARCHIVE_OBJECT_ACCESS_KEY").map(Secret::new);
        let secret_key = self.opt_string("ARCHIVE_OBJECT_SECRET_KEY").map(Secret::new);
        if access_key.is_some() != secret_key.is_some() {
            self.errors
                .push("ARCHIVE_OBJECT_ACCESS_KEY and ARCHIVE_OBJECT_SECRET_KEY: set both or neither".into());
        }
        match &url {
            Some(ObjectUrl::S3 { .. }) => {
                if endpoint_raw.is_none() {
                    self.push(ENDPOINT, "is required with an s3:// ARCHIVE_OBJECT_URL");
                }
                if production && (access_key.is_none() || secret_key.is_none()) {
                    self.push(
                        URL,
                        "needs ARCHIVE_OBJECT_ACCESS_KEY and ARCHIVE_OBJECT_SECRET_KEY when ENV=production",
                    );
                }
            }
            Some(ObjectUrl::File(_)) if production => {
                self.push(URL, "file:// cannot be used when ENV=production");
            }
            _ => {}
        }
        ObjectVars {
            url,
            endpoint,
            access_key,
            secret_key,
        }
    }

    /// A CSV of exact browser origins: `http(s)://host[:port]`, no path, no
    /// wildcard. `*` is refused: the admin API takes a key a page on any
    /// origin could then use.
    fn origins(&mut self, name: &str) -> Vec<String> {
        let mut out = Vec::new();
        for o in csv(&self.string(name, "")) {
            let host = o.strip_prefix("https://").or_else(|| o.strip_prefix("http://"));
            if o == "*" {
                self.push(name, "'*' is refused; list each origin");
            } else if host
                .is_none_or(|h| h.is_empty() || h.contains(['/', '?', '#', '*', ' ']) || !h.is_ascii())
            {
                self.push(
                    name,
                    &format!("'{o}' is not an origin like https://admin.example.com"),
                );
            } else {
                out.push(o);
            }
        }
        out
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

    /// A platform, or an error naming the variable. v1 refused unknown platforms at boot.
    fn platform(&mut self, name: &str, raw: &str) -> Platform {
        raw.parse().unwrap_or_else(|e: crate::http::ApiError| {
            self.push(name, &e.message);
            Platform::Euw1
        })
    }

    /// A ranked ladder in Riot's casing (v1 refused anything else at boot).
    /// `kind=n,kind=n`: each kind one of [`kinds::ALL`] with no lane, `n ≥ 1`.
    /// `none` for no caps (an empty value is unset, so the default).
    fn kind_limits(&mut self, name: &str, default: &str) -> BTreeMap<String, u32> {
        use crate::jobs::{kinds, lanes};
        let raw = self.string(name, default);
        let mut limits = BTreeMap::new();
        if raw.trim().eq_ignore_ascii_case("none") {
            return limits;
        }
        for item in csv(&raw) {
            let Some((kind, n)) = item.split_once('=').map(|(k, n)| (k.trim(), n.trim())) else {
                self.push(name, &format!("'{item}' is not kind=n"));
                continue;
            };
            if !kinds::ALL.contains(&kind) {
                self.push(name, &format!("'{kind}' is not a job kind"));
            } else if lanes::LANED.contains(&kind) {
                // The claim would scan past every queued row of a capped
                // laned kind; the limiter already paces those (ADR-134).
                self.push(name, &format!("'{kind}' calls Riot; the rate limiter paces it"));
            } else {
                match n.parse::<u32>() {
                    Ok(n) if n >= 1 => {
                        limits.insert(kind.to_string(), n);
                    }
                    _ => self.push(
                        name,
                        &format!("'{n}' for {kind} is not a whole number of at least 1"),
                    ),
                }
            }
        }
        limits
    }

    fn ranked_queue(&mut self, name: &str, raw: &str) -> bool {
        let ok = crate::riot::ladder::RANKED_QUEUES.contains(&raw);
        if !ok {
            let all = crate::riot::ladder::RANKED_QUEUES.join(", ");
            self.push(name, &format!("'{raw}' is not one of {all}"));
        }
        ok
    }

    /// A tier, trimmed and upper-cased (v1 `tierSetting`).
    fn tier(&mut self, name: &str, default: &str) -> String {
        let raw = self.string(name, default);
        let tier = raw.trim().to_ascii_uppercase();
        if !crate::riot::ladder::is_tier(&tier) {
            let all = crate::riot::ladder::tiers().collect::<Vec<_>>().join(", ");
            self.push(name, &format!("'{raw}' is not one of {all}"));
            return default.to_string();
        }
        tier
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
