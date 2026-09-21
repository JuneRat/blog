//! 身份用例：M1 的受控 CLI 用户创建与 Actor 解析。
//! 不开放自助注册；OAuth 绑定与角色随 M2 交付。

use std::sync::Arc;

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

#[derive(Debug, Clone, serde::Serialize)]
pub struct UserDto {
    pub id: uuid::Uuid,
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

    /// 受控创建用户。username 唯一性由数据库约束兜底（users_username_key）。
    pub async fn create_user(&self, cmd: CreateUserCmd) -> Result<UserDto, UseCaseError> {
        let user = User::new(&cmd.username, cmd.email, cmd.display_name, self.clock.now())
            .map_err(|e| UseCaseError::Invalid(e.to_string()))?;
        let snapshot = user.snapshot();
        self.users.insert(&snapshot).await?;
        Ok(UserDto::from_snapshot(&snapshot))
    }

    /// 按用户名解析 Actor；软删除用户拒绝（禁止认证及后台操作）。
    pub async fn actor_for_username(&self, username: &str) -> Result<Actor, UseCaseError> {
        let snapshot = self
            .users
            .find_by_username(username)
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
    pub async fn actor_for_user_id(&self, id: uuid::Uuid) -> Result<Actor, UseCaseError> {
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
