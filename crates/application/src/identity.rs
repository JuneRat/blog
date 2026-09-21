//! 身份用例：M1 的受控 CLI 用户创建与 Actor 解析。
//! 不开放自助注册；OAuth 绑定与角色随 M2 交付。

use std::sync::Arc;

use uuid::Uuid;

use crate::error::UseCaseError;
use crate::ports::{Clock, UserRepository};
use domain::identity::{User, UserId, UserSnapshot};

/// 调用者契约：接口层把外部凭据转换为 Actor，用例据此校权。
/// M2 引入会话/OAuth 通道后扩展 channel，前台提交的用户 ID 不构成可信身份。
#[derive(Debug, Clone)]
pub struct Actor {
    pub user_id: UserId,
    pub channel: ActorChannel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorChannel {
    /// 受控本机 CLI（M1 唯一写通道；不得暴露为公开管理 HTTP）。
    ControlledCli,
}

impl Actor {
    /// 写通道守卫：所有写用例入口必须先调用。
    /// M2 增加会话通道时，这里同步收敛为「允许写的通道」白名单，
    /// 穷举 match 保证新增通道不写用例就无法编译通过。
    pub fn ensure_write_channel(&self) -> Result<(), UseCaseError> {
        match self.channel {
            ActorChannel::ControlledCli => Ok(()),
        }
    }
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
    clock: Arc<dyn Clock>,
}

impl UserInteractor {
    pub fn new(users: Arc<dyn UserRepository>, clock: Arc<dyn Clock>) -> Self {
        Self { users, clock }
    }

    /// 受控创建用户。username 统一规范化（trim + 小写）后写入，
    /// 唯一性由数据库约束兜底（users_username_key）。
    pub async fn create_user(&self, cmd: CreateUserCmd) -> Result<UserDto, UseCaseError> {
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

    /// 按用户名解析 Actor（输入走同一规范化策略）；
    /// 软删除用户拒绝（禁止认证及后台操作）。
    pub async fn actor_for_username(&self, username: &str) -> Result<Actor, UseCaseError> {
        let username = domain::identity::normalize_username(username)
            .map_err(|e| UseCaseError::Invalid(e.to_string()))?;
        let snapshot = self
            .users
            .find_by_username(&username)
            .await?
            .ok_or_else(|| UseCaseError::NotFound(format!("用户 {username}")))?;
        let user = User::reconstitute(snapshot);
        if !user.is_active() {
            return Err(UseCaseError::Forbidden);
        }
        Ok(Actor {
            user_id: user.id(),
            channel: ActorChannel::ControlledCli,
        })
    }

    /// 文章作者兜底解析：CLI 未显式指定 --as 时使用文章作者。
    pub async fn actor_for_user_id(&self, id: Uuid) -> Result<Actor, UseCaseError> {
        let snapshot = self
            .users
            .find_by_id(id)
            .await?
            .ok_or_else(|| UseCaseError::NotFound("作者用户".into()))?;
        let user = User::reconstitute(snapshot);
        if !user.is_active() {
            return Err(UseCaseError::Forbidden);
        }
        Ok(Actor {
            user_id: user.id(),
            channel: ActorChannel::ControlledCli,
        })
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
