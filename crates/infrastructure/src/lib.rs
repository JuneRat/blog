//! 基础设施层：持久化、渲染等出站适配器。
//! 实现应用层端口；默认生产 API 封装数据库连接和错误类型，事务对象不暴露给 application。

mod database;
mod locks;
pub use database::{Database, DatabaseError, PoolSnapshot};

/// Raw-pool access for database integration fixtures, excluded from default builds.
#[cfg(feature = "sqlx-test-support")]
pub mod test_support {
    pub fn database(pool: sqlx::PgPool) -> crate::Database {
        crate::Database { pool }
    }

    pub fn pool(database: crate::Database) -> sqlx::PgPool {
        database.pool
    }
}

mod admission;
pub use admission::InMemoryRequestAdmission;
pub mod audit;
pub mod installation;
pub mod media_cleanup;
mod media_refs;
pub mod media_storage;
pub mod oauth;
pub mod password;
pub mod persistence;
pub mod plugins;
pub mod rbac;
mod render_executor;
pub mod rendering;
pub mod schema_contract;
pub mod sessions;
pub mod settings;
pub mod tasks;
pub use tasks::PostgresTaskStore;
mod theme_functions;
pub mod theme_packages;
mod theme_validation;
mod time_zone;
pub use time_zone::{IanaTimeZones, SiteTimeZone};
pub mod throttle;

pub use media_storage::LocalMediaStorage;
pub use oauth::{
    EnvSecretSource, PostgresOAuthAccountStore, PostgresOAuthConfigStore, ReqwestIdentityClient,
};
pub use password::Argon2PasswordHasher;
pub use persistence::{
    CONTENT_RENDER_VERSION, DatabasePoolConfig, PgHealthCheck, PostgresAdminContentQuery,
    PostgresCategoryRepository, PostgresHtmlRebuildStore, PostgresMediaRepository,
    PostgresPageRepository, PostgresPostRepository, PostgresPublishedCategoryQuery,
    PostgresPublishedPageQuery, PostgresPublishedPostQuery, PostgresPublishedSeriesQuery,
    PostgresPublishedTagQuery, PostgresScheduledPublicationStore, PostgresSeriesRepository,
    PostgresTagRepository, PostgresUserRepository, SystemClock, connect, connect_with_config,
    migrate_schema, verify_schema,
};
pub use rbac::PostgresRbacStore;
pub use rendering::{
    MiniJinjaThemeRenderer, RenderingLimits, RenderingRuntime, SanitizingMarkdownRenderer,
};
pub use sessions::{
    AttemptStoreConfig, InMemoryOAuthAttemptStore, InMemorySessionStore, PostgresSessionStore,
    SessionStoreConfig, SystemSecureRandom,
};
pub use settings::PostgresSettingsStore;
pub use throttle::{InMemoryLoginThrottle, ThrottleConfig};
mod comment_rendering;
pub mod comments;
pub use comment_rendering::COMMENT_RENDER_VERSION;

pub mod image_inspection;

pub mod retention;

pub use persistence::PostgresRegistrationStore;
