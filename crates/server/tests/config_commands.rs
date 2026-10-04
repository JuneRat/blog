//! Exercise read-only configuration diagnostics through the real CLI.
mod common;
use serde_json::Value;
use std::{
    path::Path,
    process::{Command, Output},
};

fn write_private(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}

fn command(dir: &Path, args: &[&str]) -> Command {
    let project = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut command = Command::new(env!("CARGO_BIN_EXE_blog"));
    command
        .env_clear()
        .current_dir(&project)
        .arg("--config")
        .arg(dir.join("config.toml"))
        .args(args)
        .env("BLOG_MIGRATIONS_DIR", project.join("migrations/postgres"))
        .env("RUST_LOG", "warn");
    command
}

fn success(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn json_logging_keeps_cli_stdout_machine_readable() {
    let dir = common::media_dir("config-json-logging");
    // Resource diagnostics must not become dependent on serve-only timezone validation.
    write_private(
        &dir.join("config.toml"),
        "[server]\ntime_zone='Unknown/Zone'\n",
    );
    let output = command(&dir, &["config", "show", "--for", "resources"])
        .env("BLOG_LOG_FORMAT", "json")
        .output()
        .unwrap();
    let value: Value = serde_json::from_str(&success(output)).unwrap();
    assert!(value["fields"].is_array());
}

#[test]
fn check_and_show_are_read_only_scoped_and_do_not_leak_credentials() {
    let dir = common::media_dir("config-cli");
    let text = "config_version=1\n[database]\nurl='postgres://user:private-token@127.0.0.1:1/offline'\n[server]\npublic_base_url='broken'\nsecure_cookies='typo'\n[paths]\nmedia_dir='custom/media'\n";
    write_private(&dir.join("config.toml"), text);
    let shown = success(
        command(&dir, &["config", "show", "--sources", "--for", "database"])
            .output()
            .unwrap(),
    );
    assert!(!shown.contains("private-token"));
    let value: Value = serde_json::from_str(&shown).unwrap();
    assert!(
        value["fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|field| field["key"] == "database.url" && field["value"] == "[redacted]")
    );
    success(
        command(&dir, &["config", "check", "--for", "database"])
            .output()
            .unwrap(),
    );
    success(
        command(&dir, &["config", "show", "--for", "resources"])
            .output()
            .unwrap(),
    );
    let invalid = command(&dir, &["config", "check"]).output().unwrap();
    assert!(!invalid.status.success());
    assert!(!String::from_utf8_lossy(&invalid.stderr).contains("private-token"));
    success(
        command(&dir, &["config", "check"])
            .env("BLOG_PUBLIC_BASE_URL", "https://example.com")
            .env("BLOG_SECURE_COOKIES", "true")
            .output()
            .unwrap(),
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("config.toml")).unwrap(),
        text
    );
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn config_defaults_to_working_directory_and_explicit_selection_takes_precedence() {
    let dir = common::media_dir("config-selection");
    write_private(
        &dir.join("config.toml"),
        "[server]\nbind='127.0.0.1:1234'\n",
    );
    std::fs::write(dir.join(".env"), "BLOG_BIND=127.0.0.1:9999\n").unwrap();
    let default = success(
        Command::new(env!("CARGO_BIN_EXE_blog"))
            .env_clear()
            .current_dir(&dir)
            .args(["config", "show", "--sources"])
            .output()
            .unwrap(),
    );
    assert!(default.contains("127.0.0.1:1234"));
    assert!(default.contains("toml:config.toml"));
    assert!(!default.contains("127.0.0.1:9999"));
    let result = success(
        command(&dir, &["config", "show", "--sources"])
            .current_dir(&dir)
            .env("BLOG_CONFIG_FILE", dir.join("wrong.toml"))
            .output()
            .unwrap(),
    );
    assert!(result.contains("127.0.0.1:1234"));
    assert!(!result.contains("127.0.0.1:9999"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn database_commands_require_an_explicit_connection() {
    let dir = common::media_dir("config-required-db");
    for args in [
        vec!["migrate"],
        vec!["config", "check", "--for", "database"],
        vec!["config", "show", "--for", "media"],
    ] {
        let result = command(&dir, &args).output().unwrap();
        assert!(!result.status.success());
        assert!(
            String::from_utf8_lossy(&result.stderr).contains("请配置 database.url 或 DATABASE_URL")
        );
        assert!(result.stdout.is_empty());
    }
    success(
        command(&dir, &["config", "check", "--for", "serve"])
            .output()
            .unwrap(),
    );
    success(
        command(&dir, &["config", "check", "--for", "resources"])
            .output()
            .unwrap(),
    );
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn log_time_zone_uses_tz_independently_of_site_fallback() {
    let dir = common::media_dir("config-log-tz");
    write_private(
        &dir.join("config.toml"),
        "[server]\ntime_zone='Asia/Shanghai'\n",
    );
    // These CLI diagnostics don't connect to a DB. TZ is validated at logger initialization.
    for zone in [None, Some("Asia/Shanghai"), Some("America/New_York")] {
        let mut cmd = command(&dir, &["config", "show", "--for", "resources"]);
        if let Some(zone) = zone {
            cmd.env("TZ", zone);
        }
        let value: Value = serde_json::from_str(&success(cmd.output().unwrap())).unwrap();
        assert!(value["fields"].is_array());
    }
    for zone in ["", "Asia/Unknown", "+08:00"] {
        let output = command(&dir, &["config", "show", "--for", "resources"])
            .env("TZ", zone)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("TZ"));
        assert!(output.stdout.is_empty());
    }
    std::fs::remove_dir_all(dir).unwrap();
}
