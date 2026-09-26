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
        "UPDATE posts SET content = $1, content_render_version = 0 WHERE slug = 'assembly-post'",
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
    assert_eq!(pending, 0, "身份维护不得触发内容派生物重建");

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
