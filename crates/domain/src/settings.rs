//! Settings 值对象：站点基本信息（site 分组）的写入规则。
//!
//! 规则来源 docs/database-design.md §6：
//! - settings 按 key 分组存 JSONB，结构由应用按分组校验；
//! - 本模块只负责 site 分组可写字段的不变量（trim、非空、长度上限），
//!   不解释存储布局——读取侧的「数据库 > 环境变量 > 默认值」回退在应用层
//!   （application::settings），与写校验分离。
//!
//! 长度按字符计（与 tags.name 一致），不受字节宽度影响；
//! 存储侧没有 CHECK 兜底，入口校验是唯一防线。

use uuid::Uuid;

/// 站点标题上限（字符）。
pub const SITE_TITLE_MAX_CHARS: usize = 200;

/// 站点描述上限（字符）。允许为空（模板渲染空串即可，不必强制写一句描述）。
pub const SITE_DESCRIPTION_MAX_CHARS: usize = 500;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum SettingsError {
    #[error("站点标题不能为空")]
    EmptyTitle,
    #[error("站点标题长度不能超过 {SITE_TITLE_MAX_CHARS} 字符")]
    TitleTooLong,
    #[error("站点描述长度不能超过 {SITE_DESCRIPTION_MAX_CHARS} 字符")]
    DescriptionTooLong,
}

/// site 分组的规范值：字段都已 trim，长度受限，标题非空。
///
/// 这是**写入契约**：保存路径只接受完整对象（标题、描述与 logo 同时落库），
/// 避免「只写了一半」造成环境变量与数据库各出一半的混合状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteSettings {
    title: String,
    description: String,
    /// 站点 logo 引用的媒体资产（None = 无 logo）。
    ///
    /// 按用户确认的取舍，id 存进 settings.site 的 JSONB 值；引用行仍由
    /// `content_media_refs` 承载，删除保护与公开来源以引用表为准。
    logo_media_id: Option<Uuid>,
}

impl SiteSettings {
    /// 规范化并校验：两侧 trim；标题 trim 后非空且 ≤200 字符；描述 ≤500 字符。
    pub fn new(
        title: String,
        description: String,
        logo_media_id: Option<Uuid>,
    ) -> Result<Self, SettingsError> {
        let title = title.trim().to_string();
        if title.is_empty() {
            return Err(SettingsError::EmptyTitle);
        }
        if title.chars().count() > SITE_TITLE_MAX_CHARS {
            return Err(SettingsError::TitleTooLong);
        }
        let description = description.trim().to_string();
        if description.chars().count() > SITE_DESCRIPTION_MAX_CHARS {
            return Err(SettingsError::DescriptionTooLong);
        }
        Ok(Self {
            title,
            description,
            logo_media_id,
        })
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn logo_media_id(&self) -> Option<Uuid> {
        self.logo_media_id
    }

    /// 已规范化的字段原组输出（写存储时使用）。
    pub fn into_parts(self) -> (String, String, Option<Uuid>) {
        (self.title, self.description, self.logo_media_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_trims_both_fields() {
        let s =
            SiteSettings::new("  Sun's Blog ".into(), "  一个 Rust 博客 ".into(), None).unwrap();
        assert_eq!(s.title(), "Sun's Blog");
        assert_eq!(s.description(), "一个 Rust 博客");
        assert_eq!(s.logo_media_id(), None);
    }

    #[test]
    fn title_must_be_non_empty_after_trim() {
        assert_eq!(
            SiteSettings::new("   ".into(), "描述".into(), None).unwrap_err(),
            SettingsError::EmptyTitle
        );
    }

    #[test]
    fn empty_description_is_allowed() {
        let s = SiteSettings::new("标题".into(), "".into(), None).unwrap();
        assert_eq!(s.description(), "");
    }

    #[test]
    fn logo_is_carried_through_unchanged() {
        let logo = Uuid::now_v7();
        let s = SiteSettings::new("标题".into(), "描述".into(), Some(logo)).unwrap();
        assert_eq!(s.logo_media_id(), Some(logo));
        assert_eq!(s.into_parts().2, Some(logo));
    }

    #[test]
    fn overlong_fields_are_rejected_by_char_count() {
        assert_eq!(
            SiteSettings::new("长".repeat(SITE_TITLE_MAX_CHARS + 1), "d".into(), None).unwrap_err(),
            SettingsError::TitleTooLong
        );
        assert_eq!(
            SiteSettings::new(
                "t".into(),
                "字".repeat(SITE_DESCRIPTION_MAX_CHARS + 1),
                None
            )
            .unwrap_err(),
            SettingsError::DescriptionTooLong
        );
        // 恰好在上限内必须通过（边界）。
        assert!(
            SiteSettings::new(
                "长".repeat(SITE_TITLE_MAX_CHARS),
                "字".repeat(SITE_DESCRIPTION_MAX_CHARS),
                None
            )
            .is_ok()
        );
    }
}
