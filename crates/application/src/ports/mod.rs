//! 出站端口按业务职责组织；这里只导出调用方使用的显式契约。
//!
//! 下述原子提交、并发校验与快照约束是事务语义要求，不是特定数据库的实现细节。
//! 替代适配器必须保持相同可观察结果；可使用串行化事务、条件写入等不同机制。
//! 具体 SQL、锁类型和获取顺序由 infrastructure 决定，不要求实现同一套锁。

mod admission;
mod comments;
mod content;
mod content_queries;
mod identity;
mod media;
mod rendering;
mod runtime;
mod site;
mod taxonomy;

pub use admission::{PublicRequest, RequestAdmission};
pub use comments::CommentRepository;
pub use content::{
    PageCommitOutcome, PageDeleteOutcome, PageRepository, PostCommitOutcome, PostRecord,
    PostRepository, PublicCategoryRef, PublicCategorySummary, PublicPageDetail, PublicPostDetail,
    PublicPostSummary, PublicSeriesRef, PublicSeriesSummary, PublicTagRef, PublicTagSummary,
    PublicUrlEntry, PublishedCategoryQuery, PublishedPageQuery, PublishedPostQuery,
    PublishedSeriesQuery, PublishedTagQuery,
};
pub use content_queries::{AdminPageQuery, AdminPostQuery};
pub use identity::{
    AccountAdministration, AdminUserPage, AdminUserRow, ClearPasswordOutcome, ExternalIdentity,
    ExternalIdentityClient, LoginThrottle, OAUTH_STATE_COOKIE, OAuthAccountSnapshot,
    OAuthAccountStore, OAuthAttempt, OAuthAttemptStore, OAuthConfigSnapshot, OAuthConfigStore,
    PasswordCredential, PasswordCredentialStore, PasswordHasher, ProviderConfig, ProviderKind,
    RbacStore, RoleDto, SESSION_COOKIE, SecretSource, SecureRandom, SessionRecord, SessionStore,
    ThrottleDecision, ThrottleSubject, UserProfileStore, UserQuery,
};
pub use media::{
    ImageInspector, MediaChangeOutcome, MediaContentKind, MediaReader, MediaRefGuard,
    MediaRepository, MediaStorage, MediaUsageRow, MediaUsageSource, MediaWithUsage, OpenedMedia,
    SITE_MEDIA_CONTENT_ID,
};
pub use rendering::{
    CONTENT_RENDER_VERSION, CommentRenderer, ContentRenderer, DateTimeFormatter, RenderedContent,
    RenderedPreview, ThemeRenderer, TimeZoneProvider,
};
pub use runtime::{Clock, HealthCheck, SaveOutcome};
pub use site::{
    SettingsReadObserver, SettingsStore, SiteSettingsReadOutcome, SiteSettingsRecord,
    SiteSettingsValue, ThemeSettingsRecord, ThemeSettingsStore,
};
pub use taxonomy::{
    CategoryDeleteOutcome, CategoryLookup, CategoryRepository, CategoryWithUsage, ReorderOutcome,
    SeriesDeleteOutcome, SeriesLookup, SeriesMember, SeriesRepository, SeriesWithUsage,
    TagDeleteOutcome, TagLookup, TagRepository, TagWithUsage,
};
