//! 身份与 RBAC 用例：受控用户创建、Actor 解析（含权限并集）、角色管理。
//!
//! 权限目录是应用可信注册表（PERMISSION_REGISTRY），随初始化同步；
//! 普通入口不能创造任意 key。内置角色 seed 保留 slug；
//! Owner 识别只来自受保护的 owner 角色分配，不从“拥有全部权限”推导。

use std::sync::Arc;

use uuid::Uuid;

use crate::error::UseCaseError;
use crate::ports::{Clock, RbacStore, RoleDto, UserRepository};
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
        key: "user.manage",
        name: "账号管理",
        description: "管理普通账号（受委派与 Owner 限制约束）。",
    },
    PermissionDescriptor {
        key: "role.manage",
        name: "角色管理",
        description: "管理角色与分配（不能绕过委派检查与 Owner 保护）。",
    },
    PermissionDescriptor {
        key: "settings.manage",
        name: "站点设置",
        description: "修改普通站点设置；不覆盖受保护的 OAuth 配置。",
    },
    PermissionDescriptor {
        key: "oauth.manage",
        name: "外部身份配置",
        description: "管理 OAuth 提供商与外部身份绑定；不受普通 settings.manage 覆盖。",
    },
    PermissionDescriptor {
        key: "ownership.manage",
        name: "所有权操作",
        description: "授予/移除 Owner 与所有权转移；普通角色分配不得授予 Owner。",
    },
];

/// Owner 角色的稳定 slug：所有权识别只来自该角色的受保护分配。
pub const OWNER_ROLE_SLUG: &str = "owner";

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
        slug: "owner",
        name: "Owner",
        description: "站点所有者：全部已注册权限；受最后 Owner 保护。",
        permissions: &[
            "post.create",
            "post.read",
            "post.read_any",
            "post.update",
            "post.update_any",
            "post.publish",
            "post.publish_any",
            "post.unpublish",
            "post.unpublish_any",
            "user.manage",
            "role.manage",
            "settings.manage",
            "oauth.manage",
            "ownership.manage",
        ],
    },
    BuiltinRoleDef {
        slug: "admin",
        name: "Administrator",
        description: "管理普通身份与站点设置；不含所有权与外部身份配置。",
        permissions: &["user.manage", "role.manage", "settings.manage"],
    },
    BuiltinRoleDef {
        slug: "editor",
        name: "Editor",
        description: "内容编辑：对所有文章执行 any 动作。",
        permissions: &[
            "post.read_any",
            "post.update_any",
            "post.publish_any",
            "post.unpublish_any",
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
        ],
    },
];

/// 调用者契约：接口层把外部凭据转换为 Actor，用例据此校权。
/// 权限集合在解析时从主库读取（初期不缓存有效权限）。
#[derive(Debug, Clone)]
pub struct Actor {
    pub user_id: UserId,
    pub channel: ActorChannel,
    permissions: PermissionSet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorChannel {
    /// 受控本机 CLI（本机信任的操作通道）。
    ControlledCli,
    /// 已认证浏览器会话（cookie + CSRF 保护；每次敏感操作重新读权限）。
    Session,
}

impl Actor {
    pub fn new(user_id: UserId, channel: ActorChannel, permissions: PermissionSet) -> Self {
        Self {
            user_id,
            channel,
            permissions,
        }
    }

    /// 受控 CLI 引导身份：本机 shell 访问等同部署权限，持有全部已注册权限。
    /// 仅用于 `ControlledCli` 通道的身份/OAuth 引导（首个 Owner 初始化等）；
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

pub struct CreateUserCmd {
    pub username: String,
    pub email: Option<String>,
    pub display_name: Option<String>,
}

pub struct UserInteractor {
    users: Arc<dyn UserRepository>,
    rbac: Arc<dyn RbacStore>,
    clock: Arc<dyn Clock>,
}

impl UserInteractor {
    pub fn new(
        users: Arc<dyn UserRepository>,
        rbac: Arc<dyn RbacStore>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self { users, rbac, clock }
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
        self.users.insert(&snapshot).await?;
        Ok(UserDto::from_snapshot(&snapshot))
    }

    /// 按用户名解析 Actor：同一规范化路径 + 软删除拒绝 + 读取当前权限并集。
    pub async fn actor_for_username(&self, username: &str) -> Result<Actor, UseCaseError> {
        let username = domain::identity::normalize_username(username)
            .map_err(|e| UseCaseError::Invalid(e.to_string()))?;
        let snapshot = self
            .users
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
        let snapshot = self
            .users
            .find_by_id(id)
            .await?
            .ok_or_else(|| UseCaseError::NotFound("作者用户".into()))?;
        let user = User::reconstitute(snapshot);
        if !user.is_active() {
            return Err(UseCaseError::Forbidden);
        }
        let permissions = self.rbac.permissions_of_user(user.id().0).await?;
        Ok(Actor::new(user.id(), channel, permissions))
    }

    pub async fn roles_of_user(&self, id: Uuid) -> Result<Vec<String>, UseCaseError> {
        self.rbac.roles_of_user(id).await
    }

    async fn actor_from_snapshot(&self, snapshot: UserSnapshot) -> Result<Actor, UseCaseError> {
        let user = User::reconstitute(snapshot);
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

/// 角色管理用例。结构性保护（内置 slug、最后 Owner）始终执行；
/// 入口接受可信 Actor：普通角色分配要求 `role.manage` 且不得超出调用者的
/// 权限集合（委派上限，docs §3）；授予/移除 Owner 另需 `ownership.manage`。
pub struct RoleInteractor {
    rbac: Arc<dyn RbacStore>,
    users: Arc<dyn UserRepository>,
}

impl RoleInteractor {
    pub fn new(rbac: Arc<dyn RbacStore>, users: Arc<dyn UserRepository>) -> Self {
        Self { rbac, users }
    }

    /// 幂等同步权限目录与内置角色（初始化命令；server 启动时调用）。
    pub async fn sync_registry(&self) -> Result<(), UseCaseError> {
        self.rbac
            .sync_permission_registry(PERMISSION_REGISTRY)
            .await?;
        self.rbac.sync_builtin_roles(BUILTIN_ROLES).await
    }

    pub async fn list(&self) -> Result<Vec<RoleDto>, UseCaseError> {
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
        // 普通角色分配不能授予 Owner：需专门的所有权权限。
        if role_slug == OWNER_ROLE_SLUG && !actor.has_permission("ownership.manage") {
            return Err(UseCaseError::Forbidden);
        }
        // 委派上限：不能授予自己不具备的权限（未知角色在这里就返回 NotFound）。
        let role_permissions = self.rbac.permissions_of_role(role_slug).await?;
        if !actor.permissions().contains_all(&role_permissions) {
            return Err(UseCaseError::Forbidden);
        }
        let user = self.find_active_user(username).await?;
        self.rbac.assign_role(user.id, role_slug).await
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
        if role_slug == OWNER_ROLE_SLUG && !actor.has_permission("ownership.manage") {
            return Err(UseCaseError::Forbidden);
        }
        let user = self.find_active_user(username).await?;
        // 最后 Owner 保护由存储在排他锁下判定并拒绝。
        self.rbac.remove_role(user.id, role_slug).await
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
        if user.deleted_at.is_some() {
            return Err(UseCaseError::Forbidden);
        }
        Ok(user)
    }
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
            User::validate_email_shape(email).map_err(|e| UseCaseError::Invalid(e.to_string()))?;
            Ok(Some(email.to_string()))
        }
    }
}

/// 展示名规范化：trim；空串视为未提供，避免空署名。
fn normalize_display_name(raw: Option<String>) -> Option<String> {
    let name = raw?.trim().to_string();
    if name.is_empty() { None } else { Some(name) }
}
