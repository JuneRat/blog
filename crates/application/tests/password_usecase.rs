//! 本地密码用例测试：统一失败语义与等开销校验、限流、受控设置/清除、
//! 自助改密与会话轮换、弱参数哈希升级。
//!
//! 全部使用内存 fake：Argon2id 的真实参数与并发上限由 infrastructure 单测覆盖，
//! 这里验证的是编排与安全语义。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use application::error::UseCaseError;
use application::identity::{Actor, ActorChannel};
use application::password::{PasswordDeps, PasswordInteractor};
use application::ports::{
    ClearPasswordOutcome, ExternalIdentity, LoginThrottle, OAuthAccountStore, PasswordCredential,
    PasswordHasher, SessionRecord, SessionStore, ThrottleDecision, ThrottleSubject, UserRepository,
};
use domain::identity::{PermissionSet, UserSnapshot};
use time::OffsetDateTime;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Fakes
// ---------------------------------------------------------------------------

/// 可预测的假哈希：`phc::<明文>`；`weak::` 前缀代表旧参数，用于升级路径。
#[derive(Default)]
struct FakeHasher {
    verify_calls: Mutex<u32>,
}

impl FakeHasher {
    fn verify_calls(&self) -> u32 {
        *self.verify_calls.lock().unwrap()
    }
}

#[async_trait::async_trait]
impl PasswordHasher for FakeHasher {
    async fn hash(&self, password: &str) -> Result<String, UseCaseError> {
        Ok(format!("phc::{password}"))
    }

    async fn verify(&self, password: &str, phc_hash: &str) -> Result<bool, UseCaseError> {
        *self.verify_calls.lock().unwrap() += 1;
        // 让出执行权：并发用例里必须真的交错，否则测不出「预占式限流」。
        tokio::task::yield_now().await;
        Ok(phc_hash == format!("phc::{password}") || phc_hash == format!("weak::{password}"))
    }

    fn needs_rehash(&self, phc_hash: &str) -> bool {
        phc_hash.starts_with("weak::")
    }
}

/// 与生产同构的阈值限流：在飞预占与失败共享上限。
///
/// `max_failures` 同时是「窗口内允许的尝试次数」。
struct FakeThrottle {
    max_failures: u32,
    failures: Mutex<HashMap<ThrottleSubject, u32>>,
    in_flight: Mutex<HashMap<ThrottleSubject, u32>>,
    success_subjects: Mutex<Vec<ThrottleSubject>>,
}

impl FakeThrottle {
    fn new(max_failures: u32) -> Self {
        Self {
            max_failures,
            failures: Mutex::new(HashMap::new()),
            in_flight: Mutex::new(HashMap::new()),
            success_subjects: Mutex::new(Vec::new()),
        }
    }

    fn failures_of(&self, subject: &ThrottleSubject) -> u32 {
        self.failures
            .lock()
            .unwrap()
            .get(subject)
            .copied()
            .unwrap_or(0)
    }

    fn in_flight_of(&self, subject: &ThrottleSubject) -> u32 {
        self.in_flight
            .lock()
            .unwrap()
            .get(subject)
            .copied()
            .unwrap_or(0)
    }

    fn success_subjects(&self) -> Vec<ThrottleSubject> {
        self.success_subjects.lock().unwrap().clone()
    }

    fn adjust(map: &Mutex<HashMap<ThrottleSubject, u32>>, subject: &ThrottleSubject, delta: i64) {
        let mut guard = map.lock().unwrap();
        let entry = guard.entry(subject.clone()).or_insert(0);
        if delta >= 0 {
            *entry = entry.saturating_add(delta as u32);
        } else {
            *entry = entry.saturating_sub(delta.unsigned_abs() as u32);
        }
    }
}

impl LoginThrottle for FakeThrottle {
    fn reserve(&self, subject: &ThrottleSubject) -> Result<ThrottleDecision, UseCaseError> {
        let used = self.failures_of(subject) + self.in_flight_of(subject);
        if used >= self.max_failures {
            return Ok(ThrottleDecision {
                allowed: false,
                retry_after_secs: 60,
            });
        }
        Self::adjust(&self.in_flight, subject, 1);
        Ok(ThrottleDecision {
            allowed: true,
            retry_after_secs: 0,
        })
    }

    fn release(&self, subject: &ThrottleSubject) -> Result<(), UseCaseError> {
        Self::adjust(&self.in_flight, subject, -1);
        Ok(())
    }

    fn record_failure(&self, subject: &ThrottleSubject) -> Result<(), UseCaseError> {
        Self::adjust(&self.in_flight, subject, -1);
        Self::adjust(&self.failures, subject, 1);
        Ok(())
    }

    fn record_success(&self, subject: &ThrottleSubject) -> Result<(), UseCaseError> {
        self.success_subjects.lock().unwrap().push(subject.clone());
        Self::adjust(&self.in_flight, subject, -1);
        if matches!(subject, ThrottleSubject::User(_)) {
            self.failures.lock().unwrap().remove(subject);
        }
        Ok(())
    }
}

#[derive(Default)]
struct FakeUserRepo {
    users: Mutex<HashMap<String, UserSnapshot>>,
    passwords: Mutex<HashMap<Uuid, String>>,
    /// 外部身份绑定：`clear_password_hash_guarded` 需要知道还有没有别的登录方式。
    bindings: Mutex<HashMap<Uuid, Vec<ExternalIdentity>>>,
    /// 模拟「写入之前另一请求改了密码」：条件写入时改写存储并报告未命中。
    concurrent_replacement: Mutex<Option<String>>,
    replacement_after_write: Mutex<Option<String>>,
}

impl FakeUserRepo {
    fn insert_user(&self, snapshot: UserSnapshot) {
        self.users
            .lock()
            .unwrap()
            .insert(snapshot.username.clone(), snapshot);
    }

    fn set_hash(&self, user_id: Uuid, hash: &str) {
        self.passwords
            .lock()
            .unwrap()
            .insert(user_id, hash.to_string());
    }

    fn hash_of(&self, user_id: Uuid) -> Option<String> {
        self.passwords.lock().unwrap().get(&user_id).cloned()
    }

    fn has_binding(&self, user_id: Uuid) -> bool {
        self.bindings
            .lock()
            .unwrap()
            .get(&user_id)
            .is_some_and(|list| !list.is_empty())
    }
}

#[async_trait::async_trait]
impl UserRepository for FakeUserRepo {
    async fn save_profile(
        &self,
        user: &domain::identity::User,
        expected_version: i64,
        now: time::OffsetDateTime,
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

    async fn revoke_authentication(
        &self,
        user_id: Uuid,
        _audit_actor: Option<uuid::Uuid>,
    ) -> Result<(), UseCaseError> {
        let mut users = self.users.lock().unwrap();
        let user = users
            .values_mut()
            .find(|u| u.id == user_id)
            .ok_or_else(|| UseCaseError::NotFound("用户".into()))?;
        user.auth_version += 1;
        Ok(())
    }

    /// 头像只走真实认证 HTTP 用例（server/tests）；本 fake 不实现，误用即失败。
    async fn set_avatar(
        &self,
        _user_id: uuid::Uuid,
        _avatar_media_id: Option<uuid::Uuid>,
        _now: time::OffsetDateTime,
    ) -> Result<(), UseCaseError> {
        unimplemented!("该用例不使用头像")
    }

    async fn insert(
        &self,
        aggregate: &domain::identity::User,
        _audit_actor: Option<uuid::Uuid>,
    ) -> Result<(), UseCaseError> {
        let snapshot = aggregate.snapshot();
        self.insert_user(snapshot.clone());
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

    // 密码用例不涉及账号管理列表；返回空列表即可。
    async fn list_admin(
        &self,
        _limit: i64,
        _offset: i64,
    ) -> Result<Vec<application::ports::AdminUserRow>, UseCaseError> {
        Ok(vec![])
    }

    async fn set_password_hash(
        &self,
        user_id: Uuid,
        phc_hash: &str,
        _audit_actor: Option<uuid::Uuid>,
    ) -> Result<(), UseCaseError> {
        self.set_hash(user_id, phc_hash);
        Ok(())
    }

    async fn compare_and_set_password_hash(
        &self,
        user_id: Uuid,
        expected: Option<&str>,
        new_hash: &str,
        _audit_actor: Option<uuid::Uuid>,
    ) -> Result<Option<i64>, UseCaseError> {
        // 模拟「读取之后、写入之前别人改了密码」：改写存储并报告未命中。
        if let Some(replacement) = self.concurrent_replacement.lock().unwrap().take() {
            self.set_hash(user_id, &replacement);
            return Ok(None);
        }
        let mut passwords = self.passwords.lock().unwrap();
        let current = passwords.get(&user_id).map(String::as_str);
        if current == expected {
            passwords.insert(user_id, new_hash.to_string());
            let mut users = self.users.lock().unwrap();
            let user = users.values_mut().find(|user| user.id == user_id).unwrap();
            user.version += 1;
            user.auth_version += 1;
            let revision = user.auth_version;
            // 模拟本次 UPDATE 完成后、调用方继续执行前的管理员重置。
            if let Some(replacement) = self.replacement_after_write.lock().unwrap().take() {
                passwords.insert(user_id, replacement);
                user.version += 1;
                user.auth_version += 1;
            }
            Ok(Some(revision))
        } else {
            Ok(None)
        }
    }

    async fn clear_password_hash(
        &self,
        user_id: Uuid,
        _audit_actor: Option<uuid::Uuid>,
    ) -> Result<(), UseCaseError> {
        self.passwords.lock().unwrap().remove(&user_id);
        Ok(())
    }

    async fn clear_password_hash_guarded(
        &self,
        user_id: Uuid,
        _audit_actor: Option<uuid::Uuid>,
    ) -> Result<ClearPasswordOutcome, UseCaseError> {
        // 生产实现把「检查 + 清除」放在同一把身份锁里；fake 在这里等价地一次完成。
        if !self.passwords.lock().unwrap().contains_key(&user_id) {
            return Ok(ClearPasswordOutcome::NoPassword);
        }
        if !self.has_binding(user_id) {
            return Ok(ClearPasswordOutcome::LastLoginMethod);
        }
        self.passwords.lock().unwrap().remove(&user_id);
        Ok(ClearPasswordOutcome::Cleared)
    }

    async fn find_password_credential(
        &self,
        username: &str,
    ) -> Result<Option<PasswordCredential>, UseCaseError> {
        let Some(user) = self.users.lock().unwrap().get(username).cloned() else {
            return Ok(None);
        };
        if !user.is_active() {
            return Ok(None);
        }
        let hash = self.hash_of(user.id);
        Ok(hash.map(|password_hash| PasswordCredential {
            user_id: user.id,
            password_hash,
            auth_version: user.auth_version,
        }))
    }

    async fn password_hash_of(&self, user_id: Uuid) -> Result<Option<String>, UseCaseError> {
        Ok(self.hash_of(user_id))
    }
}

#[derive(Default)]
struct FakeSessions {
    entries: Mutex<HashMap<String, SessionRecord>>,
    revoked_users: Mutex<Vec<Uuid>>,
    next: Mutex<u64>,
}

impl FakeSessions {
    fn revoked_users(&self) -> Vec<Uuid> {
        self.revoked_users.lock().unwrap().clone()
    }

    fn active_count(&self) -> usize {
        self.entries.lock().unwrap().len()
    }
}

#[async_trait::async_trait]
impl SessionStore for FakeSessions {
    async fn create(&self, user_id: Uuid, auth_version: i64) -> Result<String, UseCaseError> {
        let mut next = self.next.lock().unwrap();
        *next += 1;
        let token = format!("session-{}", *next);
        self.entries.lock().unwrap().insert(
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
        Ok(self.entries.lock().unwrap().get(token).cloned())
    }

    async fn revoke(&self, token: &str) -> Result<(), UseCaseError> {
        self.entries.lock().unwrap().remove(token);
        Ok(())
    }

    async fn revoke_all_for_user(&self, user_id: Uuid) -> Result<(), UseCaseError> {
        self.revoked_users.lock().unwrap().push(user_id);
        self.entries
            .lock()
            .unwrap()
            .retain(|_, r| r.user_id != user_id);
        Ok(())
    }
}

// 外部身份绑定直接放在 FakeUserRepo 上：`clear_password_hash_guarded` 需要在
// **同一个** fake 里看到「还有没有别的登录方式」，拆成两个 fake 就测不出原子性。
#[async_trait::async_trait]
impl OAuthAccountStore for FakeUserRepo {
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
            .find(|(_, list)| {
                list.iter().any(|b| {
                    b.provider_key == provider_key && b.provider_user_id == provider_user_id
                })
            })
            .map(|(user, _)| *user))
    }

    async fn bind(
        &self,
        user_id: Uuid,
        provider_key: &str,
        provider_user_id: &str,
        email: Option<String>,
        _audit_actor: Option<uuid::Uuid>,
    ) -> Result<(), UseCaseError> {
        self.bindings
            .lock()
            .unwrap()
            .entry(user_id)
            .or_default()
            .push(ExternalIdentity {
                provider_key: provider_key.into(),
                provider_user_id: provider_user_id.into(),
                email,
            });
        Ok(())
    }

    async fn unbind(
        &self,
        user_id: Uuid,
        provider_key: &str,
        provider_user_id: &str,
        _audit_actor: Option<uuid::Uuid>,
    ) -> Result<(), UseCaseError> {
        if let Some(list) = self.bindings.lock().unwrap().get_mut(&user_id) {
            list.retain(|b| {
                !(b.provider_key == provider_key && b.provider_user_id == provider_user_id)
            });
        }
        Ok(())
    }

    async fn list_for_user(&self, user_id: Uuid) -> Result<Vec<ExternalIdentity>, UseCaseError> {
        Ok(self
            .bindings
            .lock()
            .unwrap()
            .get(&user_id)
            .cloned()
            .unwrap_or_default())
    }
}

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

const PASSWORD: &str = "correct horse battery staple";

struct Fixture {
    passwords: Arc<PasswordInteractor>,
    repo: Arc<FakeUserRepo>,
    hasher: Arc<FakeHasher>,
    throttle: Arc<FakeThrottle>,
    sessions: Arc<FakeSessions>,
    /// 与 `repo` 是同一份状态：外部身份绑定由同一个 fake 承载。
    accounts: Arc<FakeUserRepo>,
    user_id: Uuid,
}

impl Fixture {
    fn session_actor(&self) -> Actor {
        Actor::new(
            domain::identity::UserId(self.user_id),
            ActorChannel::Session,
            PermissionSet::from_keys(["post.read"]),
        )
    }
}

fn active_user(username: &str) -> UserSnapshot {
    let now = OffsetDateTime::now_utc();
    UserSnapshot {
        id: Uuid::now_v7(),
        username: username.to_string(),
        email: None,
        display_name: Some(username.to_string()),
        bio: None,
        status: domain::identity::UserStatus::Active,
        auth_version: 1,
        version: 1,
        created_at: now,
        updated_at: now,
        deleted_at: None,
        avatar_media_id: None,
    }
}

/// 已带密码 `PASSWORD` 的用户 `sun`；限流阈值可配置（默认宽松）。
async fn fixture_with_threshold(max_failures: u32) -> Fixture {
    let repo = Arc::new(FakeUserRepo::default());
    let user = active_user("sun");
    let user_id = user.id;
    repo.insert_user(user);
    repo.set_hash(user_id, &format!("phc::{PASSWORD}"));

    let hasher = Arc::new(FakeHasher::default());
    let throttle = Arc::new(FakeThrottle::new(max_failures));
    let sessions = Arc::new(FakeSessions::default());
    // 外部身份绑定与用户/凭据在同一个 fake 上，`clear_password_hash_guarded` 才能看到。
    let accounts = repo.clone();

    let passwords = Arc::new(PasswordInteractor::new(PasswordDeps {
        users: repo.clone(),
        hasher: hasher.clone(),
        throttle: throttle.clone(),
        sessions: sessions.clone(),
    }));
    Fixture {
        passwords,
        repo,
        hasher,
        throttle,
        sessions,
        accounts,
        user_id,
    }
}

async fn fixture() -> Fixture {
    fixture_with_threshold(5).await
}

// ---------------------------------------------------------------------------
// 登录
// ---------------------------------------------------------------------------

#[tokio::test]
async fn login_with_correct_password_issues_session() {
    let f = fixture().await;

    let login = f
        .passwords
        .login("  Sun ", PASSWORD, Some("203.0.113.7"), "/admin")
        .await
        .unwrap();
    assert_eq!(login.user_id, f.user_id);
    assert_eq!(login.next, "/admin");
    assert_eq!(f.sessions.active_count(), 1);
    // 成功对两个维度都要落账：账号维度清空计数，来源地址维度只归还预占。
    assert_eq!(
        f.throttle.success_subjects(),
        vec![
            ThrottleSubject::User("sun".into()),
            ThrottleSubject::Client("203.0.113.7".into())
        ]
    );
}

#[tokio::test]
async fn invalid_next_fails_before_creating_a_session() {
    let f = fixture().await;
    let err = f
        .passwords
        .login("sun", PASSWORD, None, "https://evil.example/steal")
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::Invalid(_)), "{err:?}");
    assert_eq!(
        f.sessions.active_count(),
        0,
        "非法回跳必须在签发会话之前失败，不能留下孤儿会话"
    );
}

#[tokio::test]
async fn unknown_user_and_wrong_password_are_indistinguishable() {
    let f = fixture().await;

    let before = f.hasher.verify_calls();
    let wrong = f
        .passwords
        .login("sun", "wrong password value", None, "/")
        .await
        .unwrap_err();
    let wrong_calls = f.hasher.verify_calls() - before;

    let before = f.hasher.verify_calls();
    let unknown = f
        .passwords
        .login("nobody", "wrong password value", None, "/")
        .await
        .unwrap_err();
    let unknown_calls = f.hasher.verify_calls() - before;

    assert!(matches!(wrong, UseCaseError::InvalidCredentials));
    assert!(matches!(unknown, UseCaseError::InvalidCredentials));
    assert_eq!(
        wrong_calls, unknown_calls,
        "未知用户也必须执行一次等价开销的校验，避免时间侧信道"
    );
    assert_eq!(f.sessions.active_count(), 0);
}

#[tokio::test]
async fn throttle_rejects_before_verifying() {
    let f = fixture_with_threshold(1).await;

    f.passwords
        .login("sun", "wrong password value", None, "/")
        .await
        .unwrap_err();
    let calls_after_failure = f.hasher.verify_calls();

    // 即使密码正确，达到阈值后也不再校验、不再签发会话。
    let err = f
        .passwords
        .login("sun", PASSWORD, None, "/")
        .await
        .unwrap_err();
    match err {
        UseCaseError::RateLimited { retry_after_secs } => assert!(retry_after_secs >= 1),
        other => panic!("期望限流错误，得到 {other:?}"),
    }
    assert_eq!(
        f.hasher.verify_calls(),
        calls_after_failure,
        "被限流的尝试不得进入哈希校验"
    );
    assert_eq!(f.sessions.active_count(), 0);
}

#[tokio::test]
async fn client_counter_is_not_cleared_by_successful_login() {
    let f = fixture().await;

    f.passwords
        .login("sun", "wrong password value", Some("198.51.100.9"), "/")
        .await
        .unwrap_err();
    assert_eq!(
        f.throttle
            .failures_of(&ThrottleSubject::Client("198.51.100.9".into())),
        1
    );

    f.passwords
        .login("sun", PASSWORD, Some("198.51.100.9"), "/")
        .await
        .unwrap();
    assert_eq!(
        f.throttle
            .failures_of(&ThrottleSubject::Client("198.51.100.9".into())),
        1,
        "成功登录不得让攻击者凭自有账号清零来源地址计数"
    );
    assert_eq!(
        f.throttle.failures_of(&ThrottleSubject::User("sun".into())),
        0
    );
}

#[tokio::test]
async fn weak_hash_is_upgraded_on_successful_login() {
    let f = fixture().await;
    // 旧参数哈希：仍然可用，但登录成功后应被当前参数覆盖。
    let weak = format!("weak::{PASSWORD}");
    f.repo.set_hash(f.user_id, &weak);
    assert_eq!(f.repo.hash_of(f.user_id).as_deref(), Some(weak.as_str()));

    f.passwords.login("sun", PASSWORD, None, "/").await.unwrap();
    assert_eq!(
        f.repo.hash_of(f.user_id).as_deref(),
        Some(format!("phc::{PASSWORD}").as_str())
    );
}

#[tokio::test]
async fn login_that_races_a_password_change_is_rejected() {
    let f = fixture().await;
    // 旧参数触发升级；升级写入时「另一个改密请求」已把口令换成新值。
    f.repo.set_hash(f.user_id, &format!("weak::{PASSWORD}"));
    *f.repo.concurrent_replacement.lock().unwrap() = Some("phc::someone-changed-it".into());

    let err = f
        .passwords
        .login("sun", PASSWORD, None, "/")
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::InvalidCredentials));
    assert_eq!(f.sessions.active_count(), 0, "并发改密后不得用旧口令建会话");
    assert_eq!(
        f.repo.hash_of(f.user_id).as_deref(),
        Some("phc::someone-changed-it"),
        "并发改密的结果不得被登录升级覆盖（否则旧口令会被复活）"
    );
}

#[tokio::test]
async fn malformed_username_shares_the_failure_path() {
    let f = fixture().await;
    let err = f
        .passwords
        .login("空间 用户", "wrong password value", None, "/")
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::InvalidCredentials));
    assert_eq!(
        f.hasher.verify_calls(),
        1,
        "形状非法的用户名也要走一次（且仅一次）等价开销的失败校验"
    );
}

// ---------------------------------------------------------------------------
// 受控设置 / 清除
// ---------------------------------------------------------------------------

#[tokio::test]
async fn set_password_enforces_policy_and_revokes_sessions() {
    let f = fixture().await;
    f.sessions.create(f.user_id, 1).await.unwrap();
    f.sessions.create(f.user_id, 1).await.unwrap();

    // 太短：策略拒绝，且不写入。
    let err = f
        .passwords
        .set_password(&Actor::bootstrap_cli(), "sun", "short")
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::Invalid(_)), "{err:?}");

    f.passwords
        .set_password(&Actor::bootstrap_cli(), "sun", "a much better passphrase")
        .await
        .unwrap();
    assert_eq!(
        f.repo.hash_of(f.user_id).as_deref(),
        Some("phc::a much better passphrase")
    );
    assert_eq!(f.sessions.revoked_users(), vec![f.user_id]);
    assert_eq!(f.sessions.active_count(), 0);
}

#[tokio::test]
async fn set_password_requires_user_manage_permission() {
    let f = fixture().await;
    let weak_actor = Actor::new(
        domain::identity::UserId(f.user_id),
        ActorChannel::Session,
        PermissionSet::from_keys(["post.read"]),
    );
    let err = f
        .passwords
        .set_password(&weak_actor, "sun", "a much better passphrase")
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::Forbidden));
}

#[tokio::test]
async fn clear_password_refuses_to_remove_the_last_login_method() {
    let f = fixture().await;
    let err = f
        .passwords
        .clear_password(&Actor::bootstrap_cli(), "sun")
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::Invalid(_)), "{err:?}");
    assert!(f.repo.hash_of(f.user_id).is_some(), "拒绝时必须保留密码");
}

#[tokio::test]
async fn clear_password_succeeds_when_another_binding_exists() {
    let f = fixture().await;
    f.accounts
        .bind(f.user_id, "https://idp.example", "sub-1", None, None)
        .await
        .unwrap();
    f.sessions.create(f.user_id, 1).await.unwrap();

    f.passwords
        .clear_password(&Actor::bootstrap_cli(), "sun")
        .await
        .unwrap();
    assert!(f.repo.hash_of(f.user_id).is_none());
    assert_eq!(f.sessions.revoked_users(), vec![f.user_id]);
    assert!(!f.passwords.password_enabled("sun").await.unwrap());
}

#[tokio::test]
async fn clear_password_rejects_when_not_enabled() {
    let f = fixture().await;
    f.accounts
        .bind(f.user_id, "https://idp.example", "sub-1", None, None)
        .await
        .unwrap();
    // 直接清空存储，模拟「本来就没启用密码」。
    f.repo.passwords.lock().unwrap().clear();
    let err = f
        .passwords
        .clear_password(&Actor::bootstrap_cli(), "sun")
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::Invalid(_)), "{err:?}");
}

// ---------------------------------------------------------------------------
// 自助改密
// ---------------------------------------------------------------------------

#[tokio::test]
async fn change_own_password_requires_session_channel() {
    let f = fixture().await;
    let err = f
        .passwords
        .change_own_password(
            &Actor::bootstrap_cli(),
            Some(PASSWORD),
            "brand new passphrase",
            None,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::Forbidden));
}

#[tokio::test]
async fn change_own_password_requires_current_password() {
    let f = fixture().await;
    let actor = f.session_actor();

    // 缺少当前密码。
    let err = f
        .passwords
        .change_own_password(&actor, None, "brand new passphrase", None)
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::Invalid(_)), "{err:?}");

    // 当前密码错误。
    let err = f
        .passwords
        .change_own_password(
            &actor,
            Some("not the current one"),
            "brand new passphrase",
            None,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::InvalidCredentials));

    // 失败不得改动凭据或撤销会话。
    assert_eq!(
        f.repo.hash_of(f.user_id).as_deref(),
        Some(format!("phc::{PASSWORD}").as_str())
    );
    assert!(f.sessions.revoked_users().is_empty());
}

#[tokio::test]
async fn change_own_password_rotates_sessions_and_sets_new_secret() {
    let f = fixture().await;
    let actor = f.session_actor();
    // 模拟当前浏览器会话与另一个旧会话。
    let current = f.sessions.create(f.user_id, 1).await.unwrap();
    f.sessions.create(f.user_id, 1).await.unwrap();

    let rotated = f
        .passwords
        .change_own_password(&actor, Some(PASSWORD), "brand new passphrase", None)
        .await
        .unwrap();
    assert_ne!(rotated, current, "必须签发新的会话令牌");
    assert_eq!(f.sessions.revoked_users(), vec![f.user_id]);
    assert_eq!(
        f.repo.hash_of(f.user_id).as_deref(),
        Some("phc::brand new passphrase")
    );

    // 新密码可用，旧密码不可用。
    f.passwords
        .login("sun", "brand new passphrase", None, "/")
        .await
        .unwrap();
    let err = f
        .passwords
        .login("sun", PASSWORD, None, "/")
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::InvalidCredentials));
}

#[tokio::test]
async fn change_own_password_allows_setting_initial_password_for_oauth_user() {
    let f = fixture().await;
    f.repo.passwords.lock().unwrap().clear();
    f.accounts
        .bind(f.user_id, "https://idp.example", "sub-1", None, None)
        .await
        .unwrap();

    // 未启用密码时不要求当前密码（会话本身即身份证明）。
    f.passwords
        .change_own_password(&f.session_actor(), None, "first local passphrase", None)
        .await
        .unwrap();
    assert!(f.passwords.password_enabled("sun").await.unwrap());
}

// ---------------------------------------------------------------------------
// 评审回归：并发与交叉写入
// ---------------------------------------------------------------------------

/// 回归：并发登录不能绕过失败次数限制。
///
/// 旧实现是「先 check、再慢慢校验、最后才 record_failure」，N 个并发请求会在
/// 任何一次失败被记录之前**全部**通过检查，阈值形同虚设。预占模型下只有阈值内的
/// 请求能进入校验。
#[tokio::test]
async fn concurrent_logins_cannot_exceed_the_failure_budget() {
    let f = fixture_with_threshold(5).await;

    let (r1, r2, r3, r4, r5, r6) = tokio::join!(
        f.passwords.login("sun", "wrong-1", None, "/"),
        f.passwords.login("sun", "wrong-2", None, "/"),
        f.passwords.login("sun", "wrong-3", None, "/"),
        f.passwords.login("sun", "wrong-4", None, "/"),
        f.passwords.login("sun", "wrong-5", None, "/"),
        f.passwords.login("sun", "wrong-6", None, "/"),
    );
    let results = [r1, r2, r3, r4, r5, r6];

    let denied = results
        .iter()
        .filter(|r| matches!(r, Err(UseCaseError::RateLimited { .. })))
        .count();
    let rejected = results
        .iter()
        .filter(|r| matches!(r, Err(UseCaseError::InvalidCredentials)))
        .count();
    assert_eq!(denied, 1, "并发第 6 个必须被限流：{results:?}");
    assert_eq!(rejected, 5);
    assert_eq!(
        f.hasher.verify_calls(),
        5,
        "并发下也只允许阈值内的校验次数（旧实现会做 6 次）"
    );
    assert_eq!(f.sessions.active_count(), 0);
}

/// 在哈希校验让出执行权时丢弃 future，模拟超时/请求任务取消。
#[tokio::test]
async fn cancelled_authentication_releases_only_its_own_reservations() {
    use std::future::{Future, poll_fn};
    use std::task::Poll;

    for change_password in [false, true] {
        let f = fixture_with_threshold(2).await;
        let actor = f.session_actor();
        let user = ThrottleSubject::User("sun".into());
        let client = ThrottleSubject::Client("203.0.113.7".into());
        // 另一个请求的预占必须保留。
        assert!(f.throttle.reserve(&user).unwrap().allowed);
        assert!(f.throttle.reserve(&client).unwrap().allowed);
        let mut attempt = Box::pin(async {
            if change_password {
                f.passwords
                    .change_own_password(
                        &actor,
                        Some(PASSWORD),
                        "brand new passphrase",
                        Some("203.0.113.7"),
                    )
                    .await
                    .map(|_| ())
            } else {
                f.passwords
                    .login("sun", PASSWORD, Some("203.0.113.7"), "/")
                    .await
                    .map(|_| ())
            }
        });
        poll_fn(|cx| {
            assert!(attempt.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        assert_eq!(f.hasher.verify_calls(), 1);
        assert_eq!(f.throttle.in_flight_of(&user), 2);
        assert_eq!(f.throttle.in_flight_of(&client), 2);
        drop(attempt);
        for subject in [&user, &client] {
            assert_eq!(f.throttle.in_flight_of(subject), 1);
            assert_eq!(f.throttle.failures_of(subject), 0);
        }
        // 取消后额度立即可用，成功结算也不能重复释放其他请求的预占。
        f.passwords
            .login("sun", PASSWORD, Some("203.0.113.7"), "/")
            .await
            .unwrap();
        assert_eq!(f.throttle.in_flight_of(&user), 1);
        assert_eq!(f.throttle.in_flight_of(&client), 1);
    }
}

#[tokio::test]
async fn second_dimension_denial_releases_first_reservation() {
    let f = fixture_with_threshold(1).await;
    let user = ThrottleSubject::User("sun".into());
    let client = ThrottleSubject::Client("203.0.113.7".into());
    assert!(f.throttle.reserve(&client).unwrap().allowed);
    let result = f
        .passwords
        .login("sun", PASSWORD, Some("203.0.113.7"), "/")
        .await;
    assert!(matches!(result, Err(UseCaseError::RateLimited { .. })));
    assert_eq!(f.throttle.in_flight_of(&user), 0);
    assert_eq!(f.throttle.in_flight_of(&client), 1);
    assert_eq!(f.hasher.verify_calls(), 0);
}

/// 回归：改密会话不能借用随后管理员重置产生的版本。
#[tokio::test]
async fn password_change_session_uses_its_own_write_revision() {
    // 已有密码与 OAuth 用户首次设置密码共用同一条会话签发路径。
    for initial_password in [false, true] {
        for concurrent_reset in [false, true] {
            let f = fixture().await;
            if initial_password {
                f.repo.passwords.lock().unwrap().clear();
            }
            if concurrent_reset {
                *f.repo.replacement_after_write.lock().unwrap() =
                    Some("phc::admin-forced-reset".into());
            }
            let token = f
                .passwords
                .change_own_password(
                    &f.session_actor(),
                    if initial_password {
                        None
                    } else {
                        Some(PASSWORD)
                    },
                    "brand new passphrase",
                    None,
                )
                .await
                .unwrap();
            let session = f.sessions.validate(&token).await.unwrap().unwrap();
            let current = f.repo.find_by_id(f.user_id).await.unwrap().unwrap();
            assert_eq!(session.auth_version, 2, "必须使用本次写入返回的版本");
            if concurrent_reset {
                assert_eq!(current.auth_version, 3);
                // actor_from_session 以这两个版本是否相等判定会话有效性。
                assert_ne!(session.auth_version, current.auth_version);
                assert_eq!(
                    f.repo.hash_of(f.user_id).as_deref(),
                    Some("phc::admin-forced-reset")
                );
            } else {
                assert_eq!(session.auth_version, current.auth_version);
            }
        }
    }
}

/// 回归：自助改密不得覆盖并发的管理员强制重置。
#[tokio::test]
async fn self_change_does_not_overwrite_a_concurrent_admin_reset() {
    let f = fixture().await;
    let actor = f.session_actor();
    // 模拟：自助改密读到旧哈希、校验通过之后，管理员已下发新口令。
    *f.repo.concurrent_replacement.lock().unwrap() = Some("phc::admin-forced-reset".into());

    let err = f
        .passwords
        .change_own_password(&actor, Some(PASSWORD), "brand new passphrase", None)
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::VersionConflict), "{err:?}");
    assert_eq!(
        f.repo.hash_of(f.user_id).as_deref(),
        Some("phc::admin-forced-reset"),
        "管理员的重置不得被用户自助改密覆盖"
    );
    assert!(
        f.sessions.revoked_users().is_empty(),
        "未写入就不得撤销/轮换会话"
    );
}

/// 回归：OAuth 用户设置初始密码同样不得覆盖并发的管理员重置（期望值为「当前必须为空」）。
#[tokio::test]
async fn initial_password_set_does_not_overwrite_a_concurrent_admin_reset() {
    let f = fixture().await;
    f.repo.passwords.lock().unwrap().clear();
    f.accounts
        .bind(f.user_id, "https://idp.example", "sub-1", None, None)
        .await
        .unwrap();
    *f.repo.concurrent_replacement.lock().unwrap() = Some("phc::admin-forced-reset".into());

    let err = f
        .passwords
        .change_own_password(&f.session_actor(), None, "first local passphrase", None)
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::VersionConflict), "{err:?}");
    assert_eq!(
        f.repo.hash_of(f.user_id).as_deref(),
        Some("phc::admin-forced-reset")
    );
}

/// 回归：重新认证也要限流，被盗会话不能无限次试当前密码。
#[tokio::test]
async fn reauthentication_shares_the_login_failure_budget() {
    let f = fixture_with_threshold(2).await;
    let actor = f.session_actor();

    for _ in 0..2 {
        let err = f
            .passwords
            .change_own_password(
                &actor,
                Some("not the current one"),
                "brand new passphrase",
                None,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, UseCaseError::InvalidCredentials), "{err:?}");
    }
    let calls_after_budget = f.hasher.verify_calls();

    // 预算已用尽：即使这次给出正确的当前密码也直接限流，且不做哈希校验。
    let err = f
        .passwords
        .change_own_password(&actor, Some(PASSWORD), "brand new passphrase", None)
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::RateLimited { .. }), "{err:?}");
    assert_eq!(
        f.hasher.verify_calls(),
        calls_after_budget,
        "被限流的重新认证不得进入哈希校验"
    );
}
