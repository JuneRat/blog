//! Compatibility adapter for the original maintenance endpoint.
use application::{
    UseCaseError,
    audit::AuditContext,
    html_rebuild::RebuildReport,
    html_rebuild_admin::{HtmlRebuildJob, HtmlRebuildJobStatus, HtmlRebuildJobs, HtmlRebuildView},
    tasks::{TaskAdmin, TaskKind, TaskListQuery, TaskRun, TaskStartInput, TaskStatus},
};
use std::sync::Arc;

pub struct WebsiteHtmlRebuildJobs {
    tasks: Arc<dyn TaskAdmin>,
}
impl WebsiteHtmlRebuildJobs {
    pub fn new(tasks: Arc<dyn TaskAdmin>) -> Self {
        Self { tasks }
    }
}
fn job(run: TaskRun) -> HtmlRebuildJob {
    HtmlRebuildJob {
        id: run.id,
        status: match run.status {
            TaskStatus::Queued | TaskStatus::Running => HtmlRebuildJobStatus::Running,
            TaskStatus::Completed => HtmlRebuildJobStatus::Completed,
            TaskStatus::Failed => HtmlRebuildJobStatus::Failed,
            TaskStatus::Interrupted | TaskStatus::Cancelled => HtmlRebuildJobStatus::Interrupted,
        },
        report: run.report.html.unwrap_or(RebuildReport {
            has_more: true,
            ..Default::default()
        }),
    }
}
#[async_trait::async_trait]
impl HtmlRebuildJobs for WebsiteHtmlRebuildJobs {
    async fn view(&self) -> Result<HtmlRebuildView, UseCaseError> {
        let view = self
            .tasks
            .view(TaskListQuery {
                kind: Some(TaskKind::HtmlRebuild),
                ..Default::default()
            })
            .await?;
        Ok(HtmlRebuildView {
            pending: view.pending_html,
            job: view
                .latest
                .into_iter()
                .find(|run| run.kind == TaskKind::HtmlRebuild)
                .map(job),
            available: view.available,
        })
    }
    async fn start(&self, audit: AuditContext) -> Result<HtmlRebuildJob, UseCaseError> {
        self.tasks
            .enqueue(
                TaskStartInput {
                    kind: TaskKind::HtmlRebuild,
                    run_at: None,
                },
                audit,
            )
            .await
            .map(job)
    }
}
