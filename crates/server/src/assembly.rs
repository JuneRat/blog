//! Narrow command assemblies. These constructors never read website settings
//! or load themes and static assets.

use std::path::PathBuf;
use std::sync::Arc;

use application::auth::{AuthDeps, AuthInteractor, OAuthManagementInteractor};
use application::content::PostInteractor;
use application::identity::{RoleInteractor, UserInteractor};
use application::media::MediaInteractor;
use application::password::{PasswordDeps, PasswordInteractor};
use application::ports::{ContentRenderer, SessionStore};
use infrastructure::{
    PostgresCategoryRepository, PostgresMediaRepository, PostgresPostRepository, PostgresRbacStore,
    PostgresSeriesRepository, PostgresTagRepository, PostgresUserRepository, SystemClock,
};
use interfaces::cli::{PostCliDeps, UserCliDeps};
use sqlx::PgPool;

pub fn users(pool: &PgPool) -> Arc<UserInteractor> {
    Arc::new(UserInteractor::new(
        Arc::new(PostgresUserRepository::new(pool.clone())),
        Arc::new(PostgresRbacStore::new(pool.clone())),
        Arc::new(SystemClock),
        Arc::new(PostgresMediaRepository::new(pool.clone())),
    ))
}

pub fn roles(pool: &PgPool) -> Arc<RoleInteractor> {
    Arc::new(RoleInteractor::new(
        Arc::new(PostgresRbacStore::new(pool.clone())),
        Arc::new(PostgresUserRepository::new(pool.clone())),
    ))
}

pub fn sessions(pool: &PgPool) -> Arc<dyn SessionStore> {
    Arc::new(infrastructure::PostgresSessionStore::with_defaults(
        pool.clone(),
    ))
}

pub fn passwords(pool: &PgPool, sessions: Arc<dyn SessionStore>) -> Arc<PasswordInteractor> {
    Arc::new(PasswordInteractor::new(PasswordDeps {
        users: Arc::new(PostgresUserRepository::new(pool.clone())),
        hasher: Arc::new(infrastructure::Argon2PasswordHasher::with_defaults()),
        throttle: Arc::new(infrastructure::InMemoryLoginThrottle::with_defaults()),
        sessions,
    }))
}

pub fn user_commands(pool: &PgPool) -> UserCliDeps {
    UserCliDeps {
        users: users(pool),
        passwords: passwords(pool, sessions(pool)),
    }
}

pub fn oauth_commands(pool: &PgPool) -> OAuthManagementInteractor {
    OAuthManagementInteractor::new(
        Arc::new(infrastructure::PostgresOAuthConfigStore::new(pool.clone())),
        Arc::new(infrastructure::PostgresOAuthAccountStore::new(pool.clone())),
        sessions(pool),
        users(pool),
    )
}

pub fn posts(pool: &PgPool, renderer: Arc<dyn ContentRenderer>) -> Arc<PostInteractor> {
    Arc::new(PostInteractor::new(
        Arc::new(PostgresPostRepository::new(pool.clone(), renderer)),
        Arc::new(PostgresTagRepository::new(pool.clone())),
        Arc::new(PostgresCategoryRepository::new(pool.clone())),
        Arc::new(PostgresSeriesRepository::new(pool.clone())),
        Arc::new(SystemClock),
        Arc::new(PostgresMediaRepository::new(pool.clone())),
    ))
}

pub fn post_commands(pool: &PgPool, renderer: Arc<dyn ContentRenderer>) -> PostCliDeps {
    PostCliDeps {
        users: users(pool),
        posts: posts(pool, renderer),
    }
}

pub fn media(pool: &PgPool, root: PathBuf) -> Arc<MediaInteractor> {
    Arc::new(MediaInteractor::new(
        Arc::new(PostgresMediaRepository::new(pool.clone())),
        Arc::new(infrastructure::LocalMediaStorage::new(root)),
        Arc::new(SystemClock),
    ))
}

/// Browser authentication is assembled only for the HTTP server. OAuth CLI
/// maintenance uses `oauth_commands`, with no login client or callback URL.
pub fn auth(
    pool: &PgPool,
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
