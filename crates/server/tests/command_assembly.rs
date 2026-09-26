//! Maintenance commands must remain available while website configuration or
//! derived content is broken. Each child process exercises the real CLI root.

mod common;

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

fn cli(database_url: &str, args: &[&str], password: Option<&str>, public_url: &str) -> Output {
    let project = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let missing = std::env::temp_dir().join(format!(
        "blog-assembly-unavailable-{}",
        uuid::Uuid::now_v7()
    ));
    let mut child = Command::new(env!("CARGO_BIN_EXE_blog"))
        .args(args)
        .current_dir(&project)
        .env("DATABASE_URL", database_url)
        .env("BLOG_MIGRATIONS_DIR", project.join("migrations/postgres"))
        .env("BLOG_THEME_DIR", missing.join("theme"))
        .env("BLOG_ADMIN_DIST", missing.join("admin"))
        .env("BLOG_PUBLIC_BASE_URL", public_url)
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
    let migrations: Vec<i64> = sqlx::query_scalar("SELECT version FROM _sqlx_migrations")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(migrations, vec![1]);
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
    let pending: i32 =
        sqlx::query_scalar("SELECT content_render_version FROM posts WHERE slug = 'assembly-post'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(pending, 2, "身份维护不得触发内容派生物重建");

    // Restore valid content so serve reaches its own configuration validation.
    sqlx::query("UPDATE posts SET content = 'body' WHERE slug = 'assembly-post'")
        .execute(&pool)
        .await
        .unwrap();
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
