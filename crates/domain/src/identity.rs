//! 本站用户的最小领域模型。
//! M1 仅覆盖受控 CLI 创建与文章归属校权；OAuth 绑定、软删除语义随 M2 补充。

use time::OffsetDateTime;
use uuid::Uuid;

/// 用户唯一标识。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UserId(pub Uuid);

impl UserId {
    pub fn generate() -> Self {
        Self(Uuid::now_v7())
    }
}

/// 用户持久化快照：仓储重建聚合的受控载体，不直接暴露可变状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserSnapshot {
    pub id: Uuid,
    pub username: String,
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub version: i64,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    pub deleted_at: Option<OffsetDateTime>,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum UserError {
    #[error("username 长度须在 1-64 个字符之间")]
    InvalidUsernameLength,
    #[error("username 只允许 ASCII 字母、数字、- 和 _")]
    InvalidUsernameChar,
    #[error("email 格式不合法")]
    InvalidEmail,
}

/// 检查 username 采用的固定规范化策略：ASCII 字母、数字、`-`、`_`，1-64 字符。
/// 规范化后唯一值由数据库约束兜底。
fn validate_username(username: &str) -> Result<(), UserError> {
    let chars: usize = username.chars().count();
    if chars == 0 || chars > 64 {
        return Err(UserError::InvalidUsernameLength);
    }
    if !username
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(UserError::InvalidUsernameChar);
    }
    Ok(())
}

fn validate_email(email: &str) -> Result<(), UserError> {
    let (local, domain) = email.split_once('@').ok_or(UserError::InvalidEmail)?;
    if local.is_empty() || domain.is_empty() || !domain.contains('.') {
        return Err(UserError::InvalidEmail);
    }
    Ok(())
}

/// 用户实体（M1 最小集）：创建入口校验不变量，其余行为随身份用例扩展。
#[derive(Debug, Clone)]
pub struct User {
    snapshot: UserSnapshot,
}

impl User {
    /// 创建新用户；email 仅做形状校验，唯一性由数据库约束兜底。
    pub fn new(
        username: &str,
        email: Option<String>,
        display_name: Option<String>,
        now: OffsetDateTime,
    ) -> Result<Self, UserError> {
        validate_username(username)?;
        if let Some(email) = email.as_deref() {
            validate_email(email)?;
        }
        Ok(Self {
            snapshot: UserSnapshot {
                id: UserId::generate().0,
                username: username.to_string(),
                email,
                display_name,
                version: 1,
                created_at: now,
                updated_at: now,
                deleted_at: None,
            },
        })
    }

    /// 受控重建入口：仅供持久化适配器从数据库恢复，不再重复业务校验。
    pub fn reconstitute(snapshot: UserSnapshot) -> Self {
        Self { snapshot }
    }

    pub fn snapshot(&self) -> UserSnapshot {
        self.snapshot.clone()
    }

    pub fn id(&self) -> UserId {
        UserId(self.snapshot.id)
    }

    pub fn username(&self) -> &str {
        &self.snapshot.username
    }

    /// 公开署名：优先展示名，回退 username；不暴露邮箱等敏感字段。
    pub fn display_label(&self) -> String {
        self.snapshot
            .display_name
            .clone()
            .unwrap_or_else(|| self.snapshot.username.clone())
    }

    pub fn is_active(&self) -> bool {
        self.snapshot.deleted_at.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }

    #[test]
    fn valid_username_accepted() {
        assert!(User::new("sun", None, None, now()).is_ok());
        assert!(User::new("alice_dev-01", None, None, now()).is_ok());
    }

    #[test]
    fn rejects_invalid_username() {
        assert_eq!(
            User::new("", None, None, now()).unwrap_err(),
            UserError::InvalidUsernameLength
        );
        assert_eq!(
            User::new("空间 用户", None, None, now()).unwrap_err(),
            UserError::InvalidUsernameChar
        );
    }

    #[test]
    fn rejects_invalid_email() {
        assert_eq!(
            User::new("sun", Some("not-an-email".into()), None, now()).unwrap_err(),
            UserError::InvalidEmail
        );
        assert!(User::new("sun", Some("sun@example.com".into()), None, now()).is_ok());
    }
}
