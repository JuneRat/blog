//! 服务器集成测试共用装配：管理 DSN 推导、loopback 守卫与测试库重建。
//!
//! 与 `crates/infrastructure/tests/postgres.rs` 使用同一环境变量
//! `BLOG_TEST_ADMIN_URL`（默认 loopback 的 postgres 库），与 README/.env.example 一致。
#![allow(dead_code)]

use std::sync::Arc;

use application::password::{PasswordDeps, PasswordInteractor};
use application::ports::{LoginThrottle, SessionStore, UserRepository};

/// 测试用媒体文件根目录：每个测试进程/用例一个独立临时目录，互不干扰。
///
/// 集成测试用真实本地存储（不是内存假实现）：上传的中断补偿、幂等删除与
/// 「记录存在但文件缺失」这类跨系统一致性问题只有真实文件系统才能暴露。
pub fn media_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("blog-test-media-{name}-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&dir).expect("创建测试媒体目录失败");
    dir
}

/// 测试装配的媒体用例：真实 PostgreSQL 仓储 + 本地文件存储。
pub fn media_interactor(
    pool: sqlx::PgPool,
    root: std::path::PathBuf,
) -> Arc<application::media::MediaInteractor> {
    Arc::new(application::media::MediaInteractor::new(
        Arc::new(infrastructure::image_inspection::HeaderImageInspector),
        Arc::new(infrastructure::PostgresMediaRepository::new(pool)),
        Arc::new(infrastructure::LocalMediaStorage::new(root)),
        Arc::new(infrastructure::SystemClock),
    ))
}

/// 测试装配的媒体附着授权：与生产同构（Postgres 仓储实现窄端口）。
pub fn media_guard(pool: sqlx::PgPool) -> Arc<dyn application::ports::MediaRefGuard> {
    Arc::new(infrastructure::PostgresMediaRepository::new(pool))
}

/// 测试装配的本地密码用例：真实 Argon2id（生产参数）+ 默认限流。
///
/// 会话存储由调用方注入，保证与认证用例看到同一份状态。
pub fn password_interactor(
    user_repo: Arc<dyn UserRepository>,
    sessions: Arc<dyn SessionStore>,
) -> Arc<PasswordInteractor> {
    password_interactor_with_throttle(
        user_repo,
        sessions,
        Arc::new(infrastructure::InMemoryLoginThrottle::with_defaults()),
    )
}

/// 同 [`password_interactor`]，但注入自定义限流。
///
/// 用于把阈值调低，避免为跑满生产阈值（账号 5 / 来源地址 50 次）做同样多次真实 Argon2 校验。
pub fn password_interactor_with_throttle(
    user_repo: Arc<dyn UserRepository>,
    sessions: Arc<dyn SessionStore>,
    throttle: Arc<dyn LoginThrottle>,
) -> Arc<PasswordInteractor> {
    Arc::new(PasswordInteractor::new(PasswordDeps {
        users: user_repo,
        hasher: Arc::new(infrastructure::Argon2PasswordHasher::with_defaults()),
        throttle,
        sessions,
    }))
}

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
pub async fn fresh_database(db_name: &str) -> sqlx::PgPool {
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
        .unwrap();
    sqlx::raw_sql(&format!("CREATE DATABASE {db_name}"))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;

    let pool = infrastructure::connect(&test_dsn)
        .await
        .expect("连接测试库失败");
    infrastructure::migrate(&pool, "../../migrations/postgres")
        .await
        .expect("迁移失败");
    pool
}
