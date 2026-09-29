//! 有序独立页面导航。配置只存目标 slug，公开输出逐请求复核页面可见性。
use crate::UseCaseError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NavigationPlacement {
    Header,
    Footer,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NavigationItem {
    pub label: String,
    pub page_slug: String,
    pub placement: NavigationPlacement,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct NavigationLink {
    pub label: String,
    pub url: String,
    pub placement: NavigationPlacement,
}

pub const NAVIGATION_LIMIT: usize = 20;

pub fn validate_navigation(
    mut items: Vec<NavigationItem>,
) -> Result<Vec<NavigationItem>, UseCaseError> {
    if items.len() > NAVIGATION_LIMIT {
        return Err(UseCaseError::Invalid("导航最多 20 项".into()));
    }
    let mut targets = std::collections::HashSet::new();
    for item in &mut items {
        item.label = item.label.trim().to_owned();
        if item.label.is_empty()
            || item.label.chars().count() > 40
            || item.label.chars().any(char::is_control)
        {
            return Err(UseCaseError::Invalid(
                "导航名称须为 1–40 字符且不含控制字符".into(),
            ));
        }
        let slug = domain::content::Slug::new(item.page_slug.trim())
            .map_err(|e| UseCaseError::Invalid(e.to_string()))?;
        if domain::content::is_reserved_root_slug(slug.as_str()) {
            return Err(UseCaseError::Invalid("导航目标不能占用系统路径".into()));
        }
        item.page_slug = slug.into_string();
        if !targets.insert((item.page_slug.clone(), item.placement as u8)) {
            return Err(UseCaseError::Invalid("同一位置不能重复添加相同页面".into()));
        }
    }
    Ok(items)
}
