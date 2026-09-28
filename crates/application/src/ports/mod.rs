//! 出站端口按业务职责组织；这里只导出调用方使用的显式契约。
//!
//! 下述原子提交、并发校验与快照约束是事务语义要求，不是特定数据库的实现细节。
//! 替代适配器必须保持相同可观察结果；可使用串行化事务、条件写入等不同机制。
//! 具体 SQL、锁类型和获取顺序由 infrastructure 决定，不要求实现同一套锁。

mod content;
mod content_queries;
mod identity;
mod media;
mod rendering;
mod runtime;
mod site;
mod taxonomy;

pub use content::{
    PageCommitOutcome, PageDeleteOutcome, PageRepository, PostCommitOutcome, PostRecord,
    PostRepository, PublicCategoryRef, PublicCategorySummary, PublicPageDetail, PublicPostDetail,
    PublicPostSummary, PublicSeriesRef, PublicSeriesSummary, PublicTagRef, PublicTagSummary,
    PublicUrlEntry, PublishedCategoryQuery, PublishedPageQuery, PublishedPostQuery,
    PublishedSeriesQuery, PublishedTagQuery,
};
pub use content_queries::{AdminPageQuery, AdminPostQuery};
pub use identity::{
    AccountAdministration, AdminUserRow, ClearPasswordOutcome, ExternalIdentity,
    ExternalIdentityClient, LoginThrottle, OAUTH_STATE_COOKIE, OAuthAccountStore, OAuthAttempt,
    OAuthAttemptStore, OAuthConfigStore, PasswordCredential, PasswordCredentialStore,
    PasswordHasher, ProviderConfig, ProviderKind, RbacStore, RoleDto, SESSION_COOKIE, SecretSource,
    SecureRandom, SessionRecord, SessionStore, ThrottleDecision, ThrottleSubject, UserProfileStore,
    UserQuery,
};
pub use media::{
    ImageInspector, MediaChangeOutcome, MediaContentKind, MediaRefGuard, MediaRepository,
    MediaStorage, MediaUsageRow, MediaUsageSource, MediaWithUsage, SITE_MEDIA_CONTENT_ID,
};
pub use rendering::{CommentRenderer, ContentRenderer, RenderedContent, ThemeRenderer};
pub use runtime::{Clock, HealthCheck, SaveOutcome};
pub use site::{
    SettingsStore, SiteSettingsRecord, SiteSettingsValue, ThemeSettingsRecord, ThemeSettingsStore,
};
pub use taxonomy::{
    CategoryDeleteOutcome, CategoryRepository, CategoryWithUsage, ReorderOutcome,
    SeriesDeleteOutcome, SeriesMember, SeriesRepository, SeriesWithUsage, TagDeleteOutcome,
    TagRepository, TagWithUsage,
};
