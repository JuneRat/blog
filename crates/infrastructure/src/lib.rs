//! 基础设施层：持久化、渲染等出站适配器。
//! 实现应用层端口并隐藏具体库类型；数据库事务对象不暴露给 application。

pub mod audit;
pub mod installation;
mod media_refs;
pub mod media_storage;
pub mod oauth;
pub mod password;
pub mod persistence;
pub mod rbac;
mod render_executor;
pub mod rendering;
pub mod sessions;
pub mod settings;
mod theme_functions;
mod theme_validation;
pub mod throttle;

pub use media_storage::LocalMediaStorage;
pub use oauth::{
    EnvSecretSource, PostgresOAuthAccountStore, PostgresOAuthConfigStore, ReqwestIdentityClient,
};
pub use password::Argon2PasswordHasher;
pub use persistence::{
    CONTENT_RENDER_VERSION, PgHealthCheck, PostgresAdminContentQuery, PostgresCategoryRepository,
    PostgresHtmlRebuildStore, PostgresMediaRepository, PostgresPageRepository,
    PostgresPostRepository, PostgresPublishedCategoryQuery, PostgresPublishedPageQuery,
    PostgresPublishedPostQuery, PostgresPublishedSeriesQuery, PostgresPublishedTagQuery,
    PostgresScheduledPublicationStore, PostgresSeriesRepository, PostgresTagRepository,
    PostgresUserRepository, SystemClock, connect, migrate_schema, verify_schema,
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
