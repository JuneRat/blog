use super::*;
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
            .is_err()
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
