//! 基础设施集成测试共用装配：管理 DSN 推导、loopback 守卫与测试库重建。
//!
//! 与 `crates/server/tests/common/mod.rs` 使用同一环境变量 `BLOG_TEST_ADMIN_URL`
//! （默认 loopback 的 postgres 库），与 README/.env.example 一致。
//! **每个测试二进制用不同的库名**：`cargo test` 并行运行各测试二进制，
//! 同名库会被另一个进程的 DROP/CREATE 打断。
#![allow(dead_code)]

use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

pub fn admin_url() -> String {
    std::env::var("BLOG_TEST_ADMIN_URL")
        .unwrap_or_else(|_| "postgres://blog:blog@127.0.0.1:5432/postgres".into())
}

/// 从管理 DSN 推导同一主机上的测试库 DSN（替换最后一段路径）。
pub fn test_db_url(admin: &str, db_name: &str) -> String {
    let base = admin.trim_end_matches('/');
    let idx = base
        .rfind('/')
        .expect("管理 DSN 缺少路径段，形如 postgres://user:pass@host:port/postgres");
    format!("{}/{db_name}", &base[..idx])
}

/// 破坏性测试守卫：只允许 loopback 主机，防止误删远端同名库。
pub fn assert_loopback(admin: &str) {
    let after_scheme = admin.split("://").nth(1).unwrap_or_default();
    let host_port = after_scheme
        .rsplit_once('@')
        .map(|(_, rest)| rest)
        .unwrap_or(after_scheme);
    let host = host_port.split([':', '/']).next().unwrap_or_default();
    assert!(
        matches!(host, "127.0.0.1" | "::1" | "localhost"),
        "拒绝在非 loopback 主机 {host} 上执行破坏性测试（BLOG_TEST_ADMIN_URL 指向了远端？）"
    );
}

/// 删除并重建指定测试库（同文件内测试串行使用），返回迁移后的连接池。
pub async fn fresh_database(db_name: &str) -> PgPool {
    let admin_dsn = admin_url();
    assert_loopback(&admin_dsn);
    let test_dsn = test_db_url(&admin_dsn, db_name);

    let admin = infrastructure::connect(&admin_dsn)
        .await
        .expect("连接管理库失败");
    // raw_sql 走简单协议且不包事务；CREATE/DROP DATABASE 不能在事务块内执行。
    sqlx::raw_sql(&format!("DROP DATABASE IF EXISTS {db_name} WITH (FORCE)"))
        .execute(&admin)
        .await
        .expect("删除旧测试库失败");
    sqlx::raw_sql(&format!("CREATE DATABASE {db_name}"))
        .execute(&admin)
        .await
        .expect("创建测试库失败");
    admin.close().await;

    let pool = infrastructure::connect(&test_dsn)
        .await
        .expect("连接测试库失败");
    infrastructure::migrate(&pool, "../../migrations/postgres")
        .await
        .expect("迁移失败");
    pool
}

/// 建一个可登录用户（sessions.user_id 的外键目标）。
pub async fn seed_user(pool: &PgPool, username: &str) -> Uuid {
    let id = Uuid::now_v7();
    let now = OffsetDateTime::now_utc();
    sqlx::query(
        "INSERT INTO users (id, username, display_name, version, created_at, updated_at) \
         VALUES ($1, $2, $3, 1, $4, $4)",
    )
    .bind(id)
    .bind(username)
    .bind(format!("{username}的展示名"))
    .bind(now)
    .execute(pool)
    .await
    .expect("写入测试用户失败");
    id
}
