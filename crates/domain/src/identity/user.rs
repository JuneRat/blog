//! 用户聚合、身份标识与资料值对象。
//! `UserSnapshot::version` 是会话绑定的身份修订号，资料更新不能将其当作通用内容版本。

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
    /// 头像所引用的媒体资产（None = 无头像）。
    ///
    /// 用户自助设置，因此没有版本前提；公开可读性由媒体库按「账号未软删除」
    /// 实时判定，不由聚合缓存。
    pub avatar_media_id: Option<Uuid>,
    /// 身份修订号：供会话失效判定，不是资料编辑的通用乐观锁版本。
    /// 凭据、角色及停用变更递增；头像修改不递增。
    pub version: i64,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    pub deleted_at: Option<OffsetDateTime>,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum UserError {
    #[error("用户快照结构无效")]
    InvalidSnapshot,
    #[error("username 长度须在 1-64 个字符之间")]
    InvalidUsernameLength,
    #[error("username 只允许 ASCII 字母、数字、- 和 _")]
    InvalidUsernameChar,
    #[error("email 格式不合法")]
    InvalidEmail,
    #[error("display_name 不能为空白")]
    InvalidDisplayName,
}

/// Canonical username; every constructor normalizes and validates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Username(String);
impl Username {
    pub fn new(raw: &str) -> Result<Self, UserError> {
        let value = raw.trim().to_ascii_lowercase();
        validate_username(&value)?;
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn into_string(self) -> String {
        self.0
    }
}

/// Optionality belongs to the caller; an Email itself can never be empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Email(String);
impl Email {
    pub fn new(raw: &str) -> Result<Self, UserError> {
        let value = raw.trim();
        validate_email(value)?;
        Ok(Self(value.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn into_string(self) -> String {
        self.0
    }
}

/// username 固定规范化策略：trim + ASCII 小写。
/// 创建与查询必须走同一函数，保证 COLLATE "C" 下的唯一键行为一致。
pub fn normalize_username(raw: &str) -> Result<String, UserError> {
    Username::new(raw).map(Username::into_string)
}

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

/// email 形状校验：`local@domain`，domain 的每个点分段都非空。
/// 拒绝 `a@.com`、`a@b.`、`@b.com` 等。
fn validate_email(email: &str) -> Result<(), UserError> {
    if email.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(UserError::InvalidEmail);
    }
    let (local, domain) = email.split_once('@').ok_or(UserError::InvalidEmail)?;
    if local.is_empty() || domain.is_empty() || domain.contains('@') || !domain.contains('.') {
        return Err(UserError::InvalidEmail);
    }
    if domain.split('.').any(|label| label.is_empty()) {
        return Err(UserError::InvalidEmail);
    }
    Ok(())
}

/// 用户聚合：创建与重建入口校验用户名及资料不变量。
#[derive(Debug, Clone)]
pub struct User {
    snapshot: UserSnapshot,
}

impl User {
    /// 创建新用户；用户名与邮箱由值对象统一规范化和校验。
    pub fn new(
        username: &str,
        email: Option<String>,
        display_name: Option<String>,
        now: OffsetDateTime,
    ) -> Result<Self, UserError> {
        let username = Username::new(username)?;
        let email = email
            .as_deref()
            .map(Email::new)
            .transpose()?
            .map(Email::into_string);
        if let Some(name) = display_name.as_deref()
            && name.trim().is_empty()
        {
            return Err(UserError::InvalidDisplayName);
        }
        Ok(Self {
            snapshot: UserSnapshot {
                id: UserId::generate().0,
                username: username.into_string(),
                email,
                display_name,
                avatar_media_id: None,
                version: 1,
                created_at: now,
                updated_at: now,
                deleted_at: None,
            },
        })
    }

    /// 供应用层复用的 email 形状校验入口。
    pub fn validate_email_shape(email: &str) -> Result<(), UserError> {
        validate_email(email)
    }

    /// Validate persisted structure without silently changing stored identity keys.
    pub fn reconstitute(snapshot: UserSnapshot) -> Result<Self, UserError> {
        if Username::new(&snapshot.username)?.as_str() != snapshot.username || snapshot.version < 1
        {
            return Err(UserError::InvalidSnapshot);
        }
        if let Some(email) = &snapshot.email
            && Email::new(email)?.as_str() != email
        {
            return Err(UserError::InvalidSnapshot);
        }
        if snapshot
            .display_name
            .as_deref()
            .is_some_and(|name| name.trim().is_empty())
        {
            return Err(UserError::InvalidDisplayName);
        }
        Ok(Self { snapshot })
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
        // 边界形状：空点分段拒绝。
        for bad in ["a@.com", "a@b.", "@b.com", "a b@c.com"] {
            assert_eq!(
                User::new("sun", Some(bad.into()), None, now()).unwrap_err(),
                UserError::InvalidEmail,
                "{bad} 应被拒绝"
            );
        }
    }

    #[test]
    fn normalize_username_trims_and_lowercases() {
        assert_eq!(normalize_username("  Sun ").unwrap(), "sun");
        assert_eq!(normalize_username("Alice-DEV_01").unwrap(), "alice-dev_01");
        assert!(normalize_username("空间 用户").is_err());
    }

    #[test]
    fn rejects_blank_display_name() {
        assert_eq!(
            User::new("sun", None, Some("   ".into()), now()).unwrap_err(),
            UserError::InvalidDisplayName
        );
        assert!(User::new("sun", None, Some("Sun".into()), now()).is_ok());
    }

    #[test]
    fn value_objects_normalize_and_reject_ambiguous_email() {
        assert_eq!(
            Username::new("  Alice-DEV_01 ").unwrap().as_str(),
            "alice-dev_01"
        );
        assert_eq!(Email::new(" a@b.com ").unwrap().as_str(), "a@b.com");
        for bad in ["a@b@c.com", "a@@c.com", "a\u{0}@b.com", "a@b..com", ""] {
            assert_eq!(Email::new(bad), Err(UserError::InvalidEmail));
        }
        let user = User::new(" SUN ", Some(" sun@example.com ".into()), None, now()).unwrap();
        assert_eq!(user.username(), "sun");
        assert_eq!(user.snapshot().email.as_deref(), Some("sun@example.com"));
        let mut invalid = user.snapshot();
        invalid.email = Some("a@b@c.com".into());
        assert!(User::reconstitute(invalid).is_err());
    }
}
