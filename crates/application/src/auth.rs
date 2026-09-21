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
        let config = self.find_config(provider_id).await?;
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

        let config = self.find_config(provider_id).await?;

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
        let config_after = self.find_config(provider_id).await?;
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

        let actor = self
            .users
            .actor_for_user_id_with_channel(user_id, ActorChannel::Session)
            .await
            .map_err(|_| UseCaseError::Forbidden)?;

        let token = self.deps.sessions.create(user_id).await?;
        Ok(LoginSuccess {
            token,
            next: sanitize_next(&attempt.next)?.to_string(),
            user_id: actor.user_id.0,
        })
    }

    /// 从会话令牌解析 Actor：校验会话并重新读取当前用户与权限（旧 cookie 不可绕过撤权）。
    pub async fn actor_from_session(&self, token: &str) -> Result<Actor, UseCaseError> {
        let record: SessionRecord = self
            .deps
            .sessions
            .validate(token)
            .await?
            .ok_or(UseCaseError::Unauthenticated)?;
        self.users
            .actor_for_user_id_with_channel(record.user_id, ActorChannel::Session)
            .await
            .map_err(|_| UseCaseError::Unauthenticated)
    }

    /// 会话元数据（含 CSRF token）；用于受保护写请求的 CSRF 校验。
    pub async fn session_record(&self, token: &str) -> Result<SessionRecord, UseCaseError> {
        self.deps
            .sessions
            .validate(token)
            .await?
            .ok_or(UseCaseError::Unauthenticated)
    }

    /// 退出：撤销会话（受保护写操作，接口层校验 CSRF 后调用）。
    pub async fn logout(&self, token: &str) -> Result<(), UseCaseError> {
        self.deps.sessions.revoke(token).await
    }

    /// 账号事件入口：软删除/撤权时撤销全部会话（恢复账号不恢复旧会话）。
    pub async fn revoke_sessions_of_user(&self, user_id: Uuid) -> Result<(), UseCaseError> {
        self.deps.sessions.revoke_all_for_user(user_id).await
    }

    /// OAuth 配置管理（`oauth.manage`；受保护配置不受普通 settings 权限覆盖）。
    pub async fn list_providers(&self) -> Result<Vec<ProviderConfig>, UseCaseError> {
        self.deps.configs.list().await
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
        self.deps.configs.save(providers).await
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
        let config = self.find_config(provider_id).await?;
        let provider_key = provider_identity_key(&config);
        let target = self.users.actor_for_username(username).await?;
        self.deps
            .accounts
            .bind(target.user_id.0, &provider_key, external_id, email)
            .await
    }

    /// 解绑外部身份：需 `oauth.manage`；解绑后清除该用户的内存会话（docs §5）。
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
        let config = self.find_config(provider_id).await?;
        let provider_key = provider_identity_key(&config);
        let target = self.users.actor_for_username(username).await?;
        self.deps
            .accounts
            .unbind(target.user_id.0, &provider_key, external_id)
            .await?;
        // 登录方式发生变化：旧 Cookie 立即失效。
        self.revoke_sessions_of_user(target.user_id.0).await
    }

    pub async fn bindings_of(&self, username: &str) -> Result<Vec<String>, UseCaseError> {
        let actor = self.users.actor_for_username(username).await?;
        let bindings = self.deps.accounts.list_for_user(actor.user_id.0).await?;
        Ok(bindings
            .into_iter()
            .map(|b| format!("{}#{}", b.provider_key, b.provider_user_id))
            .collect())
    }

    async fn find_config(&self, provider_id: &str) -> Result<ProviderConfig, UseCaseError> {
        self.deps
            .configs
            .list()
            .await?
            .into_iter()
            .find(|c| c.id == provider_id)
            .ok_or_else(|| UseCaseError::NotFound(format!("OAuth 提供商 {provider_id}")))
    }
}

/// 身份命名空间键：OIDC 用精确 issuer，GitHub 用固定平台实例标识。
pub fn provider_identity_key(config: &ProviderConfig) -> String {
    match config.kind {
        ProviderKind::Oidc => config.issuer.clone().expect("OIDC 配置已校验 issuer 必填"),
        ProviderKind::GitHub => "github".to_string(),
    }
}

fn validate_provider_config(config: &ProviderConfig) -> Result<(), UseCaseError> {
    if config.id.is_empty() || config.id.len() > 64 {
        return Err(UseCaseError::Invalid("提供商 id 不合法".into()));
    }
    if config.id.contains('/') || config.id.contains('.') {
        return Err(UseCaseError::Invalid("提供商 id 不能包含 / 或 .".into()));
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
