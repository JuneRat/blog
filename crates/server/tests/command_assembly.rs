//! Maintenance commands must remain available while website configuration or
//! derived content is broken. Each child process exercises the real CLI root.

mod common;

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

fn cli_command(database_url: &str, args: &[&str], public_url: &str) -> Command {
    let project = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let missing = std::env::temp_dir().join(format!(
        "blog-assembly-unavailable-{}",
        uuid::Uuid::now_v7()
    ));
    let mut command = Command::new(env!("CARGO_BIN_EXE_blog"));
    command
        .args(args)
        .current_dir(&project)
        .env("DATABASE_URL", database_url)
        .env("BLOG_CONFIG_FILE", missing.join("config.toml"))
        .env_remove("BLOG_RECOVERY_MODE")
        .env_remove("BLOG_METRICS_BIND")
        .env("BLOG_LOG_FORMAT", "text")
        .env("BLOG_MIGRATIONS_DIR", project.join("migrations/postgres"))
        .env("BLOG_THEME_DIR", missing.join("theme"))
        .env("BLOG_ADMIN_DIST", missing.join("admin"))
        .env("BLOG_PUBLIC_BASE_URL", public_url);
    command
}

fn cli(database_url: &str, args: &[&str], password: Option<&str>, public_url: &str) -> Output {
    let mut child = cli_command(database_url, args, public_url)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(password) = password {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(password.as_bytes())
            .unwrap();
    }
    child.wait_with_output().unwrap()
}

fn assert_success(output: Output) -> String {
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[tokio::test]
async fn owner_bootstrap_uses_new_identity_baseline() {
    let pool = common::fresh_database("blog_owner_bootstrap_test").await;
    let database_url = common::test_db_url(&common::admin_url(), "blog_owner_bootstrap_test");
    // 清除测试辅助函数的迁移，让真实 CLI 从完全空的 schema 开始。
    sqlx::raw_sql("DROP SCHEMA public CASCADE; CREATE SCHEMA public;")
        .execute(&pool)
        .await
        .unwrap();
    let invalid_url = "identity-commands-do-not-need-a-website";
    assert_success(cli(&database_url, &["migrate"], None, invalid_url));
    assert_success(cli(
        &database_url,
        &["user", "create", "first-owner"],
        None,
        invalid_url,
    ));
    assert_success(cli(
        &database_url,
        &[
            "user",
            "passwd",
            "--user",
            "first-owner",
            "--password-stdin",
        ],
        Some("A first account password 2026!\n"),
        invalid_url,
    ));
    assert_success(cli(
        &database_url,
        &["role", "assign", "--user", "first-owner", "--role", "owner"],
        None,
        invalid_url,
    ));
    let info = assert_success(cli(
        &database_url,
        &["user", "show", "first-owner"],
        None,
        invalid_url,
    ));
    assert!(info.contains("first-owner"));
    let row: (i64, i64, bool, String) = sqlx::query_as(
        "SELECT u.version, u.auth_version, u.password_hash IS NOT NULL, r.code \
         FROM users u JOIN user_roles ur ON ur.user_id=u.id JOIN roles r ON r.id=ur.role_id \
         WHERE u.username='first-owner'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row, (3, 2, true, "owner".into()));
    let migrations: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&pool)
            .await
            .unwrap();
    let migrator = sqlx::migrate::Migrator::new(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../migrations/postgres")
            .as_path(),
    )
    .await
    .unwrap();
    let expected: Vec<_> = migrator.iter().map(|migration| migration.version).collect();
    assert_eq!(migrations, expected);
}

#[tokio::test]
async fn maintenance_does_not_require_a_working_website() {
    // A separate database keeps this subprocess test isolated from HTTP/SSR fixtures.
    let pool = common::fresh_database("blog_command_assembly_test").await;
    let database_url = common::test_db_url(&common::admin_url(), "blog_command_assembly_test");
    let invalid_url = "this-is-not-a-public-url";
    assert_success(cli(
        &database_url,
        &["user", "create", "assembly-user"],
        None,
        invalid_url,
    ));
    assert_success(cli(
        &database_url,
        &[
            "user",
            "passwd",
            "--user",
            "assembly-user",
            "--password-stdin",
        ],
        Some("A secure setup secret 583!\n"),
        invalid_url,
    ));
    assert_success(cli(
        &database_url,
        &[
            "role",
            "assign",
            "--user",
            "assembly-user",
            "--role",
            "author",
        ],
        None,
        invalid_url,
    ));
    assert_success(cli(
        &database_url,
        &["user", "show", "assembly-user"],
        None,
        invalid_url,
    ));
    assert_success(cli(&database_url, &["role", "list"], None, invalid_url));
    assert_success(cli(&database_url, &["oauth", "list"], None, invalid_url));
    assert_success(cli(
        &database_url,
        &[
            "oauth",
            "add-github",
            "--client-id",
            "assembly-client",
            "--secret-ref",
            "ASSEMBLY_UNUSED_SECRET",
        ],
        None,
        invalid_url,
    ));
    assert_success(cli(
        &database_url,
        &[
            "oauth",
            "bind",
            "--user",
            "assembly-user",
            "--provider",
            "github",
            "--external-id",
            "assembly-external",
        ],
        None,
        invalid_url,
    ));
    let bindings = assert_success(cli(
        &database_url,
        &["oauth", "bindings", "--user", "assembly-user"],
        None,
        invalid_url,
    ));
    assert!(bindings.contains("github#assembly-external"));
    assert_success(cli(
        &database_url,
        &[
            "oauth",
            "unbind",
            "--user",
            "assembly-user",
            "--provider",
            "github",
            "--external-id",
            "assembly-external",
        ],
        None,
        invalid_url,
    ));
    assert_success(cli(
        &database_url,
        &[
            "post",
            "create",
            "--author",
            "assembly-user",
            "--slug",
            "assembly-post",
            "--title",
            "Assembly draft",
        ],
        None,
        invalid_url,
    ));

    // Damaged legacy body content must not pull the HTML rebuild into password,
    // role or OAuth repair commands. There is intentionally no matching media.
    sqlx::query(
        "UPDATE posts SET content = $1, content_render_version = 2 WHERE slug = 'assembly-post'",
    )
    .bind(format!("![missing](/media/{})", uuid::Uuid::now_v7()))
    .execute(&pool)
    .await
    .unwrap();
    assert_success(cli(
        &database_url,
        &[
            "user",
            "passwd",
            "--user",
            "assembly-user",
            "--password-stdin",
        ],
        Some("A replacement secret 946!\n"),
        invalid_url,
    ));
    assert_success(cli(&database_url, &["role", "list"], None, invalid_url));
    assert_success(cli(&database_url, &["oauth", "list"], None, invalid_url));
    assert_success(cli(&database_url, &["migrate"], None, invalid_url));
    assert_success(cli(
        &database_url,
        &["post", "list", "--author", "assembly-user"],
        None,
        invalid_url,
    ));
    assert_success(cli(&database_url, &["publish-due"], None, invalid_url));
    let pending: i32 =
        sqlx::query_scalar("SELECT content_render_version FROM posts WHERE slug = 'assembly-post'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(pending, 2, "普通命令不得触发内容派生物重建");

    let failed = cli(&database_url, &["rebuild-html"], None, invalid_url);
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("HTML 重建失败：post"));

    // 真正启动监听并访问公开页：坏掉的历史源文不会阻止启动或触发即时重建。
    let project = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let logs = common::media_dir("serve-with-stale-html");
    let stdout = logs.join("stdout");
    let stderr = logs.join("stderr");
    let mut server = RunningServer(
        cli_command(
            &database_url,
            &["serve", "--addr", "127.0.0.1:0"],
            "https://blog.test",
        )
        .env("BLOG_THEME_DIR", project.join("themes/default"))
        .stdout(std::fs::File::create(&stdout).unwrap())
        .stderr(std::fs::File::create(&stderr).unwrap())
        .spawn()
        .unwrap(),
    );
    let base = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            assert!(
                server.0.try_wait().unwrap().is_none(),
                "{}",
                std::fs::read_to_string(&stderr).unwrap()
            );
            let log = std::fs::read_to_string(&stderr).unwrap();
            if let Some(base) = log.lines().find_map(|line| {
                line.split_once("  INFO 公开站点已启动：")
                    .map(|(_, url)| url)
            }) {
                break base.to_string();
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("server should become ready without rebuilding old HTML");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();
    assert!(
        client
            .get(format!("{base}/"))
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    drop(server);
    assert!(
        std::fs::read_to_string(&stdout).unwrap().is_empty(),
        "运行日志不得进入 stdout"
    );
    std::fs::remove_dir_all(logs).unwrap();
    let pending: i32 =
        sqlx::query_scalar("SELECT content_render_version FROM posts WHERE slug='assembly-post'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(pending, 2, "serve 不得隐式重建");

    // 修复历史引用后可重跑；网站配置错误仍由 serve 自己报告。
    sqlx::query("UPDATE posts SET content = 'body' WHERE slug = 'assembly-post'")
        .execute(&pool)
        .await
        .unwrap();
    let result: serde_json::Value = serde_json::from_str(&assert_success(cli(
        &database_url,
        &["rebuild-html"],
        None,
        invalid_url,
    )))
    .unwrap();
    assert_eq!(result["rebuilt"]["posts"], 1);
    assert_eq!(result["has_more"], false);
    let invalid_site = cli(
        &database_url,
        &["serve", "--addr", "127.0.0.1:0"],
        None,
        invalid_url,
    );
    assert!(!invalid_site.status.success());
    assert!(String::from_utf8_lossy(&invalid_site.stderr).contains("BLOG_PUBLIC_BASE_URL 无效"));
    let invalid_theme = cli(
        &database_url,
        &["serve", "--addr", "127.0.0.1:0"],
        None,
        "http://127.0.0.1:8080",
    );
    assert!(!invalid_theme.status.success());
    assert!(String::from_utf8_lossy(&invalid_theme.stderr).contains("加载默认主题模板失败"));
}

struct RunningServer(std::process::Child);
impl Drop for RunningServer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn explicit_html_rebuild_updates_all_three_sources_and_is_repeatable() {
    let database = "blog_explicit_html_rebuild_test";
    let pool = common::fresh_database(database).await;
    let url = common::test_db_url(&common::admin_url(), database);
    sqlx::raw_sql(
        "INSERT INTO users(id,username) VALUES(gen_random_uuid(),'html-author');
         INSERT INTO posts(id,author_id,slug,content,content_html,content_render_version)
           SELECT gen_random_uuid(),id,'html-post','**fresh**','stale',2 FROM users;
         INSERT INTO pages(id,slug,content,content_html,content_render_version)
           VALUES(gen_random_uuid(),'html-page','**fresh**','stale',2);
         INSERT INTO comments(id,post_id,author_name,content,content_html,content_render_version)
           SELECT gen_random_uuid(),id,'guest','**fresh**','stale',2 FROM posts;",
    )
    .execute(&pool)
    .await
    .unwrap();

    let blocked = cli_command(&url, &["rebuild-html"], "unused")
        .env("BLOG_RECOVERY_MODE", "1")
        .output()
        .unwrap();
    assert!(!blocked.status.success());
    assert!(String::from_utf8_lossy(&blocked.stderr).contains("恢复隔离期间禁止 HTML 重建"));

    assert_success(cli(&url, &["migrate"], None, "unused"));
    let mut before = Vec::new();
    for table in ["posts", "pages", "comments"] {
        let row: (String, i32, i64, time::OffsetDateTime) = sqlx::query_as(&format!(
            "SELECT content_html,content_render_version,version,updated_at FROM {table}"
        ))
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.0, "stale");
        assert_eq!(row.1, 2);
        before.push((row.2, row.3));
    }
    // 即使连接拥有建表权限，也必须能在 PostgreSQL 强制只读模式下预检。
    let read_only_url = format!("{url}?options=-c%20default_transaction_read_only%3Don");
    let output = cli_command(&read_only_url, &["rebuild-html", "--dry-run"], "unused")
        .env("RUST_LOG", "sqlx=debug")
        .output()
        .unwrap();
    assert!(!output.stderr.is_empty(), "数据库调试日志应进入 stderr");
    let preview: serde_json::Value = serde_json::from_str(&assert_success(output)).unwrap();
    assert_eq!(
        preview["pending"],
        serde_json::json!({"posts":1,"pages":1,"comments":1})
    );
    assert_eq!(
        preview["rebuilt"],
        serde_json::json!({"posts":0,"pages":0,"comments":0})
    );
    assert_eq!(preview["batches"], 0);
    assert_eq!(preview["dry_run"], true);
    assert_eq!(preview["has_more"], true);
    let audits: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_logs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(audits, 0);

    // 三类来源共用 max-batches；每次至多处理一种，下一次从剩余旧版本继续。
    for (i, kind) in ["posts", "pages", "comments"].into_iter().enumerate() {
        let result: serde_json::Value = serde_json::from_str(&assert_success(cli(
            &url,
            &["rebuild-html", "--batch-size", "2", "--max-batches", "1"],
            None,
            "unused",
        )))
        .unwrap();
        let mut expected = serde_json::json!({"posts":0,"pages":0,"comments":0});
        expected[kind] = 1.into();
        assert_eq!(result["rebuilt"], expected);
        assert_eq!(result["batches"], 1);
        assert_eq!(result["has_more"], i < 2);
        assert!(result["failure"].is_null());
    }
    for (i, table) in ["posts", "pages", "comments"].into_iter().enumerate() {
        let row: (String, i32, i64, time::OffsetDateTime) = sqlx::query_as(&format!(
            "SELECT content_html,content_render_version,version,updated_at FROM {table}"
        ))
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(row.0.contains("<strong>fresh</strong>"));
        assert_eq!(
            row.1,
            if table == "comments" {
                infrastructure::COMMENT_RENDER_VERSION
            } else {
                infrastructure::CONTENT_RENDER_VERSION
            }
        );
        assert_eq!((row.2, row.3), before[i]);
    }
    let result: serde_json::Value = serde_json::from_str(&assert_success(cli(
        &url,
        &["rebuild-html"],
        None,
        "unused",
    )))
    .unwrap();
    assert_eq!(
        result["rebuilt"],
        serde_json::json!({"posts":0,"pages":0,"comments":0})
    );
    assert_eq!(result["batches"], 0);
    let audit_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_logs WHERE action LIKE '%.html.rebuild'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(audit_count, 3, "每条重建记一次审计，重复执行不制造变更");
    let roles: i64 = sqlx::query_scalar("SELECT count(*) FROM roles")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(roles, 0, "结构迁移和 HTML 维护不应初始化角色");
    pool.close().await;
}

#[tokio::test]
async fn html_preflight_and_invalid_arguments_never_initialize_an_empty_schema() {
    let database = "blog_html_preflight_empty_test";
    let pool = common::fresh_database(database).await;
    let url = common::test_db_url(&common::admin_url(), database);
    sqlx::raw_sql("DROP SCHEMA public CASCADE; CREATE SCHEMA public;")
        .execute(&pool)
        .await
        .unwrap();
    let output = cli(&url, &["rebuild-html", "--dry-run"], None, "unused");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("迁移记录不存在"));
    for args in [
        ["rebuild-html", "--batch-size", "0"],
        ["rebuild-html", "--batch-size", "1001"],
        ["rebuild-html", "--max-batches", "0"],
        ["rebuild-html", "--max-batches", "1001"],
    ] {
        assert!(!cli(&url, &args, None, "unused").status.success());
    }
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.tables WHERE table_schema='public'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 0, "不得执行迁移或创建 SQLx 历史表");
    pool.close().await;
}

#[tokio::test]
async fn html_batch_failure_reports_its_record_and_committed_progress_then_resumes() {
    let database = "blog_html_partial_failure_test";
    let pool = common::fresh_database(database).await;
    let url = common::test_db_url(&common::admin_url(), database);
    sqlx::query("INSERT INTO users(id,username) VALUES(gen_random_uuid(),'partial-author')")
        .execute(&pool)
        .await
        .unwrap();
    for n in 1..=3 {
        let source = if n == 2 {
            format!("![missing](/media/{})", uuid::Uuid::now_v7())
        } else {
            "**valid**".into()
        };
        sqlx::query("INSERT INTO posts(id,author_id,slug,content,content_html,content_render_version) SELECT $1,id,$2,$3,'stale',2 FROM users")
            .bind(uuid::Uuid::from_u128(n)).bind(format!("partial-{n}")).bind(source).execute(&pool).await.unwrap();
    }
    let output = cli(&url, &["rebuild-html", "--batch-size", "3"], None, "unused");
    assert!(!output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["rebuilt"]["posts"], 1);
    assert_eq!(report["batches"], 1);
    assert_eq!(report["failure"]["kind"], "post");
    assert_eq!(
        report["failure"]["id"],
        uuid::Uuid::from_u128(2).to_string()
    );
    assert!(report["pending"].is_null());
    assert_eq!(report["has_more"], true);
    let versions: Vec<i32> =
        sqlx::query_scalar("SELECT content_render_version FROM posts ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(versions, [infrastructure::CONTENT_RENDER_VERSION, 2, 2]);
    sqlx::query("UPDATE posts SET content='**repaired**' WHERE id=$1")
        .bind(uuid::Uuid::from_u128(2))
        .execute(&pool)
        .await
        .unwrap();
    for remaining in [1, 0] {
        let report: serde_json::Value = serde_json::from_str(&assert_success(cli(
            &url,
            &["rebuild-html", "--batch-size", "1", "--max-batches", "1"],
            None,
            "unused",
        )))
        .unwrap();
        assert_eq!(report["rebuilt"]["posts"], 1);
        assert_eq!(report["pending"]["posts"], remaining);
        assert_eq!(report["has_more"], remaining > 0);
    }
    let audits: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_logs WHERE action='post.html.rebuild'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(audits, 3);
    pool.close().await;
}

#[tokio::test]
async fn media_cleanup_staging_keeps_formal_objects_and_needs_no_website_config() {
    let pool = common::fresh_database("blog_media_maintenance_test").await;
    let database_url = common::test_db_url(&common::admin_url(), "blog_media_maintenance_test");
    let project = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = common::media_dir("maintenance");
    std::fs::create_dir_all(dir.join("staging")).unwrap();
    std::fs::create_dir_all(dir.join("objects")).unwrap();
    for path in [
        "staging/old.part",
        "staging/recent.png",
        "objects/public.png",
    ] {
        std::fs::write(dir.join(path), b"image bytes").unwrap();
    }
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(7200);
    std::fs::File::open(dir.join("staging/old.part"))
        .unwrap()
        .set_modified(old)
        .unwrap();
    std::fs::File::open(dir.join("objects/public.png"))
        .unwrap()
        .set_modified(old)
        .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_blog"))
        .args(["media", "cleanup-staging"])
        .current_dir(&project)
        .env("DATABASE_URL", database_url)
        .env("BLOG_MIGRATIONS_DIR", project.join("migrations/postgres"))
        .env("BLOG_MEDIA_DIR", &dir)
        .env("BLOG_PUBLIC_BASE_URL", "maintenance-does-not-need-a-url")
        .env("BLOG_THEME_DIR", dir.join("missing-theme"))
        .env("BLOG_ADMIN_DIST", dir.join("missing-admin"))
        .output()
        .unwrap();
    assert!(assert_success(output).contains("已清理超期暂存文件：1"));
    assert!(!dir.join("staging/old.part").exists());
    assert!(dir.join("staging/recent.png").exists());
    assert_eq!(
        std::fs::read(dir.join("objects/public.png")).unwrap(),
        b"image bytes"
    );
    std::fs::remove_dir_all(dir).unwrap();
    pool.close().await;
}

#[tokio::test]
async fn publish_due_runs_without_website_configuration_and_is_repeatable() {
    let pool = common::fresh_database("blog_publish_due_test").await;
    let database_url = common::test_db_url(&common::admin_url(), "blog_publish_due_test");
    // More than one page batch with no due posts must still be fully drained.
    sqlx::query("INSERT INTO pages (id,title,slug,content,content_html,content_render_version,status,published_at) SELECT gen_random_uuid(),'预约页面','due-page-' || n,'正文','<p>正文</p>',1,'scheduled',now()-interval '1 minute' FROM generate_series(1,101) n")
        .execute(&pool).await.unwrap();
    let invalid_url = "no-website-needed";
    assert!(
        assert_success(cli(&database_url, &["publish-due"], None, invalid_url))
            .contains("已发布 101 条")
    );
    assert!(
        assert_success(cli(&database_url, &["publish-due"], None, invalid_url))
            .contains("已发布 0 条")
    );
    let published: i64 =
        sqlx::query_scalar("SELECT count(*) FROM pages WHERE status='published' AND version=2")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(published, 101);
}

#[tokio::test]
async fn recovery_database_guard_blocks_normal_start_publishing_and_retention_before_migrations() {
    let pool = common::fresh_database("blog_recovery_guard_test").await;
    let database_url = common::test_db_url(&common::admin_url(), "blog_recovery_guard_test");
    sqlx::raw_sql(
        "COMMENT ON DATABASE blog_recovery_guard_test IS 'blog:recovery-isolated:command-test'",
    )
    .execute(&pool)
    .await
    .unwrap();
    for args in [
        &["serve", "--addr", "127.0.0.1:0"][..],
        &["publish-due"][..],
        &["rebuild-html"][..],
    ] {
        let output = cli(&database_url, args, None, "broken-site-config");
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("恢复"));
    }
    let project = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let maintenance = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_blog"))
            .args(args)
            .current_dir(&project)
            .env("DATABASE_URL", "unused")
            .env("BLOG_MAINTENANCE_DATABASE_URL", &database_url)
            .env("BLOG_MIGRATIONS_DIR", "missing")
            .env("BLOG_PUBLIC_BASE_URL", "broken")
            .env_remove("BLOG_RECOVERY_MODE")
            .output()
            .unwrap()
    };
    let blocked = maintenance(&["maintenance", "--dry-run"]);
    assert!(!blocked.status.success());
    assert!(String::from_utf8_lossy(&blocked.stderr).contains("恢复隔离期间禁止保留期清理"));
    sqlx::raw_sql("COMMENT ON DATABASE blog_recovery_guard_test IS NULL")
        .execute(&pool)
        .await
        .unwrap();
    let json = assert_success(maintenance(&["maintenance", "--dry-run"]));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(json.trim()).unwrap()["dry_run"],
        true
    );
}
