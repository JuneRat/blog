//! 用例层统一错误。领域错误表达业务规则；适配器把技术错误映射为端口约定的错误。

/// 唯一性冲突的结构化原因。
///
/// 数据库只给出约束名，适配器把它翻译成稳定枚举，接口层再映射成业务码：
/// 同一个 409 下，`username`/`email` 必须能被前端区分（谁占了、给出哪条文案），
/// 不能只回一个笼统的 `conflict`。新增字段时同步 `unique_conflict_target` 与
/// `admin_error_code` 的穷尽映射；只有已有界面消费者的字段才分配专属业务码。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictKind {
    /// 文章/页面等 slug 占用；沿用既有 `conflict` 业务码。
    Slug,
    /// 用户名占用（`users_username_key`）。
    Username,
    /// 邮箱占用（`users_email_key`）。
    Email,
    /// 外部身份已被别的账号绑定。
    ExternalIdentity,
    /// 角色标识占用。
    RoleSlug,
    /// 权限标识占用。
    PermissionKey,
    /// 无法归类的唯一约束：仍然拒绝，但只回通用冲突。
    Unknown,
}

impl ConflictKind {
    /// 面向用户与日志的字段名（与既有文案保持一致）。
    pub fn field(self) -> &'static str {
        match self {
            Self::Slug => "slug",
            Self::Username => "username",
            Self::Email => "email",
            Self::ExternalIdentity => "外部身份",
            Self::RoleSlug => "角色标识",
            Self::PermissionKey => "权限标识",
            Self::Unknown => "记录",
        }
    }
}

impl std::fmt::Display for ConflictKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.field())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum UseCaseError {
    #[error("未找到：{0}")]
    NotFound(String),

    /// 乐观并发冲突：expected_version 与当前记录不一致，不自动覆盖。
    #[error("版本冲突：内容已被并发修改，请基于最新版本重试")]
    VersionConflict,

    /// 数据库唯一约束兜底命中的冲突（slug、username、email 等）。
    /// 结构化原因让接口层能给出可区分的业务码，而不是只回一个笼统的 409。
    #[error("{0} 已被占用")]
    Conflict(ConflictKind),

    /// 会移除最后一个「可登录」Owner 的操作被拒绝（docs/identity-and-admin.md §3）。
    ///
    /// 与「没有权限」区分开：调用者可能确实持有 `ownership.manage`，只是这次操作
    /// 会让站点失去唯一能登录的 Owner。前端必须能解释原因，而不是显示“无权操作”。
    #[error("不能停用或移除最后一个可登录的 Owner")]
    LastOwnerProtected,

    /// 删除仍被引用或仍含子分类的分类被拒绝（引用保护）。
    ///
    /// 文章引用不过滤可见性（草稿/私密/回收站同样占用）；子分类须先移动或删除。
    /// HTTP 层映射 409 + `category_in_use`，与 slug 占用的 `conflict` 区分。
    #[error("分类仍被 {posts} 篇文章引用、仍有 {children} 个子分类；先解除引用并移走子分类")]
    CategoryInUse { posts: i64, children: i64 },

    #[error("无权执行该操作")]
    Forbidden,

    /// 会话不存在/已过期：HTTP 层映射 401，区别于有身份但权限不足的 403。
    #[error("未登录或会话已失效")]
    Unauthenticated,

    /// 凭据无效：用户名不存在、密码错误或账号不可登录，统一返回同一错误，
    /// 不区分具体原因（避免用户名枚举）。HTTP 层映射 401。
    #[error("用户名或密码不正确")]
    InvalidCredentials,

    /// 登录失败次数超过阈值后的临时锁定；`retry_after_secs` 供 HTTP `Retry-After`。
    #[error("尝试过于频繁，请在 {retry_after_secs} 秒后重试")]
    RateLimited { retry_after_secs: u64 },

    /// 外部身份服务（OIDC/GitHub）交互失败：HTTP 层映射 502。
    #[error("外部身份服务错误：{0}")]
    External(String),

    #[error("{0}")]
    Invalid(String),

    #[error("存储错误：{0}")]
    Repository(String),

    #[error("渲染错误：{0}")]
    Render(String),
}
