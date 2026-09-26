//! Series 聚合：有序系列的目录规则；成员关系及排序权重存在 post_series 上。
//!
//! 一篇文章可以加入多个系列，position 非负且可重复。
//! 重排由应用授权，在仓储关系锁和版本条件下提交；删除系列只解除文章关联。
//! 本聚合只保护目录名称、描述和封面字段，不自行读取成员或取得数据库锁。

use time::OffsetDateTime;
use uuid::Uuid;

/// series.name 的 varchar(200) 上限（按字符计）。
pub const SERIES_NAME_MAX_CHARS: usize = 200;
/// 描述的应用层上限（DDL 为 text；管理入口仍应有界输入）。
pub const SERIES_DESCRIPTION_MAX_CHARS: usize = 2000;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum SeriesError {
    #[error("快照结构无效")]
    InvalidSnapshot,
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
    /// 封面所引用的媒体资产（None = 无封面）。与 Post 封面同一套引用规则。
    pub cover_media_id: Option<Uuid>,
    pub version: i64,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// Series 聚合。字段私有；成员与权重在 post_series 上，重排走专门用例。
#[derive(Debug, Clone)]
pub struct Series {
    snapshot: SeriesSnapshot,
}

impl Series {
    pub fn new(
        name: String,
        slug: super::Slug,
        description: Option<String>,
        now: OffsetDateTime,
    ) -> Result<Self, SeriesError> {
        Ok(Self {
            snapshot: SeriesSnapshot {
                id: Uuid::now_v7(),
                name: normalize_name(name)?,
                slug: slug.into_string(),
                description: normalize_description(description)?,
                cover_media_id: None,
                version: 1,
                created_at: now,
                updated_at: now,
            },
        })
    }

    pub fn reconstitute(snapshot: SeriesSnapshot) -> Result<Self, SeriesError> {
        super::Slug::new(&snapshot.slug).map_err(|_| SeriesError::InvalidSnapshot)?;
        if normalize_name(snapshot.name.clone())? != snapshot.name || snapshot.version < 1 {
            return Err(SeriesError::InvalidSnapshot);
        }
        if normalize_description(snapshot.description.clone())? != snapshot.description {
            return Err(SeriesError::InvalidSnapshot);
        }
        Ok(Self { snapshot })
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

    /// 改名/改描述/改封面：返回是否存在实际变化（决定 version 是否 +1）。
    /// slug 创建后不可修改。
    ///
    /// 封面是三态：`None` 不修改；`Some(None)` 移除；`Some(Some(id))` 设置。
    /// 资产存在性及软删除状态由保存事务内的引用校验兜底，聚合不查库。
    pub fn update(
        &mut self,
        name: String,
        description: Option<String>,
        cover_media_id: Option<Option<Uuid>>,
    ) -> Result<bool, SeriesError> {
        let name = normalize_name(name)?;
        let description = normalize_description(description)?;
        let cover = cover_media_id.unwrap_or(self.snapshot.cover_media_id);
        if name == self.snapshot.name
            && description == self.snapshot.description
            && cover == self.snapshot.cover_media_id
        {
            return Ok(false);
        }
        self.snapshot.name = name;
        self.snapshot.description = description;
        self.snapshot.cover_media_id = cover;
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
    use crate::content::Slug;

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
            !s.update("Rust 入门".into(), Some("从零开始".into()), None)
                .unwrap()
        );
        assert!(s.update(" Rust 进阶 ".into(), None, None).unwrap());
        let snap = s.snapshot();
        assert_eq!(snap.name, "Rust 进阶");
        assert_eq!(snap.description, None);
        let before = s.snapshot();
        assert_eq!(
            s.update("长".repeat(201), None, None).unwrap_err(),
            SeriesError::NameTooLong
        );
        assert_eq!(s.snapshot(), before, "失败不留部分修改");
    }

    #[test]
    fn cover_is_three_state_and_counts_as_a_change() {
        let mut s = series();
        let cover = Uuid::now_v7();
        // None = 不修改：封面保持为空，同值不报告变化。
        assert!(
            !s.update("Rust 入门".into(), Some("从零开始".into()), None)
                .unwrap()
        );
        assert_eq!(s.snapshot().cover_media_id, None);
        // Some(Some(id)) = 设置。
        assert!(
            s.update(
                "Rust 入门".into(),
                Some("从零开始".into()),
                Some(Some(cover))
            )
            .unwrap()
        );
        assert_eq!(s.snapshot().cover_media_id, Some(cover));
        // 仅封面变化也算变化。
        assert!(
            s.update("Rust 入门".into(), Some("从零开始".into()), Some(None))
                .unwrap()
        );
        assert_eq!(s.snapshot().cover_media_id, None);
        // 幂等：同值不再报告变化。
        assert!(
            !s.update("Rust 入门".into(), Some("从零开始".into()), Some(None))
                .unwrap()
        );
    }

    #[test]
    fn reconstitution_checks_structural_fields_without_normalizing_them() {
        let original = series().snapshot();
        assert_eq!(
            Series::reconstitute(original.clone()).unwrap().snapshot(),
            original
        );
        for field in 0..3 {
            let mut invalid = original.clone();
            match field {
                0 => invalid.slug = "bad/path".into(),
                1 => invalid.version = 0,
                _ => invalid.name = " ".into(),
            }
            assert!(Series::reconstitute(invalid).is_err());
        }
    }
}
