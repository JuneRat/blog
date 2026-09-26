//! 出站端口按业务职责组织；这里只导出调用方使用的显式契约。

mod content;
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
pub use identity::{
    AdminUserRow, ClearPasswordOutcome, ExternalIdentity, ExternalIdentityClient, LoginThrottle,
    OAUTH_STATE_COOKIE, OAuthAccountStore, OAuthAttempt, OAuthAttemptStore, OAuthConfigStore,
    PasswordCredential, PasswordHasher, ProviderConfig, ProviderKind, RbacStore, RoleDto,
    SESSION_COOKIE, SecretSource, SecureRandom, SessionRecord, SessionStore, ThrottleDecision,
    ThrottleSubject, UserRepository,
};
pub use media::{
    ImageInspector, MediaAttachStatus, MediaContentKind, MediaDeleteOutcome, MediaRefGuard,
    MediaRepository, MediaStorage, MediaUsageRow, MediaWithUsage, SITE_MEDIA_CONTENT_ID,
};
pub use rendering::{ContentRenderer, RenderedContent, ThemeRenderer};
pub use runtime::{Clock, HealthCheck, SaveOutcome};
pub use site::{
    SettingsStore, SiteSettingsRecord, SiteSettingsValue, ThemeSettingsRecord, ThemeSettingsStore,
};
pub use taxonomy::{
    CategoryDeleteOutcome, CategoryRepository, CategoryWithUsage, ReorderOutcome,
    SeriesDeleteOutcome, SeriesMember, SeriesRepository, SeriesWithUsage, TagDeleteOutcome,
    TagRepository, TagWithUsage,
};
