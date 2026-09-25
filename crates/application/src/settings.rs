//! 站点设置用例：site/theme 分组的读取视图与条件保存。
//!
//! 权限约定（docs/identity-and-admin.md §2）：
//! - 读取与写入都要求 `settings.manage`：站点设置界面只服务于持有者，
//!   与标签目录不同，Author 编辑文章不需要读站点配置，不开放目录读取；
//! - 本用例覆盖 site/theme 分组：oauth 分组的写入仍走 `oauth.manage`
//!   （受控 CLI / OAuth 用例），不受 settings.manage 覆盖；未知分组一律 404。
//!
//! 生效优先级（docs/database-design.md §6）：数据库 site 行 > 装配回退值
//! （环境变量 `BLOG_SITE_TITLE`/`BLOG_SITE_DESCRIPTION` 或内置默认值）。
//! 行内**缺字段**（历史/手工写入）时按字段回退；公开渲染在存储读取失败时
//! 整体回退到装配值，配置问题不拖垮公开页面。
//!
//! 并发约定：保存携带 expected_version（未配置行为 0），条件写入不自动覆盖；
//! 与提交内容完全一致的保存是幂等的，不递增版本。

use std::sync::Arc;

use crate::error::UseCaseError;
use crate::identity::Actor;
use crate::ports::{Clock, SaveOutcome, SettingsStore, SiteSettingsValue, ThemeSettingsStore};
use crate::public_site::SiteInfo;
use crate::themes::{ThemeOption, ThemeRegistry};
use crate::version::checked_version;
use domain::settings::SiteSettings;

/// 当前生效值的来源（管理视图展示用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SiteSettingsSource {
    /// 来自 settings.site 行。
    Database,
    /// 行未配置：环境变量或内置默认值。
    Fallback,
}

/// site 分组的管理视图：生效值 + 来源 + 并发版本。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SiteSettingsView {
    pub title: String,
    pub description: String,
    /// 站点 logo 的媒体资产 id（None = 无 logo）。
    pub logo_media_id: Option<uuid::Uuid>,
    /// 站点 logo 的站内地址（None = 无 logo）。
    pub logo_url: Option<String>,
    pub source: SiteSettingsSource,
    /// site 行当前版本；行不存在时为 0（首次保存以 0 为 expected_version）。
    pub version: i64,
}

pub struct SaveSiteSettingsCmd {
    pub title: String,
    pub description: String,
    /// 站点 logo 的媒体资产 id；None = 无 logo（PUT 是整组替换）。
    pub logo_media_id: Option<uuid::Uuid>,
    pub expected_version: Option<i64>,
}

pub struct SettingsInteractor {
    store: Arc<dyn SettingsStore>,
    clock: Arc<dyn Clock>,
    /// 装配回退值：环境变量/内置默认值（server 装配层构造，进程内不变）。
    fallback: SiteInfo,
    themes: Option<(Arc<dyn ThemeSettingsStore>, Arc<ThemeRegistry>)>,
    /// 站点 logo 附着的归属校验（`ensure_attachable`）：logo 引用是无条件公开来源。
    media_guard: Arc<dyn crate::ports::MediaRefGuard>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ThemeSettingsView {
    pub slug: String,
    pub effective_slug: String,
    pub source: SiteSettingsSource,
    pub version: i64,
    pub available: Vec<ThemeOption>,
}

pub struct SaveThemeSettingsCmd {
    pub slug: String,
    pub expected_version: Option<i64>,
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
    // logo 只来自数据库（装配回退值没有 logo）：指向失效资产时按无 logo 处理，
    // 公开页不会输出一个必然 404 的地址。
    SiteInfo {
        title,
        description,
        logo_url: value.logo_media_id.map(crate::media::media_url),
    }
}

impl SettingsInteractor {
    pub fn new(
        store: Arc<dyn SettingsStore>,
        clock: Arc<dyn Clock>,
        fallback: SiteInfo,
        media_guard: Arc<dyn crate::ports::MediaRefGuard>,
    ) -> Self {
        Self {
            store,
            clock,
            fallback,
            themes: None,
            media_guard,
        }
    }

    pub fn with_themes(
        mut self,
        store: Arc<dyn ThemeSettingsStore>,
        registry: Arc<ThemeRegistry>,
    ) -> Self {
        self.themes = Some((store, registry));
        self
    }

    pub async fn theme_view(&self, actor: &Actor) -> Result<ThemeSettingsView, UseCaseError> {
        if !actor.has_permission("settings.manage") {
            return Err(UseCaseError::Forbidden);
        }
        self.load_theme_view().await
    }

    pub async fn save_theme(
        &self,
        actor: &Actor,
        cmd: SaveThemeSettingsCmd,
    ) -> Result<ThemeSettingsView, UseCaseError> {
        actor.ensure_write_channel()?;
        if !actor.has_permission("settings.manage") {
            return Err(UseCaseError::Forbidden);
        }
        let (store, registry) = self
            .themes
            .as_ref()
            .ok_or_else(|| UseCaseError::Render("主题设置未装配".into()))?;
        if !registry.contains(&cmd.slug) {
            return Err(UseCaseError::Invalid("主题未安装或清单无效".into()));
        }
        let current = store.find_theme().await?;
        let version = current.as_ref().map_or(0, |v| v.version);
        let expected = checked_version(version, cmd.expected_version)?;
        if current.as_ref().is_some_and(|v| v.slug == cmd.slug) {
            return Ok(self.theme_view_of(
                cmd.slug,
                SiteSettingsSource::Database,
                version,
                registry,
            ));
        }
        match store
            .save_theme(&cmd.slug, expected, self.clock.now())
            .await?
        {
            SaveOutcome::Saved { new_version } => Ok(self.theme_view_of(
                cmd.slug,
                SiteSettingsSource::Database,
                new_version,
                registry,
            )),
            SaveOutcome::StaleConflict | SaveOutcome::Gone => Err(UseCaseError::VersionConflict),
        }
    }

    async fn load_theme_view(&self) -> Result<ThemeSettingsView, UseCaseError> {
        let (store, registry) = self
            .themes
            .as_ref()
            .ok_or_else(|| UseCaseError::Render("主题设置未装配".into()))?;
        match store.find_theme().await? {
            Some(record) => Ok(self.theme_view_of(
                record.slug,
                SiteSettingsSource::Database,
                record.version,
                registry,
            )),
            None => Ok(self.theme_view_of(
                registry.fallback().to_string(),
                SiteSettingsSource::Fallback,
                0,
                registry,
            )),
        }
    }

    fn theme_view_of(
        &self,
        slug: String,
        source: SiteSettingsSource,
        version: i64,
        registry: &ThemeRegistry,
    ) -> ThemeSettingsView {
        ThemeSettingsView {
            effective_slug: if registry.contains(&slug) {
                slug.clone()
            } else {
                registry.fallback().to_string()
            },
            slug,
            source,
            version,
            available: registry.options(),
        }
    }

    /// site 分组管理视图（读取也要求 `settings.manage`，见模块说明）。
    pub async fn site_view(&self, actor: &Actor) -> Result<SiteSettingsView, UseCaseError> {
        if !actor.has_permission("settings.manage") {
            return Err(UseCaseError::Forbidden);
        }
        self.load_view().await
    }

    /// 保存 site 分组：规范化校验 → 版本前提 → 条件写入。
    ///
    /// 与已存储内容完全一致的保存幂等返回，不写库、不递增版本；
    /// 行不存在时（版本 0）总是插入，即使值恰好等于装配回退值——
    /// 保存动作的意图就是「让数据库接管该配置」，之后环境变量不再生效。
    pub async fn save_site(
        &self,
        actor: &Actor,
        cmd: SaveSiteSettingsCmd,
    ) -> Result<SiteSettingsView, UseCaseError> {
        actor.ensure_write_channel()?;
        if !actor.has_permission("settings.manage") {
            return Err(UseCaseError::Forbidden);
        }
        let (title, description, logo_media_id) =
            SiteSettings::new(cmd.title, cmd.description, cmd.logo_media_id)
                .map_err(|e| UseCaseError::Invalid(e.to_string()))?
                .into_parts();
        let value = SiteSettingsValue {
            title: Some(title),
            description: Some(description),
            logo_media_id,
        };

        let current = self.store.find_site().await?;
        let current_version = current.as_ref().map_or(0, |record| record.version);
        let expected = checked_version(current_version, cmd.expected_version)?;

        // 幂等：存储内容与本次提交一致（含 null→非 null 的规范化差异）时
        // 直接返回当前视图；版本前提已校验，不掩盖中间发生的并发修改。
        if let Some(record) = &current
            && record.value.title == value.title
            && record.value.description == value.description
            && record.value.logo_media_id == value.logo_media_id
        {
            return Ok(self.view_of(record.value.clone(), record.version));
        }

        // logo 只有**换成新资产**时才过归属校验：站点 logo 引用是无条件
        // 公开来源，不得把他人私有图片经 settings 变成公开图片。
        if let Some(logo_media_id) = logo_media_id
            && !current
                .as_ref()
                .is_some_and(|record| record.value.logo_media_id == Some(logo_media_id))
        {
            crate::media::ensure_attachable(&*self.media_guard, actor, logo_media_id).await?;
        }

        match self
            .store
            .save_site(&value, expected, self.clock.now())
            .await?
        {
            SaveOutcome::Saved { new_version } => Ok(self.view_of(value, new_version)),
            // settings 行没有删除入口：Gone 理论上不可达，按同一冲突语义处理。
            SaveOutcome::StaleConflict | SaveOutcome::Gone => Err(UseCaseError::VersionConflict),
        }
    }

    async fn load_view(&self) -> Result<SiteSettingsView, UseCaseError> {
        match self.store.find_site().await? {
            Some(record) => Ok(self.view_of(record.value, record.version)),
            None => Ok(SiteSettingsView {
                title: self.fallback.title.clone(),
                description: self.fallback.description.clone(),
                logo_media_id: None,
                logo_url: None,
                source: SiteSettingsSource::Fallback,
                version: 0,
            }),
        }
    }

    fn view_of(&self, value: SiteSettingsValue, version: i64) -> SiteSettingsView {
        let info = effective_site(&value, &self.fallback);
        SiteSettingsView {
            title: info.title,
            description: info.description,
            logo_media_id: value.logo_media_id,
            logo_url: info.logo_url,
            source: SiteSettingsSource::Database,
            version,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fallback() -> SiteInfo {
        SiteInfo {
            title: "环境变量标题".into(),
            description: "环境变量描述".into(),
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
        // 没有 logo 时不输出地址（公开页不会渲染一个必然 404 的 img）。
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
            assert_eq!(info.title, "环境变量标题", "title={title:?} 应回退");
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
