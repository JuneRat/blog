use application::{UseCaseError, tasks as task};
use time::{OffsetDateTime, UtcOffset, format_description::well_known::Rfc3339};
use uuid::Uuid;

use super::HtmlRebuildReport;

macro_rules! task_enum {
    ($name:ident { $($variant:ident),+ $(,)? }) => {
        #[derive(serde::Serialize, serde::Deserialize, ts_rs::TS)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($variant),+ }
        impl From<task::$name> for $name {
            fn from(value: task::$name) -> Self { match value { $(task::$name::$variant => Self::$variant),+ } }
        }
        impl From<$name> for task::$name {
            fn from(value: $name) -> Self { match value { $($name::$variant => Self::$variant),+ } }
        }
    };
}
task_enum!(TaskKind {
    HtmlRebuild,
    Retention,
    PublishDue
});
task_enum!(TaskStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Interrupted,
    Cancelled
});
task_enum!(TaskTrigger {
    Manual,
    Once,
    Periodic,
    Retry
});

#[derive(serde::Serialize, ts_rs::TS)]
pub struct TaskRetentionResult {
    pub comment_ips: i64,
    pub audit_logs: i64,
    pub batches: u32,
    pub has_more: bool,
    pub dry_run: bool,
}
impl From<application::retention::RetentionResult> for TaskRetentionResult {
    fn from(value: application::retention::RetentionResult) -> Self {
        Self {
            comment_ips: value.comment_ips,
            audit_logs: value.audit_logs,
            batches: value.batches,
            has_more: value.has_more,
            dry_run: value.dry_run,
        }
    }
}

#[derive(serde::Serialize, ts_rs::TS)]
pub struct TaskPublicationResult {
    pub published: u64,
    pub batches: u32,
    pub has_more: bool,
}
impl From<application::publishing::PublicationResult> for TaskPublicationResult {
    fn from(value: application::publishing::PublicationResult) -> Self {
        Self {
            published: value.published,
            batches: value.batches,
            has_more: value.has_more,
        }
    }
}

#[derive(serde::Serialize, ts_rs::TS)]
pub struct TaskReport {
    pub html: Option<HtmlRebuildReport>,
    pub retention: Option<TaskRetentionResult>,
    pub publication: Option<TaskPublicationResult>,
    pub error: Option<String>,
}
impl From<task::TaskReport> for TaskReport {
    fn from(value: task::TaskReport) -> Self {
        Self {
            html: value.html.map(Into::into),
            retention: value.retention.map(Into::into),
            publication: value.publication.map(Into::into),
            error: value.error,
        }
    }
}

fn timestamp(value: OffsetDateTime) -> Result<String, UseCaseError> {
    value
        .to_offset(UtcOffset::UTC)
        .format(&Rfc3339)
        .map_err(|_| UseCaseError::DataCorrupt("任务时间无效".into()))
}

#[derive(serde::Serialize, ts_rs::TS)]
pub struct TaskRun {
    pub id: Uuid,
    pub kind: TaskKind,
    pub status: TaskStatus,
    pub trigger: TaskTrigger,
    pub run_at: String,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub retry_of: Option<Uuid>,
    pub report: TaskReport,
    pub can_retry: bool,
    pub can_cancel: bool,
}
impl TryFrom<task::TaskRun> for TaskRun {
    type Error = UseCaseError;
    fn try_from(value: task::TaskRun) -> Result<Self, Self::Error> {
        Ok(Self {
            id: value.id,
            kind: value.kind.into(),
            status: value.status.into(),
            trigger: value.trigger.into(),
            run_at: timestamp(value.run_at)?,
            created_at: timestamp(value.created_at)?,
            started_at: value.started_at.map(timestamp).transpose()?,
            finished_at: value.finished_at.map(timestamp).transpose()?,
            retry_of: value.retry_of,
            report: value.report.into(),
            can_retry: value.can_retry,
            can_cancel: value.can_cancel,
        })
    }
}

#[derive(serde::Serialize, ts_rs::TS)]
pub struct TaskSchedule {
    pub kind: TaskKind,
    pub enabled: bool,
    pub interval_seconds: i64,
    pub next_run_at: Option<String>,
    pub version: i64,
}
impl TryFrom<task::TaskSchedule> for TaskSchedule {
    type Error = UseCaseError;
    fn try_from(value: task::TaskSchedule) -> Result<Self, Self::Error> {
        Ok(Self {
            kind: value.kind.into(),
            enabled: value.enabled,
            interval_seconds: value.interval_seconds,
            next_run_at: value.next_run_at.map(timestamp).transpose()?,
            version: value.version,
        })
    }
}

#[derive(serde::Serialize, ts_rs::TS)]
pub struct TaskRunPage {
    pub items: Vec<TaskRun>,
    pub next_cursor: Option<String>,
}
impl TryFrom<task::TaskRunPage> for TaskRunPage {
    type Error = UseCaseError;
    fn try_from(value: task::TaskRunPage) -> Result<Self, Self::Error> {
        Ok(Self {
            items: value
                .items
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<_, _>>()?,
            next_cursor: value.next_cursor,
        })
    }
}

#[derive(serde::Serialize, ts_rs::TS)]
pub struct TaskView {
    pub available: bool,
    pub retention_available: bool,
    pub pending_html: Option<super::HtmlRebuildCounts>,
    pub schedules: Vec<TaskSchedule>,
    pub latest: Vec<TaskRun>,
    pub runs: TaskRunPage,
}
impl TryFrom<task::TaskView> for TaskView {
    type Error = UseCaseError;
    fn try_from(value: task::TaskView) -> Result<Self, Self::Error> {
        Ok(Self {
            available: value.available,
            retention_available: value.retention_available,
            pending_html: value.pending_html.map(Into::into),
            schedules: value
                .schedules
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<_, _>>()?,
            latest: value
                .latest
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<_, _>>()?,
            runs: value.runs.try_into()?,
        })
    }
}

#[derive(serde::Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct TaskStartBody {
    pub kind: TaskKind,
    pub run_at: Option<String>,
}
impl From<TaskStartBody> for task::TaskStartInput {
    fn from(value: TaskStartBody) -> Self {
        Self {
            kind: value.kind.into(),
            run_at: value.run_at,
        }
    }
}

#[derive(serde::Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct TaskScheduleBody {
    pub enabled: bool,
    pub interval_seconds: i64,
    pub next_run_at: Option<String>,
    pub version: i64,
}
impl From<TaskScheduleBody> for task::TaskScheduleInput {
    fn from(value: TaskScheduleBody) -> Self {
        Self {
            enabled: value.enabled,
            interval_seconds: value.interval_seconds,
            next_run_at: value.next_run_at,
            version: value.version,
        }
    }
}
