//! 本站用户的最小领域模型与权限集合。
//! OAuth 绑定、软删除语义随 M2 后续部分补充。

use std::collections::BTreeSet;

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

/// 有效权限集合：用户全部角色授权的并集。
/// key 来自可信注册表（`resource.action`），不由请求参数决定；
/// 域层不枚举业务动作，只承载集合语义。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PermissionSet(BTreeSet<String>);

impl PermissionSet {
    pub fn from_keys<I, K>(keys: I) -> Self
    where
        I: IntoIterator<Item = K>,
        K: Into<String>,
    {
        Self(keys.into_iter().map(Into::into).collect())
    }

    pub fn grant(&mut self, key: &str) {
        self.0.insert(key.to_string());
    }

    pub fn has(&self, key: &str) -> bool {
        self.0.contains(key)
    }

    /// 委派上限判定：本集合是否包含 `other` 的全部 key。
    /// 空集是任何集合的子集（无权限者不能授予任何角色）。
    pub fn contains_all(&self, other: &PermissionSet) -> bool {
        other.0.iter().all(|key| self.0.contains(key))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> + '_ {
        self.0.iter().map(String::as_str)
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
    #[error("display_name 不能为空白")]
    InvalidDisplayName,
}

/// username 固定规范化策略：trim + ASCII 小写。
/// 创建与查询必须走同一函数，保证 COLLATE "C" 下的唯一键行为一致。
pub fn normalize_username(raw: &str) -> Result<String, UserError> {
    let normalized = raw.trim().to_ascii_lowercase();
    validate_username(&normalized)?;
    Ok(normalized)
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
    if email.chars().any(|c| c.is_whitespace()) {
        return Err(UserError::InvalidEmail);
    }
    let (local, domain) = email.split_once('@').ok_or(UserError::InvalidEmail)?;
    if local.is_empty() || domain.is_empty() || !domain.contains('.') {
        return Err(UserError::InvalidEmail);
    }
    if domain.split('.').any(|label| label.is_empty()) {
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
    /// 创建新用户；username/email/display_name 应已由调用方规范化，
    /// 这里兜底校验形状（空展示名拒绝，空字符串不允许落库）。
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
        if let Some(name) = display_name.as_deref()
            && name.trim().is_empty()
        {
            return Err(UserError::InvalidDisplayName);
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

    /// 供应用层复用的 email 形状校验入口。
    pub fn validate_email_shape(email: &str) -> Result<(), UserError> {
        validate_email(email)
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
}
