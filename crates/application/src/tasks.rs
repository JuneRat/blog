//! 持久任务的白名单契约与管理授权；执行器、数据库租约及调度属于适配器。
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime, UtcOffset, format_description::well_known::Rfc3339};
use uuid::Uuid;

use crate::{
    UseCaseError,
    audit::AuditContext,
    html_rebuild::{RebuildCounts, RebuildReport},
    identity::Actor,
    ports::Clock,
    publishing::PublicationResult,
    retention::RetentionResult,
};

macro_rules! task_enum {
    ($name:ident { $($variant:ident => $value:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($variant),+ }
        impl $name {
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $value),+ }
            }
            pub fn from_stored(value: &str) -> Result<Self, UseCaseError> {
                match value {
                    $($value => Ok(Self::$variant)),+,
                    _ => Err(UseCaseError::DataCorrupt("任务状态或类型无效".into())),
                }
            }
        }
    };
}

task_enum!(TaskKind { HtmlRebuild => "html_rebuild", Retention => "retention", PublishDue => "publish_due" });
task_enum!(TaskStatus { Queued => "queued", Running => "running", Completed => "completed", Failed => "failed", Interrupted => "interrupted", Cancelled => "cancelled" });
task_enum!(TaskTrigger { Manual => "manual", Once => "once", Periodic => "periodic", Retry => "retry" });

impl TaskStatus {
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Queued | Self::Running)
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct TaskReport {
    pub html: Option<RebuildReport>,
    pub retention: Option<RetentionResult>,
    pub publication: Option<PublicationResult>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRun {
    pub id: Uuid,
    pub kind: TaskKind,
    pub status: TaskStatus,
    pub trigger: TaskTrigger,
    pub run_at: OffsetDateTime,
    pub created_at: OffsetDateTime,
    pub started_at: Option<OffsetDateTime>,
    pub finished_at: Option<OffsetDateTime>,
    pub retry_of: Option<Uuid>,
    pub report: TaskReport,
    pub can_retry: bool,
    pub can_cancel: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSchedule {
    pub kind: TaskKind,
    pub enabled: bool,
    pub interval_seconds: i64,
    pub next_run_at: Option<OffsetDateTime>,
    pub version: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskRunPage {
    pub items: Vec<TaskRun>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskView {
    pub available: bool,
    pub retention_available: bool,
    pub pending_html: Option<RebuildCounts>,
    pub schedules: Vec<TaskSchedule>,
    pub latest: Vec<TaskRun>,
    pub runs: TaskRunPage,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskListQuery {
    pub kind: Option<TaskKind>,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct TaskFilter {
    pub kind: Option<TaskKind>,
    pub before: Option<(OffsetDateTime, Uuid)>,
    pub limit: u32,
}

impl TryFrom<TaskListQuery> for TaskFilter {
    type Error = UseCaseError;
    fn try_from(query: TaskListQuery) -> Result<Self, Self::Error> {
        let limit = query.limit.unwrap_or(20);
        if !(1..=100).contains(&limit) {
            return Err(UseCaseError::Invalid("任务列表数量须为 1–100".into()));
        }
        let before = query
            .cursor
            .map(|cursor| {
                let invalid = || UseCaseError::Invalid("任务列表游标无效".into());
                if cursor.len() > 100 {
                    return Err(invalid());
                }
                let (timestamp, id) = cursor.split_once('|').ok_or_else(invalid)?;
                Ok((
                    parse_time(timestamp)?,
                    Uuid::parse_str(id).map_err(|_| invalid())?,
                ))
            })
            .transpose()?;
        Ok(Self {
            kind: query.kind,
            before,
            limit,
        })
    }
}

pub fn parse_time(value: &str) -> Result<OffsetDateTime, UseCaseError> {
    OffsetDateTime::parse(value, &Rfc3339)
        .map(|time| time.to_offset(UtcOffset::UTC))
        .map_err(|_| UseCaseError::Invalid("时间须为含时区的 ISO 8601 格式".into()))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskStartInput {
    pub kind: TaskKind,
    pub run_at: Option<String>,
}

impl TaskStartInput {
    pub fn resolve_run_at(&self, now: OffsetDateTime) -> Result<OffsetDateTime, UseCaseError> {
        let Some(value) = &self.run_at else {
            return Ok(now.to_offset(UtcOffset::UTC));
        };
        if self.kind != TaskKind::HtmlRebuild {
            return Err(UseCaseError::Invalid("此任务仅支持立即执行".into()));
        }
        let run_at = parse_time(value)?;
        if run_at <= now || run_at > now + Duration::days(365) {
            return Err(UseCaseError::Invalid("计划时间须在未来一年内".into()));
        }
        Ok(run_at)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskScheduleInput {
    pub enabled: bool,
    pub interval_seconds: i64,
    pub next_run_at: Option<String>,
    pub version: i64,
}

impl TaskScheduleInput {
    pub fn resolve_next_run_at(
        &self,
        now: OffsetDateTime,
    ) -> Result<Option<OffsetDateTime>, UseCaseError> {
        if !(3600..=2_592_000).contains(&self.interval_seconds) || self.version < 0 {
            return Err(UseCaseError::Invalid(
                "周期须为 1 小时至 30 天，版本不能为负数".into(),
            ));
        }
        if !self.enabled {
            return Ok(None);
        }
        let next = match &self.next_run_at {
            Some(value) => parse_time(value)?,
            None => now + Duration::seconds(self.interval_seconds),
        };
        if next <= now {
            return Err(UseCaseError::Invalid("下次执行时间须在未来".into()));
        }
        Ok(Some(next.to_offset(UtcOffset::UTC)))
    }
}

#[derive(Debug, Clone)]
pub struct TaskLease {
    pub run: TaskRun,
    pub token: Uuid,
    pub audit: AuditContext,
}

#[async_trait]
pub trait TaskStore: Send + Sync {
    async fn list(&self, filter: TaskFilter) -> Result<TaskRunPage, UseCaseError>;
    async fn get(&self, id: Uuid) -> Result<Option<TaskRun>, UseCaseError>;
    async fn latest(&self) -> Result<Vec<TaskRun>, UseCaseError>;
    async fn enqueue(
        &self,
        kind: TaskKind,
        run_at: OffsetDateTime,
        trigger: TaskTrigger,
        retry_of: Option<Uuid>,
        audit: AuditContext,
    ) -> Result<TaskRun, UseCaseError>;
    async fn retry(&self, id: Uuid, audit: AuditContext) -> Result<TaskRun, UseCaseError>;
    async fn cancel(&self, id: Uuid, audit: AuditContext) -> Result<TaskRun, UseCaseError>;
    async fn schedules(&self) -> Result<Vec<TaskSchedule>, UseCaseError>;
    async fn save_retention_schedule(
        &self,
        input: TaskScheduleInput,
        audit: AuditContext,
    ) -> Result<TaskSchedule, UseCaseError>;
}

#[async_trait]
pub trait TaskExecutionStore: Send + Sync {
    async fn seed_schedules(&self) -> Result<(), UseCaseError>;
    async fn tick_schedules(&self) -> Result<(), UseCaseError>;
    async fn claim(
        &self,
        allowed: &[TaskKind],
        worker_id: Uuid,
        ttl_secs: i64,
    ) -> Result<Option<TaskLease>, UseCaseError>;
    async fn renew(&self, lease: &TaskLease, ttl_secs: i64) -> Result<bool, UseCaseError>;
    async fn progress(&self, lease: &TaskLease, report: &TaskReport) -> Result<bool, UseCaseError>;
    async fn finish(
        &self,
        lease: &TaskLease,
        status: TaskStatus,
        report: &TaskReport,
    ) -> Result<bool, UseCaseError>;
    async fn recover_expired(&self) -> Result<u64, UseCaseError>;
}

#[async_trait]
pub trait TaskAdmin: Send + Sync {
    async fn view(&self, query: TaskListQuery) -> Result<TaskView, UseCaseError>;
    async fn enqueue(
        &self,
        input: TaskStartInput,
        audit: AuditContext,
    ) -> Result<TaskRun, UseCaseError>;
    async fn retry(&self, id: Uuid, audit: AuditContext) -> Result<TaskRun, UseCaseError>;
    async fn cancel(&self, id: Uuid, audit: AuditContext) -> Result<TaskRun, UseCaseError>;
    async fn save_retention_schedule(
        &self,
        input: TaskScheduleInput,
        audit: AuditContext,
    ) -> Result<TaskSchedule, UseCaseError>;
}

pub struct TasksInteractor {
    admin: Arc<dyn TaskAdmin>,
    clock: Arc<dyn Clock>,
}

impl TasksInteractor {
    pub fn new(admin: Arc<dyn TaskAdmin>, clock: Arc<dyn Clock>) -> Self {
        Self { admin, clock }
    }
    pub async fn view(
        &self,
        actor: &Actor,
        query: TaskListQuery,
    ) -> Result<TaskView, UseCaseError> {
        authorize(actor)?;
        TaskFilter::try_from(query.clone())?;
        self.admin.view(query).await
    }
    pub async fn enqueue(
        &self,
        actor: &Actor,
        input: TaskStartInput,
    ) -> Result<TaskRun, UseCaseError> {
        authorize_write(actor)?;
        input.resolve_run_at(self.clock.now())?;
        self.admin.enqueue(input, actor.audit_context()).await
    }
    pub async fn retry(&self, actor: &Actor, id: Uuid) -> Result<TaskRun, UseCaseError> {
        authorize_write(actor)?;
        self.admin.retry(id, actor.audit_context()).await
    }
    pub async fn cancel(&self, actor: &Actor, id: Uuid) -> Result<TaskRun, UseCaseError> {
        authorize_write(actor)?;
        self.admin.cancel(id, actor.audit_context()).await
    }
    pub async fn save_retention_schedule(
        &self,
        actor: &Actor,
        input: TaskScheduleInput,
    ) -> Result<TaskSchedule, UseCaseError> {
        authorize_write(actor)?;
        input.resolve_next_run_at(self.clock.now())?;
        self.admin
            .save_retention_schedule(input, actor.audit_context())
            .await
    }
}

fn authorize(actor: &Actor) -> Result<(), UseCaseError> {
    if !actor.has_permission("settings.manage") {
        return Err(UseCaseError::Forbidden);
    }
    Ok(())
}
fn authorize_write(actor: &Actor) -> Result<(), UseCaseError> {
    authorize(actor)?;
    actor.ensure_write_channel()
}
