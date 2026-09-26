use async_trait::async_trait;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use time::OffsetDateTime;

use application::error::UseCaseError;
use application::ports::{Clock, HealthCheck};

use super::content::rebuild_content_html;

pub async fn connect(url: &str) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(5)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .connect(url)
        .await
}

/// 从目录加载并执行迁移（sqlx 布局 `<version>_<description>.sql`，每条自动包事务）。
/// 身份与媒体维护只需要结构就绪，不依赖正文渲染或内容引用补齐。
pub async fn migrate_schema(
    pool: &PgPool,
    migrations_dir: impl AsRef<std::path::Path>,
) -> Result<(), UseCaseError> {
    let migrator = sqlx::migrate::Migrator::new(migrations_dir.as_ref())
        .await
        .map_err(|e| UseCaseError::Repository(format!("加载迁移失败：{e}")))?;
    migrator
        .run(pool)
        .await
        .map_err(|e| UseCaseError::Repository(format!("数据库迁移失败：{e}")))?;
    Ok(())
}

/// 执行结构迁移，并补齐公开读取所需的持久化 HTML 与媒体引用。
pub async fn migrate(
    pool: &PgPool,
    migrations_dir: impl AsRef<std::path::Path>,
) -> Result<(), UseCaseError> {
    migrate_schema(pool, migrations_dir).await?;
    rebuild_content_html(pool, &crate::rendering::RenderingRuntime::default()).await?;
    Ok(())
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }
}

/// readiness 探针：真实执行 SELECT 1，连接池不可用时报告不健康。
pub struct PgHealthCheck {
    pool: PgPool,
}

impl PgHealthCheck {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl HealthCheck for PgHealthCheck {
    async fn check(&self) -> bool {
        sqlx::query_scalar::<_, i32>("SELECT 1")
            .fetch_one(&self.pool)
            .await
            .is_ok()
    }
}
