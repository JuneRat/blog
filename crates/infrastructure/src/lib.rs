//! 基础设施层：持久化、渲染等出站适配器。
//! 实现应用层端口并隐藏具体库类型；数据库事务对象不暴露给 application。

pub mod audit;
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
    CONTENT_RENDER_VERSION, PgHealthCheck, PostgresCategoryRepository, PostgresMediaRepository,
    PostgresPageRepository, PostgresPostRepository, PostgresPublishedCategoryQuery,
    PostgresPublishedPageQuery, PostgresPublishedPostQuery, PostgresPublishedSeriesQuery,
    PostgresPublishedTagQuery, PostgresSeriesRepository, PostgresTagRepository,
    PostgresUserRepository, SystemClock, connect, migrate, migrate_schema, publish_due_content,
    rebuild_content_html,
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
pub mod comments;

pub mod image_inspection;
