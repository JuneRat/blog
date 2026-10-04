mod common;
use application::{
    UseCaseError,
    account_links::{AccountLinks, AccountMailer, LinkTarget},
    identity::{Actor, ActorChannel},
    ports::{Clock, RbacStore, SecureRandom},
};
use infrastructure::{
    Argon2PasswordHasher, SystemSecureRandom, persistence::PostgresAccountLinkStore,
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use time::{Duration, OffsetDateTime};

#[derive(Default)]
struct Mail {
    messages: Mutex<Vec<String>>,
    fail: AtomicBool,
}
#[async_trait::async_trait]
impl AccountMailer for Mail {
    async fn send_link(&self, _: &str, url: &str, _: bool) -> Result<(), UseCaseError> {
        self.messages.lock().unwrap().push(url.to_owned());
        if self.fail.load(Ordering::SeqCst) {
            Err(UseCaseError::Repository("test SMTP failure".into()))
        } else {
            Ok(())
        }
    }
}
struct TestClock(Mutex<OffsetDateTime>);
impl Clock for TestClock {
    fn now(&self) -> OffsetDateTime {
        *self.0.lock().unwrap()
    }
}
impl TestClock {
    fn advance(&self, seconds: i64) {
        *self.0.lock().unwrap() += Duration::seconds(seconds);
    }
}
struct Fixture {
    pool: sqlx::PgPool,
    id: uuid::Uuid,
    links: Arc<AccountLinks>,
    mail: Arc<Mail>,
    clock: Arc<TestClock>,
}
impl Fixture {
    async fn new(db: &str) -> Self {
        let pool = common::fresh_database(db).await;
        let id = common::seed_user(&pool, "member").await;
        sqlx::query("UPDATE users SET email='member@example.com' WHERE id=$1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        let mail = Arc::new(Mail::default());
        let clock = Arc::new(TestClock(Mutex::new(OffsetDateTime::now_utc())));
        let links = Arc::new(AccountLinks {
            store: Arc::new(PostgresAccountLinkStore::new(common::database(
                pool.clone(),
            ))),
            mailer: mail.clone(),
            random: Arc::new(SystemSecureRandom),
            hasher: Arc::new(Argon2PasswordHasher::with_defaults()),
            clock: clock.clone(),
            public_url: application::seo::PublicBaseUrl::parse("https://blog.example.com").unwrap(),
        });
        Self {
            pool,
            id,
            links,
            mail,
            clock,
        }
    }
    async fn invite(&self) -> String {
        self.links
            .request(LinkTarget::Invitation {
                user_id: self.id,
                actor: &Actor::bootstrap_cli(),
            })
            .await
            .unwrap();
        self.token()
    }
    fn token(&self) -> String {
        self.mail
            .messages
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .split("#password-reset=")
            .nth(1)
            .unwrap()
            .to_owned()
    }
}
const PASSWORD: &str = "quiet-harbor-lantern-2026";

#[tokio::test]
async fn invitation_is_hashed_single_use_and_revokes_all_sessions_atomically() {
    let f = Fixture::new("blog_links_consume_test").await;
    let token = f.invite().await;
    let stored: String = sqlx::query_scalar("SELECT token_hash FROM account_links")
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_ne!(stored, token);
    assert_eq!(stored, SystemSecureRandom.pkce_s256(&token).unwrap());
    sqlx::query("INSERT INTO sessions(token_hash,user_id,csrf_token,auth_version,expires_at) SELECT repeat('1',64),id,repeat('2',64),auth_version,now()+interval '1 day' FROM users WHERE id=$1").bind(f.id).execute(&f.pool).await.unwrap();
    let (a, b) = tokio::join!(
        f.links.reset(&token, PASSWORD, None),
        f.links.reset(&token, PASSWORD, None)
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    let hash: String = sqlx::query_scalar("SELECT password_hash FROM users WHERE id=$1")
        .bind(f.id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert!(f.links.hasher.verify(PASSWORD, &hash).await.unwrap());
    let sessions: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions")
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(sessions, 0);
    let completed: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_logs WHERE action='user.password.recovery.complete'",
    )
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(completed, 1);
    assert!(f.links.reset(&token, PASSWORD, None).await.is_err());
}

#[tokio::test]
async fn recovery_hides_ineligible_accounts_throttles_resends_and_expires_links() {
    let f = Fixture::new("blog_links_recovery_test").await;
    for email in ["unknown@example.com", "member@example.com"] {
        f.links.request(LinkTarget::Recovery(email)).await.unwrap();
    }
    assert!(f.mail.messages.lock().unwrap().is_empty()); // no password yet
    let token = f.invite().await;
    f.links.reset(&token, PASSWORD, None).await.unwrap();
    f.clock.advance(61);
    f.links
        .request(LinkTarget::Recovery("MEMBER@example.com"))
        .await
        .unwrap();
    let first = f.token();
    f.links
        .request(LinkTarget::Recovery("member@example.com"))
        .await
        .unwrap();
    assert_eq!(f.token(), first);
    f.clock.advance(61);
    f.links
        .request(LinkTarget::Recovery("member@example.com"))
        .await
        .unwrap();
    let second = f.token();
    assert_ne!(first, second);
    assert!(f.links.reset(&first, PASSWORD, None).await.is_err());
    f.clock.advance(1800);
    assert!(f.links.reset(&second, PASSWORD, None).await.is_err());
    sqlx::query("UPDATE users SET status='disabled' WHERE id=$1")
        .bind(f.id)
        .execute(&f.pool)
        .await
        .unwrap();
    let count = f.mail.messages.lock().unwrap().len();
    f.links
        .request(LinkTarget::Recovery("member@example.com"))
        .await
        .unwrap();
    assert_eq!(f.mail.messages.lock().unwrap().len(), count);
}

#[tokio::test]
async fn credential_email_and_status_changes_invalidate_pending_links_and_password_policy_is_shared()
 {
    let f = Fixture::new("blog_links_identity_test").await;
    let token = f.invite().await;
    for password in ["short", "aaaaaaaaaaaa", "member-12345678"] {
        assert!(f.links.reset(&token, password, None).await.is_err());
    }
    let digest = SystemSecureRandom.pkce_s256(&token).unwrap();
    for (statement, undo) in [
        (
            "UPDATE users SET auth_version=auth_version+1",
            "UPDATE users SET auth_version=auth_version-1",
        ),
        (
            "UPDATE users SET email='changed@example.com'",
            "UPDATE users SET email='member@example.com'",
        ),
        (
            "UPDATE users SET status='disabled'",
            "UPDATE users SET status='active'",
        ),
    ] {
        sqlx::query(statement).execute(&f.pool).await.unwrap();
        assert!(
            f.links
                .store
                .username(&digest, f.clock.now())
                .await
                .unwrap()
                .is_none()
        );
        sqlx::query(undo).execute(&f.pool).await.unwrap();
        assert!(
            f.links
                .store
                .username(&digest, f.clock.now())
                .await
                .unwrap()
                .is_some()
        );
    }
    sqlx::query("UPDATE users SET auth_version=auth_version+1")
        .execute(&f.pool)
        .await
        .unwrap();
    assert!(f.links.reset(&token, PASSWORD, None).await.is_err());
}

#[tokio::test]
async fn invitation_rechecks_current_permissions_and_admin_management_and_cancels_failed_mail() {
    let f = Fixture::new("blog_links_permissions_test").await;
    let rbac = infrastructure::PostgresRbacStore::new(common::database(f.pool.clone()));
    rbac.sync_permission_registry(application::identity::PERMISSION_REGISTRY)
        .await
        .unwrap();
    rbac.sync_builtin_roles(application::identity::BUILTIN_ROLES)
        .await
        .unwrap();
    let actor_id = common::seed_user(&f.pool, "manager").await;
    let actor = Actor::new(
        domain::identity::UserId(actor_id),
        ActorChannel::Session,
        domain::identity::PermissionSet::from_keys(["user.manage"]),
    );
    assert!(matches!(
        f.links
            .request(LinkTarget::Invitation {
                user_id: f.id,
                actor: &actor
            })
            .await,
        Err(UseCaseError::Forbidden)
    ));
    sqlx::raw_sql("INSERT INTO roles(id,code,name) VALUES(gen_random_uuid(),'user-manager','User Manager'); INSERT INTO role_permissions(role_id,permission_code) SELECT id,'user.manage' FROM roles WHERE code='user-manager'").execute(&f.pool).await.unwrap();
    sqlx::query(
        "INSERT INTO user_roles(user_id,role_id) SELECT $1,id FROM roles WHERE code='user-manager'",
    )
    .bind(actor_id)
    .execute(&f.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO user_roles(user_id,role_id) SELECT $1,id FROM roles WHERE code='admin'",
    )
    .bind(f.id)
    .execute(&f.pool)
    .await
    .unwrap();
    assert!(matches!(
        f.links
            .request(LinkTarget::Invitation {
                user_id: f.id,
                actor: &actor
            })
            .await,
        Err(UseCaseError::Forbidden)
    ));
    f.mail.fail.store(true, Ordering::SeqCst);
    assert!(
        f.links
            .request(LinkTarget::Invitation {
                user_id: f.id,
                actor: &Actor::bootstrap_cli()
            })
            .await
            .is_err()
    );
    assert!(f.links.reset(&f.token(), PASSWORD, None).await.is_err());
}
