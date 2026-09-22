//! 站点设置用例：site 分组的读取视图与条件保存。
//!
//! 权限约定（docs/identity-and-admin.md §2）：
//! - 读取与写入都要求 `settings.manage`：站点设置界面只服务于持有者，
//!   与标签目录不同，Author 编辑文章不需要读站点配置，不开放目录读取；
//! - 本用例**只覆盖 site 分组**：oauth 分组的写入仍走 `oauth.manage`
//!   （受控 CLI / OAuth 用例），不受 settings.manage 覆盖；HTTP 面上也只
//!   注册 `/settings/site` 一个地址，未知分组（含 oauth）一律 404。
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
use crate::ports::{Clock, SaveOutcome, SettingsStore, SiteSettingsValue};
use crate::public_site::SiteInfo;
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
    pub source: SiteSettingsSource,
    /// site 行当前版本；行不存在时为 0（首次保存以 0 为 expected_version）。
    pub version: i64,
}

pub struct SaveSiteSettingsCmd {
    pub title: String,
    pub description: String,
    pub expected_version: Option<i64>,
}

pub struct SettingsInteractor {
    store: Arc<dyn SettingsStore>,
    clock: Arc<dyn Clock>,
    /// 装配回退值：环境变量/内置默认值（server 装配层构造，进程内不变）。
    fallback: SiteInfo,
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
    SiteInfo { title, description }
}

impl SettingsInteractor {
    pub fn new(store: Arc<dyn SettingsStore>, clock: Arc<dyn Clock>, fallback: SiteInfo) -> Self {
        Self {
            store,
            clock,
            fallback,
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
        let (title, description) = SiteSettings::new(cmd.title, cmd.description)
            .map_err(|e| UseCaseError::Invalid(e.to_string()))?
            .into_parts();
        let value = SiteSettingsValue {
            title: Some(title),
            description: Some(description),
        };

        let current = self.store.find_site().await?;
        let current_version = current.as_ref().map_or(0, |record| record.version);
        let expected = checked_version(current_version, cmd.expected_version)?;

        // 幂等：存储内容与本次提交一致（含 null→非 null 的规范化差异）时
        // 直接返回当前视图；版本前提已校验，不掩盖中间发生的并发修改。
        if let Some(record) = &current
            && record.value.title == value.title
            && record.value.description == value.description
        {
            return Ok(self.view_of(record.value.clone(), record.version));
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
        }
    }

    fn stored(title: Option<&str>, description: Option<&str>) -> SiteSettingsValue {
        SiteSettingsValue {
            title: title.map(str::to_string),
            description: description.map(str::to_string),
        }
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
