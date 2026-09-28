//! 认证用例测试：内存 fake 验证登录闭环、一次性 state、浏览器绑定、
//! PKCE challenge/nonce/redirect_uri 传递、绑定检查与会话解析。

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use application::auth::{AuthInteractor, LoginSuccess};
use application::error::UseCaseError;
use application::identity::{Actor, CreateUserCmd, RoleInteractor, UserInteractor};
use application::ports::{
    Clock, ExternalIdentity, ExternalIdentityClient, OAuthAccountStore, OAuthAttempt,
    OAuthAttemptStore, OAuthConfigStore, ProviderConfig, ProviderKind, SecureRandom, SessionRecord,
    SessionStore,
};
use domain::identity::UserSnapshot;
use time::OffsetDateTime;
use uuid::Uuid;

mod common;

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
    /// 可验证的 challenge：由 verifier 派生，便于断言 authorize 与 exchange 一致。
    fn pkce_s256(&self, verifier: &str) -> Result<String, UseCaseError> {
        Ok(format!("challenge-{verifier}"))
    }
}

#[derive(Default)]
struct FakeSessionStore {
    sessions: Mutex<HashMap<String, SessionRecord>>,
    validates: AtomicUsize,
}

impl FakeSessionStore {
    fn validate_count(&self) -> usize {
        self.validates.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl SessionStore for FakeSessionStore {
    async fn create(&self, user_id: Uuid, auth_version: i64) -> Result<String, UseCaseError> {
        let token = format!("session-{user_id}");
        self.sessions.lock().unwrap().insert(
            token.clone(),
            SessionRecord {
                user_id,
                csrf_token: format!("csrf-{user_id}"),
                created_at: OffsetDateTime::now_utc(),
                last_seen_at: OffsetDateTime::now_utc(),
                auth_version,
            },
        );
        Ok(token)
    }
    async fn validate(&self, token: &str) -> Result<Option<SessionRecord>, UseCaseError> {
        self.validates.fetch_add(1, Ordering::SeqCst);
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

struct FakeProviderConfigStore {
    providers: Mutex<Vec<ProviderConfig>>,
}

#[async_trait::async_trait]
impl OAuthConfigStore for FakeProviderConfigStore {
    async fn read(&self) -> Result<application::ports::OAuthConfigSnapshot, UseCaseError> {
        Ok(application::ports::OAuthConfigSnapshot {
            providers: self.providers.lock().unwrap().clone(),
            version: 1,
        })
    }
    async fn save(
        &self,
        _providers: &[ProviderConfig],
        _expected_version: i64,
        _audit_actor: application::audit::AuditContext,
    ) -> Result<i64, UseCaseError> {
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
        _audit_actor: application::audit::AuditContext,
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
        _audit_actor: application::audit::AuditContext,
    ) -> Result<(), UseCaseError> {
        unimplemented!()
    }
    async fn list_for_user(&self, _user_id: Uuid) -> Result<Vec<ExternalIdentity>, UseCaseError> {
        Ok(vec![])
    }
}

/// 捕获 authorize_url 收到的参数，断言挑战/nonce/回调地址确实下发给提供商。
#[derive(Debug, Clone, PartialEq, Eq)]
struct AuthorizeRequest {
    state: String,
    challenge: Option<String>,
    nonce: Option<String>,
    redirect_uri: String,
}

/// 假身份客户端：authorize_url 记录参数；exchange 返回预置身份。
struct FakeIdentityClient {
    external_id: Mutex<String>,
    exchanges: Mutex<Vec<String>>, // 记录收到的 verifier
    authorize_requests: Mutex<Vec<AuthorizeRequest>>,
}

#[async_trait::async_trait]
impl ExternalIdentityClient for FakeIdentityClient {
    async fn authorize_url(
        &self,
        _config: &ProviderConfig,
        state: &str,
        challenge: Option<&str>,
        nonce: Option<&str>,
        redirect_uri: &str,
    ) -> Result<String, UseCaseError> {
        self.authorize_requests
            .lock()
            .unwrap()
            .push(AuthorizeRequest {
                state: state.to_string(),
                challenge: challenge.map(str::to_string),
                nonce: nonce.map(str::to_string),
                redirect_uri: redirect_uri.to_string(),
            });
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
    configs: Arc<FakeProviderConfigStore>,
    management: application::auth::OAuthManagementInteractor,
    accounts: Arc<FakeAccountStore>,
    auth: Arc<AuthInteractor>,
    identity_client: Arc<FakeIdentityClient>,
    sessions: Arc<FakeSessionStore>,
    member_id: Uuid,
}

async fn fixture() -> Fixture {
    let user_repo = Arc::new(FakeUserRepo::default());
    let rbac = Arc::new(NoopRbac);
    let clock = Arc::new(FixedClock);
    let users = Arc::new(UserInteractor::new(
        application::identity::UserStores {
            query: user_repo.clone(),
            profiles: user_repo.clone(),
            accounts: user_repo.clone(),
        },
        rbac.clone(),
        clock.clone(),
        Arc::new(common::FakeMediaGuard::new()),
    ));
    let _roles = Arc::new(RoleInteractor::new(rbac, user_repo));

    let member = users
        .create_user(
            &Actor::bootstrap_cli(),
            CreateUserCmd {
                username: "member".into(),
                email: None,
                display_name: Some("成员".into()),
            },
        )
        .await
        .unwrap();

    let providers = vec![
        ProviderConfig {
            id: "idp".into(),
            name: None,
            kind: ProviderKind::Oidc,
            issuer: Some("https://idp.example".into()),
            client_id: "client".into(),
            secret_ref: "IDP_SECRET".into(),
            scopes: vec![],
        },
        ProviderConfig {
            id: "gh".into(),
            name: Some("GitHub 登录".into()),
            kind: ProviderKind::GitHub,
            issuer: None,
            client_id: "gh-client".into(),
            secret_ref: "GH_SECRET".into(),
            scopes: vec![],
        },
    ];
    let accounts = Arc::new(FakeAccountStore::default());
    accounts
        .bind(
            member.id,
            "https://idp.example",
            "sub-42",
            None,
            None.into(),
        )
        .await
        .unwrap();

    let identity_client = Arc::new(FakeIdentityClient {
        external_id: Mutex::new("sub-42".into()),
        exchanges: Mutex::new(vec![]),
        authorize_requests: Mutex::new(vec![]),
    });

    let sessions = Arc::new(FakeSessionStore::default());
    let configs = Arc::new(FakeProviderConfigStore {
        providers: Mutex::new(providers),
    });
    let management = application::auth::OAuthManagementInteractor::new(
        configs.clone(),
        accounts.clone(),
        sessions.clone(),
        users.clone(),
    );
    let auth = Arc::new(AuthInteractor::new(
        application::auth::AuthDeps {
            sessions: sessions.clone(),
            attempts: Arc::new(FakeAttemptStore::default()),
            configs: configs.clone(),
            accounts: accounts.clone(),
            identity_client: identity_client.clone(),
            random: Arc::new(FakeRandom),
        },
        users.clone(),
        clock,
        "http://localhost:8080".into(),
    ));

    Fixture {
        configs,
        management,
        accounts,
        auth,
        identity_client,
        sessions,
        member_id: member.id,
    }
}

/// 发起登录并返回（授权 URL，浏览器绑定值）。
async fn begin_login(f: &Fixture, provider: &str, next: &str) -> (String, String) {
    let start = f.auth.login_start(provider, next).await.unwrap();
    (start.authorize_url, start.browser_binding)
}

/// 完成一次正确绑定的回调。
async fn complete_login(f: &Fixture, next: &str) -> Result<LoginSuccess, UseCaseError> {
    let (_, binding) = begin_login(f, "idp", next).await;
    f.auth
        .login_callback("idp", "auth-code", &binding, Some(&binding))
        .await
}

// 占位实现（auth 用例不需要用户仓储细节之外的断言）。
#[derive(Default)]
struct FakeUserRepo {
    users: Mutex<HashMap<String, UserSnapshot>>,
}

#[async_trait::async_trait]
impl application::ports::UserQuery for FakeUserRepo {
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

    // 认证用例不涉及账号管理列表；返回空列表即可。
    async fn list_admin(
        &self,
        _limit: i64,
        _offset: i64,
    ) -> Result<Vec<application::ports::AdminUserRow>, UseCaseError> {
        Ok(vec![])
    }
}

#[async_trait::async_trait]
impl application::ports::UserProfileStore for FakeUserRepo {
    async fn save_profile(
        &self,
        user: &domain::identity::User,
        expected_version: i64,
        now: time::OffsetDateTime,
        _audit: application::audit::AuditContext,
    ) -> Result<UserSnapshot, UseCaseError> {
        let snapshot = user.snapshot();
        let mut users = self.users.lock().unwrap();
        let current = users
            .values_mut()
            .find(|u| u.id == snapshot.id)
            .ok_or_else(|| UseCaseError::NotFound("用户".into()))?;
        if current.version != expected_version || !current.is_active() {
            return Err(UseCaseError::VersionConflict);
        }
        current.display_name = snapshot.display_name;
        current.bio = snapshot.bio;
        current.version += 1;
        current.updated_at = now;
        Ok(current.clone())
    }

    /// 头像只走真实认证 HTTP 用例（server/tests）；本 fake 不实现，误用即失败。
    async fn set_avatar(
        &self,
        _user_id: uuid::Uuid,
        _avatar_media_id: Option<uuid::Uuid>,
        _now: time::OffsetDateTime,
        _audit: application::audit::AuditContext,
    ) -> Result<(), UseCaseError> {
        unimplemented!("该用例不使用头像")
    }
}

#[async_trait::async_trait]
impl application::ports::AccountAdministration for FakeUserRepo {
    async fn insert(
        &self,
        aggregate: &domain::identity::User,
        _audit_actor: application::audit::AuditContext,
    ) -> Result<(), UseCaseError> {
        let snapshot = aggregate.snapshot();
        self.users
            .lock()
            .unwrap()
            .insert(snapshot.username.clone(), snapshot.clone());
        Ok(())
    }

    async fn change_status(
        &self,
        _user_id: uuid::Uuid,
        _status: domain::identity::UserStatus,
        _expected_version: i64,
        _now: time::OffsetDateTime,
        _actor: &application::identity::Actor,
    ) -> Result<UserSnapshot, UseCaseError> {
        unimplemented!("此用例不修改账号状态")
    }

    async fn revoke_authentication(
        &self,
        user_id: Uuid,
        _audit_actor: application::audit::AuditContext,
    ) -> Result<(), UseCaseError> {
        let mut users = self.users.lock().unwrap();
        let user = users
            .values_mut()
            .find(|u| u.id == user_id)
            .ok_or_else(|| UseCaseError::NotFound("用户".into()))?;
        user.auth_version += 1;
        Ok(())
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
        Ok(domain::identity::PermissionSet::from_keys([
            "post.read",
            "post.create",
        ]))
    }
    async fn permissions_of_role(
        &self,
        role_slug: &str,
    ) -> Result<domain::identity::PermissionSet, UseCaseError> {
        Ok(domain::identity::PermissionSet::from_keys([role_slug]))
    }
    async fn assign_role(
        &self,
        _user_id: Uuid,
        _role_slug: &str,
        _audit_actor: application::audit::AuditContext,
    ) -> Result<(), UseCaseError> {
        Ok(())
    }
    async fn remove_role(
        &self,
        _user_id: Uuid,
        _role_slug: &str,
        _audit_actor: application::audit::AuditContext,
    ) -> Result<(), UseCaseError> {
        Ok(())
    }
    async fn list_roles(&self) -> Result<Vec<application::ports::RoleDto>, UseCaseError> {
        Ok(vec![])
    }
    async fn roles_of_user(&self, _user_id: Uuid) -> Result<Vec<String>, UseCaseError> {
        Ok(vec![])
    }
    async fn roles_of_users(
        &self,
        _user_ids: &[Uuid],
    ) -> Result<Vec<(Uuid, String)>, UseCaseError> {
        Ok(vec![])
    }
    async fn loginable_owner_count(&self) -> Result<i64, UseCaseError> {
        Ok(0)
    }
}

// ---------------------------------------------------------------------------
// 用例
// ---------------------------------------------------------------------------

#[tokio::test]
async fn login_round_trip_issues_session() {
    let f = fixture().await;

    let (url, binding) = begin_login(&f, "idp", "/admin").await;
    assert!(
        url.starts_with("https://idp.example/authorize"),
        "跳转到提供商：{url}"
    );
    assert!(url.contains("state="));
    assert!(!binding.is_empty(), "必须下发浏览器绑定值");

    let success = f
        .auth
        .login_callback("idp", "auth-code", &binding, Some(&binding))
        .await
        .unwrap();
    assert_eq!(success.next, "/admin");
    assert_eq!(success.user_id, f.member_id);

    // 会话可解析为 Session 通道 Actor，并携带当前权限。
    let actor = f.auth.actor_from_session(&success.token).await.unwrap();
    assert_eq!(actor.user_id.0, f.member_id);
    assert!(actor.has_permission("post.read"));
    assert!(actor.has_permission("post.create"));
    assert_eq!(application::identity::ActorChannel::Session, actor.channel);
}

/// 管理提取器需要 CSRF（记录）与授权（Actor）两样东西；合并入口必须一次校验，
/// 否则持久存储下同一请求会写两次 `last_seen_at`。
#[tokio::test]
async fn session_actor_validates_once_and_returns_record_with_actor() {
    let f = fixture().await;
    let success = complete_login(&f, "/admin").await.unwrap();

    let before = f.sessions.validate_count();
    let (record, actor) = f.auth.session_actor(&success.token).await.unwrap();
    assert_eq!(
        f.sessions.validate_count() - before,
        1,
        "session_actor 只应校验一次会话"
    );
    assert_eq!(record.user_id, actor.user_id.0, "记录与 Actor 指向同一用户");
    assert_eq!(record.csrf_token, format!("csrf-{}", f.member_id));

    // 未登录令牌仍是未登录错误，不会因为合并入口而放行。
    assert!(matches!(
        f.auth.session_actor("not-a-session").await,
        Err(UseCaseError::Unauthenticated)
    ));
}

#[tokio::test]
async fn oidc_sends_pkce_challenge_nonce_and_exact_redirect_uri() {
    let f = fixture().await;
    let (_, binding) = begin_login(&f, "idp", "/admin").await;

    let request = f.identity_client.authorize_requests.lock().unwrap()[0].clone();
    assert_eq!(request.state, binding, "authorize 的 state 即浏览器绑定值");
    assert_eq!(
        request.redirect_uri, "http://localhost:8080/auth/callback/idp",
        "回调地址精确匹配"
    );
    let challenge = request.challenge.expect("OIDC 必须传 PKCE challenge");
    let nonce = request.nonce.expect("OIDC 必须传 nonce");
    assert!(!nonce.is_empty(), "nonce 不可为空");

    f.auth
        .login_callback("idp", "code", &binding, Some(&binding))
        .await
        .unwrap();
    let verifier = f.identity_client.exchanges.lock().unwrap()[0].clone();
    assert_ne!(verifier, "无", "OIDC 必须传 PKCE verifier");
    assert_eq!(
        challenge,
        format!("challenge-{verifier}"),
        "challenge 必须由实际使用的 verifier 派生"
    );
}

#[tokio::test]
async fn github_login_carries_no_pkce_or_nonce() {
    let f = fixture().await;
    let (_, binding) = begin_login(&f, "gh", "/").await;
    let request = f.identity_client.authorize_requests.lock().unwrap()[0].clone();
    assert_eq!(request.state, binding);
    assert!(
        request.challenge.is_none(),
        "GitHub 平台不接受 PKCE 参数：{:?}",
        request.challenge
    );
    assert!(request.nonce.is_none(), "GitHub 无 nonce");
    assert_eq!(
        request.redirect_uri,
        "http://localhost:8080/auth/callback/gh"
    );
}

#[tokio::test]
async fn missing_or_mismatched_browser_binding_is_rejected_without_consuming_attempt() {
    let f = fixture().await;
    let (_, binding) = begin_login(&f, "idp", "/").await;

    // 缺少绑定 cookie（典型登录 CSRF：攻击者把回调 URL 塞给受害者）。
    let err = f
        .auth
        .login_callback("idp", "code", &binding, None)
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::Invalid(_)), "{err:?}");

    // 绑定值不匹配。
    let err = f
        .auth
        .login_callback("idp", "code", &binding, Some("other-binding"))
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::Invalid(_)), "{err:?}");

    // 错误的绑定没有烧掉尝试：同一浏览器随后仍可完成登录。
    assert!(
        f.auth
            .login_callback("idp", "code", &binding, Some(&binding))
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn state_is_single_use() {
    let f = fixture().await;
    let (_, binding) = begin_login(&f, "idp", "/").await;

    assert!(
        f.auth
            .login_callback("idp", "code1", &binding, Some(&binding))
            .await
            .is_ok()
    );
    // 同一 state 重放被拒。
    let err = f
        .auth
        .login_callback("idp", "code2", &binding, Some(&binding))
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
        .login_callback("idp", "code", "no-such-state", Some("no-such-state"))
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::Invalid(_)));

    let (_, binding) = begin_login(&f, "idp", "/").await;
    let err = f
        .auth
        .login_callback("other-provider", "code", &binding, Some(&binding))
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

    let err = complete_login(&f, "/").await.unwrap_err();
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
async fn next_path_rejects_control_characters_and_whitespace() {
    let f = fixture().await;
    // `next=/%0d%0aX` 经查询串解码后是真实的 CR/LF：会破坏 Location 头。
    for bad in ["/\r\nX", "/a b", "/tab\there", "/del\u{7f}"] {
        let err = f.auth.login_start("idp", bad).await.unwrap_err();
        assert!(matches!(err, UseCaseError::Invalid(_)), "{bad:?} 应被拒绝");
    }
    // 正常路径（含查询与片段）仍放行。
    for good in ["/admin", "/posts/x?y=1#z", "/a/b_c-d.e~f"] {
        assert!(
            f.auth.login_start("idp", good).await.is_ok(),
            "{good} 应被接受"
        );
    }
}

#[tokio::test]
async fn logout_revokes_session() {
    let f = fixture().await;
    let success = complete_login(&f, "/").await.unwrap();

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
    complete_login(&f, "/").await.unwrap();

    let exchanges = f.identity_client.exchanges.lock().unwrap();
    assert_eq!(exchanges.len(), 1);
    assert_ne!(exchanges[0], "无", "OIDC 必须传 PKCE verifier");
}

#[tokio::test]
async fn provider_summaries_use_display_name_and_fall_back_to_id() {
    let f = fixture().await;
    let summaries = f.auth.list_provider_summaries().await.unwrap();
    assert_eq!(summaries.len(), 2);

    let idp = summaries.iter().find(|p| p.id == "idp").unwrap();
    assert_eq!(idp.name, "idp", "未配置展示名时回退到 id");
    assert_eq!(idp.kind, "oidc");

    let gh = summaries.iter().find(|p| p.id == "gh").unwrap();
    assert_eq!(gh.name, "GitHub 登录");
    assert_eq!(gh.kind, "github");
}

#[tokio::test]
async fn invalid_stored_providers_fail_before_network_or_identity_writes() {
    for corruption in [
        "missing-issuer",
        "invalid-issuer",
        "duplicate-id",
        "empty-client",
    ] {
        let f = fixture().await;
        let mut providers = f.configs.providers.lock().unwrap().clone();
        match corruption {
            "missing-issuer" => providers[0].issuer = None,
            "invalid-issuer" => providers[0].issuer = Some("http://idp.example".into()),
            "duplicate-id" => providers.push(providers[0].clone()),
            _ => providers[0].client_id.clear(),
        }
        *f.configs.providers.lock().unwrap() = providers.clone();
        assert!(
            matches!(
                f.auth.login_start("idp", "/").await,
                Err(UseCaseError::Repository(_))
            ),
            "{corruption}"
        );
        assert!(matches!(
            f.auth.list_provider_summaries().await,
            Err(UseCaseError::Repository(_))
        ));
        assert!(matches!(
            f.management.provider_settings().await,
            Err(UseCaseError::Repository(_))
        ));
        assert!(matches!(
            f.management
                .bind_external_id(&Actor::bootstrap_cli(), "member", "idp", "new-sub", None)
                .await,
            Err(UseCaseError::Repository(_))
        ));
        assert!(matches!(
            f.management
                .unbind_external_id(&Actor::bootstrap_cli(), "member", "idp", "sub-42")
                .await,
            Err(UseCaseError::Repository(_))
        ));
        assert!(matches!(
            f.management
                .save_providers(&Actor::bootstrap_cli(), &providers, 1)
                .await,
            Err(UseCaseError::Invalid(_))
        ));
        assert!(
            f.identity_client
                .authorize_requests
                .lock()
                .unwrap()
                .is_empty()
        );
        assert!(f.identity_client.exchanges.lock().unwrap().is_empty());
        assert_eq!(f.accounts.bindings.lock().unwrap().len(), 1);
        assert!(f.sessions.sessions.lock().unwrap().is_empty());
    }
    let f = fixture().await;
    let mut config = f.configs.providers.lock().unwrap()[0].clone();
    config.issuer = None;
    assert!(matches!(
        application::auth::provider_identity_key(&config),
        Err(UseCaseError::Repository(_))
    ));
}

#[tokio::test]
async fn callback_rechecks_stored_configuration_before_exchange() {
    let f = fixture().await;
    let (_, binding) = begin_login(&f, "idp", "/").await;
    f.configs.providers.lock().unwrap()[0].issuer = None;
    assert!(matches!(
        f.auth
            .login_callback("idp", "code", &binding, Some(&binding))
            .await,
        Err(UseCaseError::Repository(_))
    ));
    assert!(f.identity_client.exchanges.lock().unwrap().is_empty());
    assert!(f.sessions.sessions.lock().unwrap().is_empty());
}
