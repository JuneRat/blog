//! 站点生效信息与字段级回退规则；供设置、公开展示和 SEO 共用。

use crate::ports::SiteSettingsValue;

/// 站点基础信息：一次渲染的生效值。
///
/// 生效优先级（M3 起由 settings 驱动）：数据库 site 行 > 装配回退值
/// （内置默认值；部署配置中的 bootstrap 仅用于初始化数据库）。
/// 解析见 [`effective_site`]。
#[derive(Debug, Clone, serde::Serialize)]
pub struct SiteInfo {
    pub title: String,
    pub description: String,
    /// 站点 logo 的站内地址（None = 无 logo）。站点配置公开，因此有 logo 即公开来源。
    pub logo_url: Option<String>,
}

impl Default for SiteInfo {
    fn default() -> Self {
        Self {
            title: "Sun's Blog".into(),
            description: "一个 Rust 博客".into(),
            logo_url: None,
        }
    }
}

/// Validate a partial installation input using the same domain rules as
/// admin settings. Missing fields remain missing, including an absent description.
pub fn initial_site_settings(
    value: SiteSettingsValue,
) -> Result<SiteSettingsValue, crate::UseCaseError> {
    let defaults = SiteInfo::default();
    let (title, description, _) = domain::settings::SiteSettings::new(
        value.title.clone().unwrap_or(defaults.title),
        value.description.clone().unwrap_or(defaults.description),
        None,
    )
    .map_err(|e| crate::UseCaseError::Invalid(e.to_string()))?
    .into_parts();
    Ok(SiteSettingsValue {
        title: value.title.map(|_| title),
        description: value.description.map(|_| description),
        logo_media_id: None,
    })
}

/// 行值 + 装配回退值 → 生效值（字段级回退）。
///
/// - 标题：缺失或 trim 后为空 → 回退（空标题对任何页面都不可用）；
/// - 描述：缺失 → 回退；已保存的空串是**合法选择**（管理员清空描述），
///   不回退，否则「清空描述」永远不生效。
pub fn effective_site(value: &SiteSettingsValue, fallback: &SiteInfo) -> SiteInfo {
    let title = value
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| fallback.title.clone());
    let description = value
        .description
        .as_deref()
        .map(|d| d.trim().to_string())
        .unwrap_or_else(|| fallback.description.clone());
    // logo 只来自已保存的媒体 ID，不使用装配回退值；媒体引用有效性由保存路径校验。
    SiteInfo {
        title,
        description,
        logo_url: value.logo_media_id.map(crate::media::media_url),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fallback() -> SiteInfo {
        SiteInfo {
            title: "默认标题".into(),
            description: "默认描述".into(),
            logo_url: None,
        }
    }

    fn stored(title: Option<&str>, description: Option<&str>) -> SiteSettingsValue {
        SiteSettingsValue {
            title: title.map(str::to_string),
            description: description.map(str::to_string),
            logo_media_id: None,
        }
    }

    #[test]
    fn logo_media_id_becomes_a_public_url() {
        let logo = uuid::Uuid::now_v7();
        let value = SiteSettingsValue {
            title: Some("t".into()),
            description: Some("d".into()),
            logo_media_id: Some(logo),
        };
        let info = effective_site(&value, &fallback());
        assert_eq!(info.logo_url, Some(format!("/media/{logo}")));
        // 没有 logo 时不输出地址。
        assert_eq!(
            effective_site(&stored(Some("t"), Some("d")), &fallback()).logo_url,
            None
        );
    }

    #[test]
    fn complete_row_overrides_fallback_per_field() {
        let info = effective_site(
            &stored(Some(" 数据库标题 "), Some("数据库描述")),
            &fallback(),
        );
        assert_eq!(info.title, "数据库标题");
        assert_eq!(info.description, "数据库描述");
    }

    #[test]
    fn missing_or_blank_title_falls_back() {
        for title in [None, Some(""), Some("   ")] {
            let info = effective_site(&stored(title, Some("d")), &fallback());
            assert_eq!(info.title, "默认标题", "title={title:?} 应回退");
            assert_eq!(info.description, "d");
        }
    }

    #[test]
    fn saved_empty_description_is_kept_not_fallen_back() {
        // 清空描述是合法保存结果：Some("") 生效，只有缺失（None）才回退。
        let info = effective_site(&stored(Some("t"), Some("")), &fallback());
        assert_eq!(info.description, "");
    }
}
