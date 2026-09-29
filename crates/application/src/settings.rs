//! 站点设置用例：site/theme 分组的读取视图与条件保存。
//!
//! 权限约定（docs/identity-and-admin.md §2）：
//! - 管理视图读取与写入都要求 `settings.manage`：站点设置界面只服务于持有者，
//!   `/me` 与公开评论只通过窄方法 public_time_zone 读取显示时区；
//! - 本用例覆盖 site/theme 分组：oauth 分组的写入仍走 `oauth.manage`
//!   （受控 CLI / OAuth 用例），不受 settings.manage 覆盖；未知分组一律 404。
//!
//! 生效优先级（docs/database-design.md §6）：数据库 site 行 > 装配回退值
//! （内置默认值）。
//! 行内**缺字段**（历史/手工写入）时按字段回退；公开渲染在存储读取失败时
//! 整体回退到装配值，配置问题不拖垮公开页面。
//!
//! 并发约定：保存携带 expected_version（未配置行为 0），条件写入不自动覆盖；
//! 与提交内容完全一致的保存是幂等的，不递增版本。

use std::sync::Arc;

use crate::error::UseCaseError;
use crate::identity::Actor;
use crate::ports::{
    Clock, SaveOutcome, SettingsStore, SiteSettingsValue, ThemeSettingsStore, TimeZoneProvider,
};
use crate::site_info::{SiteInfo, effective_site};
use crate::themes::{ThemeOption, ThemeRegistry};
use crate::version::checked_version;
use domain::settings::SiteSettings;

/// 当前生效值的来源（管理视图展示用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SiteSettingsSource {
    /// 来自 settings.site 行。
    Database,
    /// 行未配置：内置默认值。
    Fallback,
}

/// site 分组的管理视图：生效值 + 来源 + 并发版本。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SiteSettingsView {
    pub home_page_size: i64,
    pub navigation: Vec<crate::navigation::NavigationItem>,
    pub time_zone: String,
    pub time_zones: Vec<String>,
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
    pub home_page_size: Option<i64>,
    pub navigation: Option<Vec<crate::navigation::NavigationItem>>,
    /// 旧客户端缺省时保留现有时区，避免标题编辑意外重置预约时区。
    pub time_zone: Option<String>,
    pub title: String,
    pub description: String,
    /// 站点 logo 的媒体资产 id；None = 无 logo（PUT 是整组替换）。
    pub logo_media_id: Option<uuid::Uuid>,
    pub expected_version: Option<i64>,
}

pub struct SettingsInteractor {
    time_zones: Arc<dyn TimeZoneProvider>,
    store: Arc<dyn SettingsStore>,
    clock: Arc<dyn Clock>,
    /// 装配回退值：内置默认值（server 装配层构造，进程内不变）。
    fallback: SiteInfo,
    themes: Option<(Arc<dyn ThemeSettingsStore>, Arc<ThemeRegistry>)>,
    /// 站点 logo 附着的可用性校验（`ensure_attachable`）。
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

impl SettingsInteractor {
    pub fn new(
        store: Arc<dyn SettingsStore>,
        clock: Arc<dyn Clock>,
        fallback: SiteInfo,
        media_guard: Arc<dyn crate::ports::MediaRefGuard>,
    ) -> Self {
        Self {
            time_zones: Arc::new(crate::public_site::UtcTimeZones),
            store,
            clock,
            fallback,
            themes: None,
            media_guard,
        }
    }

    pub fn with_time_zones(mut self, time_zones: Arc<dyn TimeZoneProvider>) -> Self {
        self.time_zones = time_zones;
        self
    }

    /// 公开展示信息，不需要 settings.manage；存储失败必须上报，避免错误预约。
    pub async fn public_time_zone(&self) -> Result<String, UseCaseError> {
        Ok(match self.store.find_site().await? {
            Some(record) => effective_site(&record.value, &self.fallback).time_zone,
            None => self.fallback.time_zone.clone(),
        })
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
            .save_theme(&cmd.slug, expected, self.clock.now(), actor.audit_context())
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
    /// 保存动作的意图就是「让数据库接管该配置」，之后直接使用保存值。
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
        let current = self.store.find_site().await?;
        let time_zone = match cmd.time_zone {
            Some(name) => {
                let name = name.trim().to_owned();
                self.time_zones.resolve(&name)?;
                Some(name)
            }
            None => current
                .as_ref()
                .and_then(|record| record.value.time_zone.clone()),
        };
        let navigation =
            crate::navigation::validate_navigation(cmd.navigation.unwrap_or_else(|| {
                current
                    .as_ref()
                    .map(|record| record.value.navigation.clone())
                    .unwrap_or_default()
            }))?;
        let home_page_size = cmd
            .home_page_size
            .or_else(|| {
                current
                    .as_ref()
                    .and_then(|record| record.value.home_page_size)
            })
            .map(crate::site_info::validate_home_page_size)
            .transpose()?;
        let value = SiteSettingsValue {
            home_page_size,
            navigation,
            time_zone,
            title: Some(title),
            description: Some(description),
            logo_media_id,
        };

        let current_version = current.as_ref().map_or(0, |record| record.version);
        let expected = checked_version(current_version, cmd.expected_version)?;

        // 幂等：存储内容与本次提交一致（含 null→非 null 的规范化差异）时
        // 直接返回当前视图；版本前提已校验，不掩盖中间发生的并发修改。
        if let Some(record) = &current
            && record.value.title == value.title
            && record.value.description == value.description
            && record.value.logo_media_id == value.logo_media_id
            && record.value.time_zone == value.time_zone
            && record.value.navigation == value.navigation
            && record.value.home_page_size == value.home_page_size
        {
            return Ok(self.view_of(record.value.clone(), record.version));
        }

        // 更换 logo 时要求媒体可用；同一来源保留历史引用。
        if let Some(logo_media_id) = logo_media_id
            && !current
                .as_ref()
                .is_some_and(|record| record.value.logo_media_id == Some(logo_media_id))
        {
            crate::media::ensure_attachable(&*self.media_guard, logo_media_id).await?;
        }

        match self
            .store
            .save_site(&value, expected, self.clock.now(), actor.audit_context())
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
                home_page_size: self.fallback.home_page_size,
                navigation: vec![],
                time_zone: self.fallback.time_zone.clone(),
                time_zones: self.time_zones.names(),
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
            home_page_size: info.home_page_size,
            navigation: value.navigation,
            time_zone: info.time_zone,
            time_zones: self.time_zones.names(),
            title: info.title,
            description: info.description,
            logo_media_id: value.logo_media_id,
            logo_url: info.logo_url,
            source: SiteSettingsSource::Database,
            version,
        }
    }
}
