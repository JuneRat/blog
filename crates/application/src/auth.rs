//! 认证用例：OAuth 登录闭环（OIDC + GitHub）与本站会话。
//!
//! 流程遵循 docs/identity-and-admin.md §4/§5：
//! - 后端 Authorization Code；OIDC 使用 PKCE S256 + nonce，GitHub 平台不支持 PKCE 仅 state。
//! - state/nonce/verifier 存有 TTL 与容量限制的服务端内存尝试存储，原子一次消费。
//! - 网络交换在锁/事务外执行；返回后重新读取提供商配置与账号状态再签发会话。
//! - 相同邮箱不合并用户；未绑定的外部身份拒绝登录（不自动注册）。
//! - 会话为单实例内存存储，重启全部失效；每次敏感操作重新读用户与权限。

use std::sync::Arc;

use uuid::Uuid;

use crate::error::UseCaseError;
use crate::identity::{Actor, ActorChannel, UserInteractor};
use crate::ports::{
    ExternalIdentityClient, OAuthAccountStore, OAuthAttempt, OAuthAttemptStore, OAuthConfigStore,
    ProviderConfig, ProviderKind, SESSION_COOKIE, SecureRandom, SessionRecord, SessionStore,
};

/// OAuth 尝试有效期（登录发起后允许完成回调的窗口）。
pub const ATTEMPT_TTL_SECS: i64 = 600;

/// 认证用例的出站依赖集合。
pub struct AuthDeps {
    pub sessions: Arc<dyn SessionStore>,
    pub attempts: Arc<dyn OAuthAttemptStore>,
    pub configs: Arc<dyn OAuthConfigStore>,
    pub accounts: Arc<dyn OAuthAccountStore>,
    pub identity_client: Arc<dyn ExternalIdentityClient>,
    pub random: Arc<dyn SecureRandom>,
}

pub struct AuthInteractor {
    deps: AuthDeps,
    users: Arc<UserInteractor>,
    clock: Arc<dyn crate::ports::Clock>,
    /// 对外可达基础 URL（构建回调 redirect_uri）。
    base_url: String,
}

/// 登录页可用的提供商摘要：只暴露 id / 展示名 / 类型，
/// 不含 client_id、issuer、secret_ref 或任何配置细节。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProviderSummary {
    pub id: String,
    pub name: String,
    pub kind: &'static str,
}

/// 回调产物：浏览器需写入的会话 cookie 值与回跳路径。
#[derive(Debug)]
pub struct LoginSuccess {
    pub token: String,
    pub next: String,
    pub user_id: Uuid,
}

/// 登录发起产物：提供商授权 URL + 需写入浏览器的短命绑定值。
/// 绑定值即 state（双重提交）：回调时必须在同一浏览器 cookie 中回读，
/// 防止攻击者把自己的授权码/回调 URL 塞给受害者完成登录 CSRF。
#[derive(Debug)]
pub struct LoginStart {
    pub authorize_url: String,
    pub browser_binding: String,
}

impl AuthInteractor {
    pub fn new(
        deps: AuthDeps,
        users: Arc<UserInteractor>,
        clock: Arc<dyn crate::ports::Clock>,
        base_url: String,
    ) -> Self {
        Self {
            deps,
            users,
            clock,
            base_url,
        }
    }

    /// 发起登录：生成 state/PKCE/nonce，保存一次性尝试，返回提供商授权 URL
    /// 与浏览器绑定值（接口层写入短命 cookie，回调核对）。
    pub async fn login_start(
        &self,
        provider_id: &str,
        next: &str,
    ) -> Result<LoginStart, UseCaseError> {
        let config = find_provider_config(self.deps.configs.as_ref(), provider_id).await?;
        let next = sanitize_next(next)?;

        let state = self.deps.random.token_hex()?;
        let redirect_uri = format!(
            "{}/auth/callback/{}",
            self.base_url.trim_end_matches('/'),
            config.id
        );

        let (verifier, challenge, nonce) = match config.kind {
            ProviderKind::Oidc => {
                let verifier = self.deps.random.token_hex()?;
                let challenge = self.deps.random.pkce_s256(&verifier)?;
                let nonce = self.deps.random.token_hex()?;
                (Some(verifier), Some(challenge), Some(nonce))
            }
            // GitHub OAuth 平台不接受 PKCE 参数；仅 state 防 CSRF。
            ProviderKind::GitHub => (None, None, None),
        };

        let url = self
            .deps
            .identity_client
            .authorize_url(
                &config,
                &state,
                challenge.as_deref(),
                nonce.as_deref(),
                &redirect_uri,
            )
            .await?;

        self.deps
            .attempts
            .save(
                state.clone(),
                OAuthAttempt {
                    provider_id: config.id.clone(),
                    verifier,
                    nonce,
                    redirect_uri,
                    next: next.to_string(),
                    created_at: self.clock.now(),
                },
            )
            .await?;
        Ok(LoginStart {
            authorize_url: url,
            browser_binding: state,
        })
    }

    /// 回调：先核对浏览器绑定，再原子消费尝试、交换授权码、核对绑定并签发会话。
    pub async fn login_callback(
        &self,
        provider_id: &str,
        code: &str,
        state: &str,
        browser_binding: Option<&str>,
    ) -> Result<LoginSuccess, UseCaseError> {
        // 浏览器绑定先于消费：不匹配的请求不得烧掉真实尝试（docs §4 第 2 步）。
        if browser_binding.is_none_or(|binding| binding.is_empty() || binding != state) {
            return Err(UseCaseError::Invalid(
                "浏览器绑定缺失或不匹配，请重新发起登录".into(),
            ));
        }
        let attempt = self
            .deps
            .attempts
            .consume(state)
            .await?
            .ok_or_else(|| UseCaseError::Invalid("state 无效或已被使用".into()))?;
        if attempt.provider_id != provider_id {
            return Err(UseCaseError::Invalid("state 与提供商不匹配".into()));
        }
        if self.clock.now() - attempt.created_at > time::Duration::seconds(ATTEMPT_TTL_SECS) {
            return Err(UseCaseError::Invalid("登录尝试已过期，请重新发起".into()));
        }

        let config = find_provider_config(self.deps.configs.as_ref(), provider_id).await?;

        // 网络交换在锁外执行（docs §4 第 3 步）。
        let identity = self
            .deps
            .identity_client
            .exchange(
                &config,
                code,
                attempt.verifier.as_deref(),
                attempt.nonce.as_deref(),
                &attempt.redirect_uri,
            )
            .await?;

        // 返回后重新检查配置（可能在交换期间被禁用/修改）。
        let config_after = find_provider_config(self.deps.configs.as_ref(), provider_id).await?;
        if config_after.client_id != config.client_id {
            return Err(UseCaseError::External(
                "提供商配置在登录过程中发生变化".into(),
            ));
        }

        // 相同邮箱不自动合并：未知绑定一律拒绝，不自动注册。
        let user_id = self
            .deps
            .accounts
            .find_user_by_external_id(&identity.provider_key, &identity.provider_user_id)
            .await?
            .ok_or(UseCaseError::Forbidden)?;

        let (actor, revision) = self
            .users
            .actor_with_revision(user_id, ActorChannel::Session)
            .await
            .map_err(|_| UseCaseError::Forbidden)?;

        let token = self.deps.sessions.create(user_id, revision).await?;
        Ok(LoginSuccess {
            token,
            next: sanitize_next(&attempt.next)?.to_string(),
            user_id: actor.user_id.0,
        })
    }

    /// 从会话令牌解析 Actor：校验会话、重新读取当前用户与权限，并核对身份修订号。
    ///
    /// 版本比对让**跨进程**的改密/认证撤销/软删除同样立刻生效——CLI 在另一个进程
    /// 改了 `users.auth_version`，这里读到的版本就不再等于会话签发时的值。
    pub async fn actor_from_session(&self, token: &str) -> Result<Actor, UseCaseError> {
        let record = self.validate_session(token).await?;
        self.actor_from_validated_record(&record).await
    }

    /// 一次校验同时得到会话记录与 Actor。
    ///
    /// 管理提取器既要用记录的 `csrf_token` 校验 CSRF，又要用 Actor 授权；若分别
    /// 调用 [`Self::session_record`] 与 [`Self::actor_from_session`]，同一请求会校验
    /// 两次会话。持久存储下 `validate` 会刷新 `last_seen_at`，也就是同一请求写两次库，
    /// 因此提供这个合并入口。校验顺序与两次调用一致：先令牌有效，再版本比对。
    pub async fn session_actor(&self, token: &str) -> Result<(SessionRecord, Actor), UseCaseError> {
        let record = self.validate_session(token).await?;
        let actor = self.actor_from_validated_record(&record).await?;
        Ok((record, actor))
    }

    /// 会话元数据（含 CSRF token）；用于受保护写请求的 CSRF 校验。
    pub async fn session_record(&self, token: &str) -> Result<SessionRecord, UseCaseError> {
        self.validate_session(token).await
    }

    /// 校验令牌并返回记录；未知/过期映射为未登录。
    async fn validate_session(&self, token: &str) -> Result<SessionRecord, UseCaseError> {
        self.deps
            .sessions
            .validate(token)
            .await?
            .ok_or(UseCaseError::Unauthenticated)
    }

    /// 用**已校验**的会话记录解析 Actor：只做版本比对与权限读取，不重复校验令牌。
    async fn actor_from_validated_record(
        &self,
        record: &SessionRecord,
    ) -> Result<Actor, UseCaseError> {
        let (actor, revision) = self
            .users
            .actor_with_revision(record.user_id, ActorChannel::Session)
            .await
            .map_err(|error| match error {
                UseCaseError::NotFound(_) | UseCaseError::Forbidden => {
                    UseCaseError::Unauthenticated
                }
                // A failed lookup does not prove that the session is invalid.
                other => other,
            })?;
        if revision != record.auth_version {
            // 账号身份材料已变化（改密/认证撤销/软删除）：旧会话立即失效。
            return Err(UseCaseError::Unauthenticated);
        }
        Ok(actor)
    }

    /// 退出：撤销会话（受保护写操作，接口层校验 CSRF 后调用）。
    pub async fn logout(&self, token: &str) -> Result<(), UseCaseError> {
        self.deps.sessions.revoke(token).await
    }

    /// 明确撤销全部登录：递增认证版本，并清理该用户的会话。
    pub async fn revoke_sessions_of_user(&self, user_id: Uuid) -> Result<(), UseCaseError> {
        self.users.revoke_authentication(user_id).await?;
        self.deps.sessions.revoke_all_for_user(user_id).await
    }

    /// 公开登录页用的提供商摘要（只读、匿名可访问）。
    pub async fn list_provider_summaries(&self) -> Result<Vec<ProviderSummary>, UseCaseError> {
        Ok(self
            .deps
            .configs
            .list()
            .await?
            .into_iter()
            .map(|config| ProviderSummary {
                name: config.name.clone().unwrap_or_else(|| config.id.clone()),
                kind: provider_kind_label(config.kind),
                id: config.id,
            })
            .collect())
    }
}

/// 受控 OAuth 维护用例：仅依赖配置、绑定、用户和会话，不需要登录网络客户端或站点地址。
pub struct OAuthManagementInteractor {
    configs: Arc<dyn OAuthConfigStore>,
    accounts: Arc<dyn OAuthAccountStore>,
    sessions: Arc<dyn SessionStore>,
    users: Arc<UserInteractor>,
}

impl OAuthManagementInteractor {
    pub fn new(
        configs: Arc<dyn OAuthConfigStore>,
        accounts: Arc<dyn OAuthAccountStore>,
        sessions: Arc<dyn SessionStore>,
        users: Arc<UserInteractor>,
    ) -> Self {
        Self {
            configs,
            accounts,
            sessions,
            users,
        }
    }

    /// OAuth 配置管理（`oauth.manage`；受保护配置不受普通 settings 权限覆盖）。
    pub async fn list_providers(&self) -> Result<Vec<ProviderConfig>, UseCaseError> {
        self.configs.list().await
    }

    pub async fn save_providers(
        &self,
        actor: &Actor,
        providers: &[ProviderConfig],
    ) -> Result<(), UseCaseError> {
        actor.ensure_write_channel()?;
        if !actor.has_permission("oauth.manage") {
            return Err(UseCaseError::Forbidden);
        }
        for config in providers {
            validate_provider_config(config)?;
        }
        self.configs.save(providers, actor.audit_actor_id()).await
    }

    /// 显式绑定外部身份（需 `oauth.manage`；操作者需核对稳定外部 ID）。
    pub async fn bind_external_id(
        &self,
        actor: &Actor,
        username: &str,
        provider_id: &str,
        external_id: &str,
        email: Option<String>,
    ) -> Result<(), UseCaseError> {
        actor.ensure_write_channel()?;
        if !actor.has_permission("oauth.manage") {
            return Err(UseCaseError::Forbidden);
        }
        let config = find_provider_config(self.configs.as_ref(), provider_id).await?;
        let provider_key = provider_identity_key(&config);
        let target = self.users.actor_for_username(username).await?;
        self.accounts
            .bind(
                target.user_id.0,
                &provider_key,
                external_id,
                email,
                actor.audit_actor_id(),
            )
            .await
    }

    /// 解绑外部身份：需 `oauth.manage`；解绑后撤销该用户的全部会话（docs §5）。
    pub async fn unbind_external_id(
        &self,
        actor: &Actor,
        username: &str,
        provider_id: &str,
        external_id: &str,
    ) -> Result<(), UseCaseError> {
        actor.ensure_write_channel()?;
        if !actor.has_permission("oauth.manage") {
            return Err(UseCaseError::Forbidden);
        }
        let config = find_provider_config(self.configs.as_ref(), provider_id).await?;
        let provider_key = provider_identity_key(&config);
        let target = self.users.actor_for_username(username).await?;
        self.accounts
            .unbind(
                target.user_id.0,
                &provider_key,
                external_id,
                actor.audit_actor_id(),
            )
            .await?;
        // 登录方式发生变化：旧 Cookie 立即失效。
        self.sessions.revoke_all_for_user(target.user_id.0).await
    }

    pub async fn bindings_of(&self, username: &str) -> Result<Vec<String>, UseCaseError> {
        let actor = self.users.actor_for_username(username).await?;
        let bindings = self.accounts.list_for_user(actor.user_id.0).await?;
        Ok(bindings
            .into_iter()
            .map(|b| format!("{}#{}", b.provider_key, b.provider_user_id))
            .collect())
    }
}

async fn find_provider_config(
    configs: &dyn OAuthConfigStore,
    provider_id: &str,
) -> Result<ProviderConfig, UseCaseError> {
    configs
        .list()
        .await?
        .into_iter()
        .find(|config| config.id == provider_id)
        .ok_or_else(|| UseCaseError::NotFound(format!("OAuth 提供商 {provider_id}")))
}

/// 身份命名空间键：OIDC 用精确 issuer，GitHub 用固定平台实例标识。
pub fn provider_identity_key(config: &ProviderConfig) -> String {
    match config.kind {
        ProviderKind::Oidc => config.issuer.clone().expect("OIDC 配置已校验 issuer 必填"),
        ProviderKind::GitHub => "github".to_string(),
    }
}

/// 对外展示 / API 使用的提供商类型标签。
fn provider_kind_label(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::Oidc => "oidc",
        ProviderKind::GitHub => "github",
    }
}

fn validate_provider_config(config: &ProviderConfig) -> Result<(), UseCaseError> {
    if config.id.is_empty() || config.id.len() > 64 {
        return Err(UseCaseError::Invalid("提供商 id 不合法".into()));
    }
    if config.id.contains('/') || config.id.contains('.') {
        return Err(UseCaseError::Invalid("提供商 id 不能包含 / 或 .".into()));
    }
    if let Some(name) = config.name.as_deref() {
        let name = name.trim();
        if name.is_empty() {
            return Err(UseCaseError::Invalid("提供商展示名不能为空白".into()));
        }
        if name.chars().count() > 100 {
            return Err(UseCaseError::Invalid("提供商展示名过长".into()));
        }
    }
    if config.secret_ref.is_empty() {
        return Err(UseCaseError::Invalid("secret_ref 不能为空".into()));
    }
    if matches!(config.kind, ProviderKind::Oidc) {
        let issuer = config.issuer.as_deref().unwrap_or("");
        if !issuer.starts_with("https://") || issuer.len() < 12 {
            return Err(UseCaseError::Invalid(
                "OIDC issuer 必须是精确 https URL".into(),
            ));
        }
    }
    Ok(())
}

/// 回跳路径白名单：仅本站相对路径，禁止协议相对与外站。
/// 同时按字符白名单校验，拒绝控制字符（CR/LF 会破坏 Location 头）与空白。
pub fn sanitize_next(next: &str) -> Result<&str, UseCaseError> {
    let invalid = || UseCaseError::Invalid("回跳路径必须是本站相对路径".into());
    if !next.starts_with('/') || next.starts_with("//") || next.contains('\\') {
        return Err(invalid());
    }
    if !next.chars().all(is_allowed_next_char) {
        return Err(invalid());
    }
    Ok(next)
}

/// 允许出现在 `next` 中的字符：URL 路径/查询安全字符 + 非 ASCII 可见字符。
fn is_allowed_next_char(c: char) -> bool {
    if c.is_ascii() {
        c.is_ascii_alphanumeric()
            || matches!(
                c,
                '-' | '_'
                    | '.'
                    | '~'
                    | '/'
                    | '?'
                    | '#'
                    | '['
                    | ']'
                    | '@'
                    | '!'
                    | '$'
                    | '&'
                    | '\''
                    | '('
                    | ')'
                    | '*'
                    | '+'
                    | ','
                    | ';'
                    | '='
                    | ':'
                    | '%'
            )
    } else {
        !c.is_control() && !c.is_whitespace()
    }
}

/// cookie 名供接口层使用。
pub const SESSION_COOKIE_NAME: &str = SESSION_COOKIE;
