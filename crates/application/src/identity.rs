//! 身份与 RBAC 用例：受控用户创建、Actor 解析（含权限并集）、角色管理。
//!
//! 权限目录是应用可信注册表（PERMISSION_REGISTRY），随初始化同步；
//! 普通入口不能创造任意 key。内置角色 seed 保留 slug；
//! Admin 识别只来自受保护的 admin 角色分配，不从“拥有全部权限”推导。

use std::sync::Arc;

pub mod policy;

use uuid::Uuid;

use crate::error::UseCaseError;
use crate::ports::{AccountAdministration, Clock, RbacStore, RoleDto, UserProfileStore, UserQuery};
pub use domain::identity::UserStatus;
use domain::identity::{PermissionSet, User, UserId, UserSnapshot};

/// 受信权限描述符（resource.action）。仅随用例落地注册；
/// `*_any` 覆盖对应 own 动作的关系在描述中声明。
#[derive(Debug, Clone)]
pub struct PermissionDescriptor {
    pub key: &'static str,
    pub name: &'static str,
    pub description: &'static str,
}

/// 当前注册的权限目录。新增用例时在此登记，随 role sync 幂等同步。
pub const PERMISSION_REGISTRY: &[PermissionDescriptor] = &[
    PermissionDescriptor {
        key: "post.create",
        name: "创建文章",
        description: "创建本人文章草稿。",
    },
    PermissionDescriptor {
        key: "post.read",
        name: "读取本人文章",
        description: "查看本人文章的草稿与私有内容。",
    },
    PermissionDescriptor {
        key: "post.read_any",
        name: "读取任意文章",
        description: "查看所有文章（含他人草稿/私有）；覆盖 post.read。",
    },
    PermissionDescriptor {
        key: "post.update",
        name: "编辑本人文章",
        description: "修改本人文章；保存已发布内容直接更新线上。",
    },
    PermissionDescriptor {
        key: "post.update_any",
        name: "编辑任意文章",
        description: "修改所有文章；覆盖 post.update。",
    },
    PermissionDescriptor {
        key: "post.publish",
        name: "发布本人文章",
        description: "发布/重新发布本人文章。",
    },
    PermissionDescriptor {
        key: "post.publish_any",
        name: "发布任意文章",
        description: "发布/重新发布所有文章；覆盖 post.publish。",
    },
    PermissionDescriptor {
        key: "post.unpublish",
        name: "撤回本人文章",
        description: "将本人已发布文章撤回为草稿。",
    },
    PermissionDescriptor {
        key: "post.unpublish_any",
        name: "撤回任意文章",
        description: "撤回所有已发布文章；覆盖 post.unpublish。",
    },
    PermissionDescriptor {
        key: "post.delete",
        name: "移入本人文章回收站",
        description: "移入或恢复本人回收站文章。",
    },
    PermissionDescriptor {
        key: "post.delete_any",
        name: "移入任意文章回收站",
        description: "移入或恢复任意回收站文章。",
    },
    PermissionDescriptor {
        key: "post.purge",
        name: "永久删除文章",
        description: "仅永久删除回收站文章。",
    },
    PermissionDescriptor {
        key: "page.read",
        name: "读取页面",
        description: "查看所有独立页面（含草稿与私有）；Page 无作者，按站点范围判定。",
    },
    PermissionDescriptor {
        key: "page.create",
        name: "创建页面",
        description: "创建独立页面草稿；Page 无作者归属。",
    },
    PermissionDescriptor {
        key: "page.update",
        name: "编辑页面",
        description: "修改任意独立页面；保存已发布页面直接更新线上。",
    },
    PermissionDescriptor {
        key: "page.publish",
        name: "发布页面",
        description: "发布/重新发布独立页面。",
    },
    PermissionDescriptor {
        key: "page.unpublish",
        name: "撤回页面",
        description: "将已发布页面撤回为草稿。",
    },
    PermissionDescriptor {
        key: "page.archive",
        name: "归档页面",
        description: "归档页面；可退回草稿。",
    },
    PermissionDescriptor {
        key: "page.delete",
        name: "删除页面",
        description: "移入或恢复页面回收站。",
    },
    PermissionDescriptor {
        key: "page.purge",
        name: "永久删除页面",
        description: "仅永久删除回收站页面。",
    },
    PermissionDescriptor {
        key: "tag.manage",
        name: "标签管理",
        description: "创建/改名/删除标签目录；文章与标签的关联仍按文章授权核验。",
    },
    PermissionDescriptor {
        key: "category.manage",
        name: "分类管理",
        description: "创建/更新/移动/删除分类树；防环在分类树事务锁内校验。",
    },
    PermissionDescriptor {
        key: "series.manage",
        name: "系列管理",
        description: "创建/更新/删除系列与整体重排；改他人文章仍需相应 any 权限。",
    },
    PermissionDescriptor {
        key: "user.manage",
        name: "账号管理",
        description: "管理普通账号（受委派与 Admin 限制约束）。",
    },
    PermissionDescriptor {
        key: "role.manage",
        name: "角色管理",
        description: "管理角色与分配（不能绕过委派检查与 Admin 保护）。",
    },
    PermissionDescriptor {
        key: "plugins.manage",
        name: "插件管理",
        description: "查看、启停和配置已注册的站点插件。",
    },
    PermissionDescriptor {
        key: "settings.manage",
        name: "站点设置",
        description: "修改普通站点设置；不覆盖受保护的 OAuth 配置。",
    },
    PermissionDescriptor {
        key: "media.read",
        name: "浏览媒体库",
        description: "浏览共享媒体库；图片链接独立公开，使用位置按内容权限展示。",
    },
    PermissionDescriptor {
        key: "media.upload",
        name: "上传图片",
        description: "上传位图到媒体库；仅接受经内容校验的 PNG/JPEG/GIF/WebP。",
    },
    PermissionDescriptor {
        key: "media.delete",
        name: "管理本人图片回收站",
        description: "将本人上传的图片移入回收站或恢复；链接和已有引用保留。",
    },
    PermissionDescriptor {
        key: "media.delete_any",
        name: "管理全部图片回收站",
        description: "将任意图片移入回收站或恢复；可清理超期上传暂存文件。",
    },
    PermissionDescriptor {
        key: "oauth.manage",
        name: "外部身份配置",
        description: "管理 OAuth 提供商与外部身份绑定；不受普通 settings.manage 覆盖。",
    },
    PermissionDescriptor {
        key: "audit.read",
        name: "查看审计记录",
        description: "读取全站成功业务变更及来源信息；默认仅 Admin，独立于 settings.manage。",
    },
    PermissionDescriptor {
        key: "admin.manage",
        name: "管理管理员",
        description: "授予、移除管理员角色及停用管理员账号。",
    },
];

/// Admin 角色的稳定 slug：所有权识别只来自该角色的受保护分配。
pub const ADMIN_ROLE_SLUG: &str = "admin";

/// 内置角色定义。slug 由 seed 保留，普通 API 不可创建、改名或删除。
#[derive(Debug, Clone)]
pub struct BuiltinRoleDef {
    pub slug: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    /// 只能引用 PERMISSION_REGISTRY 中已注册的 key。
    pub permissions: &'static [&'static str],
}

pub const BUILTIN_ROLES: &[BuiltinRoleDef] = &[
    BuiltinRoleDef {
        slug: "admin",
        name: "Administrator",
        description: "管理员：全部已注册权限；保护最后一个可登录管理员。",
        permissions: &[
            "audit.read",
            "post.create",
            "post.read",
            "post.read_any",
            "post.update",
            "post.update_any",
            "post.publish",
            "post.publish_any",
            "post.unpublish",
            "post.unpublish_any",
            "post.delete",
            "post.delete_any",
            "post.purge",
            "page.read",
            "page.create",
            "page.update",
            "page.publish",
            "page.unpublish",
            "page.archive",
            "page.delete",
            "page.purge",
            "tag.manage",
            "category.manage",
            "series.manage",
            "user.manage",
            "role.manage",
            "settings.manage",
            "plugins.manage",
            "media.read",
            "media.upload",
            "media.delete",
            "media.delete_any",
            "oauth.manage",
            "admin.manage",
        ],
    },
    BuiltinRoleDef {
        slug: "editor",
        name: "Editor",
        description: "内容编辑：对所有文章执行 any 动作，并管理站点级页面与标签目录。",
        permissions: &[
            "post.create",
            "post.read",
            "post.update",
            "post.publish",
            "post.unpublish",
            "post.delete",
            "media.delete",
            "post.read_any",
            "post.update_any",
            "post.publish_any",
            "post.unpublish_any",
            "post.delete_any",
            "page.read",
            "page.create",
            "page.update",
            "page.publish",
            "page.unpublish",
            "page.archive",
            "page.delete",
            "tag.manage",
            "category.manage",
            "series.manage",
            "media.read",
            "media.upload",
            "media.delete_any",
        ],
    },
    BuiltinRoleDef {
        slug: "author",
        name: "Author",
        description: "作者：创建并管理本人文章（own 动作）。",
        permissions: &[
            "post.create",
            "post.read",
            "post.update",
            "post.publish",
            "post.unpublish",
            "post.delete",
            "media.read",
            "media.upload",
            "media.delete",
        ],
    },
    BuiltinRoleDef {
        slug: "reader",
        name: "Reader",
        description: "读者：管理本人资料、发表评论，不能发表文章。",
        permissions: &[],
    },
];

/// 调用者契约：接口层把外部凭据转换为 Actor，用例据此校权。
/// 权限集合在解析时从主库读取（初期不缓存有效权限）。
#[derive(Debug, Clone)]
pub struct Actor {
    pub user_id: UserId,
    pub channel: ActorChannel,
    permissions: PermissionSet,
    audit_ip: Option<std::net::IpAddr>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorChannel {
    /// 受控本机 CLI（本机信任的操作通道）。
    ControlledCli,
    /// 已认证浏览器会话（cookie + CSRF 保护；每次敏感操作重新读权限）。
    Session,
}

impl Actor {
    pub fn with_audit_ip(mut self, ip: Option<std::net::IpAddr>) -> Self {
        self.audit_ip = ip;
        self
    }

    pub fn audit_context(&self) -> crate::audit::AuditContext {
        crate::audit::AuditContext {
            actor_id: self.audit_actor_id(),
            ip_address: self.audit_ip,
        }
    }
    /// 安装/恢复 CLI 的空身份及系统任务不伪造用户外键。
    pub fn audit_actor_id(&self) -> Option<Uuid> {
        if self.channel == ActorChannel::ControlledCli && self.user_id.0.is_nil() {
            None
        } else {
            Some(self.user_id.0)
        }
    }

    pub fn new(user_id: UserId, channel: ActorChannel, permissions: PermissionSet) -> Self {
        Self {
            user_id,
            channel,
            permissions,
            audit_ip: None,
        }
    }

    /// 受控 CLI 引导身份：本机 shell 访问等同部署权限，持有全部已注册权限。
    /// 仅用于 `ControlledCli` 通道的身份/OAuth 引导（首个 Admin 初始化等）；
    /// 不承载文章归属，`user_id` 为占位值。
    pub fn bootstrap_cli() -> Self {
        Self::new(
            UserId(Uuid::nil()),
            ActorChannel::ControlledCli,
            PermissionSet::from_keys(PERMISSION_REGISTRY.iter().map(|d| d.key)),
        )
    }

    /// 写通道守卫：所有写用例入口必须先调用。
    /// 这里是「允许写的通道」白名单；穷举 match 保证新增通道不写用例就无法编译通过。
    pub fn ensure_write_channel(&self) -> Result<(), UseCaseError> {
        match self.channel {
            ActorChannel::ControlledCli | ActorChannel::Session => Ok(()),
        }
    }

    /// 检查权限 key。角色名称不替代动作检查。
    pub fn has_permission(&self, key: &str) -> bool {
        self.permissions.has(key)
    }

    pub fn permissions(&self) -> &PermissionSet {
        &self.permissions
    }
}

/// own/any 授权判定：any 直接放行；own 要求资源归属本人。
/// any 覆盖 own 的关系在注册表声明，这里统一执行。
pub fn authorize_own_or_any(
    actor: &Actor,
    own_key: &str,
    any_key: &str,
    resource_owner: UserId,
) -> Result<(), UseCaseError> {
    if actor.has_permission(any_key) {
        return Ok(());
    }
    if actor.has_permission(own_key) && resource_owner == actor.user_id {
        return Ok(());
    }
    Err(UseCaseError::Forbidden)
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct UserDto {
    pub id: Uuid,
    pub username: String,
    pub display_name: Option<String>,
    pub created_at: time::OffsetDateTime,
}

impl UserDto {
    fn from_snapshot(snapshot: &UserSnapshot) -> Self {
        Self {
            id: snapshot.id,
            username: snapshot.username.clone(),
            display_name: snapshot.display_name.clone(),
            created_at: snapshot.created_at,
        }
    }
}

/// 本人资料视图：`/me` 与头像入口共用，只暴露账号自身的公开字段。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProfileView {
    pub user_id: Uuid,
    pub username: String,
    pub display_name: Option<String>,
    pub bio: Option<String>,
    pub version: i64,
    pub avatar_media_id: Option<Uuid>,
    /// 头像站内地址（None = 无头像）。
    pub avatar_url: Option<String>,
}

impl ProfileView {
    fn from_snapshot(snapshot: &UserSnapshot) -> Self {
        Self {
            user_id: snapshot.id,
            username: snapshot.username.clone(),
            display_name: snapshot.display_name.clone(),
            bio: snapshot.bio.clone(),
            version: snapshot.version,
            avatar_media_id: snapshot.avatar_media_id,
            avatar_url: snapshot.avatar_media_id.map(crate::media::media_url),
        }
    }
}

/// 账号管理列表默认页大小与硬上限。
/// 上限既防一次拉取无限账号，也让下面批量查角色的 `IN` 列表有界。
pub const ADMIN_USER_PAGE_DEFAULT: i64 = 50;
pub const ADMIN_USER_PAGE_MAX: i64 = 200;

#[derive(Debug, Clone)]
pub struct UserPageDto {
    pub items: Vec<AdminUserDto>,
    pub total: i64,
    pub page: i64,
    pub per_page: i64,
}

/// 账号管理列表条目：账号字段 + 角色 + 登录方式是否存在。
///
/// `can_login` 与最后 Admin 保护使用同一谓词，界面可据它在移除 Admin 前提示；
/// `is_last_loginable_admin` 是**全局**结论（不受分页影响）：为 true 时移除其
/// Admin 角色会被 `remove_role` 拒绝。邮箱只回给持有
/// `user.manage`/`role.manage` 的调用者。
#[derive(Debug, Clone, serde::Serialize)]
pub struct AdminUserDto {
    pub id: Uuid,
    pub username: String,
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub status: &'static str,
    pub version: i64,
    pub deleted: bool,
    pub can_login: bool,
    pub is_last_loginable_admin: bool,
    pub password_enabled: bool,
    pub external_identities: i64,
    pub roles: Vec<String>,
}

pub struct CreateUserCmd {
    pub username: String,
    pub email: Option<String>,
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct UserStatusView {
    pub id: Uuid,
    pub status: &'static str,
    pub version: i64,
}

/// 独立注入读取、资料提交和账号管理能力；不包含密码凭据。
pub struct UserStores {
    pub query: Arc<dyn UserQuery>,
    pub profiles: Arc<dyn UserProfileStore>,
    pub accounts: Arc<dyn AccountAdministration>,
}

pub struct UserInteractor {
    users: UserStores,
    rbac: Arc<dyn RbacStore>,
    clock: Arc<dyn Clock>,
    /// 新增头像引用的可用性校验（`ensure_attachable`）。
    media_guard: Arc<dyn crate::ports::MediaRefGuard>,
}

impl UserInteractor {
    pub fn new(
        users: UserStores,
        rbac: Arc<dyn RbacStore>,
        clock: Arc<dyn Clock>,
        media_guard: Arc<dyn crate::ports::MediaRefGuard>,
    ) -> Self {
        Self {
            users,
            rbac,
            clock,
            media_guard,
        }
    }

    /// 受控创建用户。username 统一规范化（trim + 小写）后写入，
    /// 唯一性由数据库约束兜底（users_username_key）。角色需另行显式分配。
    /// 调用者必须持有 `user.manage`。
    pub async fn create_user(
        &self,
        actor: &Actor,
        cmd: CreateUserCmd,
    ) -> Result<UserDto, UseCaseError> {
        actor.ensure_write_channel()?;
        if !actor.has_permission("user.manage") {
            return Err(UseCaseError::Forbidden);
        }
        let username = domain::identity::normalize_username(&cmd.username)
            .map_err(|e| UseCaseError::Invalid(e.to_string()))?;
        let email = normalize_email(cmd.email).map_err(|e| UseCaseError::Invalid(e.to_string()))?;
        let display_name = normalize_display_name(cmd.display_name);

        let user = User::new(&username, email, display_name, self.clock.now())
            .map_err(|e| UseCaseError::Invalid(e.to_string()))?;
        let snapshot = user.snapshot();
        self.users
            .accounts
            .insert(&user, actor.audit_context())
            .await?;
        Ok(UserDto::from_snapshot(&snapshot))
    }

    /// 本人资料（`/me`）：任何有效会话都可读，只回账号自身的公开字段。
    ///
    /// 账号在读取前已软删除时按不存在处理——会话本应已被撤销，
    /// 这里再兜一层，避免旧 Cookie 在软删除后仍能读到资料。
    pub async fn profile_of(&self, actor: &Actor) -> Result<ProfileView, UseCaseError> {
        let snapshot = self
            .users
            .query
            .find_by_id(actor.user_id.0)
            .await?
            .filter(UserSnapshot::is_active)
            .ok_or_else(|| UseCaseError::NotFound("用户".into()))?;
        Ok(ProfileView::from_snapshot(&snapshot))
    }

    pub async fn update_own_profile(
        &self,
        actor: &Actor,
        display_name: Option<String>,
        bio: Option<String>,
        expected_version: i64,
    ) -> Result<ProfileView, UseCaseError> {
        actor.ensure_write_channel()?;
        let snapshot = self
            .users
            .query
            .find_by_id(actor.user_id.0)
            .await?
            .filter(UserSnapshot::is_active)
            .ok_or(UseCaseError::Unauthenticated)?;
        if snapshot.version != expected_version {
            return Err(UseCaseError::VersionConflict);
        }
        let mut user = User::reconstitute(snapshot)
            .map_err(|error| UseCaseError::DataCorrupt(error.to_string()))?;
        user.edit_profile(normalize_display_name(display_name), bio)
            .map_err(|error| UseCaseError::Invalid(error.to_string()))?;
        let result = self
            .users
            .profiles
            .save_profile(
                &user,
                expected_version,
                self.clock.now(),
                actor.audit_context(),
            )
            .await?;
        Ok(ProfileView::from_snapshot(&result))
    }

    pub async fn revoke_authentication(&self, user_id: Uuid) -> Result<(), UseCaseError> {
        self.users
            .accounts
            .revoke_authentication(user_id, crate::audit::AuditContext::system())
            .await
    }

    pub async fn change_status(
        &self,
        actor: &Actor,
        user_id: Uuid,
        status: UserStatus,
        expected_version: i64,
    ) -> Result<UserStatusView, UseCaseError> {
        actor.ensure_write_channel()?;
        policy::require_account_management(actor.permissions())?;
        if expected_version < 1 {
            return Err(UseCaseError::Invalid(
                "expected_version 必须为正整数".into(),
            ));
        }
        let snapshot = self
            .users
            .accounts
            .change_status(user_id, status, expected_version, self.clock.now(), actor)
            .await?;
        Ok(UserStatusView {
            id: snapshot.id,
            status: snapshot.status.as_str(),
            version: snapshot.version,
        })
    }

    /// 自助设置/清除头像：只允许改本人，不需要额外权限。
    ///
    /// 新头像必须存在且未软删除；同一用户可以保留已软删除的当前头像。
    /// 引用与资料、审计在仓储事务中提交，只递增编辑版本，保持登录。
    pub async fn set_own_avatar(
        &self,
        actor: &Actor,
        avatar_media_id: Option<Uuid>,
        expected_version: i64,
    ) -> Result<ProfileView, UseCaseError> {
        actor.ensure_write_channel()?;
        if expected_version < 1 {
            return Err(UseCaseError::Invalid(
                "expected_version 必须为正整数".into(),
            ));
        }
        let snapshot = self
            .users
            .query
            .find_by_id(actor.user_id.0)
            .await?
            .filter(UserSnapshot::is_active)
            .ok_or_else(|| UseCaseError::NotFound("用户".into()))?;
        if snapshot.version != expected_version {
            return Err(UseCaseError::VersionConflict);
        }
        if let Some(id) = avatar_media_id
            && snapshot.avatar_media_id != Some(id)
        {
            crate::media::ensure_attachable(&*self.media_guard, id).await?;
        }
        let saved = self
            .users
            .profiles
            .set_avatar(
                snapshot.id,
                avatar_media_id,
                expected_version,
                self.clock.now(),
                actor.audit_context(),
            )
            .await?;
        Ok(ProfileView::from_snapshot(&saved))
    }

    /// 账号管理列表：持有 `user.manage` 或 `role.manage` 才可读取。
    ///
    /// 读取不改状态，因此不要求写通道，但仍由用例执行授权——接口层不做权限判断。
    /// 角色一次批量读取，避免逐账号查询。
    ///
    /// `is_last_loginable_admin` 由**全局**可登录 Admin 数判定，不能按当前页推断：
    /// 另一个可登录 Admin 落在后续页时，按页推断会误标并错误禁用移除
    /// （docs §8.3）。这里读出全局计数，逐行给出结论；存储侧执行时仍会复核。
    pub async fn list_users(
        &self,
        actor: &Actor,
        page: i64,
        per_page: i64,
    ) -> Result<UserPageDto, UseCaseError> {
        if !can_administer_accounts(actor) {
            return Err(UseCaseError::Forbidden);
        }
        if !(1..=100_000).contains(&page) {
            return Err(UseCaseError::Invalid("页码超出范围".into()));
        }
        if !(1..=ADMIN_USER_PAGE_MAX).contains(&per_page) {
            return Err(UseCaseError::Invalid(format!(
                "每页数量必须为 1–{ADMIN_USER_PAGE_MAX}"
            )));
        }

        let result = self
            .users
            .query
            .list_admin(per_page, (page - 1) * per_page)
            .await?;
        let rows = result.items;
        let ids: Vec<Uuid> = rows.iter().map(|row| row.id).collect();
        let mut roles: std::collections::HashMap<Uuid, Vec<String>> =
            std::collections::HashMap::new();
        for (user_id, slug) in self.rbac.roles_of_users(&ids).await? {
            roles.entry(user_id).or_default().push(slug);
        }
        let loginable_admins = self.rbac.loginable_admin_count().await?;

        let items = rows
            .into_iter()
            .map(|row| {
                let can_login = row.can_login();
                let roles = roles.remove(&row.id).unwrap_or_default();
                // 与 remove_role 的保护条件同构：确实持有 admin 且可登录，
                // 且全站只剩这一个可登录 Admin。
                let is_last_loginable_admin = can_login
                    && !policy::has_other_loginable_admin(loginable_admins)
                    && roles.iter().any(|slug| slug == ADMIN_ROLE_SLUG);
                AdminUserDto {
                    id: row.id,
                    username: row.username,
                    email: row.email,
                    display_name: row.display_name,
                    status: row.status.as_str(),
                    version: row.version,
                    deleted: row.deleted,
                    can_login,
                    is_last_loginable_admin,
                    password_enabled: row.password_enabled,
                    external_identities: row.external_identities,
                    roles,
                }
            })
            .collect();
        Ok(UserPageDto {
            items,
            total: result.total,
            page,
            per_page,
        })
    }

    /// 按用户名解析 Actor：同一规范化路径 + 软删除拒绝 + 读取当前权限并集。
    pub async fn actor_for_username(&self, username: &str) -> Result<Actor, UseCaseError> {
        let username = domain::identity::normalize_username(username)
            .map_err(|e| UseCaseError::Invalid(e.to_string()))?;
        let snapshot = self
            .users
            .query
            .find_by_username(&username)
            .await?
            .ok_or_else(|| UseCaseError::NotFound(format!("用户 {username}")))?;
        self.actor_from_snapshot(snapshot).await
    }

    /// 文章作者兜底解析：CLI 未显式指定 --as 时使用文章作者。
    pub async fn actor_for_user_id(&self, id: Uuid) -> Result<Actor, UseCaseError> {
        self.actor_for_user_id_with_channel(id, ActorChannel::ControlledCli)
            .await
    }

    /// 按通道构造 Actor（会话认证走 Session；每次重新读当前权限）。
    pub async fn actor_for_user_id_with_channel(
        &self,
        id: Uuid,
        channel: ActorChannel,
    ) -> Result<Actor, UseCaseError> {
        self.actor_with_revision(id, channel)
            .await
            .map(|(actor, _)| actor)
    }

    /// 构造 Actor 并同时返回账号的身份修订号（`users.auth_version`）。
    ///
    /// 会话签发时绑定该版本，校验时比对：改密、认证撤销、软删除等操作递增认证版本，
    /// 因此**另一个进程**（CLI 改密、撤销登录）也能让旧会话立即失效。
    /// 这里只读一次用户，版本是顺带得到的，不增加查询。
    pub async fn actor_with_revision(
        &self,
        id: Uuid,
        channel: ActorChannel,
    ) -> Result<(Actor, i64), UseCaseError> {
        let snapshot = self
            .users
            .query
            .find_by_id(id)
            .await?
            .ok_or_else(|| UseCaseError::NotFound("作者用户".into()))?;
        let version = snapshot.auth_version;
        let user =
            User::reconstitute(snapshot).map_err(|e| UseCaseError::DataCorrupt(e.to_string()))?;
        if !user.is_active() {
            return Err(UseCaseError::Forbidden);
        }
        let permissions = self.rbac.permissions_of_user(user.id().0).await?;
        Ok((Actor::new(user.id(), channel, permissions), version))
    }

    pub async fn roles_of_user(&self, id: Uuid) -> Result<Vec<String>, UseCaseError> {
        self.rbac.roles_of_user(id).await
    }

    async fn actor_from_snapshot(&self, snapshot: UserSnapshot) -> Result<Actor, UseCaseError> {
        let user =
            User::reconstitute(snapshot).map_err(|e| UseCaseError::DataCorrupt(e.to_string()))?;
        if !user.is_active() {
            return Err(UseCaseError::Forbidden);
        }
        let permissions = self.rbac.permissions_of_user(user.id().0).await?;
        Ok(Actor::new(
            user.id(),
            ActorChannel::ControlledCli,
            permissions,
        ))
    }
}

/// 角色管理用例。结构性保护（内置 slug、最后 Admin）始终执行；
/// 入口接受可信 Actor：普通角色分配要求 `role.manage` 且不得超出调用者的
/// 权限集合（委派上限，docs §3）；授予/移除 Admin 另需 `admin.manage`。
pub struct RoleInteractor {
    rbac: Arc<dyn RbacStore>,
    users: Arc<dyn UserQuery>,
}

impl RoleInteractor {
    pub fn new(rbac: Arc<dyn RbacStore>, users: Arc<dyn UserQuery>) -> Self {
        Self { rbac, users }
    }

    /// 幂等同步权限目录与内置角色（初始化命令；server 启动时调用）。
    pub async fn sync_registry(&self) -> Result<(), UseCaseError> {
        self.rbac
            .sync_permission_registry(PERMISSION_REGISTRY)
            .await?;
        self.rbac.sync_builtin_roles(BUILTIN_ROLES).await
    }

    pub async fn list(&self, actor: &Actor) -> Result<Vec<RoleDto>, UseCaseError> {
        if !can_administer_accounts(actor) {
            return Err(UseCaseError::Forbidden);
        }
        let mut roles = self.rbac.list_roles().await?;
        for role in &mut roles {
            role.builtin = BUILTIN_ROLES.iter().any(|d| d.slug == role.slug);
        }
        Ok(roles)
    }

    pub async fn assign_to_username(
        &self,
        actor: &Actor,
        username: &str,
        role_slug: &str,
    ) -> Result<(), UseCaseError> {
        actor.ensure_write_channel()?;
        if !actor.has_permission("role.manage") {
            return Err(UseCaseError::Forbidden);
        }
        // 普通角色分配不能授予 Admin：需专门的所有权权限。
        if role_slug == ADMIN_ROLE_SLUG && !actor.has_permission("admin.manage") {
            return Err(UseCaseError::Forbidden);
        }
        // 委派上限：不能授予自己不具备的权限（未知角色在这里就返回 NotFound）。
        let role_permissions = self.rbac.permissions_of_role(role_slug).await?;
        if !actor.permissions().contains_all(&role_permissions) {
            return Err(UseCaseError::Forbidden);
        }
        let user = self.find_active_user(username).await?;
        self.rbac
            .assign_role(user.id, role_slug, actor.audit_context())
            .await
    }

    pub async fn remove_from_username(
        &self,
        actor: &Actor,
        username: &str,
        role_slug: &str,
    ) -> Result<(), UseCaseError> {
        actor.ensure_write_channel()?;
        if !actor.has_permission("role.manage") {
            return Err(UseCaseError::Forbidden);
        }
        if role_slug == ADMIN_ROLE_SLUG && !actor.has_permission("admin.manage") {
            return Err(UseCaseError::Forbidden);
        }
        let user = self.find_active_user(username).await?;
        // 最后 Admin 保护由存储在排他锁下判定并拒绝。
        self.rbac
            .remove_role(user.id, role_slug, actor.audit_context())
            .await
    }

    async fn find_active_user(
        &self,
        username: &str,
    ) -> Result<domain::identity::UserSnapshot, UseCaseError> {
        let username = domain::identity::normalize_username(username)
            .map_err(|e| UseCaseError::Invalid(e.to_string()))?;
        let user = self
            .users
            .find_by_username(&username)
            .await?
            .ok_or_else(|| UseCaseError::NotFound(format!("用户 {username}")))?;
        if !user.is_active() {
            return Err(UseCaseError::Forbidden);
        }
        Ok(user)
    }
}

/// 账号与角色管理读取所需的权限：`user.manage` 或 `role.manage` 任一即可。
/// 只有 `role.manage` 的委派管理员也必须能看到账号列表才能分配角色。
fn can_administer_accounts(actor: &Actor) -> bool {
    actor.has_permission("user.manage") || actor.has_permission("role.manage")
}

/// email 规范化：trim；空串视为未提供；形状校验复用 domain 规则。
fn normalize_email(raw: Option<String>) -> Result<Option<String>, UseCaseError> {
    match raw {
        None => Ok(None),
        Some(email) => {
            let email = email.trim();
            if email.is_empty() {
                return Ok(None);
            }
            domain::identity::Email::new(email)
                .map(|email| Some(email.into_string()))
                .map_err(|e| UseCaseError::Invalid(e.to_string()))
        }
    }
}

/// 展示名规范化：trim；空串视为未提供，避免空署名。
fn normalize_display_name(raw: Option<String>) -> Option<String> {
    let name = raw?.trim().to_string();
    if name.is_empty() { None } else { Some(name) }
}
