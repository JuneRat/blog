//! 本地密码用例：Argon2id 校验、失败限流、受控设置/重置与自助改密。
//!
//! 契约见 docs/identity-and-admin.md §7：
//! - 「用户不存在」「密码错误」「账号已停用」返回同一个 [`UseCaseError::InvalidCredentials`]，
//!   且未知用户也执行一次等价开销的哈希校验，避免用错误文案或响应时间枚举用户名。
//! - 失败按用户名与客户端地址分别计数；成功登录只清空用户名计数，
//!   避免攻击者用自己的账号反复把来源地址的计数清零。
//! - 设置/清除密码需 `user.manage`；清除若会移除最后一种登录方式则拒绝。
//! - 自助改密必须由已认证会话发起并重新提供当前密码；成功后轮换该用户全部会话。
//! - 密码重置的**唯一**入口是受控 CLI（部署权限）；面向公众的自助找回需要
//!   一次性令牌存储与邮件投递，未在本版交付，也不以 13 表之外的临时方案冒充。

use std::sync::{Arc, OnceLock};

use uuid::Uuid;

use crate::error::UseCaseError;
use crate::identity::{Actor, ActorChannel};
use crate::ports::{
    ClearPasswordOutcome, LoginThrottle, PasswordHasher, SessionStore, ThrottleSubject,
    UserRepository,
};
use domain::identity::UserSnapshot;

/// 登录成功产物：会话令牌 + 受控回跳路径 + 用户 id。
#[derive(Debug)]
pub struct PasswordLogin {
    pub token: String,
    pub next: String,
    pub user_id: Uuid,
}

/// 登录用例的出站依赖集合。
pub struct PasswordDeps {
    pub users: Arc<dyn UserRepository>,
    pub hasher: Arc<dyn PasswordHasher>,
    pub throttle: Arc<dyn LoginThrottle>,
    pub sessions: Arc<dyn SessionStore>,
}

pub struct PasswordInteractor {
    deps: PasswordDeps,
    /// 未知用户也做一次等价开销校验；首次使用时用固定明文生成。
    dummy_hash: OnceLock<String>,
}

impl PasswordInteractor {
    pub fn new(deps: PasswordDeps) -> Self {
        Self {
            deps,
            dummy_hash: OnceLock::new(),
        }
    }

    /// 密码登录：预占限流额度 → 凭据校验（未知用户等价开销）→ 落账 → 签发会话。
    pub async fn login(
        &self,
        username: &str,
        password: &str,
        client_key: Option<&str>,
        next: &str,
        ip_address: Option<std::net::IpAddr>,
    ) -> Result<PasswordLogin, UseCaseError> {
        // 回跳路径先校验：非法值必须在任何写入（含签发会话）之前失败，避免留下孤儿会话。
        let next = crate::auth::sanitize_next(next)?.to_string();

        let subjects = throttle_subjects(&throttle_username(username), client_key);
        // 预占必须在校验之前：只做事后计数的话，并发请求会在任何一次失败被记录之前
        // 全部通过检查，阈值形同虚设（N 个并发请求 = N 次 Argon2 猜测机会）。
        let reservation = self.reserve_all(&subjects)?;

        match self.verify_login(username, password).await? {
            Some(credential) => {
                // 成功：账号维度清空计数，来源地址维度只归还预占（实现按主体区分）。
                reservation.finish(true)?;
                self.finish_login(username, password, credential, next, ip_address)
                    .await
            }
            None => {
                reservation.finish(false)?;
                Err(UseCaseError::InvalidCredentials)
            }
        }
    }

    /// 校验口令并返回凭据；`None` 表示凭据错误。任何内部错误都会先归还预占。
    async fn verify_login(
        &self,
        username: &str,
        password: &str,
    ) -> Result<Option<crate::ports::PasswordCredential>, UseCaseError> {
        // 用户名形状非法与「不存在」不可区分：一律走统一失败路径。
        let normalized = domain::identity::normalize_username(username).ok();
        let credential = match normalized.as_deref() {
            Some(normalized) => self.deps.users.find_password_credential(normalized).await,
            None => Ok(None),
        };
        let credential = credential?;
        let verified = match credential.as_ref() {
            Some(credential) => {
                self.deps
                    .hasher
                    .verify(password, &credential.password_hash)
                    .await
            }
            None => match self.dummy_hash().await {
                // 等价的固定开销：否则「立即失败」会暴露用户名不存在。
                // 命中固定明文也一律按失败处理（此处没有可登录的账号）。
                Ok(dummy) => self
                    .deps
                    .hasher
                    .verify(password, dummy)
                    .await
                    .map(|_| false),
                Err(e) => Err(e),
            },
        };
        match verified {
            Ok(true) => Ok(credential),
            Ok(false) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// 登录成功后的收尾：透明升级弱参数哈希、复核凭据未被并发替换、签发会话。
    async fn finish_login(
        &self,
        username: &str,
        password: &str,
        credential: crate::ports::PasswordCredential,
        next: String,
        ip_address: Option<std::net::IpAddr>,
    ) -> Result<PasswordLogin, UseCaseError> {
        // 透明升级（条件写入）+ 并发复核：改密与登录同时发生时，不能用已失效的口令建会话。
        let current_hash = self
            .upgrade_hash_if_needed(password, &credential, ip_address)
            .await;
        let normalized =
            domain::identity::normalize_username(username).expect("持有凭据时用户名必然规范化成功");
        let current = self
            .deps
            .users
            .find_password_credential(&normalized)
            .await?
            .filter(|current| {
                current.user_id == credential.user_id && current.password_hash == current_hash
            })
            .ok_or(UseCaseError::InvalidCredentials)?;

        // 会话绑定读取到的 users.auth_version：此后改密/认证撤销/软删除都会让它失效。
        let token = self
            .deps
            .sessions
            .create(current.user_id, current.auth_version)
            .await?;
        Ok(PasswordLogin {
            token,
            next,
            user_id: current.user_id,
        })
    }

    /// 受控设置/重置密码：需 `user.manage`；成功后撤销目标用户全部会话。
    ///
    /// 这也是泄露处置的强制重置入口，因此**不做**「必须已有某种登录方式」的限制：
    /// 随时可以给任意活跃账号下发新密码。
    pub async fn set_password(
        &self,
        actor: &Actor,
        username: &str,
        new_password: &str,
    ) -> Result<(), UseCaseError> {
        actor.ensure_write_channel()?;
        if !actor.has_permission("user.manage") {
            return Err(UseCaseError::Forbidden);
        }
        let target = self.active_user(username).await?;
        domain::identity::validate_password(new_password, &target.username)
            .map_err(|e| UseCaseError::Invalid(e.to_string()))?;
        let hash = self.deps.hasher.hash(new_password).await?;
        self.deps
            .users
            .set_password_hash(target.id, &hash, actor.audit_context())
            .await?;
        // 凭据变更即撤销全部既有会话：被盗会话不能靠旧 cookie 存活。
        self.deps.sessions.revoke_all_for_user(target.id).await?;
        Ok(())
    }

    /// 清除密码（禁用密码登录）。与解绑外部身份同一条保护：
    /// 不能把账号的最后一种有效登录方式去掉，否则账号直接失去可登录性。
    ///
    /// 「是否还有别的登录方式」的判定与清除在同一把身份锁、同一事务里完成，
    /// 与解绑外部身份互斥——分开做会两条路径同时通过，把登录方式一起清空。
    pub async fn clear_password(&self, actor: &Actor, username: &str) -> Result<(), UseCaseError> {
        actor.ensure_write_channel()?;
        if !actor.has_permission("user.manage") {
            return Err(UseCaseError::Forbidden);
        }
        let target = self.active_user(username).await?;
        match self
            .deps
            .users
            .clear_password_hash_guarded(target.id, actor.audit_context())
            .await?
        {
            ClearPasswordOutcome::Cleared => {
                // 登录方式变化：旧 Cookie 立即失效。
                self.deps.sessions.revoke_all_for_user(target.id).await?;
                Ok(())
            }
            ClearPasswordOutcome::NoPassword => Err(UseCaseError::Invalid(format!(
                "用户 {} 未启用密码登录",
                target.username
            ))),
            ClearPasswordOutcome::LastLoginMethod => Err(UseCaseError::Invalid(
                "密码是该用户最后一种登录方式；如需更换请直接设置新密码，而不是清除".into(),
            )),
        }
    }

    /// 自助改密：已认证会话 + 重新提供当前密码（已启用密码登录时）。
    ///
    /// 成功后撤销该用户全部会话并签发新会话令牌返回给调用方，
    /// 因此当前 cookie 之外的其它会话（可能已泄露）一并失效。
    /// 重新认证与登录**共用失败预算**，被盗会话无法无限次试当前密码。
    pub async fn change_own_password(
        &self,
        actor: &Actor,
        current_password: Option<&str>,
        new_password: &str,
        client_key: Option<&str>,
    ) -> Result<String, UseCaseError> {
        actor.ensure_write_channel()?;
        // 自助改密是「重新认证」动作：只接受已认证浏览器会话，不接受 CLI 引导身份。
        if actor.channel != ActorChannel::Session {
            return Err(UseCaseError::Forbidden);
        }
        let target = self
            .deps
            .users
            .find_by_id(actor.user_id.0)
            .await?
            .filter(UserSnapshot::is_active)
            .ok_or(UseCaseError::Unauthenticated)?;

        let existing = self.deps.users.password_hash_of(actor.user_id.0).await?;
        let Some(existing) = existing else {
            // 未启用密码登录（OAuth 用户设置初始密码）：没有可校验的凭据，也就没有爆破面。
            return self
                .apply_new_password(
                    actor.user_id.0,
                    &target.username,
                    new_password,
                    None,
                    actor.audit_context(),
                )
                .await;
        };
        let Some(current) = current_password else {
            return Err(UseCaseError::Invalid("请输入当前密码以确认身份".into()));
        };

        let subjects = throttle_subjects(&throttle_username(&target.username), client_key);
        let reservation = self.reserve_all(&subjects)?;
        match self.deps.hasher.verify(current, &existing).await {
            Ok(true) => {
                reservation.finish(true)?;
            }
            Ok(false) => {
                reservation.finish(false)?;
                return Err(UseCaseError::InvalidCredentials);
            }
            Err(e) => return Err(e),
        }
        self.apply_new_password(
            actor.user_id.0,
            &target.username,
            new_password,
            Some(&existing),
            actor.audit_context(),
        )
        .await
    }

    /// 校验新口令、条件写入并轮换会话。
    ///
    /// `expected_current` 是刚校验过的旧哈希（`None` 表示当前必须为空）：条件写入，
    /// 期间若被管理员强制重置，本次自助改密作废，而不是把新口令覆盖上去。
    async fn apply_new_password(
        &self,
        user_id: Uuid,
        username: &str,
        new_password: &str,
        expected_current: Option<&str>,
        audit: crate::audit::AuditContext,
    ) -> Result<String, UseCaseError> {
        domain::identity::validate_password(new_password, username)
            .map_err(|e| UseCaseError::Invalid(e.to_string()))?;
        let hash = self.deps.hasher.hash(new_password).await?;
        let revision = self
            .deps
            .users
            .compare_and_set_password_hash(user_id, expected_current, &hash, audit)
            .await?
            .ok_or(UseCaseError::VersionConflict)?;
        self.deps.sessions.revoke_all_for_user(user_id).await?;
        // 只绑定本次写入产生的版本；之后发生的管理员重置必须让该会话失效。
        self.deps.sessions.create(user_id, revision).await
    }

    /// 是否已启用密码登录（CLI/后台展示用）。
    pub async fn password_enabled(&self, username: &str) -> Result<bool, UseCaseError> {
        let normalized = domain::identity::normalize_username(username)
            .map_err(|e| UseCaseError::Invalid(e.to_string()))?;
        Ok(self
            .deps
            .users
            .find_password_credential(&normalized)
            .await?
            .is_some())
    }

    /// 预占全部限流维度；任一维度拒绝或出错时，归还本次已占用的额度。
    fn reserve_all<'a>(
        &'a self,
        subjects: &'a [ThrottleSubject],
    ) -> Result<ThrottleReservation<'a>, UseCaseError> {
        let mut reservation = ThrottleReservation {
            throttle: self.deps.throttle.as_ref(),
            subjects,
            reserved: 0,
            settled: 0,
        };
        for subject in subjects {
            let decision = reservation.throttle.reserve(subject)?;
            if !decision.allowed {
                return Err(UseCaseError::RateLimited {
                    retry_after_secs: decision.retry_after_secs.max(1),
                });
            }
            reservation.reserved += 1;
        }
        Ok(reservation)
    }

    /// 按当前策略升级弱参数哈希，返回调用方应视为「当前」的哈希值。
    ///
    /// 用条件写入（compare-and-swap）避免覆盖并发改密的结果；升级失败不影响本次
    /// 登录（下次登录会再试），返回原哈希交由调用方复核。
    async fn upgrade_hash_if_needed(
        &self,
        password: &str,
        credential: &crate::ports::PasswordCredential,
        ip_address: Option<std::net::IpAddr>,
    ) -> String {
        if !self.deps.hasher.needs_rehash(&credential.password_hash) {
            return credential.password_hash.clone();
        }
        let Ok(upgraded) = self.deps.hasher.hash(password).await else {
            return credential.password_hash.clone();
        };
        match self
            .deps
            .users
            .compare_and_set_password_hash(
                credential.user_id,
                Some(&credential.password_hash),
                &upgraded,
                crate::audit::AuditContext {
                    actor_id: Some(credential.user_id),
                    ip_address,
                },
            )
            .await
        {
            Ok(Some(_)) => upgraded,
            // 期间已被改密/清除：保持原值，由调用方复核后作废本次登录。
            Ok(None) | Err(_) => credential.password_hash.clone(),
        }
    }

    async fn active_user(&self, username: &str) -> Result<UserSnapshot, UseCaseError> {
        let normalized = domain::identity::normalize_username(username)
            .map_err(|e| UseCaseError::Invalid(e.to_string()))?;
        let user = self
            .deps
            .users
            .find_by_username(&normalized)
            .await?
            .ok_or_else(|| UseCaseError::NotFound(format!("用户 {normalized}")))?;
        if !user.is_active() {
            return Err(UseCaseError::Forbidden);
        }
        Ok(user)
    }

    /// 未知用户的等价开销哈希：首次使用时生成，进程内复用。
    ///
    /// 与真实存储使用同一套参数（由 hasher 实现保证），因此校验耗时可比。
    async fn dummy_hash(&self) -> Result<&str, UseCaseError> {
        if let Some(hash) = self.dummy_hash.get() {
            return Ok(hash.as_str());
        }
        let generated = self
            .deps
            .hasher
            .hash("timing-equalization-placeholder")
            .await?;
        // 并发首用时多个线程可能各生成一次；任意一个都等价可用。
        let _ = self.dummy_hash.set(generated);
        Ok(self
            .dummy_hash
            .get()
            .expect("刚写入或已被其他线程写入")
            .as_str())
    }
}

/// 持有本次请求的预占；future 被丢弃时也同步归还，不依赖运行时继续调度。
struct ThrottleReservation<'a> {
    throttle: &'a dyn LoginThrottle,
    subjects: &'a [ThrottleSubject],
    reserved: usize,
    settled: usize,
}

impl ThrottleReservation<'_> {
    fn finish(mut self, success: bool) -> Result<(), UseCaseError> {
        for subject in &self.subjects[..self.reserved] {
            if success {
                self.throttle.record_success(subject)?;
            } else {
                self.throttle.record_failure(subject)?;
            }
            self.settled += 1;
        }
        Ok(())
    }
}

impl Drop for ThrottleReservation<'_> {
    fn drop(&mut self) {
        for subject in &self.subjects[self.settled..self.reserved] {
            let _ = self.throttle.release(subject);
        }
    }
}

/// 限流键用的用户名：规范化失败时退化为 trim + 小写并截断，保证键长度有界。
///
/// 规范化失败不影响「凭据错误」判定，只是让它落在一个有界的限流桶里。
fn throttle_username(username: &str) -> String {
    match domain::identity::normalize_username(username) {
        Ok(name) => name,
        Err(_) => username
            .trim()
            .to_ascii_lowercase()
            .chars()
            .take(64)
            .collect(),
    }
}

/// 一次登录尝试涉及的限流主体：账号维度 + （若可用）客户端地址维度。
pub fn throttle_subjects(username: &str, client_key: Option<&str>) -> Vec<ThrottleSubject> {
    let mut subjects = vec![ThrottleSubject::User(username.to_string())];
    if let Some(client) = client_key.filter(|key| !key.is_empty()) {
        subjects.push(ThrottleSubject::Client(client.to_string()));
    }
    subjects
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn throttle_username_is_bounded_for_invalid_input() {
        let long = "A".repeat(500);
        assert_eq!(throttle_username(&long).chars().count(), 64);
        assert_eq!(throttle_username("  Sun "), "sun");
        assert_eq!(throttle_username("空间用户"), "空间用户");
    }

    #[test]
    fn throttle_subjects_skip_empty_client() {
        assert_eq!(
            throttle_subjects("sun", None),
            vec![ThrottleSubject::User("sun".into())]
        );
        assert_eq!(
            throttle_subjects("sun", Some("")),
            vec![ThrottleSubject::User("sun".into())]
        );
        assert_eq!(
            throttle_subjects("sun", Some("203.0.113.7")),
            vec![
                ThrottleSubject::User("sun".into()),
                ThrottleSubject::Client("203.0.113.7".into())
            ]
        );
    }
}
