//! Category 聚合：分类树的节点规则；树形不变量（防环）在仓储事务层校验。
//!
//! 规则来源 docs/content-lifecycle.md §3：
//! - 一篇文章至多一个分类；parent_id 支持树结构，不允许自身或祖先形成环；
//! - 防环不能只靠自引用 CHECK（只排除直接自父）：创建、移动、删除统一在
//!   分类树事务锁内做祖先链校验（见仓储实现）；
//! - slug 唯一且创建后不可修改（与标签同理：避免历史路径需求）；
//! - 分类被文章引用或有子分类时默认拒绝删除，不级联静默改变文章。

use time::OffsetDateTime;
use uuid::Uuid;

/// categories.name 的 varchar(100) 上限（按字符计）。
pub const CATEGORY_NAME_MAX_CHARS: usize = 100;
/// 描述的应用层上限（DDL 为 text；管理入口仍应有界输入）。
pub const CATEGORY_DESCRIPTION_MAX_CHARS: usize = 2000;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum CategoryError {
    #[error("分类名称不能为空")]
    EmptyName,
    #[error("分类名称长度不能超过 {CATEGORY_NAME_MAX_CHARS} 字符")]
    NameTooLong,
    #[error("分类描述长度不能超过 {CATEGORY_DESCRIPTION_MAX_CHARS} 字符")]
    DescriptionTooLong,
}

/// 仓储重建聚合的受控快照载体。
#[derive(Debug, Clone, PartialEq)]
pub struct CategorySnapshot {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub parent_id: Option<Uuid>,
    pub description: Option<String>,
    pub version: i64,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// Category 聚合。字段私有；父节点移动与环校验是树级事务，由用例+仓储执行。
#[derive(Debug, Clone)]
pub struct Category {
    snapshot: CategorySnapshot,
}

impl Category {
    /// 创建分类：name trim 后非空且受限；slug 必须已通过 [`Slug`] 验证；
    /// parent 由用例校验存在（新节点不可能是自己的祖先，创建本身无环风险）。
    pub fn new(
        name: String,
        slug: super::post::Slug,
        parent_id: Option<Uuid>,
        description: Option<String>,
        now: OffsetDateTime,
    ) -> Result<Self, CategoryError> {
        Ok(Self {
            snapshot: CategorySnapshot {
                id: Uuid::now_v7(),
                name: normalize_name(name)?,
                slug: slug.into_string(),
                parent_id,
                description: normalize_description(description)?,
                version: 1,
                created_at: now,
                updated_at: now,
            },
        })
    }

    /// 受控重建入口：仅供持久化适配器从数据库恢复聚合。
    pub fn reconstitute(snapshot: CategorySnapshot) -> Self {
        Self { snapshot }
    }

    pub fn snapshot(&self) -> CategorySnapshot {
        self.snapshot.clone()
    }

    pub fn id(&self) -> Uuid {
        self.snapshot.id
    }

    pub fn slug(&self) -> &str {
        &self.snapshot.slug
    }

    pub fn parent_id(&self) -> Option<Uuid> {
        self.snapshot.parent_id
    }

    pub fn version(&self) -> i64 {
        self.snapshot.version
    }

    /// 改名/改描述：返回是否存在实际变化（决定 version 是否 +1）。
    /// slug 与父节点不在此处变更（slug 终身不变；移动走专门的树事务）。
    pub fn update(
        &mut self,
        name: String,
        description: Option<String>,
    ) -> Result<bool, CategoryError> {
        let name = normalize_name(name)?;
        let description = normalize_description(description)?;
        if name == self.snapshot.name && description == self.snapshot.description {
            return Ok(false);
        }
        self.snapshot.name = name;
        self.snapshot.description = description;
        Ok(true)
    }
}

fn normalize_name(raw: String) -> Result<String, CategoryError> {
    let name = raw.trim().to_string();
    if name.is_empty() {
        return Err(CategoryError::EmptyName);
    }
    if name.chars().count() > CATEGORY_NAME_MAX_CHARS {
        return Err(CategoryError::NameTooLong);
    }
    Ok(name)
}

fn normalize_description(raw: Option<String>) -> Result<Option<String>, CategoryError> {
    let Some(desc) = raw else {
        return Ok(None);
    };
    let desc = desc.trim().to_string();
    if desc.is_empty() {
        return Ok(None);
    }
    if desc.chars().count() > CATEGORY_DESCRIPTION_MAX_CHARS {
        return Err(CategoryError::DescriptionTooLong);
    }
    Ok(Some(desc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::post::Slug;

    fn category() -> Category {
        Category::new(
            "技术".into(),
            Slug::new("tech").unwrap(),
            None,
            Some(" 技术文章 ".into()),
            OffsetDateTime::now_utc(),
        )
        .unwrap()
    }

    #[test]
    fn new_trims_and_validates() {
        let c = category();
        let s = c.snapshot();
        assert_eq!(s.name, "技术");
        assert_eq!(s.description.as_deref(), Some("技术文章"));
        assert_eq!(s.version, 1);
    }

    #[test]
    fn empty_and_long_names_rejected() {
        assert_eq!(
            Category::new(
                "  ".into(),
                Slug::new("x").unwrap(),
                None,
                None,
                OffsetDateTime::now_utc()
            )
            .unwrap_err(),
            CategoryError::EmptyName
        );
        assert_eq!(
            Category::new(
                "长".repeat(101),
                Slug::new("x").unwrap(),
                None,
                None,
                OffsetDateTime::now_utc()
            )
            .unwrap_err(),
            CategoryError::NameTooLong
        );
    }

    #[test]
    fn update_reports_change_and_blanks_description() {
        let mut c = category();
        assert!(
            !c.update("技术".into(), Some("技术文章".into())).unwrap(),
            "同名同描述不算变化"
        );
        assert!(c.update(" 技术笔记 ".into(), None).unwrap());
        let s = c.snapshot();
        assert_eq!(s.name, "技术笔记");
        assert_eq!(s.description, None, "空白描述归一为无描述");
    }

    #[test]
    fn update_failure_leaves_snapshot_untouched() {
        let mut c = category();
        let before = c.snapshot();
        assert_eq!(
            c.update("长".repeat(101), None).unwrap_err(),
            CategoryError::NameTooLong
        );
        assert_eq!(c.snapshot(), before);
    }
}
