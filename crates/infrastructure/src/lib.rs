//! 基础设施层：持久化、渲染等出站适配器。
//! 实现应用层端口并隐藏具体库类型；数据库事务对象不暴露给 application。

pub mod media_refs;
pub mod media_storage;
pub mod oauth;
pub mod password;
pub mod persistence;
pub mod rbac;
pub mod rendering;
pub mod sessions;
pub mod settings;
mod theme_functions;
pub mod throttle;

pub use media_storage::LocalMediaStorage;
pub use oauth::{
    EnvSecretSource, PostgresOAuthAccountStore, PostgresOAuthConfigStore, ReqwestIdentityClient,
};
pub use password::Argon2PasswordHasher;
pub use persistence::{
    PgHealthCheck, PostgresCategoryRepository, PostgresMediaRepository, PostgresPageRepository,
    PostgresPostRepository, PostgresPublishedCategoryQuery, PostgresPublishedPageQuery,
    PostgresPublishedPostQuery, PostgresPublishedSeriesQuery, PostgresPublishedTagQuery,
    PostgresSeriesRepository, PostgresTagRepository, PostgresUserRepository, SystemClock, connect,
    migrate,
};
pub use rbac::PostgresRbacStore;
pub use rendering::{MiniJinjaThemeRenderer, SanitizingMarkdownRenderer};
pub use sessions::{
    AttemptStoreConfig, InMemoryOAuthAttemptStore, InMemorySessionStore, SessionStoreConfig,
    SystemSecureRandom,
};
pub use settings::PostgresSettingsStore;
pub use throttle::{InMemoryLoginThrottle, ThrottleConfig};
