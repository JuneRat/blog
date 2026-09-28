use async_trait::async_trait;
use sqlx::PgPool;
use sqlx::migrate::Migrate;
use sqlx::postgres::PgPoolOptions;
use time::OffsetDateTime;

use application::error::UseCaseError;
use application::ports::{Clock, HealthCheck};

pub async fn connect(url: &str) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(5)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .connect(url)
        .await
}

/// 从目录加载并执行迁移（sqlx 布局 `<version>_<description>.sql`，每条自动包事务）。
/// 所有命令只在这里准备或校验结构；HTML 重建由显式维护入口执行。
pub async fn migrate_schema(
    pool: &PgPool,
    migrations_dir: impl AsRef<std::path::Path>,
) -> Result<(), UseCaseError> {
    let legacy: bool = sqlx::query_scalar(
        "SELECT to_regclass('users') IS NOT NULL AND NOT EXISTS ( \
         SELECT 1 FROM information_schema.columns \
         WHERE table_schema = current_schema() AND table_name = 'users' AND column_name = 'auth_version')",
    )
    .fetch_one(pool)
    .await
    .map_err(|error| UseCaseError::Repository(error.to_string()))?;
    if legacy {
        return Err(UseCaseError::Repository(
            "检测到旧版数据库。新初始迁移仅支持空库；请显式重建指定开发库或改用新空库，程序不会自动清库。".into(),
        ));
    }
    let migrator = sqlx::migrate::Migrator::new(migrations_dir.as_ref())
        .await
        .map_err(|e| UseCaseError::Repository(format!("加载迁移失败：{e}")))?;
    // SQLx creates its history table even when every migration is applied.
    // An application role must not own the schema merely to start the server.
    let can_create: bool =
        sqlx::query_scalar("SELECT has_schema_privilege(current_schema(),'CREATE')")
            .fetch_one(pool)
            .await
            .map_err(|e| UseCaseError::Repository(e.to_string()))?;
    if !can_create {
        // Share SQLx's migration lock so verification cannot observe history
        // halfway through an owner's migration. Never pool a locked session,
        // including on errors or cancellation.
        let mut connection = pool
            .acquire()
            .await
            .map_err(|e| UseCaseError::Repository(e.to_string()))?;
        connection.close_on_drop();
        connection
            .lock()
            .await
            .map_err(|e| UseCaseError::Repository(e.to_string()))?;
        let exists: bool = sqlx::query_scalar("SELECT to_regclass('_sqlx_migrations') IS NOT NULL")
            .fetch_one(&mut *connection)
            .await
            .map_err(|e| UseCaseError::Repository(e.to_string()))?;
        if !exists {
            return Err(UseCaseError::Repository(
                "运行账号不能建表；请先使用结构管理账号执行 migrate 和授权脚本".into(),
            ));
        }
        let applied: Vec<(i64, Vec<u8>, bool)> = sqlx::query_as(
            "SELECT version,checksum,success FROM _sqlx_migrations ORDER BY version",
        )
        .fetch_all(&mut *connection)
        .await
        .map_err(|e| UseCaseError::Repository(e.to_string()))?;
        let expected: Vec<_> = migrator
            .iter()
            .filter(|m| m.migration_type.is_up_migration())
            .collect();
        if applied.len() != expected.len()
            || applied
                .iter()
                .zip(expected)
                .any(|((version, checksum, success), migration)| {
                    !success
                        || *version != migration.version
                        || checksum.as_slice() != migration.checksum.as_ref()
                })
        {
            return Err(UseCaseError::Repository(
                "迁移版本或校验和不匹配；请先使用匹配版本与结构管理账号完成迁移".into(),
            ));
        }
        connection
            .close()
            .await
            .map_err(|e| UseCaseError::Repository(e.to_string()))?;
        return Ok(());
    }
    migrator
        .run(pool)
        .await
        .map_err(|e| UseCaseError::Repository(format!("数据库迁移失败：{e}")))?;
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
