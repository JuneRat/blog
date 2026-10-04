use application::{html_rebuild as rebuild, html_rebuild_admin as admin};
use uuid::Uuid;

#[derive(serde::Serialize, ts_rs::TS)]
pub struct HtmlRebuildCounts {
    pub posts: u64,
    pub pages: u64,
    pub comments: u64,
}
impl From<rebuild::RebuildCounts> for HtmlRebuildCounts {
    fn from(value: rebuild::RebuildCounts) -> Self {
        Self {
            posts: value.posts,
            pages: value.pages,
            comments: value.comments,
        }
    }
}

#[derive(serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum HtmlRebuildKind {
    Post,
    Page,
    Comment,
}
impl From<rebuild::HtmlKind> for HtmlRebuildKind {
    fn from(value: rebuild::HtmlKind) -> Self {
        match value {
            rebuild::HtmlKind::Post => Self::Post,
            rebuild::HtmlKind::Page => Self::Page,
            rebuild::HtmlKind::Comment => Self::Comment,
        }
    }
}

#[derive(serde::Serialize, ts_rs::TS)]
pub struct HtmlRebuildFailure {
    pub kind: Option<HtmlRebuildKind>,
    pub id: Option<Uuid>,
    pub message: String,
}
impl From<rebuild::RebuildFailure> for HtmlRebuildFailure {
    fn from(value: rebuild::RebuildFailure) -> Self {
        Self {
            kind: value.kind.map(Into::into),
            id: value.id,
            message: value.message,
        }
    }
}

#[derive(serde::Serialize, ts_rs::TS)]
pub struct HtmlRebuildReport {
    pub rebuilt: HtmlRebuildCounts,
    pub skipped: HtmlRebuildCounts,
    pub pending: Option<HtmlRebuildCounts>,
    pub batches: u32,
    pub has_more: bool,
    pub dry_run: bool,
    pub failure: Option<HtmlRebuildFailure>,
}
impl From<rebuild::RebuildReport> for HtmlRebuildReport {
    fn from(value: rebuild::RebuildReport) -> Self {
        Self {
            rebuilt: value.rebuilt.into(),
            skipped: value.skipped.into(),
            pending: value.pending.map(Into::into),
            batches: value.batches,
            has_more: value.has_more,
            dry_run: value.dry_run,
            failure: value.failure.map(Into::into),
        }
    }
}

#[derive(serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum HtmlRebuildJobStatus {
    Running,
    Completed,
    Failed,
    Interrupted,
}
impl From<admin::HtmlRebuildJobStatus> for HtmlRebuildJobStatus {
    fn from(value: admin::HtmlRebuildJobStatus) -> Self {
        match value {
            admin::HtmlRebuildJobStatus::Running => Self::Running,
            admin::HtmlRebuildJobStatus::Completed => Self::Completed,
            admin::HtmlRebuildJobStatus::Failed => Self::Failed,
            admin::HtmlRebuildJobStatus::Interrupted => Self::Interrupted,
        }
    }
}

#[derive(serde::Serialize, ts_rs::TS)]
pub struct HtmlRebuildJob {
    pub id: Uuid,
    pub status: HtmlRebuildJobStatus,
    pub report: HtmlRebuildReport,
}
impl From<admin::HtmlRebuildJob> for HtmlRebuildJob {
    fn from(value: admin::HtmlRebuildJob) -> Self {
        Self {
            id: value.id,
            status: value.status.into(),
            report: value.report.into(),
        }
    }
}

#[derive(serde::Serialize, ts_rs::TS)]
pub struct HtmlRebuildView {
    pub pending: Option<HtmlRebuildCounts>,
    pub job: Option<HtmlRebuildJob>,
    pub available: bool,
}
impl From<admin::HtmlRebuildView> for HtmlRebuildView {
    fn from(value: admin::HtmlRebuildView) -> Self {
        Self {
            pending: value.pending.map(Into::into),
            job: value.job.map(Into::into),
            available: value.available,
        }
    }
}
