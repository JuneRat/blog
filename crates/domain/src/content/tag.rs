//! Tag 聚合：多对多标签的名称规则与不可变 slug。
//!
//! 规则来源 docs/content-lifecycle.md §3：
//! - post_tags 复合主键去重；名称读取当前值，改名影响全部引用文章；
//! - slug 创建后不可修改（无历史路径需求），没有 rename slug 的行为入口；
//! - 标签被文章引用时默认拒绝删除（引用检查在用例/仓储层，聚合只管自身不变量）。

use time::OffsetDateTime;
use uuid::Uuid;

/// tags.name 的 varchar(100) 上限（按字符计）。
pub const TAG_NAME_MAX_CHARS: usize = 100;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum TagError {
    #[error("标签名称不能为空")]
    EmptyName,
    #[error("标签名称长度不能超过 {TAG_NAME_MAX_CHARS} 字符")]
    NameTooLong,
}

/// 仓储重建聚合的受控快照载体。
#[derive(Debug, Clone, PartialEq)]
pub struct TagSnapshot {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub version: i64,
    pub created_at: OffsetDateTime,
}

/// Tag 聚合。字段私有；改名是唯一可变行为，slug 与 id 终身不变。
#[derive(Debug, Clone)]
pub struct Tag {
    snapshot: TagSnapshot,
}

impl Tag {
    /// 创建标签：name 先 trim 再校验；slug 必须已通过 [`Slug`] 验证。
    pub fn new(
        name: String,
        slug: super::post::Slug,
        now: OffsetDateTime,
    ) -> Result<Self, TagError> {
        let name = normalize_name(name)?;
        Ok(Self {
            snapshot: TagSnapshot {
                id: Uuid::now_v7(),
                name,
                slug: slug.into_string(),
                version: 1,
                created_at: now,
            },
        })
    }

    /// 受控重建入口：仅供持久化适配器从数据库恢复聚合。
    pub fn reconstitute(snapshot: TagSnapshot) -> Self {
        Self { snapshot }
    }

    pub fn snapshot(&self) -> TagSnapshot {
        self.snapshot.clone()
    }

    pub fn id(&self) -> Uuid {
        self.snapshot.id
    }

    pub fn slug(&self) -> &str {
        &self.snapshot.slug
    }

    pub fn version(&self) -> i64 {
        self.snapshot.version
    }

    /// 改名：返回是否存在实际变化（决定 version 是否 +1）。
    /// slug 不参与改名——创建后不可修改。
    pub fn rename(&mut self, new_name: String) -> Result<bool, TagError> {
        let new_name = normalize_name(new_name)?;
        if new_name == self.snapshot.name {
            return Ok(false);
        }
        self.snapshot.name = new_name;
        Ok(true)
    }
}

/// 名称规范化：trim 后非空、长度受限。空名在入口就拒绝，
/// 不依赖数据库 CHECK 兜底。
fn normalize_name(raw: String) -> Result<String, TagError> {
    let name = raw.trim().to_string();
    if name.is_empty() {
        return Err(TagError::EmptyName);
    }
    if name.chars().count() > TAG_NAME_MAX_CHARS {
        return Err(TagError::NameTooLong);
    }
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::post::Slug;

    fn tag() -> Tag {
        Tag::new(
            "Rust".into(),
            Slug::new("rust").unwrap(),
            OffsetDateTime::now_utc(),
        )
        .unwrap()
    }

    #[test]
    fn new_trims_and_validates_name() {
        let t = Tag::new(
            "  Rust 语言  ".into(),
            Slug::new("rust").unwrap(),
            OffsetDateTime::now_utc(),
        )
        .unwrap();
        assert_eq!(t.snapshot().name, "Rust 语言");
        assert_eq!(t.version(), 1);
    }

    #[test]
    fn empty_name_is_rejected() {
        assert_eq!(
            Tag::new(
                "   ".into(),
                Slug::new("x").unwrap(),
                OffsetDateTime::now_utc()
            )
            .unwrap_err(),
            TagError::EmptyName
        );
    }

    #[test]
    fn long_name_is_rejected() {
        let err = Tag::new(
            "长".repeat(TAG_NAME_MAX_CHARS + 1),
            Slug::new("x").unwrap(),
            OffsetDateTime::now_utc(),
        )
        .unwrap_err();
        assert_eq!(err, TagError::NameTooLong);
    }

    #[test]
    fn rename_reports_change_and_trims() {
        let mut t = tag();
        assert!(!t.rename("Rust".into()).unwrap(), "同名不算变化");
        assert!(t.rename(" Rust 语言 ".into()).unwrap());
        assert_eq!(t.snapshot().name, "Rust 语言");
    }

    #[test]
    fn rename_rejects_invalid_names_atomically() {
        let mut t = tag();
        assert_eq!(t.rename("  ".into()).unwrap_err(), TagError::EmptyName);
        assert_eq!(t.snapshot().name, "Rust", "失败不留部分修改");
    }

    #[test]
    fn slug_uses_shared_path_segment_rules() {
        // Slug 规则与文章一致（复用同一实现）：路径分隔与符号被拒绝。
        assert!(Slug::new("a/b").is_err());
        assert!(Slug::new("你好-标签").is_ok());
    }
}
