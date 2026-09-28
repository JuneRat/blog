//! Real-process acceptance: first run, interrupted setup, atomic ownership,
//! protected one-time entry and subsequent config-based startup.
mod common;

use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::{Child, Command},
    time::Duration,
};

const PASSWORD: &str = "A unique initial secret 728!";

struct Server {
    child: Child,
    url: String,
    token: String,
    log: PathBuf,
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn command(dir: &Path) -> Command {
    let project = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut command = Command::new(env!("CARGO_BIN_EXE_blog"));
    command
        .current_dir(&project)
        .args(["serve", "--addr", "127.0.0.1:0"])
        .env_remove("DATABASE_URL")
        .env_remove("BLOG_PUBLIC_BASE_URL")
        .env_remove("BLOG_SECURE_COOKIES")
        .env_remove("BLOG_RECOVERY_MODE")
        .env_remove("BLOG_TRUSTED_PROXIES")
        .env_remove("BLOG_METRICS_BIND")
        .env("BLOG_LOG_FORMAT", "text")
        .env("BLOG_CONFIG_FILE", dir.join("config.toml"))
        .env("BLOG_MIGRATIONS_DIR", project.join("migrations/postgres"))
        .env("BLOG_THEME_DIR", project.join("themes/default"))
        .env("BLOG_ADMIN_DIST", dir.join("admin"))
        .env("BLOG_MEDIA_DIR", dir.join("media"))
        .env("RUST_LOG", "warn");
    command
}

async fn start(dir: &Path, broken_theme: bool) -> Server {
    start_with_migrations(dir, broken_theme, None).await
}

async fn start_with_migrations(
    dir: &Path,
    broken_theme: bool,
    migrations: Option<&Path>,
) -> Server {
    std::fs::create_dir_all(dir.join("admin")).unwrap();
    std::fs::write(dir.join("admin/index.html"), "<h1>Admin bundle</h1>").unwrap();
    let log = dir.join(format!("server-{}.log", uuid::Uuid::now_v7()));
    let output = std::fs::File::create(&log).unwrap();
    let mut command = command(dir);
    if let Some(migrations) = migrations {
        command.env("BLOG_MIGRATIONS_DIR", migrations);
    }
    if broken_theme {
        command.env("BLOG_THEME_DIR", dir.join("missing-theme"));
    }
    let child = command
        .stdout(output.try_clone().unwrap())
        .stderr(output)
        .spawn()
        .unwrap();
    let mut server = Server {
        child,
        url: String::new(),
        token: String::new(),
        log,
    };
    for _ in 0..200 {
        let text = std::fs::read_to_string(&server.log).unwrap();
        for line in text.lines() {
            if let Some(url) = line.strip_prefix("首次安装：") {
                server.url = url.trim_end_matches("/install").to_owned();
            }
            if let Some(url) = line.strip_prefix("公开站点已启动：") {
                server.url = url.to_owned();
            }
            if let Some(token) = line.strip_prefix("安装码：") {
                server.token = token.to_owned();
            }
        }
        if !server.url.is_empty() && (text.contains("公开站点已启动") || !server.token.is_empty())
        {
            return server;
        }
        assert!(
            server.child.try_wait().unwrap().is_none(),
            "server exited: {text}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!(
        "server did not start: {}",
        std::fs::read_to_string(&server.log).unwrap()
    );
}

fn client() -> Client {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}
fn input(url: &str) -> Value {
    json!({"database_url":url,"public_base_url":"http://example.test","username":"First-Writer","password":PASSWORD})
}
async fn submit(server: &Server, value: Value) -> reqwest::Response {
    client()
        .post(format!("{}/api/install", server.url))
        .header("x-install-token", &server.token)
        .header("origin", &server.url)
        .json(&value)
        .send()
        .await
        .unwrap()
}
async fn empty_database(name: &str) -> (sqlx::PgPool, String) {
    let pool = common::fresh_database(name).await;
    sqlx::raw_sql("DROP SCHEMA public CASCADE; CREATE SCHEMA public")
        .execute(&pool)
        .await
        .unwrap();
    (pool, common::test_db_url(&common::admin_url(), name))
}
async fn login(server: &Server) -> String {
    let response = client()
        .post(format!("{}/auth/login/password", server.url))
        .header("origin", &server.url)
        .json(&json!({"username":"first-writer","password":PASSWORD}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        response.text().await.unwrap()
    );
    response
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn empty_database_installs_once_logs_in_and_restarts_from_saved_config() {
    let (pool, url) = empty_database("blog_install_complete_test").await;
    let dir = common::media_dir("installation-complete");
    let server = start(&dir, false).await;
    let response = client()
        .get(format!("{}/", server.url))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(response.headers()["location"], "/install");
    let info = client()
        .get(format!("{}/api/install", server.url))
        .send()
        .await
        .unwrap();
    assert_eq!(info.headers()["cache-control"], "no-store");
    assert_eq!(
        info.json::<Value>().await.unwrap()["database_configured"],
        false
    );
    let (first, second) = tokio::join!(submit(&server, input(&url)), submit(&server, input(&url)));
    let statuses = [first.status(), second.status()];
    assert_eq!(
        statuses.iter().filter(|s| **s == StatusCode::OK).count(),
        1,
        "{statuses:?}"
    );
    assert!(
        statuses.contains(&StatusCode::TOO_MANY_REQUESTS)
            || statuses.contains(&StatusCode::NOT_FOUND)
    );
    let completed = if first.status() == StatusCode::OK {
        first
    } else {
        second
    };
    assert_eq!(
        completed.json::<Value>().await.unwrap()["redirect"],
        "/admin/"
    );
    assert_eq!(
        submit(&server, input(&url)).await.status(),
        StatusCode::NOT_FOUND
    );
    let cookie = login(&server).await;
    // Exercise the production router after its one-time installation switch.
    // These routes used to be assembled separately by server; authentication,
    // no-store and request IDs must now be applied by the common HTTP entry.
    for path in [
        "/api/admin/v1/comments",
        "/api/admin/v1/settings/retention",
        "/api/admin/v1/audit-logs",
    ] {
        let anonymous = client()
            .get(format!("{}{path}", server.url))
            .send()
            .await
            .unwrap();
        assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED, "{path}");
        assert_eq!(anonymous.headers()["cache-control"], "no-store");
        let request_id = anonymous.headers()["x-request-id"]
            .to_str()
            .unwrap()
            .to_owned();
        assert_eq!(
            anonymous.json::<Value>().await.unwrap()["request_id"],
            request_id
        );
        let authenticated = client()
            .get(format!("{}{path}", server.url))
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(authenticated.status(), StatusCode::OK, "{path}");
        assert_eq!(authenticated.headers()["cache-control"], "no-store");
        assert!(authenticated.headers().contains_key("x-request-id"));
    }
    let me = client()
        .get(format!("{}/api/admin/v1/me", server.url))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(me.status(), StatusCode::OK);
    let me = me.json::<Value>().await.unwrap();
    assert!(
        me["permissions"]
            .as_array()
            .unwrap()
            .contains(&json!("ownership.manage"))
    );
    let policy = client()
        .get(format!("{}/api/admin/v1/settings/retention", server.url))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    let mut updated_policy = policy;
    updated_policy["audit_days"] = json!(181);
    let saved = client()
        .put(format!("{}/api/admin/v1/settings/retention", server.url))
        .header("cookie", &cookie)
        .header("origin", &server.url)
        .header("x-csrf-token", me["csrf_token"].as_str().unwrap())
        .header("x-forwarded-for", "198.51.100.10")
        .json(&updated_policy)
        .send()
        .await
        .unwrap();
    assert_eq!(saved.status(), StatusCode::OK);
    let source_ip: String = sqlx::query_scalar(
        "SELECT host(ip_address) FROM audit_logs WHERE action='settings.retention'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        source_ip, "127.0.0.1",
        "untrusted forwarding must not replace the socket peer"
    );
    let row: (i64, i64, String) = sqlx::query_as("SELECT version,auth_version,status FROM users")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row, (1, 1, "active".into()));
    let audits: (i64, Option<String>) = sqlx::query_as("SELECT count(*),min(host(ip_address)) FROM audit_logs WHERE action='installation.complete' AND actor_id IS NULL").fetch_one(&pool).await.unwrap();
    assert_eq!(audits, (1, Some("127.0.0.1".into())));
    let config = std::fs::read_to_string(dir.join("config.toml")).unwrap();
    assert!(!dir.join("config.install-state.json").exists());
    assert!(!config.contains(PASSWORD));
    assert!(!config.contains(&server.token));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(dir.join("config.toml"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    let log = std::fs::read_to_string(&server.log).unwrap();
    assert!(!log.contains(&url));
    assert!(!log.contains(PASSWORD));
    drop(server);
    let restarted = start(&dir, false).await;
    assert!(restarted.token.is_empty());
    let me = client()
        .get(format!("{}/api/admin/v1/me", restarted.url))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(me.status(), StatusCode::OK);
    assert_eq!(
        client()
            .get(format!("{}/install", restarted.url))
            .send()
            .await
            .unwrap()
            .headers()["location"],
        "/admin/"
    );
    assert_eq!(
        submit(&restarted, input(&url)).await.status(),
        StatusCode::NOT_FOUND
    );
    drop(restarted);
    // Even an unusable Owner must never reopen installation.
    sqlx::query("UPDATE users SET status='disabled'")
        .execute(&pool)
        .await
        .unwrap();
    let disabled = start(&dir, false).await;
    assert!(disabled.token.is_empty());
    assert_eq!(
        submit(&disabled, input(&url)).await.status(),
        StatusCode::NOT_FOUND
    );
    drop(disabled);
    pool.close().await;
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn authorization_and_input_failures_never_save_configuration_or_mutate_database() {
    let (pool, url) = empty_database("blog_install_guards_test").await;
    let dir = common::media_dir("installation-guards");
    let server = start(&dir, false).await;
    let endpoint = format!("{}/api/install", server.url);
    let missing = client()
        .post(&endpoint)
        .json(&input(&url))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::FORBIDDEN);
    let origin = client()
        .post(&endpoint)
        .header("x-install-token", &server.token)
        .header("origin", "https://evil.invalid")
        .json(&input(&url))
        .send()
        .await
        .unwrap();
    assert_eq!(origin.status(), StatusCode::FORBIDDEN);
    let mut weak = input(&url);
    weak["password"] = json!("short");
    assert_eq!(
        submit(&server, weak).await.status(),
        StatusCode::BAD_REQUEST
    );
    let mut malformed = input(&url);
    malformed["public_base_url"] = json!("https://example.test/blog");
    assert_eq!(
        submit(&server, malformed).await.status(),
        StatusCode::BAD_REQUEST
    );
    let mut extra = input(&url);
    extra["role"] = json!("owner");
    assert_eq!(
        submit(&server, extra).await.status(),
        StatusCode::BAD_REQUEST
    );
    let bad = submit(
        &server,
        input("postgres://secret-user:private-password@127.0.0.1:1/missing"),
    )
    .await;
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
    let error = bad.text().await.unwrap();
    assert!(!error.contains("private-password"));
    assert!(!dir.join("config.toml").exists());
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.tables WHERE table_schema='public'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 0);
    drop(server);
    pool.close().await;
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn interrupted_install_resumes_without_overwriting_saved_database_and_rolls_back_audit_failure()
 {
    let (pool, url) = empty_database("blog_install_resume_test").await;
    let dir = common::media_dir("installation-resume");
    let server = start(&dir, true).await;
    let old_token = server.token.clone();
    let response = submit(&server, input(&url)).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(response.text().await.unwrap().contains("主题"));
    assert!(dir.join("config.toml").exists());
    let journal_path = dir.join("config.install-state.json");
    let journal = std::fs::read_to_string(&journal_path).unwrap();
    assert!(!journal.contains(PASSWORD));
    assert!(!journal.contains(&server.token));
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(users, 0);
    drop(server);
    sqlx::raw_sql("CREATE FUNCTION reject_install_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'audit unavailable'; END $$; CREATE TRIGGER reject_install BEFORE INSERT ON audit_logs FOR EACH ROW EXECUTE FUNCTION reject_install_audit();").execute(&pool).await.unwrap();
    let server = start(&dir, false).await;
    assert_ne!(server.token, old_token);
    let old = client()
        .post(format!("{}/api/install", server.url))
        .header("x-install-token", old_token)
        .json(&input(&url))
        .send()
        .await
        .unwrap();
    assert_eq!(old.status(), StatusCode::FORBIDDEN);
    let retry = input("postgres://ignored:ignored@127.0.0.1:1/ignored");
    let response = submit(&server, retry.clone()).await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(std::fs::read_to_string(&journal_path).unwrap(), journal);
    let counts: (i64,i64,i64) = sqlx::query_as("SELECT (SELECT count(*) FROM users),(SELECT count(*) FROM roles),(SELECT count(*) FROM settings)").fetch_one(&pool).await.unwrap();
    assert_eq!(counts, (0, 0, 0));
    sqlx::raw_sql(
        "DROP TRIGGER reject_install ON audit_logs; DROP FUNCTION reject_install_audit()",
    )
    .execute(&pool)
    .await
    .unwrap();
    let response = submit(&server, retry).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        response.text().await.unwrap()
    );
    login(&server).await;
    assert!(!journal_path.exists());
    drop(server);
    pool.close().await;
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn existing_and_recovery_databases_are_refused_without_cleaning_them() {
    let (pool, url) = empty_database("blog_install_existing_test").await;
    sqlx::raw_sql("CREATE TABLE keep_me(value text); INSERT INTO keep_me VALUES('keep');")
        .execute(&pool)
        .await
        .unwrap();
    let dir = common::media_dir("installation-existing");
    let server = start(&dir, false).await;
    let response = submit(&server, input(&url)).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(response.text().await.unwrap().contains("空数据库"));
    let value: String = sqlx::query_scalar("SELECT value FROM keep_me")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(value, "keep");
    sqlx::raw_sql(
        "COMMENT ON DATABASE blog_install_existing_test IS 'blog:recovery-isolated:test'",
    )
    .execute(&pool)
    .await
    .unwrap();
    let response = submit(&server, input(&url)).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(response.text().await.unwrap().contains("恢复隔离"));
    assert!(!dir.join("config.toml").exists());
    drop(server);
    pool.close().await;
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn committed_installation_with_a_remaining_journal_is_verified_then_cleaned_on_startup() {
    let (pool, url) = empty_database("blog_install_committed_journal_test").await;
    let dir = common::media_dir("installation-committed-journal");
    let server = start(&dir, true).await;
    assert_eq!(
        submit(&server, input(&url)).await.status(),
        StatusCode::BAD_REQUEST
    );
    drop(server);
    let path = dir.join("config.toml");
    let journal_path = dir.join("config.install-state.json");
    let journal_text = std::fs::read_to_string(&journal_path).unwrap();
    let mut journal: Value = serde_json::from_str(&journal_text).unwrap();
    let installation_id = journal["installation_id"].as_str().unwrap().to_owned();
    // Reproduce a crash after the real DB transaction commits, before the
    // server can clean the journal or activate its router.
    let owner = application::installation::InitialOwner::prepare(
        "first-writer",
        PASSWORD,
        &infrastructure::Argon2PasswordHasher::with_defaults(),
        time::OffsetDateTime::now_utc(),
    )
    .await
    .unwrap();
    let schema_contract =
        infrastructure::schema_contract::SchemaContract::load("../../migrations/postgres").unwrap();
    infrastructure::installation::initialize(
        &common::database(pool.clone()),
        &schema_contract,
        &installation_id,
        &owner,
        &application::ports::SiteSettingsValue {
            title: None,
            description: None,
            logo_media_id: None,
        },
        application::audit::AuditContext::system(),
    )
    .await
    .unwrap();
    let edited = format!(
        "{}\n# Keep this deployment edit\n",
        std::fs::read_to_string(&path).unwrap()
    );
    std::fs::write(&path, &edited).unwrap();

    journal["installation_id"] = json!(if installation_id == "a".repeat(64) {
        "b".repeat(64)
    } else {
        "a".repeat(64)
    });
    let mismatched = serde_json::to_string(&journal).unwrap();
    std::fs::write(&journal_path, &mismatched).unwrap();
    let output = command(&dir).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("不属于本次安装"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("安装码"));
    assert_eq!(std::fs::read_to_string(&journal_path).unwrap(), mismatched);
    std::fs::write(&journal_path, &journal_text).unwrap();

    let output = command(&dir)
        .env("DATABASE_URL", "postgres://blog:blog@127.0.0.1:1/missing")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("安装码"));
    assert_eq!(
        std::fs::read_to_string(&journal_path).unwrap(),
        journal_text
    );
    std::fs::write(&path, "broken").unwrap();
    assert!(!command(&dir).output().unwrap().status.success());
    assert_eq!(
        std::fs::read_to_string(&journal_path).unwrap(),
        journal_text
    );
    std::fs::write(&path, &edited).unwrap();

    let restarted = start(&dir, false).await;
    assert!(restarted.token.is_empty());
    assert!(!journal_path.exists());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), edited);
    login(&restarted).await;
    assert_eq!(
        submit(&restarted, input(&url)).await.status(),
        StatusCode::NOT_FOUND
    );
    let counts: (i64, i64) = sqlx::query_as("SELECT (SELECT count(*) FROM users),(SELECT count(*) FROM audit_logs WHERE action='installation.complete')").fetch_one(&pool).await.unwrap();
    assert_eq!(counts, (1, 1));
    drop(restarted);
    pool.close().await;
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn cleanup_permission_failure_does_not_fail_installation_and_restart_retries_it() {
    use std::os::unix::fs::PermissionsExt;
    struct RestorePermissions(PathBuf, std::fs::Permissions);
    impl Drop for RestorePermissions {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, self.1.clone());
        }
    }

    let (pool, url) = empty_database("blog_install_cleanup_failure_test").await;
    let dir = common::media_dir("installation-cleanup-failure");
    let server = start(&dir, true).await;
    assert_eq!(
        submit(&server, input(&url)).await.status(),
        StatusCode::BAD_REQUEST
    );
    drop(server);
    let server = start(&dir, false).await;
    let journal_path = dir.join("config.install-state.json");
    let journal = std::fs::read_to_string(&journal_path).unwrap();
    let config = std::fs::read_to_string(dir.join("config.toml")).unwrap();
    let probe = dir.join("unlink-probe");
    std::fs::write(&probe, "probe").unwrap();
    let permissions =
        RestorePermissions(dir.clone(), std::fs::metadata(&dir).unwrap().permissions());
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    if std::fs::remove_file(&probe).is_ok() {
        // Privileged runners can bypass mode bits; do not claim to have tested
        // a permission failure on a filesystem that cannot reproduce one.
        eprintln!("skipping unlink-denied case: process bypasses directory permissions");
        drop(permissions);
        drop(server);
        pool.close().await;
        std::fs::remove_dir_all(dir).unwrap();
        return;
    }
    let response = submit(&server, input(&url)).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        response.text().await.unwrap()
    );
    login(&server).await;
    assert_eq!(
        submit(&server, input(&url)).await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(std::fs::read_to_string(&journal_path).unwrap(), journal);
    assert_eq!(
        std::fs::read_to_string(dir.join("config.toml")).unwrap(),
        config
    );
    let log = std::fs::read_to_string(&server.log).unwrap();
    assert!(log.contains("临时安装日志清理失败"));
    assert!(!log.contains(&url));
    assert!(!log.contains(PASSWORD));
    drop(permissions);
    drop(server);

    let restarted = start(&dir, false).await;
    assert!(restarted.token.is_empty());
    assert!(!journal_path.exists());
    login(&restarted).await;
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_logs WHERE action='installation.complete'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 1);
    drop(restarted);
    pool.close().await;
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn broken_config_or_explicit_database_never_falls_back_to_an_open_installer() {
    let dir = common::media_dir("installation-fail-closed");
    let output = command(&dir)
        .env("DATABASE_URL", "postgres://blog:blog@127.0.0.1:1/missing")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("安装码"));
    std::fs::write(dir.join("config.toml"), "broken").unwrap();
    let output = command(&dir).output().unwrap();
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("安装码"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn toml_bootstrap_is_saved_once_and_runtime_edits_survive_restart() {
    let (pool, url) = empty_database("blog_install_bootstrap_test").await;
    let dir = common::media_dir("installation-bootstrap");
    let path = dir.join("config.toml");
    std::fs::write(&path, "# Operator comment\n[bootstrap]\ntitle='Initial title'\ndescription='Initial description'\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let server = start(&dir, false).await;
    let response = submit(&server, input(&url)).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        response.text().await.unwrap()
    );
    let initial: Value = sqlx::query_scalar("SELECT value FROM settings WHERE key='site'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(initial["title"], "Initial title");
    assert_eq!(initial["description"], "Initial description");
    let config = std::fs::read_to_string(&path).unwrap();
    assert!(config.contains("# Operator comment"));
    assert!(!config.contains("installation_id"));
    assert!(!dir.join("config.install-state.json").exists());
    // Model a saved runtime setting, then change deployment bootstrap values.
    sqlx::query("UPDATE settings SET value=jsonb_build_object('title','Admin title','description',''), version=version+1 WHERE key='site'").execute(&pool).await.unwrap();
    drop(server);
    std::fs::write(&path, config.replace("Initial title", "Changed bootstrap")).unwrap();
    let restarted = start(&dir, false).await;
    assert!(restarted.token.is_empty());
    let page = client()
        .get(format!("{}/", restarted.url))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(page.contains("Admin title"));
    assert!(!page.contains("Changed bootstrap"));
    let current: Value = sqlx::query_scalar("SELECT value FROM settings WHERE key='site'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(current["description"], "");
    drop(restarted);
    pool.close().await;
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn competing_bootstraps_commit_exactly_one_owner_and_marker() {
    let pool = common::fresh_database("blog_install_atomic_test").await;
    let hasher = infrastructure::Argon2PasswordHasher::with_defaults();
    let first = application::installation::InitialOwner::prepare(
        "first-writer",
        PASSWORD,
        &hasher,
        time::OffsetDateTime::now_utc(),
    )
    .await
    .unwrap();
    let second = application::installation::InitialOwner::prepare(
        "second-writer",
        PASSWORD,
        &hasher,
        time::OffsetDateTime::now_utc(),
    )
    .await
    .unwrap();
    let site = application::ports::SiteSettingsValue {
        title: None,
        description: None,
        logo_media_id: None,
    };
    let first_id = "a".repeat(64);
    let second_id = "b".repeat(64);
    let audit = application::audit::AuditContext::system();
    let schema_contract =
        infrastructure::schema_contract::SchemaContract::load("../../migrations/postgres").unwrap();
    let database = common::database(pool.clone());
    let (a, b) = tokio::join!(
        infrastructure::installation::initialize(
            &database,
            &schema_contract,
            &first_id,
            &first,
            &site,
            audit
        ),
        infrastructure::installation::initialize(
            &database,
            &schema_contract,
            &second_id,
            &second,
            &site,
            audit
        ),
    );
    assert_ne!(a.is_ok(), b.is_ok());
    let row: (i64,i64,i64) = sqlx::query_as("SELECT (SELECT count(*) FROM users),(SELECT count(*) FROM user_roles),(SELECT count(*) FROM audit_logs WHERE action='installation.complete')").fetch_one(&pool).await.unwrap();
    assert_eq!(row, (1, 1, 1));
    let marker: String =
        sqlx::query_scalar("SELECT value->>'id' FROM settings WHERE key='installation'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(marker, if a.is_ok() { first_id } else { second_id });
    pool.close().await;
}

#[tokio::test]
async fn a_concurrently_created_config_is_never_overwritten_and_prevents_database_writes() {
    let (pool, url) = empty_database("blog_install_config_race_test").await;
    let dir = common::media_dir("installation-config-race");
    let server = start(&dir, false).await;
    let path = dir.join("config.toml");
    std::fs::write(&path, "another process owns this path").unwrap();
    let response = submit(&server, input(&url)).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        std::fs::read_to_string(path).unwrap(),
        "another process owns this path"
    );
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.tables WHERE table_schema='public'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 0);
    drop(server);
    pool.close().await;
    std::fs::remove_dir_all(dir).unwrap();
}

// A release can extend the baseline without hardcoded installation table lists.
#[tokio::test]
async fn next_schema_installs_and_resumes_without_accepting_populated_extension() {
    let (pool, url) = empty_database("blog_install_next_schema_test").await;
    let dir = common::media_dir("installation-next-schema");
    let migrations = dir.join("migrations");
    std::fs::create_dir_all(&migrations).unwrap();
    let baseline = Path::new("../../migrations/postgres");
    for entry in std::fs::read_dir(baseline).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), migrations.join(entry.file_name())).unwrap();
    }
    let mut contract: Value =
        serde_json::from_slice(&std::fs::read(migrations.join("schema.json")).unwrap()).unwrap();
    let version = contract["migrations"].as_array().unwrap().last().unwrap()["version"]
        .as_i64()
        .unwrap()
        + 1;
    let filename = format!("{version:04}_installation_drill.sql");
    std::fs::write(
        migrations.join(&filename),
        "CREATE TABLE installation_drill (id uuid PRIMARY KEY);\n",
    )
    .unwrap();
    let migrator = sqlx::migrate::Migrator::new(migrations.as_path())
        .await
        .unwrap();
    let migration = migrator
        .iter()
        .find(|migration| migration.version == version)
        .unwrap();
    let checksum: String = migration
        .checksum
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    contract["id"] = json!("blog-installation-drill");
    contract["migrations"]
        .as_array_mut()
        .unwrap()
        .push(json!({"version":version,"file":filename,"checksum":checksum}));
    contract["tables"]["installation_drill"] = json!({"app":[],"maintenance":[]});
    std::fs::write(
        migrations.join("schema.json"),
        serde_json::to_vec(&contract).unwrap(),
    )
    .unwrap();

    // Migrate the extended schema, then interrupt before ownership commits.
    let server = start_with_migrations(&dir, true, Some(&migrations)).await;
    let response = submit(&server, input(&url)).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(response.text().await.unwrap().contains("主题"));
    drop(server);
    sqlx::query("INSERT INTO installation_drill VALUES(gen_random_uuid())")
        .execute(&pool)
        .await
        .unwrap();
    let server = start_with_migrations(&dir, false, Some(&migrations)).await;
    let response = submit(&server, input(&url)).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(response.text().await.unwrap().contains("已有数据"));
    sqlx::query("DELETE FROM installation_drill")
        .execute(&pool)
        .await
        .unwrap();
    let response = submit(&server, input(&url)).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        response.text().await.unwrap()
    );
    let owners: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(owners, 1);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        count as usize,
        contract["migrations"].as_array().unwrap().len()
    );
    drop(server);
    pool.close().await;
}
