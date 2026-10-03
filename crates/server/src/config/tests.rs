use super::*;

#[test]
fn legacy_site_time_zone_defaults_to_utc_validates_iana_and_honors_env() {
    assert_eq!(config("", &[]).site(None).unwrap().site.time_zone, "UTC");
    let toml = "[server]\ntime_zone='Asia/Shanghai'";
    assert_eq!(
        config(toml, &[]).site(None).unwrap().site.time_zone,
        "Asia/Shanghai"
    );
    assert_eq!(
        config(toml, &[("BLOG_TIME_ZONE", "Europe/London")])
            .site(None)
            .unwrap()
            .site
            .time_zone,
        "Europe/London"
    );
    for bad in ["", "Asia/Unknown", "+08:00"] {
        let bad = config(toml, &[("BLOG_TIME_ZONE", bad)]);
        assert!(
            bad.check(ConfigScope::Serve)
                .unwrap_err()
                .contains("server.time_zone")
        );
        assert!(bad.check(ConfigScope::Resources).is_ok());
    }
    assert!(config("[server]\ntime_zone=8", &[]).site(None).is_err());
}

#[test]
fn database_pool_policy_is_typed_bounded_and_scoped() {
    let default = config("", &[]).database_pool().unwrap();
    assert_eq!(
        default.max_connections,
        infrastructure::DatabasePoolConfig::default().max_connections
    );
    let chosen = config(
        "[database]\nmax_connections=8\nmin_connections=2\nstatement_timeout_ms=30000",
        &[
            ("BLOG_DB_MAX_CONNECTIONS", "12"),
            ("BLOG_DB_ACQUIRE_TIMEOUT_MS", "800"),
        ],
    )
    .database_pool()
    .unwrap();
    assert_eq!(
        (
            chosen.max_connections,
            chosen.min_connections,
            chosen.acquire_timeout_ms,
            chosen.statement_timeout_ms
        ),
        (12, 2, 800, 30000)
    );
    for (key, value) in [
        ("MAX_CONNECTIONS", "0"),
        ("MAX_CONNECTIONS", "1001"),
        ("MAX_CONNECTIONS", "4294967296"),
        ("MIN_CONNECTIONS", "6"),
        ("ACQUIRE_TIMEOUT_MS", "0"),
        ("ACQUIRE_TIMEOUT_MS", "invalid"),
        ("IDLE_TIMEOUT_SECS", "-1"),
        ("MAX_LIFETIME_SECS", "86401"),
        ("STATEMENT_TIMEOUT_MS", "86400001"),
        ("LOCK_TIMEOUT_MS", "1.5"),
        ("IDLE_IN_TRANSACTION_TIMEOUT_MS", "-1"),
        ("CONNECT_RETRIES", "11"),
        ("CONNECT_RETRY_BACKOFF_MS", "0"),
    ] {
        let key = format!("BLOG_DB_{key}");
        assert!(
            config("", &[(&key, value)]).database_pool().is_err(),
            "{key}"
        );
    }
    let invalid = config("[database]\nmax_connections='bad'", &[]);
    assert!(invalid.check(ConfigScope::Serve).is_err());
    assert!(invalid.check(ConfigScope::Resources).is_ok());
    let valid = config(
        "[database]\nmax_connections='bad'",
        &[("BLOG_DB_MAX_CONNECTIONS", "7")],
    );
    assert_eq!(valid.database_pool().unwrap().max_connections, 7);
    let maintenance = config(
        "[database]\nurl=false\nmax_connections=3",
        &[(
            "BLOG_MAINTENANCE_DATABASE_URL",
            "postgres://m:p@localhost/blog",
        )],
    );
    let shown = maintenance.show(ConfigScope::Maintenance, false).unwrap();
    assert!(
        shown["fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["key"] == "database.max_connections" && f["value"] == 3)
    );
    assert!(
        !shown["fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["key"] == "database.url")
    );
}

#[test]
fn observability_configuration_is_validated_and_scoped() {
    assert!(!config("", &[]).log_json().unwrap());
    assert!(config("", &[]).metrics_bind().unwrap().is_none());
    assert!(config("[logging]\nformat='json'", &[]).log_json().unwrap());
    assert!(
        config("", &[("BLOG_LOG_FORMAT", "yaml")])
            .log_json()
            .is_err()
    );
    let deployment = config("[metrics]\nbind='broken'", &[]);
    assert!(deployment.check(ConfigScope::Serve).is_err());
    assert!(deployment.check(ConfigScope::Resources).is_ok());
    assert_eq!(
        config(
            "[metrics]\nbind='broken'",
            &[("BLOG_METRICS_BIND", "127.0.0.1:0")]
        )
        .metrics_bind()
        .unwrap()
        .unwrap()
        .port(),
        0
    );
}

#[test]
fn http_database_deadlines_are_bounded_without_changing_cli_or_explicit_policy() {
    let defaults = config("", &[]);
    let http = defaults.http_database_pool().unwrap();
    assert_eq!(
        (
            http.statement_timeout_ms,
            http.lock_timeout_ms,
            http.idle_in_transaction_timeout_ms
        ),
        (20_000, 3_000, 60_000)
    );
    assert_eq!(defaults.database_pool().unwrap().statement_timeout_ms, 0);
    let short = config("[server]\nrequest_timeout_secs=1", &[])
        .http_database_pool()
        .unwrap();
    assert!(short.statement_timeout_ms < 1000);
    assert!(short.lock_timeout_ms < short.statement_timeout_ms);
    let delegated = config(
        "[database]\nstatement_timeout_ms=0\nlock_timeout_ms=0\nidle_in_transaction_timeout_ms=0",
        &[],
    )
    .http_database_pool()
    .unwrap();
    assert_eq!(
        (
            delegated.statement_timeout_ms,
            delegated.lock_timeout_ms,
            delegated.idle_in_transaction_timeout_ms
        ),
        (0, 0, 0)
    );
    for invalid in [
        "statement_timeout_ms=30000",
        "statement_timeout_ms=10000\nlock_timeout_ms=10000",
    ] {
        assert!(
            config(&format!("[database]\n{invalid}"), &[])
                .check(ConfigScope::Serve)
                .is_err()
        );
        assert!(
            config(&format!("[database]\n{invalid}"), &[])
                .database_pool()
                .is_ok()
        );
    }
    let shown = defaults.show(ConfigScope::Serve, true).unwrap();
    let statement = shown["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|field| field["key"] == "database.statement_timeout_ms")
        .unwrap();
    assert_eq!(statement["value"], 20_000);
    assert_eq!(statement["source"], "default:serve");
}
use std::path::Path;

fn config(source: &str, env: &[(&str, &str)]) -> DeploymentConfig {
    DeploymentConfig::parse(
        PathBuf::from("config.toml"),
        Some(source.into()),
        env.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    )
    .unwrap()
}

fn dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("blog-config-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn precedence_is_cli_then_environment_then_toml_then_defaults() {
    let config = config(
        "[server]\nbind='127.0.0.1:7000'\npublic_base_url='https://example.com'\ntrusted_proxies=['::1']\n",
        &[
            ("BLOG_BIND", "127.0.0.1:8000"),
            ("BLOG_TRUSTED_PROXIES", "127.0.0.1, ::1"),
        ],
    );
    assert_eq!(config.site(None).unwrap().bind, "127.0.0.1:8000");
    let site = config.site(Some("127.0.0.1:9000".into())).unwrap();
    assert_eq!(site.bind, "127.0.0.1:9000");
    assert!(site.secure_cookies);
    assert_eq!(site.trusted_proxies.len(), 2);
    assert_eq!(site.media_dir, Path::new("data/media"));
    assert_eq!(site.site.title, SiteInfo::default().title);
}

#[test]
fn maintenance_reuses_the_effective_site_connection_unless_overridden() {
    let site = "postgres://site:site-secret@localhost/blog";
    let file = format!("[database]\nurl='{site}'");
    let from_file = config(&file, &[]);
    assert_eq!(from_file.maintenance_url().unwrap(), site);
    let shown = from_file.show(ConfigScope::Maintenance, true).unwrap();
    let connection = shown["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["key"] == "maintenance.database_url")
        .unwrap();
    assert_eq!(connection["value"], "[redacted]");
    assert!(
        connection["source"]
            .as_str()
            .unwrap()
            .starts_with("fallback:toml:")
    );
    assert!(!shown.to_string().contains("site-secret"));

    let env_site = "postgres://env:p@localhost/blog";
    assert_eq!(
        config(&file, &[("DATABASE_URL", env_site)])
            .maintenance_url()
            .unwrap(),
        env_site
    );
    let dedicated = "postgres://maintenance:p@localhost/blog";
    let separate = format!("{file}\n[maintenance]\ndatabase_url='{dedicated}'");
    assert_eq!(
        config(&separate, &[("DATABASE_URL", env_site)])
            .maintenance_url()
            .unwrap(),
        dedicated
    );
    assert_eq!(
        config(&separate, &[("BLOG_MAINTENANCE_DATABASE_URL", env_site)])
            .maintenance_url()
            .unwrap(),
        env_site
    );
    // An explicit but invalid override must never silently gain the site's privileges.
    for bad in ["", "not-a-database-url"] {
        assert!(
            config(&file, &[("BLOG_MAINTENANCE_DATABASE_URL", bad)])
                .maintenance_url()
                .is_err()
        );
    }
    assert!(
        config(
            "[maintenance]\ndatabase_url=false",
            &[("DATABASE_URL", site)]
        )
        .maintenance_url()
        .is_err()
    );
    assert!(config("", &[]).maintenance_url().is_err());
}

#[test]
fn bad_website_values_do_not_block_database_or_maintenance_commands() {
    let config = config(
        "[server]\npublic_base_url=42\nsecure_cookies='typo'\ntrusted_proxies=false\n[paths]\ntheme_dir=false\n",
        &[
            ("DATABASE_URL", "postgres://app:secret@localhost/blog"),
            (
                "BLOG_MAINTENANCE_DATABASE_URL",
                "postgres://m:secret@localhost/blog",
            ),
        ],
    );
    assert!(config.check(ConfigScope::Database).is_ok());
    assert!(config.check(ConfigScope::Maintenance).is_ok());
    assert!(config.check(ConfigScope::Serve).is_err());
    let config = super::tests::config(
        "[database]\nurl=false",
        &[(
            "BLOG_MAINTENANCE_DATABASE_URL",
            "postgres://m:secret@localhost/blog",
        )],
    );
    assert!(config.check(ConfigScope::Maintenance).is_ok());
    assert!(config.check(ConfigScope::Database).is_err());
}

#[test]
fn booleans_are_strict_and_environment_can_repair_bad_file_values() {
    for value in ["", "yes", "treu", "2"] {
        assert!(
            config("", &[("BLOG_SECURE_COOKIES", value)])
                .site(None)
                .is_err()
        );
        assert!(
            config("", &[("BLOG_RECOVERY_MODE", value)])
                .recovery_mode()
                .is_err()
        );
    }
    assert!(
        config(
            "[server]\nsecure_cookies='bad'",
            &[("BLOG_SECURE_COOKIES", "TRUE")]
        )
        .site(None)
        .unwrap()
        .secure_cookies
    );
    assert!(
        config("[server]\nbind=false", &[])
            .site(Some("127.0.0.1:0".into()))
            .is_ok()
    );
    assert!(config("", &[("DATABASE_URL", "")]).database().is_err());
    assert!(
        config("", &[("DATABASE_URL", "postgres://x:y@localhost/blog")])
            .maintenance_url()
            .is_ok()
    );
}

#[test]
fn unknown_fields_and_versions_fail_without_echoing_secret_source_lines() {
    for source in [
        "config_version=2",
        "[server]\nbnid='x'",
        "[unknown]\nx=1",
        "[database]\nurl='private-password",
    ] {
        let error = DeploymentConfig::parse(PathBuf::new(), Some(source.into()), BTreeMap::new())
            .err()
            .unwrap();
        assert!(!error.contains("private-password"));
    }
}

#[test]
fn show_redacts_connections_and_identifies_sources_and_effects() {
    let config = config(
        "[bootstrap]\ntitle='Initial title'",
        &[("DATABASE_URL", "postgres://u:secret@localhost/blog")],
    );
    let shown = config.show(ConfigScope::Serve, true).unwrap();
    assert!(!shown.to_string().contains("secret"));
    let fields = shown["fields"].as_array().unwrap();
    assert!(fields.iter().any(|f| f["key"] == "database.url"
        && f["value"] == "[redacted]"
        && f["source"] == "env:DATABASE_URL"));
    assert!(
        fields
            .iter()
            .any(|f| f["key"] == "bootstrap.title" && f["effect"] == "installation-only")
    );
    assert!(
        !config
            .show(ConfigScope::Serve, false)
            .unwrap()
            .to_string()
            .contains("\"source\"")
    );
}

#[test]
fn bootstrap_is_validated_and_is_not_a_runtime_fallback() {
    let config = config("[bootstrap]\ntitle='Bootstrap'\ndescription=''", &[]);
    assert_eq!(
        config.site(None).unwrap().site.title,
        SiteInfo::default().title
    );
    assert_eq!(
        config.bootstrap_site().unwrap().title.as_deref(),
        Some("Bootstrap")
    );
    assert_eq!(
        config.bootstrap_site().unwrap().description.as_deref(),
        Some("")
    );
    let mut overlong = config.clone();
    overlong
        .values
        .get_mut("bootstrap")
        .unwrap()
        .as_table_mut()
        .unwrap()
        .insert("description".into(), "字".repeat(501).into());
    assert!(overlong.bootstrap_site().is_err());
}

#[test]
fn publication_is_private_durable_and_preserves_existing_configuration() {
    let dir = dir();
    let path = dir.join("config.toml");
    let original = "[paths]\nmedia_dir='custom/media'\n[bootstrap]\ntitle='My blog'\n";
    files::publish_new(&path, original.as_bytes()).unwrap();
    let config =
        DeploymentConfig::parse(path.clone(), Some(original.into()), BTreeMap::new()).unwrap();
    let journal = InstallJournal::prepare(
        &config,
        "postgres://u:secret@localhost/blog",
        "https://example.com",
        "a".repeat(64),
    )
    .unwrap();
    journal.publish(&config).unwrap();
    let loaded = config.reload().unwrap();
    assert_eq!(loaded.media_dir().unwrap(), Path::new("custom/media"));
    assert_eq!(
        loaded.configured_database_url().unwrap().as_deref(),
        Some("postgres://u:secret@localhost/blog")
    );
    assert_eq!(
        InstallJournal::read(&path)
            .unwrap()
            .unwrap()
            .installation_id,
        "a".repeat(64)
    );
    assert!(journal.publish(&config).is_err());
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for file in [&path, &InstallJournal::path(&path)] {
            assert_eq!(
                std::fs::metadata(file).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn journal_only_crash_recovers_but_later_operator_edits_are_preserved() {
    let dir = dir();
    let path = dir.join("config.toml");
    let config = DeploymentConfig::parse(path.clone(), None, BTreeMap::new()).unwrap();
    let journal = InstallJournal::prepare(
        &config,
        "postgres://u:secret@localhost/blog",
        "https://example.com",
        "b".repeat(64),
    )
    .unwrap();
    files::publish_new(
        &InstallJournal::path(&path),
        &serde_json::to_vec(&journal).unwrap(),
    )
    .unwrap();
    assert!(!path.exists());
    let recovered = journal.recover_config(&config).unwrap();
    assert!(recovered.configured_database_url().unwrap().is_some());
    let changed = "[database]\nurl='postgres://new:secret@localhost/restored'\n";
    std::fs::write(&path, changed).unwrap();
    let edited = recovered.reload().unwrap();
    assert_eq!(
        journal.recover_config(&edited).unwrap().original.as_deref(),
        Some(changed)
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(unix)]
#[test]
fn symlinks_permissions_and_concurrent_file_creation_fail_closed() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let dir = dir();
    let path = dir.join("config.toml");
    let config = DeploymentConfig::parse(path.clone(), None, BTreeMap::new()).unwrap();
    let journal = InstallJournal::prepare(
        &config,
        "postgres://u:secret@localhost/blog",
        "https://example.com",
        "c".repeat(64),
    )
    .unwrap();
    symlink(dir.join("missing"), &path).unwrap();
    assert!(files::read_private(&path).is_err());
    assert!(journal.publish(&config).is_err());
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, "config_version=1").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(files::read_private(&path).err().unwrap().contains("600"));
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(journal.publish(&config).is_err());
    assert!(!InstallJournal::path(&path).exists());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "config_version=1");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn completed_journal_cleanup_preserves_configuration_and_rejects_changed_records() {
    let dir = dir();
    let path = dir.join("config.toml");
    let config = DeploymentConfig::parse(path.clone(), None, BTreeMap::new()).unwrap();
    let journal = InstallJournal::prepare(
        &config,
        "postgres://u:secret@localhost/blog",
        "https://example.com",
        "a".repeat(64),
    )
    .unwrap();
    journal.publish(&config).unwrap();
    let journal_path = InstallJournal::path(&path);
    let mut replacement = journal.clone();
    replacement.installation_id = "b".repeat(64);
    let replacement_text = serde_json::to_string(&replacement).unwrap();
    std::fs::write(&journal_path, &replacement_text).unwrap();
    assert!(journal.remove_completed(&config).is_err());
    assert_eq!(
        std::fs::read_to_string(&journal_path).unwrap(),
        replacement_text
    );

    std::fs::write(&journal_path, serde_json::to_string(&journal).unwrap()).unwrap();
    std::fs::remove_file(&path).unwrap();
    assert!(journal.remove_completed(&config).is_err());
    assert!(journal_path.exists());
    let edited = "# Operator change\n[database]\nurl='postgres://new:secret@localhost/restored'\n";
    files::publish_new(&path, edited.as_bytes()).unwrap();
    journal.remove_completed(&config).unwrap();
    journal.remove_completed(&config).unwrap();
    assert!(!journal_path.exists());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), edited);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn http_limits_validate_ranges_relationships_and_environment_overrides() {
    let defaults = config("", &[]).http_limits().unwrap();
    assert_eq!(defaults.shutdown.as_secs(), 25);
    assert_eq!(defaults.requests.upload.as_secs(), 120);
    let chosen = config(
        "[server]\nrequest_timeout_secs=45",
        &[("BLOG_UPLOAD_TIMEOUT_SECS", "150")],
    )
    .http_limits()
    .unwrap();
    assert_eq!(chosen.requests.request.as_secs(), 45);
    assert_eq!(chosen.requests.upload.as_secs(), 150);
    for setting in [
        "request_timeout_secs=0",
        "shutdown_timeout_secs=-1",
        "header_timeout_secs=3601",
        "max_http_connections=0",
        "request_timeout_secs=150",
        "connection_max_age_secs=120",
    ] {
        assert!(
            config(&format!("[server]\n{setting}"), &[])
                .http_limits()
                .is_err(),
            "{setting}"
        );
    }
}

#[test]
fn https_public_origin_cannot_disable_secure_cookies() {
    let https = "[server]\npublic_base_url='https://blog.example.com'";
    assert!(config(https, &[]).site(None).unwrap().secure_cookies);
    for (source, env) in [
        (format!("{https}\nsecure_cookies=false"), vec![]),
        (https.into(), vec![("BLOG_SECURE_COOKIES", "false")]),
    ] {
        let deployment = config(&source, &env);
        assert!(
            deployment
                .site(None)
                .err()
                .unwrap()
                .contains("Secure Cookie")
        );
        assert!(deployment.check(ConfigScope::Resources).is_ok());
    }
    // HTTP 开发仍可用；环境变量覆盖错误的文件配置。
    assert!(!config("", &[]).site(None).unwrap().secure_cookies);
    assert!(
        config(
            &format!("{https}\nsecure_cookies=false"),
            &[("BLOG_SECURE_COOKIES", "true")]
        )
        .site(None)
        .unwrap()
        .secure_cookies
    );
}

#[test]
fn smtp_config_is_optional_scoped_and_redacted() {
    assert!(config("", &[]).site(None).unwrap().mail.is_none());
    let source = "[mail]\nhost='smtp.example.com'\nfrom='Blog <noreply@example.com>'\nusername='smtp-user'\npassword='smtp-secret'";
    let cfg = config(
        source,
        &[("BLOG_SMTP_PORT", "465"), ("BLOG_SMTP_SECURITY", "tls")],
    );
    let mail = cfg.site(None).unwrap().mail.unwrap();
    assert_eq!(mail.port, 465);
    assert_eq!(mail.security, "tls");
    assert!(mail.ca_pem.is_none());
    let shown = cfg.show(ConfigScope::Serve, true).unwrap().to_string();
    assert!(!shown.contains("smtp-secret"));
    assert!(!shown.contains("smtp-user"));
    for bad in [
        "[mail]\nhost='smtp.test'",
        "[mail]\npassword='secret'",
        "[mail]\nca_pem='invalid'",
        "[mail]\nhost='smtp.test'\nfrom='a@example.com'\nca_pem='invalid'",
        "[mail]\nhost='smtp.test'\nfrom='a@example.com'\nport=0",
        "[mail]\nhost='smtp.test'\nfrom='a@example.com'\nsecurity='local'",
    ] {
        assert!(config(bad, &[]).site(None).is_err());
        assert!(config(bad, &[]).check(ConfigScope::Resources).is_ok());
    }
    assert!(
        config(source, &[("BLOG_SMTP_CA_PEM", "invalid")])
            .mail()
            .is_err()
    );
}
