//! Capture the real binding SELECT, commit a real identity change, then resume
//! login at precisely the two boundaries where its proof must remain stale.
mod common;

use std::sync::{Arc, Mutex};

use application::UseCaseError;
use application::auth::{AuthDeps, AuthInteractor};
use application::identity::{BUILTIN_ROLES, PERMISSION_REGISTRY, UserInteractor, UserStores};
use application::ports::{
    AccountAdministration, ExternalIdentity, ExternalIdentityClient, MediaRefGuard,
    OAuthAccountSnapshot, OAuthAccountStore, OAuthConfigStore, PasswordCredentialStore,
    PasswordHasher, ProviderConfig, ProviderKind, RbacStore, SessionRecord, SessionStore,
};
use infrastructure::{
    PostgresOAuthAccountStore, PostgresOAuthConfigStore, PostgresRbacStore, PostgresSessionStore,
    PostgresUserRepository, SystemClock,
};
use uuid::Uuid;

struct NoMedia;
#[async_trait::async_trait]
impl MediaRefGuard for NoMedia {
    async fn is_attachable(&self, _: Uuid) -> Result<bool, UseCaseError> {
        Ok(false)
    }
}

struct IdentityClient(String);
#[async_trait::async_trait]
impl ExternalIdentityClient for IdentityClient {
    async fn authorize_url(
        &self,
        _: &ProviderConfig,
        _: &str,
        _: Option<&str>,
        _: Option<&str>,
        _: &str,
    ) -> Result<String, UseCaseError> {
        Ok("https://idp.example/authorize".into())
    }
    async fn exchange(
        &self,
        _: &ProviderConfig,
        _: &str,
        _: Option<&str>,
        _: Option<&str>,
        _: &str,
    ) -> Result<ExternalIdentity, UseCaseError> {
        Ok(ExternalIdentity {
            provider_key: "https://idp.example".into(),
            provider_user_id: self.0.clone(),
            email: None,
        })
    }
}

#[derive(Clone, Copy, Debug)]
enum Change {
    Unbind,
    Revoke,
}

struct IdentityChange {
    change: Change,
    user_id: Uuid,
    subject: String,
    accounts: Arc<PostgresOAuthAccountStore>,
    repository: Arc<PostgresUserRepository>,
}
impl IdentityChange {
    async fn run(&self) -> Result<(), UseCaseError> {
        match self.change {
            Change::Unbind => {
                self.accounts
                    .unbind(
                        self.user_id,
                        "https://idp.example",
                        &self.subject,
                        None.into(),
                    )
                    .await
            }
            Change::Revoke => {
                self.repository
                    .revoke_authentication(self.user_id, None.into())
                    .await
            }
        }
    }
}

struct Accounts {
    inner: Arc<PostgresOAuthAccountStore>,
    after_lookup: Mutex<Option<Arc<IdentityChange>>>,
}
#[async_trait::async_trait]
impl OAuthAccountStore for Accounts {
    async fn find_user_by_external_id(
        &self,
        provider: &str,
        subject: &str,
    ) -> Result<Option<OAuthAccountSnapshot>, UseCaseError> {
        let snapshot = self
            .inner
            .find_user_by_external_id(provider, subject)
            .await?;
        let change = self.after_lookup.lock().unwrap().take();
        if let Some(change) = change {
            change.run().await?;
        }
        Ok(snapshot)
    }
    async fn bind(
        &self,
        user: Uuid,
        provider: &str,
        subject: &str,
        email: Option<String>,
        audit: application::audit::AuditContext,
    ) -> Result<(), UseCaseError> {
        self.inner.bind(user, provider, subject, email, audit).await
    }
    async fn unbind(
        &self,
        user: Uuid,
        provider: &str,
        subject: &str,
        audit: application::audit::AuditContext,
    ) -> Result<(), UseCaseError> {
        self.inner.unbind(user, provider, subject, audit).await
    }
    async fn list_for_user(&self, user: Uuid) -> Result<Vec<ExternalIdentity>, UseCaseError> {
        self.inner.list_for_user(user).await
    }
}

struct Sessions {
    inner: Arc<PostgresSessionStore>,
    before_create: Mutex<Option<Arc<IdentityChange>>>,
}
#[async_trait::async_trait]
impl SessionStore for Sessions {
    async fn create(&self, user: Uuid, revision: i64) -> Result<String, UseCaseError> {
        let change = self.before_create.lock().unwrap().take();
        if let Some(change) = change {
            change.run().await?;
        }
        self.inner.create(user, revision).await
    }
    async fn validate(&self, token: &str) -> Result<Option<SessionRecord>, UseCaseError> {
        self.inner.validate(token).await
    }
    async fn revoke(&self, token: &str) -> Result<(), UseCaseError> {
        self.inner.revoke(token).await
    }
    async fn revoke_all_for_user(&self, user: Uuid) -> Result<(), UseCaseError> {
        self.inner.revoke_all_for_user(user).await
    }
}

#[derive(Clone, Copy, Debug)]
enum Timing {
    None,
    BeforeCallback,
    AfterLookup,
    BeforeCreate,
}

#[tokio::test]
async fn authentication_snapshots_do_not_survive_unbind_or_explicit_revocation() {
    // This test owns a random database; it never drops a shared fixture database.
    let admin_url = common::admin_url();
    common::assert_loopback(&admin_url);
    let admin = common::connect(&admin_url).await.unwrap();
    let name = format!("blog_oauth_login_{}", Uuid::now_v7().simple());
    sqlx::raw_sql(&format!("CREATE DATABASE {name}"))
        .execute(&admin)
        .await
        .unwrap();
    let pool = common::connect(&common::test_db_url(&admin_url, &name))
        .await
        .unwrap();
    let database = common::database(pool.clone());
    infrastructure::migrate_schema(&database, "../../migrations/postgres")
        .await
        .unwrap();
    let repository = Arc::new(PostgresUserRepository::new(database.clone()));
    let rbac = Arc::new(PostgresRbacStore::new(database.clone()));
    rbac.sync_permission_registry(PERMISSION_REGISTRY)
        .await
        .unwrap();
    rbac.sync_builtin_roles(BUILTIN_ROLES).await.unwrap();
    let configs = Arc::new(PostgresOAuthConfigStore::new(database.clone()));
    configs
        .save(
            &[ProviderConfig {
                id: "idp".into(),
                name: None,
                kind: ProviderKind::Oidc,
                issuer: Some("https://idp.example".into()),
                client_id: "client".into(),
                secret_ref: "UNUSED_TEST_SECRET".into(),
                scopes: vec![],
            }],
            0,
            None.into(),
        )
        .await
        .unwrap();
    let accounts = Arc::new(PostgresOAuthAccountStore::new(database.clone()));
    let sessions = Arc::new(PostgresSessionStore::with_defaults(database));
    let clock = Arc::new(SystemClock);
    let users = Arc::new(UserInteractor::new(
        UserStores {
            query: repository.clone(),
            profiles: repository.clone(),
            accounts: repository.clone(),
        },
        rbac.clone(),
        clock.clone(),
        Arc::new(NoMedia),
    ));
    let password = infrastructure::Argon2PasswordHasher::with_defaults()
        .hash("Independent-test-password!2026")
        .await
        .unwrap();
    let mut outcomes = Vec::new();
    for (index, (timing, change)) in [
        (Timing::None, Change::Unbind),
        (Timing::BeforeCallback, Change::Unbind),
        (Timing::AfterLookup, Change::Unbind),
        (Timing::AfterLookup, Change::Revoke),
        (Timing::BeforeCreate, Change::Unbind),
        (Timing::BeforeCreate, Change::Revoke),
    ]
    .into_iter()
    .enumerate()
    {
        let owner = common::seed_user(&pool, &format!("owner{index}")).await;
        repository
            .set_password_hash(owner, &password, None.into())
            .await
            .unwrap();
        rbac.assign_role(owner, "admin", None.into()).await.unwrap();
        let subject = format!("subject-{index}");
        accounts
            .bind(owner, "https://idp.example", &subject, None, None.into())
            .await
            .unwrap();
        let captured = accounts
            .find_user_by_external_id("https://idp.example", &subject)
            .await
            .unwrap()
            .unwrap();
        let revision: i64 = sqlx::query_scalar("SELECT auth_version FROM users WHERE id=$1")
            .bind(owner)
            .fetch_one(&pool)
            .await
            .unwrap();
        let old_token = sessions.create(owner, revision).await.unwrap();
        let change = Arc::new(IdentityChange {
            change,
            user_id: owner,
            subject: subject.clone(),
            accounts: accounts.clone(),
            repository: repository.clone(),
        });
        let auth = AuthInteractor::new(
            AuthDeps {
                sessions: Arc::new(Sessions {
                    inner: sessions.clone(),
                    before_create: Mutex::new(
                        matches!(timing, Timing::BeforeCreate).then(|| change.clone()),
                    ),
                }),
                accounts: Arc::new(Accounts {
                    inner: accounts.clone(),
                    after_lookup: Mutex::new(
                        matches!(timing, Timing::AfterLookup).then(|| change.clone()),
                    ),
                }),
                attempts: Arc::new(infrastructure::InMemoryOAuthAttemptStore::with_defaults()),
                configs: configs.clone(),
                identity_client: Arc::new(IdentityClient(subject.clone())),
                random: Arc::new(infrastructure::SystemSecureRandom),
            },
            users.clone(),
            clock.clone(),
            "http://localhost:8080".into(),
        );
        let start = auth.login_start("idp", "/admin").await.unwrap();
        if matches!(timing, Timing::BeforeCallback) {
            change.run().await.unwrap();
        }
        let login = auth
            .login_callback(
                "idp",
                "code",
                &start.browser_binding,
                Some(&start.browser_binding),
            )
            .await;
        let old_valid = sessions.validate(&old_token).await.unwrap().is_some();
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions WHERE user_id=$1")
            .bind(owner)
            .fetch_one(&pool)
            .await
            .unwrap();
        let outcome = match timing {
            Timing::None => {
                let login = login.unwrap();
                let record = sessions.validate(&login.token).await.unwrap().unwrap();
                captured.user_id == owner
                    && captured.auth_version == revision
                    && old_valid
                    && record.auth_version == captured.auth_version
                    && auth
                        .actor_from_session(&login.token)
                        .await
                        .unwrap()
                        .has_permission("user.manage")
            }
            Timing::BeforeCallback | Timing::AfterLookup => {
                matches!(login, Err(UseCaseError::Forbidden)) && !old_valid && count == 0
            }
            Timing::BeforeCreate => {
                let login = login.unwrap();
                let stored: i64 =
                    sqlx::query_scalar("SELECT auth_version FROM sessions WHERE user_id=$1")
                        .bind(owner)
                        .fetch_one(&pool)
                        .await
                        .unwrap();
                stored == captured.auth_version
                    && !old_valid
                    && sessions.validate(&login.token).await.unwrap().is_none()
                    && matches!(
                        auth.actor_from_session(&login.token).await,
                        Err(UseCaseError::Unauthenticated)
                    )
            }
        };
        let bound = accounts
            .find_user_by_external_id("https://idp.example", &subject)
            .await
            .unwrap();
        let expected_binding =
            matches!(timing, Timing::None) || matches!(change.change, Change::Revoke);
        outcomes.push((
            timing,
            change.change,
            outcome && bound.is_some() == expected_binding,
        ));
    }
    let unknown = accounts
        .find_user_by_external_id("https://idp.example", "unknown-subject")
        .await
        .unwrap();
    pool.close().await;
    sqlx::raw_sql(&format!("DROP DATABASE {name}"))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    assert!(unknown.is_none());
    for (timing, change, correct) in outcomes {
        assert!(
            correct,
            "{timing:?}/{change:?} violated the authentication snapshot contract"
        );
    }
}
