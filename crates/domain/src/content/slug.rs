//! 多种内容模型共享的单段路径。

pub const SLUG_MAX_BYTES: usize = 200;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SlugError {
    #[error("slug 不合法：{0}")]
    InvalidSlug(String),
}

/// 单一路径片段 slug：非空、UTF-8 字节数不超过上限。
/// 字符集固定为 Unicode 字母数字加 `-`、`_`；禁止其他 ASCII 符号与空白，
/// 防止路径分隔、编码与模板输出层面的绕过。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Slug(String);

impl Slug {
    pub fn new(raw: &str) -> Result<Self, SlugError> {
        if raw.is_empty() {
            return Err(SlugError::InvalidSlug("不能为空".into()));
        }
        if raw.len() > SLUG_MAX_BYTES {
            return Err(SlugError::InvalidSlug(format!(
                "超过 {} 字节上限",
                SLUG_MAX_BYTES
            )));
        }
        for ch in raw.chars() {
            let allowed = ch.is_alphanumeric() || ch == '-' || ch == '_';
            if !allowed {
                return Err(SlugError::InvalidSlug(format!(
                    "只允许 Unicode 字母数字、-、_，包含非法字符 {ch:?}"
                )));
            }
        }
        Ok(Self(raw.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_rejects_path_characters() {
        assert!(Slug::new("a/b").is_err());
        assert!(Slug::new("a.b").is_err());
        assert!(Slug::new("a%2Fb").is_err());
        assert!(Slug::new("a b").is_err());
        assert!(Slug::new("").is_err());
        assert!(Slug::new("a&b").is_err(), "符号 & 不再允许");
        assert!(Slug::new("a+b").is_err(), "符号 + 不再允许");
        assert!(Slug::new("a:b").is_err(), "符号 : 不再允许");
        assert!(Slug::new(&"x".repeat(201)).is_err());
        assert!(Slug::new("你好-世界").is_ok());
        assert!(Slug::new("Hello_World-01").is_ok());
    }
}
