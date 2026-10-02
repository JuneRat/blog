//! PostgreSQL 出站适配器，按业务边界组织事务与查询。
//!
//! UUID 与业务时间由应用生成；写侧通过条件更新执行乐观并发协议。
//! 模块内部共享 SQL 错误映射和跨业务使用的媒体引用锁协议。

mod connection;
mod content;
mod content_queries;
mod html_rebuild;
mod pool;
pub use content_queries::PostgresAdminContentQuery;
pub use html_rebuild::PostgresHtmlRebuildStore;
mod identity;
mod media;
mod sql;
mod taxonomy;

pub use connection::{PgHealthCheck, SystemClock, migrate_schema, verify_schema};
pub use content::{
    CONTENT_RENDER_VERSION, PostgresPageRepository, PostgresPostRepository,
    PostgresPublishedPageQuery, PostgresPublishedPostQuery, PostgresScheduledPublicationStore,
};
pub use identity::PostgresUserRepository;
pub use media::PostgresMediaRepository;
pub use pool::{DatabasePoolConfig, connect, connect_with_config};
pub use taxonomy::{
    PostgresCategoryRepository, PostgresPublishedCategoryQuery, PostgresPublishedSeriesQuery,
    PostgresPublishedTagQuery, PostgresSeriesRepository, PostgresTagRepository,
};

pub(crate) use identity::acquire_identity_lock;
pub(crate) use media::{media_ids_for, sync_media_refs};

pub(crate) mod registration;
pub use registration::PostgresRegistrationStore;

mod account_links;
pub use account_links::PostgresAccountLinkStore;

mod discovery;
pub use discovery::PostgresPublicDiscoveryQuery;
