use super::*;

const KEY: &str = "RGAPI-test-key-not-real";

fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn env(list: &[(&str, &str)]) -> Sources {
    let mut all = vec![("RIOT_API_KEY", KEY)];
    all.extend_from_slice(list);
    Sources {
        env: pairs(&all),
        ..Sources::default()
    }
}

fn load(sources: Sources) -> Config {
    Config::from_sources(sources).expect("config should load")
}

fn errors(sources: Sources) -> Vec<String> {
    match Config::from_sources(sources) {
        Err(ConfigError::Invalid(errors)) => errors,
        other => panic!("expected Invalid, got {other:?}"),
    }
}

#[test]
fn defaults_match_v1_and_design_07() {
    let c = load(env(&[]));
    assert_eq!(c.env, Environment::Development);
    assert_eq!(c.host, "0.0.0.0");
    assert_eq!(c.port, 8080);
    assert_eq!(c.log_level, "info");
    assert_eq!(c.log_format, LogFormat::Json);
    assert_eq!(c.data_dir, PathBuf::from("./data"));
    assert_eq!(
        c.database,
        Database::Sqlite(PathBuf::from("./data/riot-proxy.db"))
    );
    assert_eq!(c.ddragon_dir, PathBuf::from("./data/ddragon"));
    assert_eq!(c.role, Role::All);
    assert_eq!(c.job_concurrency, 8);
    assert!(!c.tls);
    assert_eq!(c.riot_user_agent, DEFAULT_USER_AGENT);
    assert_eq!(c.default_platform, Platform::Euw1);
    assert_eq!(c.neg_ttl_seconds, 30);
    assert_eq!(c.neg_ttl_account_seconds, 300);
    assert_eq!(c.client_wait_budget_ms, 2000);
    assert_eq!(c.bulk_usage_ceiling, 0.8);
    assert!(c.stale_while_revalidate);
    assert_eq!(c.metrics_interval_s, 5);
    assert_eq!(c.metrics_history_interval_s, 60);
    assert_eq!(c.track_poll_live_s, 60);
    assert_eq!(c.track_poll_rank_s, 600);
    assert_eq!(c.track_poll_match_s, 300);
    assert_eq!(c.ddragon_sync_s, 3600);
    assert!(!c.archive_timelines);
    assert_eq!(c.lookup_backfill_limit, 10_000);
    assert_eq!(c.track_catchup_limit, 500);
    assert_eq!(c.ladder_crawl_s, 0);
    assert_eq!(c.ladder_queues, vec!["RANKED_SOLO_5x5"]);
    assert_eq!(c.ladder_platforms, vec![Platform::Euw1]);
    assert_eq!(c.ladder_tier_floor, "MASTER");
    assert_eq!(c.ladder_backfill_limit, 100);
    assert_eq!(c.facts_reextract_batch, 500);
    assert_eq!(c.aggregate_min_games, 10);
    assert_eq!(c.aggregate_patch_limit, 4);
    assert_eq!(c.aggregate_interval_s, 0);
    assert_eq!(c.ddragon_locale, "en_US");
    assert!(c.admin_ip_allowlist.is_empty());
    assert!(c.bootstrap_admin_key.is_none());
    assert!(!c.auth_disabled);
    assert!(c.dev_ui);
    assert!(c.docs_ui);
    assert!(c.dashboard_ui);
}

#[test]
fn precedence_is_flag_over_env_over_dotenv() {
    let dotenv = pairs(&[("PORT", "1001"), ("HOST", "dotenv-host"), ("LOG_LEVEL", "debug")]);
    let env_vars = pairs(&[("RIOT_API_KEY", KEY), ("PORT", "1002"), ("HOST", "env-host")]);
    let args = ConfigArgs {
        port: Some(1003),
        ..ConfigArgs::default()
    };

    let c = load(Sources {
        dotenv: dotenv.clone(),
        env: env_vars.clone(),
        args,
        stdout_is_tty: false,
    });
    assert_eq!(c.port, 1003, "flag beats env and .env");
    assert_eq!(c.host, "env-host", "env beats .env");
    assert_eq!(c.log_level, "debug", ".env beats default");

    let c = load(Sources {
        dotenv: dotenv.clone(),
        env: env_vars,
        ..Sources::default()
    });
    assert_eq!(c.port, 1002);

    let c = load(Sources {
        dotenv,
        env: pairs(&[("RIOT_API_KEY", KEY)]),
        ..Sources::default()
    });
    assert_eq!(c.port, 1001);
}

#[test]
fn every_flag_overrides_its_variable() {
    let args = ConfigArgs {
        host: Some("127.0.0.1".into()),
        port: Some(9000),
        data_dir: Some("/srv/rp".into()),
        database_url: Some("sqlite:///srv/other.db".into()),
        environment: Some("test".into()),
        role: Some("all".into()),
        log_level: Some("warn".into()),
        log_format: Some("pretty".into()),
        tls: true,
        domain: Some("api.example.test".into()),
        acme_email: Some("ops@example.test".into()),
    };
    let c = load(Sources {
        args,
        ..env(&[("TLS", "false"), ("ENV", "production")])
    });
    assert_eq!(c.host, "127.0.0.1");
    assert_eq!(c.port, 9000);
    assert_eq!(c.data_dir, PathBuf::from("/srv/rp"));
    assert_eq!(c.database, Database::Sqlite(PathBuf::from("/srv/other.db")));
    assert_eq!(c.env, Environment::Test);
    assert_eq!(c.log_level, "warn");
    assert_eq!(c.log_format, LogFormat::Pretty);
    assert!(c.tls);
    assert_eq!(c.tls_domain.as_deref(), Some("api.example.test"));
    assert_eq!(c.acme_email.as_deref(), Some("ops@example.test"));
}

#[test]
fn production_refuses_auth_disabled() {
    let errs = errors(env(&[("ENV", "production"), ("AUTH_DISABLED", "true")]));
    assert_eq!(errs, vec!["AUTH_DISABLED cannot be enabled when ENV=production"]);

    let message = Config::from_sources(env(&[("ENV", "production"), ("AUTH_DISABLED", "true")]))
        .expect_err("must refuse")
        .to_string();
    assert!(
        message.starts_with("Invalid environment configuration:"),
        "{message}"
    );

    // Allowed outside production.
    assert!(load(env(&[("AUTH_DISABLED", "true")])).auth_disabled);
}

#[test]
fn production_refusal_also_applies_via_the_flag() {
    let args = ConfigArgs {
        environment: Some("production".into()),
        ..ConfigArgs::default()
    };
    let errs = errors(Sources {
        args,
        ..env(&[("AUTH_DISABLED", "1")])
    });
    assert_eq!(errs.len(), 1);
}

#[test]
fn legacy_node_env_is_honoured_when_env_is_unset() {
    let errs = errors(env(&[("NODE_ENV", "production"), ("AUTH_DISABLED", "true")]));
    assert_eq!(errs, vec!["AUTH_DISABLED cannot be enabled when ENV=production"]);

    // ENV wins when both are present.
    assert_eq!(
        load(env(&[("NODE_ENV", "production"), ("ENV", "test")])).env,
        Environment::Test
    );
}

#[test]
fn dev_ui_follows_env_unless_set() {
    assert!(!load(env(&[("ENV", "production")])).dev_ui);
    assert!(load(env(&[("ENV", "production"), ("DEV_UI", "true")])).dev_ui);
    assert!(!load(env(&[("DEV_UI", "false")])).dev_ui);
}

#[test]
fn missing_or_short_riot_key_is_rejected() {
    let errs = errors(Sources::default());
    assert_eq!(errs, vec!["RIOT_API_KEY: is required"]);

    let errs = errors(Sources {
        env: pairs(&[("RIOT_API_KEY", "short")]),
        ..Sources::default()
    });
    assert_eq!(errs, vec!["RIOT_API_KEY: must be at least 8 characters"]);
}

#[test]
fn numeric_looking_strings_stay_strings() {
    let c = load(Sources {
        env: pairs(&[("RIOT_API_KEY", "12345678"), ("HOST", "1")]),
        ..Sources::default()
    });
    assert_eq!(c.riot_api_key.expose(), "12345678");
    assert_eq!(c.host, "1");
}

#[test]
fn coerces_numbers_and_booleans() {
    let c = load(env(&[
        ("PORT", "9090"),
        ("STALE_WHILE_REVALIDATE", "false"),
        ("BULK_USAGE_CEILING", "0.5"),
        ("ARCHIVE_TIMELINES", "1"),
        ("DOCS_UI", "FALSE"),
    ]));
    assert_eq!(c.port, 9090);
    assert!(!c.stale_while_revalidate);
    assert_eq!(c.bulk_usage_ceiling, 0.5);
    assert!(c.archive_timelines);
    assert!(!c.docs_ui);
}

#[test]
fn empty_values_fall_back_to_defaults() {
    let c = load(Sources {
        dotenv: pairs(&[("PORT", "7000")]),
        env: pairs(&[("RIOT_API_KEY", KEY), ("PORT", ""), ("HOST", "")]),
        ..Sources::default()
    });
    // An empty env value is absent, so the .env value underneath it applies.
    assert_eq!(c.port, 7000);
    assert_eq!(c.host, "0.0.0.0");
}

#[test]
fn every_invalid_value_is_reported_at_once() {
    let errs = errors(env(&[
        ("PORT", "0"),
        ("BULK_USAGE_CEILING", "2"),
        ("METRICS_INTERVAL_S", "abc"),
        ("TRACK_POLL_LIVE_S", "5"),
        ("STALE_WHILE_REVALIDATE", "maybe"),
        ("ENV", "staging"),
        ("LOG_FORMAT", "xml"),
        ("ROLE", "boss"),
        ("LOOKUP_BACKFILL_LIMIT", "10001"),
        ("BULK_USAGE_CEILING_TYPO", "nothing"),
    ]));
    let names: Vec<&str> = errs
        .iter()
        .map(|e| e.split(':').next().unwrap_or_default())
        .collect();
    assert_eq!(
        names,
        vec![
            "ENV",
            "ROLE",
            "LOG_FORMAT",
            "PORT",
            "BULK_USAGE_CEILING",
            "STALE_WHILE_REVALIDATE",
            "METRICS_INTERVAL_S",
            "TRACK_POLL_LIVE_S",
            "LOOKUP_BACKFILL_LIMIT",
        ],
        "{errs:#?}"
    );
}

#[test]
fn database_url_and_role_rules() {
    let c = load(env(&[("DATA_DIR", "/var/lib/rp")]));
    assert_eq!(
        c.database,
        Database::Sqlite(PathBuf::from("/var/lib/rp/riot-proxy.db"))
    );
    assert_eq!(c.ddragon_dir, PathBuf::from("/var/lib/rp/ddragon"));

    let c = load(env(&[("DDRAGON_DIR", "/mnt/dd")]));
    assert_eq!(c.ddragon_dir, PathBuf::from("/mnt/dd"));

    let errs = errors(env(&[("DATABASE_URL", "postgres://u:p@h/db")]));
    assert!(errs[0].starts_with("DATABASE_URL: postgres:// needs"), "{errs:?}");

    let errs = errors(env(&[("DATABASE_URL", "mysql://x")]));
    assert!(errs[0].starts_with("DATABASE_URL: must start with"), "{errs:?}");

    let errs = errors(env(&[("ROLE", "worker")]));
    assert!(errs[0].starts_with("ROLE: api and worker need"), "{errs:?}");
}

#[test]
fn log_format_defaults_to_pretty_on_a_tty() {
    assert_eq!(
        load(Sources {
            stdout_is_tty: true,
            ..env(&[])
        })
        .log_format,
        LogFormat::Pretty
    );
    let c = load(Sources {
        stdout_is_tty: true,
        ..env(&[("LOG_FORMAT", "json")])
    });
    assert_eq!(c.log_format, LogFormat::Json);
}

#[test]
fn lists_are_split_and_trimmed() {
    let c = load(env(&[
        ("ADMIN_IP_ALLOWLIST", " 127.0.0.1 , ::1,,10.0.0.0/8 "),
        ("LADDER_PLATFORMS", "EUW1, na1"),
        ("LADDER_QUEUES", "RANKED_SOLO_5x5,RANKED_FLEX_SR"),
        ("LADDER_TIER_floor", "ignored: wrong case"),
        ("LADDER_TIER_FLOOR", "diamond"),
    ]));
    assert_eq!(c.admin_ip_allowlist, vec!["127.0.0.1", "::1", "10.0.0.0/8"]);
    assert_eq!(c.ladder_platforms, vec![Platform::Euw1, Platform::Na1]);
    assert_eq!(c.ladder_queues, vec!["RANKED_SOLO_5x5", "RANKED_FLEX_SR"]);
    assert_eq!(c.ladder_tier_floor, "DIAMOND");

    let c = load(env(&[("DEFAULT_PLATFORM", "kr")]));
    assert_eq!(
        c.ladder_platforms,
        vec![Platform::Kr],
        "empty LADDER_PLATFORMS means DEFAULT_PLATFORM"
    );
}

#[test]
fn secrets_never_appear_in_debug_output() {
    let c = load(env(&[("BOOTSTRAP_ADMIN_KEY", "rpx_bootstrapsecret")]));
    let debug = format!("{c:?}");
    assert!(!debug.contains(KEY), "{debug}");
    assert!(!debug.contains("rpx_bootstrapsecret"), "{debug}");
    assert!(debug.contains("[REDACTED]"));
    assert_eq!(c.riot_api_key.expose(), KEY);
}

#[test]
fn load_with_reads_dotenv_file_and_creates_data_dir() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data_dir = tmp.path().join("nested/data");
    let dotenv = tmp.path().join(".env");
    std::fs::write(
        &dotenv,
        format!(
            "# comment\nRIOT_API_KEY={KEY}\nPORT=7777\nHOST=from-dotenv\nDATA_DIR={}\n",
            data_dir.display()
        ),
    )
    .expect("write .env");

    let env_vars = pairs(&[("HOST", "from-env"), ("PATH", "/usr/bin")]);
    let args = ConfigArgs {
        port: Some(7778),
        ..ConfigArgs::default()
    };
    assert!(!data_dir.exists());

    let c = Config::load_with(Some(&dotenv), env_vars, args).expect("load");
    assert_eq!(c.port, 7778);
    assert_eq!(c.host, "from-env");
    assert!(data_dir.is_dir(), "DATA_DIR is created when missing");

    // Idempotent on an existing directory; a missing .env file is fine.
    let c = Config::load_with(
        Some(&tmp.path().join("absent.env")),
        pairs(&[
            ("RIOT_API_KEY", KEY),
            ("DATA_DIR", &data_dir.display().to_string()),
        ]),
        ConfigArgs::default(),
    )
    .expect("load");
    assert_eq!(c.port, 8080);
}

#[test]
fn malformed_dotenv_is_an_error() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dotenv = tmp.path().join(".env");
    std::fs::write(&dotenv, "NOT A VALID LINE\n").expect("write .env");
    let err = Config::load_with(Some(&dotenv), pairs(&[]), ConfigArgs::default()).expect_err("must fail");
    assert!(matches!(err, ConfigError::DotEnv { .. }), "{err:?}");
}

#[test]
fn env_example_documents_every_variable() {
    let example = include_str!("../../.env.example");
    let documented: Vec<&str> = example
        .lines()
        .map(|l| l.trim_start_matches('#').trim_start())
        .filter_map(|l| l.split_once('=').map(|(k, _)| k))
        .filter(|k| !k.is_empty() && k.chars().all(|c| c.is_ascii_uppercase() || c == '_'))
        .collect();
    for var in VARS {
        assert!(documented.contains(var), "{var} missing from .env.example");
    }
    for var in &documented {
        assert!(VARS.contains(var), "{var} in .env.example but not read by config");
    }
}

#[test]
fn unknown_platforms_are_refused_at_boot() {
    let errs = errors(env(&[
        ("DEFAULT_PLATFORM", "euw"),
        ("LADDER_PLATFORMS", "na1,xx"),
    ]));
    assert_eq!(errs.len(), 2, "{errs:?}");
    assert!(
        errs[0].starts_with("DEFAULT_PLATFORM: Unknown platform 'euw'"),
        "{errs:?}"
    );
    assert!(
        errs[1].starts_with("LADDER_PLATFORMS: Unknown platform 'xx'"),
        "{errs:?}"
    );
    assert_eq!(
        load(env(&[("DEFAULT_PLATFORM", "KR")])).default_platform,
        Platform::Kr
    );
}
