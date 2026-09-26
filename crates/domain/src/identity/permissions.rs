//! 有效权限集合；权限目录和授权流程由应用层定义。

use std::collections::BTreeSet;

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
