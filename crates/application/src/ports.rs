//! 出站端口：由使用方（应用层）定义，由基础设施实现。
//!
//! M1 写用例均为单条件语句原子更新，事务即语句本身；
//! 多写用例出现时再引入工作单元抽象（见 docs/architecture.md §5）。

use async_trait::async_trait;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::UseCaseError;
use crate::identity::{BuiltinRoleDef, PermissionDescriptor};
use domain::content::post::PostSnapshot;
use domain::identity::UserSnapshot;

// ---------------------------------------------------------------------------
// 写侧端口
// ---------------------------------------------------------------------------

/// 条件保存的三态结果：
/// 区分「版本过期可重试」与「记录已消失/被删（重试无意义）」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveOutcome {
    /// 写入成功，携带数据库返回的递增后版本。
    Saved { new_version: i64 },
    /// expected_version 与当前记录不匹配；调用方应报并发冲突。
    StaleConflict,
    /// 记录不存在或已软删除。
    Gone,
}

#[async_trait]
pub trait PostRepository: Send + Sync {
    async fn find_by_slug(&self, slug: &str) -> Result<Option<PostSnapshot>, UseCaseError>;
    async fn find_by_id(&self, id: Uuid) -> Result<Option<PostSnapshot>, UseCaseError>;
    async fn list_by_author(&self, author_id: Uuid) -> Result<Vec<PostSnapshot>, UseCaseError>;
    async fn insert(&self, snapshot: &PostSnapshot) -> Result<(), UseCaseError>;

    /// 条件保存：`expected_version` 匹配当前记录时写入并 version+1。
    async fn save(
        &self,
        snapshot: &PostSnapshot,
        expected_version: i64,
        now: OffsetDateTime,
    ) -> Result<SaveOutcome, UseCaseError>;
}

#[async_trait]
pub trait UserRepository: Send + Sync {
    async fn insert(&self, snapshot: &UserSnapshot) -> Result<(), UseCaseError>;
    async fn find_by_id(&self, id: Uuid) -> Result<Option<UserSnapshot>, UseCaseError>;
    async fn find_by_username(&self, username: &str) -> Result<Option<UserSnapshot>, UseCaseError>;
}

/// 视图用角色条目。
#[derive(Debug, Clone, serde::Serialize)]
pub struct RoleDto {
    pub slug: String,
    pub name: String,
    pub description: Option<String>,
    pub builtin: bool,
    pub permission_count: i64,
}

/// 角色分配/授权存储端口。
/// 权限目录只接受应用可信注册表；未知 key 拒绝授权。
#[async_trait]
pub trait RbacStore: Send + Sync {
    /// 幂等同步权限目录（按 key upsert；不删除已有 key）。
    async fn sync_permission_registry(
        &self,
        entries: &[PermissionDescriptor],
    ) -> Result<(), UseCaseError>;

    /// 幂等同步内置角色定义与授权集合（内置 slug 不可改名）。
    async fn sync_builtin_roles(&self, defs: &[BuiltinRoleDef]) -> Result<(), UseCaseError>;

    /// 用户有效权限（全部角色并集；软删除用户为空集）。
    async fn permissions_of_user(
        &self,
        user_id: Uuid,
    ) -> Result<domain::identity::PermissionSet, UseCaseError>;

    /// 角色的授权集合（委派上限判定用；未知 slug 返回 NotFound）。
    async fn permissions_of_role(
        &self,
        role_slug: &str,
    ) -> Result<domain::identity::PermissionSet, UseCaseError>;

    async fn assign_role(&self, user_id: Uuid, role_slug: &str) -> Result<(), UseCaseError>;

    /// 移除角色分配；内置保护（如最后一个有效 Owner）由实现拒绝。
    async fn remove_role(&self, user_id: Uuid, role_slug: &str) -> Result<(), UseCaseError>;

    async fn list_roles(&self) -> Result<Vec<RoleDto>, UseCaseError>;

    async fn roles_of_user(&self, user_id: Uuid) -> Result<Vec<String>, UseCaseError>;
}

// ---------------------------------------------------------------------------
// 读侧端口（轻量 CQRS：面向页面的公开只读查询）
// ---------------------------------------------------------------------------

/// 公开列表条目。
#[derive(Debug, Clone, serde::Serialize)]
pub struct PublicPostSummary {
    pub title: String,
    pub slug: String,
    pub excerpt: Option<String>,
    pub published_at: Option<OffsetDateTime>,
    pub author_display: String,
}

/// 公开详情（正文为 Markdown 源文，渲染交给出站端口）。
#[derive(Debug, Clone)]
pub struct PublicPostDetail {
    pub title: String,
    pub slug: String,
    pub excerpt: Option<String>,
    pub published_at: Option<OffsetDateTime>,
    pub updated_at: OffsetDateTime,
    pub author_display: String,
    pub author_username: String,
    pub content: String,
}

#[async_trait]
pub trait PublishedPostQuery: Send + Sync {
    /// 只返回 status=published AND visibility=public AND deleted_at IS NULL 的文章。
    async fn list_public(
        &self,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<PublicPostSummary>, UseCaseError>;
    async fn find_public_by_slug(
        &self,
        slug: &str,
    ) -> Result<Option<PublicPostDetail>, UseCaseError>;
}

// ---------------------------------------------------------------------------
// 基础能力端口
// ---------------------------------------------------------------------------

pub trait Clock: Send + Sync {
    fn now(&self) -> OffsetDateTime;
}

/// readiness 探针：实现方执行最小健康动作（如 SELECT 1）。
/// None/未装配时调用方按“无依赖可检”处理。
#[async_trait]
pub trait HealthCheck: Send + Sync {
    async fn check(&self) -> bool;
}

/// Markdown → 清洗后 HTML。清洗规则由实现方（基础设施）负责。
pub trait ContentRenderer: Send + Sync {
    fn render_markdown(&self, source: &str) -> String;
}

// ---------------------------------------------------------------------------
// 认证端口：单实例内存会话、OAuth 尝试与外部身份
// ---------------------------------------------------------------------------

/// 会话 cookie 名（前后端共享契约）。
pub const SESSION_COOKIE: &str = "blog_session";

/// OAuth 浏览器绑定 cookie 基名：`login_start` 下发、`login_callback` 核对。
/// 生产（Secure）部署使用 `__Host-` 前缀强制 host-only + Path=/（见接口层）。
pub const OAUTH_STATE_COOKIE: &str = "blog_oauth_state";

/// 服务端会话记录（浏览器只持有不透明令牌）。
#[derive(Debug, Clone)]
pub struct SessionRecord {
    pub user_id: Uuid,
    pub csrf_token: String,
    pub created_at: OffsetDateTime,
    pub last_seen_at: OffsetDateTime,
}

/// 单实例内存会话存储：有 TTL 与容量上限，重启全部失效。
#[async_trait]
pub trait SessionStore: Send + Sync {
    /// 创建会话，返回不透明令牌（明文只出现一次；服务端保存验证摘要）。
    async fn create(&self, user_id: Uuid) -> Result<String, UseCaseError>;
    /// 校验令牌并刷新 last_seen；过期/未知/已撤销返回 None。
    async fn validate(&self, token: &str) -> Result<Option<SessionRecord>, UseCaseError>;
    async fn revoke(&self, token: &str) -> Result<(), UseCaseError>;
    /// 撤销某用户全部会话（账号软删除/撤权入口调用）。
    async fn revoke_all_for_user(&self, user_id: Uuid) -> Result<(), UseCaseError>;
}

/// 一次性 OAuth 尝试（state → PKCE verifier / nonce / 回跳）。
#[derive(Debug, Clone)]
pub struct OAuthAttempt {
    pub provider_id: String,
    pub verifier: Option<String>,
    pub nonce: Option<String>,
    pub redirect_uri: String,
    pub next: String,
    pub created_at: OffsetDateTime,
}

/// 有 TTL 与容量限制的尝试存储；state 原子一次消费。
#[async_trait]
pub trait OAuthAttemptStore: Send + Sync {
    async fn save(&self, state: String, attempt: OAuthAttempt) -> Result<(), UseCaseError>;
    /// 取出并删除（一次性）；不存在或过期返回 None。
    async fn consume(&self, state: &str) -> Result<Option<OAuthAttempt>, UseCaseError>;
}

/// OAuth 提供商类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ProviderKind {
    /// 通用 OIDC：绑定精确 issuer；PKCE S256 + nonce。
    Oidc,
    /// GitHub 固定平台实例：稳定数值用户 ID；平台不支持 PKCE，仅 state。
    GitHub,
}

/// 提供商非敏感配置（存 settings 的 oauth 分组；秘密经 secret_ref 由部署环境提供）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProviderConfig {
    /// URL 与回调路径使用的稳定 id。
    pub id: String,
    pub kind: ProviderKind,
    /// OIDC 必填：精确 issuer（不接受回调参数指定）。
    pub issuer: Option<String>,
    pub client_id: String,
    /// 指向保存 client secret 的环境变量名。
    pub secret_ref: String,
    #[serde(default)]
    pub scopes: Vec<String>,
}

#[async_trait]
pub trait OAuthConfigStore: Send + Sync {
    async fn list(&self) -> Result<Vec<ProviderConfig>, UseCaseError>;
    async fn save(&self, providers: &[ProviderConfig]) -> Result<(), UseCaseError>;
}

/// 外部身份（身份命名空间键 + 稳定用户 ID + 资料快照邮箱）。
#[derive(Debug, Clone)]
pub struct ExternalIdentity {
    /// OIDC 为精确 issuer；GitHub 为固定 "github"。
    pub provider_key: String,
    pub provider_user_id: String,
    pub email: Option<String>,
}

#[async_trait]
pub trait OAuthAccountStore: Send + Sync {
    async fn find_user_by_external_id(
        &self,
        provider_key: &str,
        provider_user_id: &str,
    ) -> Result<Option<Uuid>, UseCaseError>;
    /// 绑定外部身份；同一 (provider_key, provider_user_id) 只能属于一个用户。
    async fn bind(
        &self,
        user_id: Uuid,
        provider_key: &str,
        provider_user_id: &str,
        email: Option<String>,
    ) -> Result<(), UseCaseError>;
    /// 解绑；当它是该用户最后一种有效登录方式时返回 Err 拒绝。
    async fn unbind(
        &self,
        user_id: Uuid,
        provider_key: &str,
        provider_user_id: &str,
    ) -> Result<(), UseCaseError>;
    async fn list_for_user(&self, user_id: Uuid) -> Result<Vec<ExternalIdentity>, UseCaseError>;
}

/// 出站身份客户端（OIDC/GitHub 适配器）。
/// authorize_url 可能需要发现文档（异步）；exchange 在事务/锁外执行网络请求。
#[async_trait]
pub trait ExternalIdentityClient: Send + Sync {
    async fn authorize_url(
        &self,
        config: &ProviderConfig,
        state: &str,
        challenge: Option<&str>,
        nonce: Option<&str>,
        redirect_uri: &str,
    ) -> Result<String, UseCaseError>;

    /// 用授权码换取外部身份。校验 issuer/audience/有效期/nonce（OIDC）。
    #[allow(clippy::too_many_arguments)]
    async fn exchange(
        &self,
        config: &ProviderConfig,
        code: &str,
        verifier: Option<&str>,
        nonce: Option<&str>,
        redirect_uri: &str,
    ) -> Result<ExternalIdentity, UseCaseError>;
}

/// 安全随机与 PKCE 派生（实现方使用系统 CSPRNG 与 SHA-256）。
pub trait SecureRandom: Send + Sync {
    /// 32 字节随机数的 hex（state / nonce / 令牌 / verifier 共用形态）。
    fn token_hex(&self) -> Result<String, UseCaseError>;
    /// PKCE S256 challenge：base64url(SHA-256(verifier))，无填充。
    fn pkce_s256(&self, verifier: &str) -> Result<String, UseCaseError>;
}

/// secret_ref → 秘密值（环境变量等部署来源）。
pub trait SecretSource: Send + Sync {
    fn secret_for(&self, secret_ref: &str) -> Result<String, UseCaseError>;
}
