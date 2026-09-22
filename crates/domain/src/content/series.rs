//! Series 聚合：有序系列的目录规则；文章顺序本身存在 posts 上。
//!
//! 规则来源 docs/content-lifecycle.md §3 / database-design.md §4：
//! - 系列是有序文章集合，一篇文章至多一个系列；series_id 与 series_order
//!   同空或同非空，序号为正整数、系列内唯一（可延后约束，重排事务内检查）；
//! - 重排在系列行锁 + series.version 校验下进行，同时递增涉及 posts.version
//!   与 series.version（防止用旧目录重排）；
//! - 系列被文章引用时默认拒绝删除；跨系列移动按 ID 序锁两个系列。

use time::OffsetDateTime;
use uuid::Uuid;

/// series.name 的 varchar(200) 上限（按字符计）。
pub const SERIES_NAME_MAX_CHARS: usize = 200;
/// 描述的应用层上限（DDL 为 text；管理入口仍应有界输入）。
pub const SERIES_DESCRIPTION_MAX_CHARS: usize = 2000;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum SeriesError {
    #[error("系列名称不能为空")]
    EmptyName,
    #[error("系列名称长度不能超过 {SERIES_NAME_MAX_CHARS} 字符")]
    NameTooLong,
    #[error("系列描述长度不能超过 {SERIES_DESCRIPTION_MAX_CHARS} 字符")]
    DescriptionTooLong,
}

/// 仓储重建聚合的受控快照载体。
#[derive(Debug, Clone, PartialEq)]
pub struct SeriesSnapshot {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub cover: Option<String>,
    pub version: i64,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// Series 聚合。字段私有；成员与顺序在 posts 上，重排走专门用例。
#[derive(Debug, Clone)]
pub struct Series {
    snapshot: SeriesSnapshot,
}

impl Series {
    pub fn new(
        name: String,
        slug: super::post::Slug,
        description: Option<String>,
        now: OffsetDateTime,
    ) -> Result<Self, SeriesError> {
        Ok(Self {
            snapshot: SeriesSnapshot {
                id: Uuid::now_v7(),
                name: normalize_name(name)?,
                slug: slug.into_string(),
                description: normalize_description(description)?,
                cover: None,
                version: 1,
                created_at: now,
                updated_at: now,
            },
        })
    }

    pub fn reconstitute(snapshot: SeriesSnapshot) -> Self {
        Self { snapshot }
    }

    pub fn snapshot(&self) -> SeriesSnapshot {
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

    /// 改名/改描述：返回是否存在实际变化（决定 version 是否 +1）。
    /// slug 创建后不可修改。
    pub fn update(
        &mut self,
        name: String,
        description: Option<String>,
    ) -> Result<bool, SeriesError> {
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

fn normalize_name(raw: String) -> Result<String, SeriesError> {
    let name = raw.trim().to_string();
    if name.is_empty() {
        return Err(SeriesError::EmptyName);
    }
    if name.chars().count() > SERIES_NAME_MAX_CHARS {
        return Err(SeriesError::NameTooLong);
    }
    Ok(name)
}

fn normalize_description(raw: Option<String>) -> Result<Option<String>, SeriesError> {
    let Some(desc) = raw else {
        return Ok(None);
    };
    let desc = desc.trim().to_string();
    if desc.is_empty() {
        return Ok(None);
    }
    if desc.chars().count() > SERIES_DESCRIPTION_MAX_CHARS {
        return Err(SeriesError::DescriptionTooLong);
    }
    Ok(Some(desc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::post::Slug;

    fn series() -> Series {
        Series::new(
            "Rust 入门".into(),
            Slug::new("rust-intro").unwrap(),
            Some(" 从零开始 ".into()),
            OffsetDateTime::now_utc(),
        )
        .unwrap()
    }

    #[test]
    fn new_trims_and_validates() {
        let s = series().snapshot();
        assert_eq!(s.name, "Rust 入门");
        assert_eq!(s.description.as_deref(), Some("从零开始"));
    }

    #[test]
    fn empty_and_long_names_rejected() {
        assert_eq!(
            Series::new(
                "  ".into(),
                Slug::new("x").unwrap(),
                None,
                OffsetDateTime::now_utc()
            )
            .unwrap_err(),
            SeriesError::EmptyName
        );
        assert_eq!(
            Series::new(
                "长".repeat(201),
                Slug::new("x").unwrap(),
                None,
                OffsetDateTime::now_utc()
            )
            .unwrap_err(),
            SeriesError::NameTooLong
        );
    }

    #[test]
    fn update_reports_change_atomically() {
        let mut s = series();
        assert!(
            !s.update("Rust 入门".into(), Some("从零开始".into()))
                .unwrap()
        );
        assert!(s.update(" Rust 进阶 ".into(), None).unwrap());
        let snap = s.snapshot();
        assert_eq!(snap.name, "Rust 进阶");
        assert_eq!(snap.description, None);
        let before = s.snapshot();
        assert_eq!(
            s.update("长".repeat(201), None).unwrap_err(),
            SeriesError::NameTooLong
        );
        assert_eq!(s.snapshot(), before, "失败不留部分修改");
    }
}
