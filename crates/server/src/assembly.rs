//! Narrow command assemblies. These constructors never read website settings
//! or load themes and static assets.

use std::path::PathBuf;
use std::sync::Arc;

use application::auth::{AuthDeps, AuthInteractor, OAuthManagementInteractor};
use application::content::PostInteractor;
use application::identity::{RoleInteractor, UserInteractor, UserStores};
use application::media::MediaInteractor;
use application::password::{PasswordDeps, PasswordInteractor};
use application::ports::{ContentRenderer, SessionStore};
use infrastructure::Database;
use infrastructure::{
    PostgresCategoryRepository, PostgresMediaRepository, PostgresPostRepository, PostgresRbacStore,
    PostgresSeriesRepository, PostgresTagRepository, PostgresUserRepository, SystemClock,
};
use interfaces::cli::{PostCliDeps, UserCliDeps};

pub fn publisher(pool: &Database) -> application::publishing::PublishDueInteractor {
    application::publishing::PublishDueInteractor::new(
        Arc::new(infrastructure::PostgresScheduledPublicationStore::new(
            pool.clone(),
        )),
        Arc::new(SystemClock),
    )
}

pub fn retention_maintenance(pool: &Database) -> application::retention::RetentionMaintenance {
    application::retention::RetentionMaintenance::new(Arc::new(
        infrastructure::retention::PostgresRetentionCleanupStore::new(pool.clone()),
    ))
}

pub fn html_rebuilder(pool: &Database) -> application::html_rebuild::HtmlRebuildInteractor {
    let runtime = Arc::new(infrastructure::RenderingRuntime::default());
    application::html_rebuild::HtmlRebuildInteractor::new(Arc::new(
        infrastructure::PostgresHtmlRebuildStore::new(pool.clone(), runtime.clone(), runtime),
    ))
}

pub fn users(pool: &Database) -> Arc<UserInteractor> {
    let store = Arc::new(PostgresUserRepository::new(pool.clone()));
    Arc::new(UserInteractor::new(
        UserStores {
            query: store.clone(),
            profiles: store.clone(),
            accounts: store,
        },
        Arc::new(PostgresRbacStore::new(pool.clone())),
        Arc::new(SystemClock),
        Arc::new(PostgresMediaRepository::new(pool.clone())),
    ))
}

pub fn roles(pool: &Database) -> Arc<RoleInteractor> {
    Arc::new(RoleInteractor::new(
        Arc::new(PostgresRbacStore::new(pool.clone())),
        Arc::new(PostgresUserRepository::new(pool.clone())),
    ))
}

pub fn sessions(pool: &Database) -> Arc<dyn SessionStore> {
    Arc::new(infrastructure::PostgresSessionStore::with_defaults(
        pool.clone(),
    ))
}

pub fn passwords(pool: &Database, sessions: Arc<dyn SessionStore>) -> Arc<PasswordInteractor> {
    let store = Arc::new(PostgresUserRepository::new(pool.clone()));
    Arc::new(PasswordInteractor::new(PasswordDeps {
        users: store.clone(),
        credentials: store,
        hasher: Arc::new(infrastructure::Argon2PasswordHasher::with_defaults()),
        throttle: Arc::new(infrastructure::InMemoryLoginThrottle::with_defaults()),
        sessions,
    }))
}

pub fn user_commands(pool: &Database) -> UserCliDeps {
    UserCliDeps {
        users: users(pool),
        passwords: passwords(pool, sessions(pool)),
    }
}

pub fn oauth_commands(pool: &Database) -> OAuthManagementInteractor {
    OAuthManagementInteractor::new(
        Arc::new(infrastructure::PostgresOAuthConfigStore::new(pool.clone())),
        Arc::new(infrastructure::PostgresOAuthAccountStore::new(pool.clone())),
        sessions(pool),
        users(pool),
    )
}

pub fn posts(pool: &Database, renderer: Arc<dyn ContentRenderer>) -> Arc<PostInteractor> {
    Arc::new(PostInteractor::new(
        Arc::new(PostgresPostRepository::new(pool.clone(), renderer)),
        Arc::new(PostgresTagRepository::new(pool.clone())),
        Arc::new(PostgresCategoryRepository::new(pool.clone())),
        Arc::new(PostgresSeriesRepository::new(pool.clone())),
        Arc::new(SystemClock),
        Arc::new(PostgresMediaRepository::new(pool.clone())),
    ))
}

pub fn post_commands(pool: &Database, renderer: Arc<dyn ContentRenderer>) -> PostCliDeps {
    PostCliDeps {
        users: users(pool),
        posts: posts(pool, renderer),
        content_queries: content_queries(pool),
    }
}

pub fn media(pool: &Database, root: PathBuf) -> Arc<MediaInteractor> {
    Arc::new(MediaInteractor::new(
        Arc::new(infrastructure::image_inspection::HeaderImageInspector),
        Arc::new(PostgresMediaRepository::new(pool.clone())),
        Arc::new(infrastructure::LocalMediaStorage::new(root)),
        Arc::new(SystemClock),
    ))
}

pub fn media_cleanup(
    pool: &Database,
    root: PathBuf,
    legacy_container: Option<String>,
) -> application::media_cleanup::MediaCleanup {
    use infrastructure::media_cleanup::{
        LocalMediaPurgeFiles, LocalMediaPurgePlans, PostgresMediaPurgeStore,
    };
    application::media_cleanup::MediaCleanup::new(
        Arc::new(PostgresMediaPurgeStore::new(pool.clone(), legacy_container)),
        Arc::new(LocalMediaPurgeFiles::new(root)),
        Arc::new(LocalMediaPurgePlans),
        Arc::new(SystemClock),
    )
}

/// Browser authentication is assembled only for the HTTP server. OAuth CLI
/// maintenance uses `oauth_commands`, with no login client or callback URL.
pub fn auth(
    pool: &Database,
    users: Arc<UserInteractor>,
    sessions: Arc<dyn SessionStore>,
    base_url: String,
) -> Arc<AuthInteractor> {
    Arc::new(AuthInteractor::new(
        AuthDeps {
            sessions,
            attempts: Arc::new(infrastructure::InMemoryOAuthAttemptStore::with_defaults()),
            configs: Arc::new(infrastructure::PostgresOAuthConfigStore::new(pool.clone())),
            accounts: Arc::new(infrastructure::PostgresOAuthAccountStore::new(pool.clone())),
            identity_client: Arc::new(infrastructure::ReqwestIdentityClient::new(Arc::new(
                infrastructure::EnvSecretSource,
            ))),
            random: Arc::new(infrastructure::SystemSecureRandom),
        },
        users,
        Arc::new(SystemClock),
        base_url,
    ))
}

/// 后台与 CLI 共享只读列表用例。
pub fn content_queries(pool: &Database) -> Arc<application::content_queries::ContentQueries> {
    let query = Arc::new(infrastructure::PostgresAdminContentQuery::new(pool.clone()));
    Arc::new(application::content_queries::ContentQueries::new(
        query.clone(),
        query,
        Arc::new(infrastructure::PostgresUserRepository::new(pool.clone())),
    ))
}
