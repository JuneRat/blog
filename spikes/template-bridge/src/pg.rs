//! 原型的 PostgreSQL 侧：测试库装配（raw SQL，不用生产迁移）与查询门面。
//!
//! 管理连接与守卫风格照抄 crates/server/tests/common/mod.rs：
//! `BLOG_TEST_ADMIN_URL`（默认 loopback postgres 库）→ 推导同主机测试库 DSN
//! → 非 loopback 直接 panic 拒绝。测试库为破坏性重建（DROP DATABASE）。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use minijinja::Value;
use serde::Serialize;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

/// 测试库名：替换管理 DSN 的最后路径段。
pub const DB_NAME: &str = "spike_bridge_test";

pub fn admin_url() -> String {
    std::env::var("BLOG_TEST_ADMIN_URL")
        .unwrap_or_else(|_| "postgres://blog:blog@127.0.0.1:5432/postgres".into())
}

/// 从管理 DSN 推导同主机上的测试库 DSN（替换最后一段路径）。
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

async fn connect(dsn: &str, max_connections: u32) -> PgPool {
    PgPoolOptions::new()
        .max_connections(max_connections)
        .connect(dsn)
        .await
        .unwrap_or_else(|e| panic!("连接 {dsn} 失败：{e}"))
}

/// 删除并重建测试库，用 raw SQL 建最小表并播种；返回测试库 DSN。
///
/// 每个测试自建连接池（`connect_test`）：sqlx 连接池绑定创建它的 runtime，
/// `#[tokio::test]` 每测一个 runtime，共享池会在首测 runtime 销毁后挂死。
/// DDL 刻意不使用生产迁移目录：原型只验证桥接，不共享生产 schema。
pub async fn recreate_database() -> String {
    let admin_dsn = admin_url();
    assert_loopback(&admin_dsn);
    let test_dsn = test_db_url(&admin_dsn, DB_NAME);

    let admin = connect(&admin_dsn, 2).await;
    // raw_sql 走简单协议且不包事务；CREATE/DROP DATABASE 不能在事务块内执行。
    sqlx::raw_sql(&format!("DROP DATABASE IF EXISTS {DB_NAME} WITH (FORCE)"))
        .execute(&admin)
        .await
        .expect("删除旧测试库失败");
    sqlx::raw_sql(&format!("CREATE DATABASE {DB_NAME}"))
        .execute(&admin)
        .await
        .expect("创建测试库失败");
    admin.close().await;

    let pool = connect(&test_dsn, 4).await;
    sqlx::raw_sql(
        "CREATE TABLE spike_posts (
            id uuid PRIMARY KEY,
            slug text NOT NULL,
            title text NOT NULL,
            published boolean NOT NULL DEFAULT true
        )",
    )
    .execute(&pool)
    .await
    .expect("建表失败");

    let mut seed = String::from("INSERT INTO spike_posts (id, slug, title) VALUES ");
    for i in 0..50 {
        if i > 0 {
            seed.push(',');
        }
        seed.push_str(&format!(
            "('{}', 'post-{:02}', 'Post {:02}')",
            Uuid::new_v4(),
            i,
            i
        ));
    }
    sqlx::raw_sql(&seed).execute(&pool).await.expect("播种失败");
    pool.close().await;
    test_dsn
}

/// 连接（已存在的）测试库，自建连接池；调用方 runtime 必须与使用方一致。
pub async fn connect_test(dsn: &str, max_connections: u32) -> PgPool {
    connect(dsn, max_connections).await
}

// ---------------------------------------------------------------------------
// 查询门面：key → 异步查询 future
// ---------------------------------------------------------------------------

/// 查询 future：Value 或数据库错误字符串。
pub type BoxedQuery = Pin<Box<dyn Future<Output = Result<Value, String>> + Send>>;
/// 查询门面：application 层 ThemeDataProvider 的原型替身。
pub type QueryFacade = Arc<dyn Fn(&str) -> BoxedQuery + Send + Sync>;

/// 分页摘要 DTO：与主题 API 草案的 `get_posts(...) → { items: [...] }` 对齐。
#[derive(Serialize)]
pub struct PostList {
    pub items: Vec<PostDto>,
}

/// 查询返回的文章摘要 DTO（只暴露公开字段）。
#[derive(Serialize)]
pub struct PostDto {
    pub id: Uuid,
    pub slug: String,
    pub title: String,
}

/// 构造真实 sqlx 查询门面；返回值附带门面级查询计数器（跨请求观测）。
///
/// 支持的 key：
/// - `posts:<limit>:<offset>`：真实 SELECT spike_posts（已发布，slug 稳定排序）；
/// - `sleep:<ms>`：pg_sleep 慢查询；
/// - `boom`：SELECT 1/0，必然 SQL 错误；
/// - `echo:<x>`：不查库立即返回，用于预算/deadline 高频调用。
pub fn query_facade(pool: PgPool) -> (QueryFacade, Arc<AtomicUsize>) {
    let counter = Arc::new(AtomicUsize::new(0));
    let c = counter.clone();
    let facade = move |key: &str| -> BoxedQuery {
        c.fetch_add(1, Ordering::Relaxed);
        let pool = pool.clone();
        let key = key.to_string();
        Box::pin(async move {
            if key == "boom" {
                sqlx::query("SELECT 1/0")
                    .execute(&pool)
                    .await
                    .map_err(|e| e.to_string())?;
                return Ok(Value::UNDEFINED);
            }
            if let Some(ms) = key.strip_prefix("sleep:") {
                let secs: f64 = ms.parse::<f64>().map_err(|e| e.to_string())? / 1000.0;
                sqlx::query("SELECT pg_sleep($1)")
                    .bind(secs)
                    .execute(&pool)
                    .await
                    .map_err(|e| e.to_string())?;
                return Ok(Value::from(ms.to_string()));
            }
            if let Some(rest) = key.strip_prefix("echo:") {
                return Ok(Value::from(rest.to_string()));
            }
            if let Some(args) = key.strip_prefix("posts:") {
                let (limit, offset) = args
                    .split_once(':')
                    .ok_or_else(|| format!("非法 posts 查询键：{key}"))?;
                let (limit, offset): (i64, i64) = (
                    limit
                        .parse()
                        .map_err(|e: std::num::ParseIntError| e.to_string())?,
                    offset
                        .parse()
                        .map_err(|e: std::num::ParseIntError| e.to_string())?,
                );
                let rows = sqlx::query_as::<_, (Uuid, String, String)>(
                    "SELECT id, slug, title FROM spike_posts
                     WHERE published ORDER BY slug, id LIMIT $1 OFFSET $2",
                )
                .bind(limit)
                .bind(offset)
                .fetch_all(&pool)
                .await
                .map_err(|e| e.to_string())?;
                let items: Vec<PostDto> = rows
                    .into_iter()
                    .map(|(id, slug, title)| PostDto { id, slug, title })
                    .collect();
                return Ok(Value::from_serialize(&PostList { items }));
            }
            Err(format!("未知查询键：{key}"))
        })
    };
    (Arc::new(facade), counter)
}
