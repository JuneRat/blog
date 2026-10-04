//! 身份上下文：用户资料、密码策略与权限集合。
//! 对外统一从 `domain::identity` 导入，模块按职责组织。

mod login_methods;
pub mod password;
pub mod permissions;
pub mod user;

pub use login_methods::{LoginMethod, LoginMethods};
pub use password::{
    COMMON_PASSWORDS, PASSWORD_MAX_CHARS, PASSWORD_MIN_CHARS, PasswordError, validate_password,
};
pub use permissions::PermissionSet;
pub use user::{
    Email, User, UserError, UserId, UserSnapshot, UserStatus, Username, normalize_username,
};
