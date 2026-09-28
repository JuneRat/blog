//! 站点与主题选择设置端口。

use async_trait::async_trait;
use time::OffsetDateTime;
use uuid::Uuid;

use super::runtime::SaveOutcome;
use crate::error::UseCaseError;

/// settings.site 分组的存储形态。
///
/// 字段 Option 化以容忍**历史/手工写入的不完整行**：缺字段按「该字段未配置」
/// 处理，读取时逐字段回退到装配值（见 `application::site_info::effective_site`）。
/// 写入路径（`SettingsInteractor::save_site`）只产生完整对象。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SiteSettingsValue {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_zone: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// 站点 logo 的媒体资产 id；按用户确认的取舍存在 JSONB 值里。
    /// 读取侧对缺失/失效 id 按「无 logo」处理；写入侧由引用表校验资产可用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logo_media_id: Option<Uuid>,
}

/// site 分组当前行：值 + 并发版本；行不存在时整体为 None（版本视为 0）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteSettingsRecord {
    pub value: SiteSettingsValue,
    pub version: i64,
}

/// settings 的 site 分组存储端口。
///
/// 分组隔离：本端口只读写 key='site'，oauth 等受保护分组各有自己的存储端口
/// 与授权（docs/database-design.md §6），settings.manage 不能借道这里触达。
#[async_trait]
pub trait SettingsStore: Send + Sync {
    /// 读取 site 分组；未配置返回 None。
    async fn find_site(&self) -> Result<Option<SiteSettingsRecord>, UseCaseError>;

    /// 条件保存（UPSERT + CAS）：
    /// - 行不存在且 `expected_version = 0`：插入，版本从 1 起；
    /// - 行存在且版本匹配：整体替换值并 version+1；
    /// - 其余情形（版本不匹配，含并发插入/更新先落地）：StaleConflict。
    ///
    /// settings 行没有删除入口，因此不产生 Gone；调用方把 StaleConflict
    /// 翻译为可基于最新版本重试的版本冲突。
    async fn save_site(
        &self,
        value: &SiteSettingsValue,
        expected_version: i64,
        now: OffsetDateTime,
        audit_actor: crate::audit::AuditContext,
    ) -> Result<SaveOutcome, UseCaseError>;
}

/// 与 site/oauth 分组隔离的主题选择设置。主题目录由部署方安装，数据库只保存 slug。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeSettingsRecord {
    pub slug: String,
    pub version: i64,
}

#[async_trait]
pub trait ThemeSettingsStore: Send + Sync {
    async fn find_theme(&self) -> Result<Option<ThemeSettingsRecord>, UseCaseError>;
    async fn save_theme(
        &self,
        slug: &str,
        expected_version: i64,
        now: OffsetDateTime,
        audit_actor: crate::audit::AuditContext,
    ) -> Result<SaveOutcome, UseCaseError>;
}
