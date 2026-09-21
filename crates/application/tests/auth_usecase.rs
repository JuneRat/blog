//! 认证用例测试：内存 fake 验证登录闭环、一次性 state、绑定检查与会话解析。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use application::auth::AuthInteractor;
use application::error::UseCaseError;
use application::identity::{CreateUserCmd, RoleInteractor, UserInteractor};
use application::ports::{
    Clock, ExternalIdentity, ExternalIdentityClient, OAuthAccountStore, OAuthAttempt,
    OAuthAttemptStore, OAuthConfigStore, PostRepository, ProviderConfig, ProviderKind,
    SecureRandom, SessionRecord, SessionStore, UserRepository,
};
use domain::identity::UserSnapshot;
use time::OffsetDateTime;
use uuid::Uuid;

struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        time::macros::datetime!(2026-09-22 12:00:00 UTC)
    }
}

// ---------------------------------------------------------------------------
// Fakes
// ---------------------------------------------------------------------------

struct FakeRandom;

impl SecureRandom for FakeRandom {
    fn token_hex(&self) -> Result<String, UseCaseError> {
        Ok(Uuid::now_v7().simple().to_string())
    }
    fn pkce_s256(&self, _verifier: &str) -> Result<String, UseCaseError> {
        Ok("challenge".into())
    }
}

#[derive(Default)]
struct FakeSessionStore {
    sessions: Mutex<HashMap<String, SessionRecord>>,
}

#[async_trait::async_trait]
impl SessionStore for FakeSessionStore {
    async fn create(&self, user_id: Uuid) -> Result<String, UseCaseError> {
        let token = format!("session-{user_id}");
        self.sessions.lock().unwrap().insert(
            token.clone(),
            SessionRecord {
                user_id,
                csrf_token: format!("csrf-{user_id}"),
                created_at: OffsetDateTime::now_utc(),
                last_seen_at: OffsetDateTime::now_utc(),
            },
        );
        Ok(token)
    }
    async fn validate(&self, token: &str) -> Result<Option<SessionRecord>, UseCaseError> {
        Ok(self.sessions.lock().unwrap().get(token).cloned())
    }
    async fn revoke(&self, token: &str) -> Result<(), UseCaseError> {
        self.sessions.lock().unwrap().remove(token);
        Ok(())
    }
    async fn revoke_all_for_user(&self, user_id: Uuid) -> Result<(), UseCaseError> {
        self.sessions
            .lock()
            .unwrap()
            .retain(|_, r| r.user_id != user_id);
        Ok(())
    }
}

#[derive(Default)]
struct FakeAttemptStore {
    attempts: Mutex<HashMap<String, OAuthAttempt>>,
}

#[async_trait::async_trait]
impl OAuthAttemptStore for FakeAttemptStore {
    async fn save(&self, state: String, attempt: OAuthAttempt) -> Result<(), UseCaseError> {
        self.attempts.lock().unwrap().insert(state, attempt);
        Ok(())
    }
    async fn consume(&self, state: &str) -> Result<Option<OAuthAttempt>, UseCaseError> {
        Ok(self.attempts.lock().unwrap().remove(state))
    }
}

#[derive(Clone)]
struct FakeProviderConfigStore {
    providers: Vec<ProviderConfig>,
}

#[async_trait::async_trait]
impl OAuthConfigStore for FakeProviderConfigStore {
    async fn list(&self) -> Result<Vec<ProviderConfig>, UseCaseError> {
        Ok(self.providers.clone())
    }
    async fn save(&self, _providers: &[ProviderConfig]) -> Result<(), UseCaseError> {
        unimplemented!("测试不覆盖保存")
    }
}

#[derive(Default)]
struct FakeAccountStore {
    bindings: Mutex<Vec<(Uuid, String, String)>>, // (user, provider_key, external_id)
}

#[async_trait::async_trait]
impl OAuthAccountStore for FakeAccountStore {
    async fn find_user_by_external_id(
        &self,
        provider_key: &str,
        provider_user_id: &str,
    ) -> Result<Option<Uuid>, UseCaseError> {
        Ok(self
            .bindings
            .lock()
            .unwrap()
            .iter()
            .find(|(_, pk, pid)| pk == provider_key && pid == provider_user_id)
            .map(|(u, _, _)| *u))
    }
    async fn bind(
        &self,
        user_id: Uuid,
        provider_key: &str,
        provider_user_id: &str,
        _email: Option<String>,
    ) -> Result<(), UseCaseError> {
        self.bindings
            .lock()
            .unwrap()
            .push((user_id, provider_key.into(), provider_user_id.into()));
        Ok(())
    }
    async fn unbind(
        &self,
        _user_id: Uuid,
        _provider_key: &str,
        _provider_user_id: &str,
    ) -> Result<(), UseCaseError> {
        unimplemented!()
    }
    async fn list_for_user(&self, _user_id: Uuid) -> Result<Vec<ExternalIdentity>, UseCaseError> {
        Ok(vec![])
    }
}

/// 假身份客户端：authorize_url 返回固定形态；exchange 返回预置身份。
struct FakeIdentityClient {
    external_id: Mutex<String>,
    exchanges: Mutex<Vec<String>>, // 记录收到的 verifier
}

#[async_trait::async_trait]
impl ExternalIdentityClient for FakeIdentityClient {
    async fn authorize_url(
        &self,
        _config: &ProviderConfig,
        state: &str,
        _challenge: Option<&str>,
        _nonce: Option<&str>,
        redirect_uri: &str,
    ) -> Result<String, UseCaseError> {
        Ok(format!(
            "https://idp.example/authorize?state={state}&redirect_uri={redirect_uri}"
        ))
    }

    async fn exchange(
        &self,
        _config: &ProviderConfig,
        _code: &str,
        verifier: Option<&str>,
        _nonce: Option<&str>,
        _redirect_uri: &str,
    ) -> Result<ExternalIdentity, UseCaseError> {
        self.exchanges
            .lock()
            .unwrap()
            .push(verifier.unwrap_or("无").to_string());
        let id = self.external_id.lock().unwrap().clone();
        Ok(ExternalIdentity {
            provider_key: "https://idp.example".into(),
            provider_user_id: id,
            email: Some("member@example.com".into()),
        })
    }
}

// ---------------------------------------------------------------------------
// 装配
// ---------------------------------------------------------------------------

struct Fixture {
    auth: Arc<AuthInteractor>,
    identity_client: Arc<FakeIdentityClient>,
    member_id: Uuid,
}

async fn fixture() -> Fixture {
    let user_repo = Arc::new(FakeUserRepo::default());
    let rbac = Arc::new(NoopRbac);
    let clock = Arc::new(FixedClock);
    let users = Arc::new(UserInteractor::new(
        user_repo.clone(),
        rbac.clone(),
        clock.clone(),
    ));
    let _roles = Arc::new(RoleInteractor::new(rbac, user_repo));

    let member = users
        .create_user(CreateUserCmd {
            username: "member".into(),
            email: None,
            display_name: Some("成员".into()),
        })
        .await
        .unwrap();

    let providers = vec![ProviderConfig {
        id: "idp".into(),
        kind: ProviderKind::Oidc,
        issuer: Some("https://idp.example".into()),
        client_id: "client".into(),
        secret_ref: "IDP_SECRET".into(),
        scopes: vec![],
    }];
    let accounts = Arc::new(FakeAccountStore::default());
    accounts
        .bind(member.id, "https://idp.example", "sub-42", None)
        .await
        .unwrap();

    let identity_client = Arc::new(FakeIdentityClient {
        external_id: Mutex::new("sub-42".into()),
        exchanges: Mutex::new(vec![]),
    });

    let auth = Arc::new(AuthInteractor::new(
        application::auth::AuthDeps {
            sessions: Arc::new(FakeSessionStore::default()),
            attempts: Arc::new(FakeAttemptStore::default()),
            configs: Arc::new(FakeProviderConfigStore { providers }),
            accounts,
            identity_client: identity_client.clone(),
            random: Arc::new(FakeRandom),
        },
        users.clone(),
        clock,
        "http://localhost:8080".into(),
    ));

    Fixture {
        auth,
        identity_client,
        member_id: member.id,
    }
}

// 占位实现（auth 用例不需要用户仓储细节之外的断言）。
#[derive(Default)]
struct FakeUserRepo {
    users: Mutex<HashMap<String, UserSnapshot>>,
}

#[async_trait::async_trait]
impl UserRepository for FakeUserRepo {
    async fn insert(&self, snapshot: &UserSnapshot) -> Result<(), UseCaseError> {
        self.users
            .lock()
            .unwrap()
            .insert(snapshot.username.clone(), snapshot.clone());
        Ok(())
    }
    async fn find_by_id(&self, id: Uuid) -> Result<Option<UserSnapshot>, UseCaseError> {
        Ok(self
            .users
            .lock()
            .unwrap()
            .values()
            .find(|u| u.id == id)
            .cloned())
    }
    async fn find_by_username(&self, username: &str) -> Result<Option<UserSnapshot>, UseCaseError> {
        Ok(self.users.lock().unwrap().get(username).cloned())
    }
}

struct NoopRbac;

#[async_trait::async_trait]
impl application::ports::RbacStore for NoopRbac {
    async fn sync_permission_registry(
        &self,
        _entries: &[application::identity::PermissionDescriptor],
    ) -> Result<(), UseCaseError> {
        Ok(())
    }
    async fn sync_builtin_roles(
        &self,
        _defs: &[application::identity::BuiltinRoleDef],
    ) -> Result<(), UseCaseError> {
        Ok(())
    }
    async fn permissions_of_user(
        &self,
        _user_id: Uuid,
    ) -> Result<domain::identity::PermissionSet, UseCaseError> {
        Ok(domain::identity::PermissionSet::from_keys(["post.read"]))
    }
    async fn assign_role(&self, _user_id: Uuid, _role_slug: &str) -> Result<(), UseCaseError> {
        Ok(())
    }
    async fn remove_role(&self, _user_id: Uuid, _role_slug: &str) -> Result<(), UseCaseError> {
        Ok(())
    }
    async fn list_roles(&self) -> Result<Vec<application::ports::RoleDto>, UseCaseError> {
        Ok(vec![])
    }
    async fn roles_of_user(&self, _user_id: Uuid) -> Result<Vec<String>, UseCaseError> {
        Ok(vec![])
    }
}

// PostRepository 端口在认证用例中未使用，占位以满足 UserInteractor 组装。
#[allow(dead_code)]
struct Unused;

// ---------------------------------------------------------------------------
// 用例
// ---------------------------------------------------------------------------

#[tokio::test]
async fn login_round_trip_issues_session() {
    let f = fixture().await;

    let url = f.auth.login_start("idp", "/admin").await.unwrap();
    assert!(
        url.starts_with("https://idp.example/authorize"),
        "跳转到提供商：{url}"
    );
    assert!(url.contains("state="));

    let state = url
        .split("state=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap()
        .to_string();
    let success = f
        .auth
        .login_callback("idp", "auth-code", &state)
        .await
        .unwrap();
    assert_eq!(success.next, "/admin");
    assert_eq!(success.user_id, f.member_id);

    // 会话可解析为 Session 通道 Actor，并携带当前权限。
    let actor = f.auth.actor_from_session(&success.token).await.unwrap();
    assert_eq!(actor.user_id.0, f.member_id);
    assert!(actor.has_permission("post.read"));
    assert_eq!(application::identity::ActorChannel::Session, actor.channel);
}

#[tokio::test]
async fn state_is_single_use() {
    let f = fixture().await;
    let url = f.auth.login_start("idp", "/").await.unwrap();
    let state = url
        .split("state=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap()
        .to_string();

    assert!(f.auth.login_callback("idp", "code1", &state).await.is_ok());
    // 同一 state 重放被拒。
    let err = f
        .auth
        .login_callback("idp", "code2", &state)
        .await
        .unwrap_err();
    assert!(
        matches!(err, UseCaseError::Invalid(_)),
        "state 一次性：{err:?}"
    );
}

#[tokio::test]
async fn unknown_state_and_provider_mismatch_rejected() {
    let f = fixture().await;
    let err = f
        .auth
        .login_callback("idp", "code", "no-such-state")
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::Invalid(_)));

    let url = f.auth.login_start("idp", "/").await.unwrap();
    let state = url
        .split("state=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap()
        .to_string();
    let err = f
        .auth
        .login_callback("other-provider", "code", &state)
        .await
        .unwrap_err();
    assert!(
        matches!(err, UseCaseError::Invalid(_)),
        "state 与提供商不匹配"
    );
}

#[tokio::test]
async fn unbound_identity_is_rejected_without_registration() {
    let f = fixture().await;
    *f.identity_client.external_id.lock().unwrap() = "sub-stranger".into();

    let url = f.auth.login_start("idp", "/").await.unwrap();
    let state = url
        .split("state=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap()
        .to_string();
    let err = f
        .auth
        .login_callback("idp", "code", &state)
        .await
        .unwrap_err();
    assert!(
        matches!(err, UseCaseError::Forbidden),
        "未绑定外部身份不得自动注册：{err:?}"
    );
}

#[tokio::test]
async fn next_path_must_be_site_relative() {
    let f = fixture().await;
    for bad in [
        "https://evil.example",
        "//evil.example",
        "javascript:alert(1)",
        "relative",
    ] {
        let err = f.auth.login_start("idp", bad).await.unwrap_err();
        assert!(matches!(err, UseCaseError::Invalid(_)), "{bad} 应被拒绝");
    }
}

#[tokio::test]
async fn logout_revokes_session() {
    let f = fixture().await;
    let url = f.auth.login_start("idp", "/").await.unwrap();
    let state = url
        .split("state=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap()
        .to_string();
    let success = f.auth.login_callback("idp", "code", &state).await.unwrap();

    f.auth.logout(&success.token).await.unwrap();
    let err = f.auth.actor_from_session(&success.token).await.unwrap_err();
    assert!(matches!(err, UseCaseError::Unauthenticated));
}

#[tokio::test]
async fn unknown_provider_rejected() {
    let f = fixture().await;
    let err = f.auth.login_start("ghost", "/").await.unwrap_err();
    assert!(matches!(err, UseCaseError::NotFound(_)));
}

#[tokio::test]
async fn oidc_carries_pkce_verifier_to_exchange() {
    let f = fixture().await;
    let url = f.auth.login_start("idp", "/").await.unwrap();
    let state = url
        .split("state=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap()
        .to_string();
    f.auth.login_callback("idp", "code", &state).await.unwrap();

    let exchanges = f.identity_client.exchanges.lock().unwrap();
    assert_eq!(exchanges.len(), 1);
    assert_ne!(exchanges[0], "无", "OIDC 必须传 PKCE verifier");
}

// 确认 PostRepository 未被误删引用（编译期占位）。
#[allow(dead_code)]
fn _assert_port_types(_: Option<Arc<dyn PostRepository>>) {}
