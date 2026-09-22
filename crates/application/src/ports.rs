//! 出站端口：由使用方（应用层）定义，由基础设施实现。
//!
//! M1 写用例均为单条件语句原子更新，事务即语句本身；
//! 多写用例出现时再引入工作单元抽象（见 docs/architecture.md §5）。

use async_trait::async_trait;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::UseCaseError;
use crate::identity::{BuiltinRoleDef, PermissionDescriptor};
use domain::content::page::PageSnapshot;
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

/// 登录用密码凭据：只含校验所需最小信息，不携带软删除等实体状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasswordCredential {
    pub user_id: Uuid,
    /// PHC 格式的 Argon2id 字符串（算法与参数自描述）。
    pub password_hash: String,
    /// 读取时的 `users.version`：会话签发时绑定它，避免并发改密后用旧口令建会话。
    pub version: i64,
}

/// 账号管理列表行：用户基本字段 + 是否仍有登录方式。
///
/// 「是否可登录」是最后 Owner 保护判定的输入（docs §3），界面据此在移除 Owner
/// 角色前给出提示；因此它必须与 `PostgresRbacStore::active_owner_count` 用同一套
/// 定义——未软删除，且至少一条 oauth_accounts 或已启用本地密码。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminUserRow {
    pub id: Uuid,
    pub username: String,
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub deleted: bool,
    /// `users.password_hash IS NOT NULL`。
    pub password_enabled: bool,
    /// oauth_accounts 条数。
    pub external_identities: i64,
}

impl AdminUserRow {
    /// 未软删除且至少一种登录方式：与 RBAC 存储的最后 Owner 判定同义。
    pub fn can_login(&self) -> bool {
        !self.deleted && (self.password_enabled || self.external_identities > 0)
    }
}

#[async_trait]
pub trait UserRepository: Send + Sync {
    async fn insert(&self, snapshot: &UserSnapshot) -> Result<(), UseCaseError>;
    async fn find_by_id(&self, id: Uuid) -> Result<Option<UserSnapshot>, UseCaseError>;
    async fn find_by_username(&self, username: &str) -> Result<Option<UserSnapshot>, UseCaseError>;

    /// 管理列表：按用户名排序的分页读取（含软删除账号，供界面标注）。
    ///
    /// 调用方负责给出已收敛的 `limit`/`offset`；实现方不再做范围裁剪。
    async fn list_admin(&self, limit: i64, offset: i64) -> Result<Vec<AdminUserRow>, UseCaseError>;

    // --- 本地密码凭据 ---
    //
    // 口令材料单独走这几个方法，不进入 `UserSnapshot`，避免哈希随实体到处传播。

    /// 写入/覆盖密码哈希（调用方已完成策略校验与哈希），并递增 `users.version`。
    ///
    /// **无条件覆盖**，只用于受控重置/设置（`user.manage`）：那是明确要「以本次为准」。
    /// 任何可能被并发写入抢先的场景都必须走 [`Self::compare_and_set_password_hash`]。
    async fn set_password_hash(&self, user_id: Uuid, phc_hash: &str) -> Result<(), UseCaseError>;

    /// 条件写入（compare-and-swap）：仅当当前值等于 `expected` 时替换，并递增版本。
    ///
    /// `expected = None` 表示「当前必须为空」（OAuth 用户设置初始密码）。
    /// 写入成功返回本次更新原子产生的版本，未命中返回 None。
    /// 会话必须绑定该版本，不能重新读取并借用后续凭据变更的版本。
    /// 用途是让并发的凭据写入不会互相覆盖：
    /// 登录时的透明升级、自助改密、设置初始密码都走这里——否则一次并发的
    /// 自助改密就能把管理员刚下发的强制重置口令覆盖掉。
    async fn compare_and_set_password_hash(
        &self,
        user_id: Uuid,
        expected: Option<&str>,
        new_hash: &str,
    ) -> Result<Option<i64>, UseCaseError>;

    /// 清除密码哈希（禁用密码登录），并递增 `users.version`。
    ///
    /// 不做保护：只应在「确定还有其他登录方式」时调用。带保护请用
    /// [`Self::clear_password_hash_guarded`]。
    async fn clear_password_hash(&self, user_id: Uuid) -> Result<(), UseCaseError>;

    /// 在身份排他锁内清除密码，并原子校验该用户仍有其他登录方式。
    ///
    /// 「检查是否还有别的登录方式」与「清除密码」必须与解绑外部身份用**同一把锁、
    /// 同一事务**：分开做会让两条路径各自看到「对方还在」而同时通过，最终把账号的
    /// 登录方式清空（write skew）。
    async fn clear_password_hash_guarded(
        &self,
        user_id: Uuid,
    ) -> Result<ClearPasswordOutcome, UseCaseError>;

    /// 未软删除用户的密码凭据；未设置密码或已软删除返回 None。
    /// 软删除用户在查询层就被排除，登录失败路径因此无法区分「不存在」与「已停用」。
    async fn find_password_credential(
        &self,
        username: &str,
    ) -> Result<Option<PasswordCredential>, UseCaseError>;

    /// 按 id 读取当前哈希（供自助改密重新认证；未设置返回 None）。
    async fn password_hash_of(&self, user_id: Uuid) -> Result<Option<String>, UseCaseError>;
}

/// [`UserRepository::clear_password_hash_guarded`] 的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClearPasswordOutcome {
    /// 已清除（该用户仍有其他登录方式）。
    Cleared,
    /// 该用户本来就没有启用密码登录。
    NoPassword,
    /// 密码是最后一种登录方式，拒绝清除。
    LastLoginMethod,
}

// ---------------------------------------------------------------------------
// Page（站点级内容，无作者归属）
// ---------------------------------------------------------------------------

/// Page 写侧端口。Page 无软删除：不存在「已消失但仍是版本冲突」之外的第三态，
/// 但沿用同一 `SaveOutcome` 以便与 Post 的并发语义保持一致。
#[async_trait]
pub trait PageRepository: Send + Sync {
    async fn find_by_slug(&self, slug: &str) -> Result<Option<PageSnapshot>, UseCaseError>;
    async fn find_by_id(&self, id: Uuid) -> Result<Option<PageSnapshot>, UseCaseError>;
    /// 站点级列表：无作者过滤，按更新时间倒序。
    async fn list(&self) -> Result<Vec<PageSnapshot>, UseCaseError>;
    async fn insert(&self, snapshot: &PageSnapshot) -> Result<(), UseCaseError>;

    /// 条件保存：`expected_version` 匹配当前记录时写入并 version+1。
    async fn save(
        &self,
        snapshot: &PageSnapshot,
        expected_version: i64,
        now: OffsetDateTime,
    ) -> Result<SaveOutcome, UseCaseError>;
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

    /// 批量读取多个用户的角色 slug（`(user_id, slug)` 对，按用户与 slug 排序）。
    ///
    /// 管理列表一次读整页账号的角色；逐个 `roles_of_user` 会退化成 N+1 查询。
    async fn roles_of_users(&self, user_ids: &[Uuid]) -> Result<Vec<(Uuid, String)>, UseCaseError>;

    /// 全站「可登录」Owner 数：未软删除、持有 owner 角色、且仍有登录方式。
    ///
    /// 这是**全局**计数，不受管理列表分页影响。列表接口用它判断某个账号是不是
    /// 最后一个可登录 Owner；若前端按当前页推断，另一个 Owner 落在后续页时就会
    /// 被误判并错误禁用移除。与 `remove_role` 的最后 Owner 保护使用同一谓词。
    async fn loginable_owner_count(&self) -> Result<i64, UseCaseError>;
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

/// 公开页面详情（Page 无作者、无软删除）。
#[derive(Debug, Clone)]
pub struct PublicPageDetail {
    pub title: String,
    pub slug: String,
    pub published_at: Option<OffsetDateTime>,
    pub updated_at: OffsetDateTime,
    pub content: String,
}

#[async_trait]
pub trait PublishedPageQuery: Send + Sync {
    /// 只返回 status=published AND visibility=public 的页面。
    async fn find_public_by_slug(
        &self,
        slug: &str,
    ) -> Result<Option<PublicPageDetail>, UseCaseError>;
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
    /// 签发时账号的 `users.version`（身份修订号）。
    ///
    /// 校验会话时与当前版本比对：改密、改角色、软删除都会递增该版本，
    /// 因此**跨进程**动作（例如运维在另一个进程跑 `blog user passwd`）也能
    /// 让旧会话立即失效，而不依赖只在同一进程有效的「内存撤销」。
    pub user_version: i64,
}

/// 单实例内存会话存储：有 TTL 与容量上限，重启全部失效。
#[async_trait]
pub trait SessionStore: Send + Sync {
    /// 创建会话，返回不透明令牌（明文只出现一次；服务端保存验证摘要）。
    /// `user_version` 为签发时账号的 `users.version`，校验时用于比对。
    async fn create(&self, user_id: Uuid, user_version: i64) -> Result<String, UseCaseError>;
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
    /// 登录页展示名（可选；缺省回退到 id）。不参与身份命名空间。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
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

// ---------------------------------------------------------------------------
// 本地密码：哈希与登录限流
// ---------------------------------------------------------------------------

/// 口令哈希端口。实现方负责选参数、随机盐与并发上限。
///
/// 必须是内存硬 KDF（当前为 Argon2id）：验证在常量时间内完成，
/// 且单次成本足以让离线爆破昂贵。异步是因为 KDF 是长时间 CPU 计算，
/// 实现方应在阻塞线程池执行，避免占住异步执行器。
#[async_trait]
pub trait PasswordHasher: Send + Sync {
    /// 生成 PHC 字符串（自描述算法/参数/盐）；同一明文每次结果不同。
    async fn hash(&self, password: &str) -> Result<String, UseCaseError>;

    /// 常量时间校验。存储值格式非法返回 Err（存储损坏，按内部错误处理），
    /// 密码不匹配返回 Ok(false)。
    async fn verify(&self, password: &str, phc_hash: &str) -> Result<bool, UseCaseError>;

    /// 存储参数是否弱于当前策略（成功登录后据此升级重哈希）；无法解析时返回 true。
    fn needs_rehash(&self, phc_hash: &str) -> bool;
}

/// 限流主体：用户名与客户端地址分开计数，两者阈值不同。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ThrottleSubject {
    /// 规范化后的用户名（截断后），用于账号维度的锁定。
    User(String),
    /// 与服务端直接建立 TCP 连接的客户端地址。
    ///
    /// 只使用 socket 对端地址，**不信任 `X-Forwarded-For` 等可伪造头**；
    /// 反向代理后的真实客户端地址需要部署侧显式配置可信转发，另行设计。
    Client(String),
}

/// 限流判定结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThrottleDecision {
    pub allowed: bool,
    /// 被拒绝时的建议等待秒数（≥1）。
    pub retry_after_secs: u64,
}

/// 登录失败限流/临时锁定端口。
///
/// 采用**预占**模型：必须在昂贵的凭据校验（Argon2 约 19 MiB / 数十毫秒）**之前**
/// `reserve`，出结果后按结果 `record_failure` 或 `record_success`；中途放弃则 `release`。
/// 只在事后记失败（先读检查、再慢慢校验、最后才计数）会留下 TOCTOU 窗口：
/// N 个并发请求会在任何一次失败被记录之前全部通过检查，阈值形同虚设。
///
/// 并发额度与已累计失败共享同一个上限，因此「同时在飞」的尝试也不会超发。
/// 本端口是进程内的短临界区，操作必须同步且不执行阻塞 I/O，便于在 Drop 中归还。
/// 返回 Err 时不得改变预占状态；release 必须可靠归还，以免请求取消泄漏额度。
pub trait LoginThrottle: Send + Sync {
    /// 预占一次尝试额度。锁定中直接拒绝（既不占额度，也不延长锁定）。
    fn reserve(&self, subject: &ThrottleSubject) -> Result<ThrottleDecision, UseCaseError>;

    /// 归还一次预占（未得出结论就返回：回跳非法、另一维度已锁定、内部错误等）。
    /// 只归还额度，不影响已累计的失败次数。
    fn release(&self, subject: &ThrottleSubject) -> Result<(), UseCaseError>;

    /// 预占转为失败：失败计数 +1，达到阈值即进入临时锁定。
    fn record_failure(&self, subject: &ThrottleSubject) -> Result<(), UseCaseError>;

    /// 预占转为成功。
    ///
    /// [`ThrottleSubject::User`]：清空历史失败和锁定，只归还本次预占，保留其他在飞请求。
    /// [`ThrottleSubject::Client`]：只归还本次预占，**不清**历史失败——否则攻击者
    /// 可以用自己的账号反复成功登录，把来源地址维度的计数清零。
    fn record_success(&self, subject: &ThrottleSubject) -> Result<(), UseCaseError>;
}
