//! 后台 HTML 重建管理契约。运行时、任务生命周期与并发协调由装配适配器负责。

use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;
use uuid::Uuid;

use crate::{
    UseCaseError,
    audit::AuditContext,
    html_rebuild::{RebuildCounts, RebuildReport},
    identity::Actor,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HtmlRebuildJobStatus {
    Running,
    Completed,
    Failed,
    Interrupted,
}

#[derive(Debug, Clone, Serialize)]
pub struct HtmlRebuildJob {
    pub id: Uuid,
    pub status: HtmlRebuildJobStatus,
    pub report: RebuildReport,
}

#[derive(Debug, Clone, Serialize)]
pub struct HtmlRebuildView {
    pub pending: Option<RebuildCounts>,
    pub job: Option<HtmlRebuildJob>,
    pub available: bool,
}

#[async_trait]
pub trait HtmlRebuildJobs: Send + Sync {
    /// 读取当前任务与待处理快照；任务运行时未知的剩余数量用 None 表示。
    async fn view(&self) -> Result<HtmlRebuildView, UseCaseError>;

    /// 适配器负责防止重复并发任务；审计来源只来自可信 Actor。
    async fn start(&self, audit: AuditContext) -> Result<HtmlRebuildJob, UseCaseError>;
}

pub struct HtmlRebuildAdminInteractor {
    jobs: Arc<dyn HtmlRebuildJobs>,
}

impl HtmlRebuildAdminInteractor {
    pub fn new(jobs: Arc<dyn HtmlRebuildJobs>) -> Self {
        Self { jobs }
    }

    pub async fn view(&self, actor: &Actor) -> Result<HtmlRebuildView, UseCaseError> {
        if !actor.has_permission("settings.manage") {
            return Err(UseCaseError::Forbidden);
        }
        self.jobs.view().await
    }

    pub async fn start(&self, actor: &Actor) -> Result<HtmlRebuildJob, UseCaseError> {
        actor.ensure_write_channel()?;
        if !actor.has_permission("settings.manage") {
            return Err(UseCaseError::Forbidden);
        }
        self.jobs.start(actor.audit_context()).await
    }
}
