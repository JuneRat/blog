//! PostgreSQL queue, durable reports and fenced execution of three maintenance kinds.
use application::{
    UseCaseError,
    audit::AuditContext,
    error::ConflictKind,
    tasks::{
        TaskExecutionStore, TaskFilter, TaskKind, TaskLease, TaskReport, TaskRun, TaskRunPage,
        TaskSchedule, TaskScheduleInput, TaskStatus, TaskStore, TaskTrigger,
    },
};
use async_trait::async_trait;
use serde_json::json;
use sqlx::{Postgres, Row, Transaction, postgres::PgRow};
use time::{OffsetDateTime, UtcOffset, format_description::well_known::Rfc3339};
use uuid::Uuid;

const INTERRUPTED: &str = "任务执行已中断；已提交操作保留，请检查后重试";
const COLUMNS: &str = "r.*, (r.status='running' AND r.lease_expires_at<=clock_timestamp()) AS expired,
    NOT EXISTS(SELECT 1 FROM task_runs a WHERE a.kind=r.kind AND a.status IN ('queued','running')) AS no_active";

fn db(error: sqlx::Error) -> UseCaseError {
    UseCaseError::Repository(error.to_string())
}

/// The recovery check also runs inside business transactions, including workers
/// that have already claimed before an isolation marker was applied.
pub async fn guard_writes(tx: &mut Transaction<'_, Postgres>) -> Result<(), UseCaseError> {
    let isolated: bool = sqlx::query_scalar("SELECT COALESCE(shobj_description(oid,'pg_database'),'') LIKE 'blog:recovery-isolated:%' FROM pg_database WHERE datname=current_database()")
        .fetch_one(&mut **tx).await.map_err(db)?;
    if isolated {
        return Err(UseCaseError::Invalid(
            "恢复隔离期间禁止任务写入和执行".into(),
        ));
    }
    Ok(())
}

/// Lock the durable owner row before business locks and keep it until commit.
/// Recovery/finish/claim cannot invalidate or replace ownership during a write.
pub async fn guard_execution(
    tx: &mut Transaction<'_, Postgres>,
    lease: &TaskLease,
) -> Result<(), UseCaseError> {
    guard_writes(tx).await?;
    if !lock_lease(tx, lease).await? {
        return Err(UseCaseError::Invalid("任务租约已失效，执行已停止".into()));
    }
    Ok(())
}

async fn lock_lease(
    tx: &mut Transaction<'_, Postgres>,
    lease: &TaskLease,
) -> Result<bool, UseCaseError> {
    let locked: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM task_runs WHERE id=$1 FOR UPDATE")
            .bind(lease.run.id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(db)?;
    if locked.is_none() {
        return Ok(false);
    }
    // clock_timestamp must be evaluated AFTER any row-lock wait, not as a
    // pre-lock WHERE predicate whose truth can outlive its lease.
    sqlx::query_scalar("SELECT lease_token=$2 AND kind=$3 AND status='running' AND lease_expires_at>clock_timestamp() FROM task_runs WHERE id=$1")
        .bind(lease.run.id).bind(lease.token).bind(lease.run.kind.as_str())
        .fetch_one(&mut **tx).await.map_err(db).map(|valid:Option<bool>|valid.unwrap_or(false))
}

pub async fn supports_retention_execution(
    database: &crate::Database,
) -> Result<bool, UseCaseError> {
    sqlx::query_scalar(
        "SELECT has_table_privilege(current_user,'settings','SELECT')
        AND has_column_privilege(current_user,'comments','id','SELECT')
        AND has_column_privilege(current_user,'comments','created_at','SELECT')
        AND has_column_privilege(current_user,'comments','ip_address','SELECT')
        AND has_column_privilege(current_user,'comments','ip_address','UPDATE')
        AND has_column_privilege(current_user,'audit_logs','id','SELECT')
        AND has_column_privilege(current_user,'audit_logs','created_at','SELECT')
        AND has_table_privilege(current_user,'audit_logs','INSERT')
        AND has_table_privilege(current_user,'audit_logs','DELETE')
        AND has_table_privilege(current_user,'task_runs','SELECT')
        AND has_table_privilege(current_user,'task_runs','UPDATE')",
    )
    .fetch_one(&database.pool)
    .await
    .map_err(db)
}

fn interrupt_report(report: &mut TaskReport) {
    report.error = Some(INTERRUPTED.into());
    if let Some(html) = &mut report.html {
        html.pending = None;
        html.has_more = true;
    }
    if let Some(retention) = &mut report.retention {
        retention.has_more = true;
    }
    if let Some(publication) = &mut report.publication {
        publication.has_more = true;
    }
}

fn run(row: &PgRow) -> Result<TaskRun, UseCaseError> {
    let raw_status = TaskStatus::from_stored(row.try_get("status").map_err(db)?)?;
    let expired: bool = row.try_get("expired").map_err(db)?;
    let no_active: bool = row.try_get("no_active").map_err(db)?;
    let trigger = TaskTrigger::from_stored(row.try_get("trigger").map_err(db)?)?;
    let mut report: TaskReport = serde_json::from_value(row.try_get("report").map_err(db)?)
        .map_err(|_| UseCaseError::DataCorrupt("任务报告格式无效".into()))?;
    let status = if expired {
        interrupt_report(&mut report);
        TaskStatus::Interrupted
    } else {
        raw_status
    };
    Ok(TaskRun {
        id: row.try_get("id").map_err(db)?,
        kind: TaskKind::from_stored(row.try_get("kind").map_err(db)?)?,
        status,
        trigger,
        run_at: row.try_get("run_at").map_err(db)?,
        created_at: row.try_get("created_at").map_err(db)?,
        started_at: row.try_get("started_at").map_err(db)?,
        finished_at: row.try_get("finished_at").map_err(db)?,
        retry_of: row.try_get("retry_of").map_err(db)?,
        report,
        can_retry: matches!(status, TaskStatus::Failed | TaskStatus::Interrupted) && no_active,
        can_cancel: raw_status == TaskStatus::Queued && trigger != TaskTrigger::Periodic,
    })
}

fn report_json(report: &TaskReport) -> Result<serde_json::Value, UseCaseError> {
    serde_json::to_value(report).map_err(|_| UseCaseError::Invalid("任务报告无法保存".into()))
}

async fn fetch(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> Result<TaskRun, UseCaseError> {
    let row = sqlx::query(&format!("SELECT {COLUMNS} FROM task_runs r WHERE r.id=$1"))
        .bind(id)
        .fetch_one(&mut **tx)
        .await
        .map_err(db)?;
    run(&row)
}

async fn audit(
    tx: &mut Transaction<'_, Postgres>,
    context: AuditContext,
    action: &str,
    id: Uuid,
    metadata: serde_json::Value,
) -> Result<(), UseCaseError> {
    crate::audit::record_change(tx, context, action, "task_run", &id.to_string(), metadata).await
}

async fn insert_run(
    tx: &mut Transaction<'_, Postgres>,
    kind: TaskKind,
    run_at: OffsetDateTime,
    trigger: TaskTrigger,
    retry_of: Option<Uuid>,
    context: AuditContext,
    allow_existing: bool,
) -> Result<TaskRun, UseCaseError> {
    let proposed = Uuid::now_v7();
    // RETURNING the conflicting row is atomic even if it completes while an
    // enqueue waits. A follow-up SELECT after DO NOTHING would lose that proof.
    let id: Uuid = sqlx::query_scalar("INSERT INTO task_runs(id,kind,status,trigger,run_at,retry_of,audit_actor_id,audit_ip_address)
        VALUES($1,$2,'queued',$3,$4,$5,$6,$7::text::inet)
        ON CONFLICT(kind) WHERE status IN ('queued','running') DO UPDATE SET kind=EXCLUDED.kind RETURNING id")
        .bind(proposed).bind(kind.as_str()).bind(trigger.as_str()).bind(run_at).bind(retry_of)
        .bind(context.actor_id).bind(context.ip_address.map(|ip| ip.to_string()))
        .fetch_one(&mut **tx).await.map_err(db)?;
    if id != proposed && !allow_existing {
        return Err(UseCaseError::Conflict(ConflictKind::Unknown));
    }
    if id == proposed {
        audit(
            tx,
            context,
            if trigger == TaskTrigger::Retry {
                "task.retry"
            } else {
                "task.enqueue"
            },
            id,
            json!({"kind":kind.as_str(),"trigger":trigger.as_str(),"retry_of":retry_of}),
        )
        .await?;
    }
    fetch(tx, id).await
}

async fn prune(tx: &mut Transaction<'_, Postgres>, kind: TaskKind) -> Result<(), UseCaseError> {
    sqlx::query("DELETE FROM task_runs WHERE id IN (SELECT id FROM task_runs WHERE kind=$1 AND status NOT IN ('queued','running') ORDER BY created_at DESC,id DESC OFFSET 500)")
        .bind(kind.as_str()).execute(&mut **tx).await.map_err(db)?;
    Ok(())
}

fn ttl(ttl_secs: i64) -> Result<(), UseCaseError> {
    if !(1..=3600).contains(&ttl_secs) {
        return Err(UseCaseError::Invalid("任务租约须为 1–3,600 秒".into()));
    }
    Ok(())
}

pub struct PostgresTaskStore {
    database: crate::Database,
}
impl PostgresTaskStore {
    pub fn new(database: crate::Database) -> Self {
        Self { database }
    }
}

#[async_trait]
impl TaskStore for PostgresTaskStore {
    async fn list(&self, filter: TaskFilter) -> Result<TaskRunPage, UseCaseError> {
        if !(1..=100).contains(&filter.limit) {
            return Err(UseCaseError::Invalid("任务列表数量须为 1–100".into()));
        }
        let rows = sqlx::query(&format!("SELECT {COLUMNS} FROM task_runs r WHERE ($1::text IS NULL OR r.kind=$1)
            AND ($2::timestamptz IS NULL OR (r.created_at,r.id)<($2,$3)) ORDER BY r.created_at DESC,r.id DESC LIMIT $4"))
            .bind(filter.kind.map(TaskKind::as_str)).bind(filter.before.map(|before| before.0))
            .bind(filter.before.map(|before| before.1)).bind(i64::from(filter.limit)+1)
            .fetch_all(&self.database.pool).await.map_err(db)?;
        let mut items = rows.iter().map(run).collect::<Result<Vec<_>, _>>()?;
        let more = items.len() > filter.limit as usize;
        items.truncate(filter.limit as usize);
        let next_cursor = if more {
            items
                .last()
                .map(|last| {
                    last.created_at
                        .to_offset(UtcOffset::UTC)
                        .format(&Rfc3339)
                        .map(|timestamp| format!("{timestamp}|{}", last.id))
                        .map_err(|_| UseCaseError::DataCorrupt("任务记录时间无效".into()))
                })
                .transpose()?
        } else {
            None
        };
        Ok(TaskRunPage { items, next_cursor })
    }
    async fn get(&self, id: Uuid) -> Result<Option<TaskRun>, UseCaseError> {
        sqlx::query(&format!("SELECT {COLUMNS} FROM task_runs r WHERE r.id=$1"))
            .bind(id)
            .fetch_optional(&self.database.pool)
            .await
            .map_err(db)?
            .as_ref()
            .map(run)
            .transpose()
    }
    async fn latest(&self) -> Result<Vec<TaskRun>, UseCaseError> {
        let rows = sqlx::query(&format!("SELECT DISTINCT ON(r.kind) {COLUMNS} FROM task_runs r ORDER BY r.kind,r.created_at DESC,r.id DESC"))
            .fetch_all(&self.database.pool).await.map_err(db)?;
        rows.iter().map(run).collect()
    }
    async fn enqueue(
        &self,
        kind: TaskKind,
        run_at: OffsetDateTime,
        trigger: TaskTrigger,
        retry_of: Option<Uuid>,
        context: AuditContext,
    ) -> Result<TaskRun, UseCaseError> {
        let mut tx = self.database.pool.begin().await.map_err(db)?;
        guard_writes(&mut tx).await?;
        let result = insert_run(&mut tx, kind, run_at, trigger, retry_of, context, true).await?;
        tx.commit().await.map_err(db)?;
        Ok(result)
    }
    async fn retry(&self, id: Uuid, context: AuditContext) -> Result<TaskRun, UseCaseError> {
        let mut tx = self.database.pool.begin().await.map_err(db)?;
        guard_writes(&mut tx).await?;
        // Terminal status/kind are immutable. Do not lock an old terminal row
        // before the active row: finish/prune lock in the opposite direction.
        let kind: Option<(String, String)> =
            sqlx::query_as("SELECT kind,status FROM task_runs WHERE id=$1")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(db)?;
        let (kind, status) = kind.ok_or_else(|| UseCaseError::NotFound("任务".into()))?;
        if !matches!(
            TaskStatus::from_stored(&status)?,
            TaskStatus::Failed | TaskStatus::Interrupted
        ) {
            return Err(UseCaseError::Invalid("只有失败或中断的任务可以重试".into()));
        }
        let now = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut *tx)
            .await
            .map_err(db)?;
        let result = insert_run(
            &mut tx,
            TaskKind::from_stored(&kind)?,
            now,
            TaskTrigger::Retry,
            Some(id),
            context,
            false,
        )
        .await?;
        tx.commit().await.map_err(db)?;
        Ok(result)
    }
    async fn cancel(&self, id: Uuid, context: AuditContext) -> Result<TaskRun, UseCaseError> {
        let mut tx = self.database.pool.begin().await.map_err(db)?;
        guard_writes(&mut tx).await?;
        let row: Option<(String, String, String)> =
            sqlx::query_as("SELECT kind,status,trigger FROM task_runs WHERE id=$1 FOR UPDATE")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(db)?;
        let (kind, status, trigger) = row.ok_or_else(|| UseCaseError::NotFound("任务".into()))?;
        if status != "queued" {
            return Err(UseCaseError::Conflict(ConflictKind::Unknown));
        }
        if trigger == "periodic" {
            return Err(UseCaseError::Invalid("只能取消尚未执行的手动计划".into()));
        }
        sqlx::query(
            "UPDATE task_runs SET status='cancelled',finished_at=clock_timestamp() WHERE id=$1",
        )
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        audit(&mut tx, context, "task.cancel", id, json!({"kind":kind})).await?;
        let result = fetch(&mut tx, id).await?;
        prune(&mut tx, TaskKind::from_stored(&kind)?).await?;
        tx.commit().await.map_err(db)?;
        Ok(result)
    }
    async fn schedules(&self) -> Result<Vec<TaskSchedule>, UseCaseError> {
        let rows = sqlx::query("SELECT kind,enabled,interval_seconds,next_run_at,version FROM task_schedules ORDER BY kind")
            .fetch_all(&self.database.pool).await.map_err(db)?;
        let mut schedules = vec![
            TaskSchedule {
                kind: TaskKind::PublishDue,
                enabled: true,
                interval_seconds: 30,
                next_run_at: None,
                version: 0,
            },
            TaskSchedule {
                kind: TaskKind::Retention,
                enabled: false,
                interval_seconds: 86400,
                next_run_at: None,
                version: 0,
            },
        ];
        for row in rows {
            let kind = TaskKind::from_stored(row.try_get("kind").map_err(db)?)?;
            let target = schedules
                .iter_mut()
                .find(|schedule| schedule.kind == kind)
                .ok_or_else(|| UseCaseError::DataCorrupt("周期任务类型无效".into()))?;
            *target = TaskSchedule {
                kind,
                enabled: row.try_get("enabled").map_err(db)?,
                interval_seconds: row.try_get("interval_seconds").map_err(db)?,
                next_run_at: row.try_get("next_run_at").map_err(db)?,
                version: row.try_get("version").map_err(db)?,
            };
        }
        Ok(schedules)
    }
    async fn save_retention_schedule(
        &self,
        input: TaskScheduleInput,
        context: AuditContext,
    ) -> Result<TaskSchedule, UseCaseError> {
        let mut tx = self.database.pool.begin().await.map_err(db)?;
        guard_writes(&mut tx).await?;
        let now: OffsetDateTime = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut *tx)
            .await
            .map_err(db)?;
        let next = input.resolve_next_run_at(now)?;
        let current: Option<i64> = sqlx::query_scalar(
            "SELECT version FROM task_schedules WHERE kind='retention' FOR UPDATE",
        )
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?;
        if current.unwrap_or(0) != input.version {
            return Err(UseCaseError::VersionConflict);
        }
        let version: Option<i64> = sqlx::query_scalar("INSERT INTO task_schedules(kind,enabled,interval_seconds,next_run_at) VALUES('retention',$1,$2,$3)
            ON CONFLICT(kind) DO UPDATE SET enabled=$1,interval_seconds=$2,next_run_at=$3,version=task_schedules.version+1
            WHERE task_schedules.version=$4 AND $4>0 RETURNING version")
            .bind(input.enabled).bind(input.interval_seconds).bind(next).bind(input.version)
            .fetch_optional(&mut *tx).await.map_err(db)?;
        let version = version.ok_or(UseCaseError::VersionConflict)?;
        crate::audit::record_change(&mut tx,context,"task.schedule_retention","task_schedule","retention",
            json!({"enabled":input.enabled,"interval_seconds":input.interval_seconds,"version":version})).await?;
        tx.commit().await.map_err(db)?;
        Ok(TaskSchedule {
            kind: TaskKind::Retention,
            enabled: input.enabled,
            interval_seconds: input.interval_seconds,
            next_run_at: next,
            version,
        })
    }
}

#[async_trait]
impl TaskExecutionStore for PostgresTaskStore {
    async fn seed_schedules(&self) -> Result<(), UseCaseError> {
        let mut tx = self.database.pool.begin().await.map_err(db)?;
        guard_writes(&mut tx).await?;
        sqlx::query("INSERT INTO task_schedules(kind,enabled,interval_seconds,next_run_at) VALUES('retention',false,86400,NULL),('publish_due',true,30,clock_timestamp()) ON CONFLICT(kind) DO NOTHING")
            .execute(&mut *tx).await.map_err(db)?;
        tx.commit().await.map_err(db)
    }
    async fn tick_schedules(&self) -> Result<(), UseCaseError> {
        let mut tx = self.database.pool.begin().await.map_err(db)?;
        guard_writes(&mut tx).await?;
        let due: Vec<(String,i64)> = sqlx::query_as("SELECT kind,interval_seconds FROM task_schedules WHERE enabled AND next_run_at<=clock_timestamp() ORDER BY kind FOR UPDATE SKIP LOCKED")
            .fetch_all(&mut *tx).await.map_err(db)?;
        for (kind, interval) in due {
            let now: OffsetDateTime = sqlx::query_scalar("SELECT clock_timestamp()")
                .fetch_one(&mut *tx)
                .await
                .map_err(db)?;
            insert_run(
                &mut tx,
                TaskKind::from_stored(&kind)?,
                now,
                TaskTrigger::Periodic,
                None,
                AuditContext::system(),
                true,
            )
            .await?;
            sqlx::query("UPDATE task_schedules SET next_run_at=clock_timestamp()+$2::bigint*interval '1 second' WHERE kind=$1")
                .bind(kind).bind(interval).execute(&mut *tx).await.map_err(db)?;
        }
        tx.commit().await.map_err(db)
    }
    async fn claim(
        &self,
        allowed: &[TaskKind],
        worker_id: Uuid,
        ttl_secs: i64,
    ) -> Result<Option<TaskLease>, UseCaseError> {
        ttl(ttl_secs)?;
        let mut tx = self.database.pool.begin().await.map_err(db)?;
        guard_writes(&mut tx).await?;
        let allowed: Vec<&str> = allowed.iter().map(|kind| kind.as_str()).collect();
        let id: Option<Uuid> = sqlx::query_scalar("SELECT id FROM task_runs WHERE status='queued' AND run_at<=clock_timestamp() AND kind=ANY($1) ORDER BY run_at,id LIMIT 1 FOR UPDATE SKIP LOCKED")
            .bind(allowed).fetch_optional(&mut *tx).await.map_err(db)?;
        let Some(id) = id else {
            tx.commit().await.map_err(db)?;
            return Ok(None);
        };
        let token = Uuid::now_v7();
        let context: (Option<Uuid>,Option<String>)=sqlx::query_as("UPDATE task_runs SET status='running',started_at=clock_timestamp(),worker_id=$2,lease_token=$3,lease_expires_at=clock_timestamp()+$4::bigint*interval '1 second' WHERE id=$1 RETURNING audit_actor_id,host(audit_ip_address)")
            .bind(id).bind(worker_id).bind(token).bind(ttl_secs).fetch_one(&mut *tx).await.map_err(db)?;
        let audit_context = AuditContext {
            actor_id: context.0,
            ip_address: context
                .1
                .map(|ip| ip.parse())
                .transpose()
                .map_err(|_| UseCaseError::DataCorrupt("任务来源地址无效".into()))?,
        };
        let run = fetch(&mut tx, id).await?;
        audit(
            &mut tx,
            audit_context,
            "task.claim",
            id,
            json!({"kind":run.kind.as_str()}),
        )
        .await?;
        tx.commit().await.map_err(db)?;
        Ok(Some(TaskLease {
            run,
            token,
            audit: audit_context,
        }))
    }
    async fn renew(&self, lease: &TaskLease, ttl_secs: i64) -> Result<bool, UseCaseError> {
        ttl(ttl_secs)?;
        let mut tx = self.database.pool.begin().await.map_err(db)?;
        guard_writes(&mut tx).await?;
        if !lock_lease(&mut tx, lease).await? {
            tx.commit().await.map_err(db)?;
            return Ok(false);
        }
        let changed=sqlx::query("UPDATE task_runs SET lease_expires_at=clock_timestamp()+$3::bigint*interval '1 second' WHERE id=$1 AND lease_token=$2 AND status='running' AND lease_expires_at>clock_timestamp()")
            .bind(lease.run.id).bind(lease.token).bind(ttl_secs).execute(&mut *tx).await.map_err(db)?.rows_affected()==1;
        tx.commit().await.map_err(db)?;
        Ok(changed)
    }
    async fn progress(&self, lease: &TaskLease, report: &TaskReport) -> Result<bool, UseCaseError> {
        let mut tx = self.database.pool.begin().await.map_err(db)?;
        guard_writes(&mut tx).await?;
        if !lock_lease(&mut tx, lease).await? {
            tx.commit().await.map_err(db)?;
            return Ok(false);
        }
        let changed=sqlx::query("UPDATE task_runs SET report=$3 WHERE id=$1 AND lease_token=$2 AND status='running' AND lease_expires_at>clock_timestamp()")
            .bind(lease.run.id).bind(lease.token).bind(report_json(report)?).execute(&mut *tx).await.map_err(db)?.rows_affected()==1;
        tx.commit().await.map_err(db)?;
        Ok(changed)
    }
    async fn finish(
        &self,
        lease: &TaskLease,
        status: TaskStatus,
        report: &TaskReport,
    ) -> Result<bool, UseCaseError> {
        if !matches!(
            status,
            TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Interrupted
        ) {
            return Err(UseCaseError::Invalid("任务结束状态无效".into()));
        }
        let mut tx = self.database.pool.begin().await.map_err(db)?;
        guard_writes(&mut tx).await?;
        if !lock_lease(&mut tx, lease).await? {
            tx.commit().await.map_err(db)?;
            return Ok(false);
        }
        let changed=sqlx::query("UPDATE task_runs SET status=$3,report=$4,finished_at=clock_timestamp(),worker_id=NULL,lease_token=NULL,lease_expires_at=NULL WHERE id=$1 AND lease_token=$2 AND status='running' AND lease_expires_at>clock_timestamp()")
            .bind(lease.run.id).bind(lease.token).bind(status.as_str()).bind(report_json(report)?).execute(&mut *tx).await.map_err(db)?.rows_affected()==1;
        if changed {
            audit(
                &mut tx,
                lease.audit,
                "task.finish",
                lease.run.id,
                json!({"kind":lease.run.kind.as_str(),"status":status.as_str()}),
            )
            .await?;
            prune(&mut tx, lease.run.kind).await?;
        }
        tx.commit().await.map_err(db)?;
        Ok(changed)
    }
    async fn recover_expired(&self) -> Result<u64, UseCaseError> {
        let mut tx = self.database.pool.begin().await.map_err(db)?;
        guard_writes(&mut tx).await?;
        let rows=sqlx::query("SELECT id,kind,report FROM task_runs WHERE status='running' AND lease_expires_at<=clock_timestamp() ORDER BY kind FOR UPDATE SKIP LOCKED")
            .fetch_all(&mut *tx).await.map_err(db)?;
        for row in &rows {
            let id: Uuid = row.try_get("id").map_err(db)?;
            let kind = TaskKind::from_stored(row.try_get("kind").map_err(db)?)?;
            let mut report: TaskReport = serde_json::from_value(row.try_get("report").map_err(db)?)
                .map_err(|_| UseCaseError::DataCorrupt("任务报告格式无效".into()))?;
            interrupt_report(&mut report);
            sqlx::query("UPDATE task_runs SET status='interrupted',report=$2,finished_at=clock_timestamp(),worker_id=NULL,lease_token=NULL,lease_expires_at=NULL WHERE id=$1")
                .bind(id).bind(report_json(&report)?).execute(&mut *tx).await.map_err(db)?;
            audit(
                &mut tx,
                AuditContext::system(),
                "task.interrupted",
                id,
                json!({"kind":kind.as_str()}),
            )
            .await?;
            prune(&mut tx, kind).await?;
        }
        tx.commit().await.map_err(db)?;
        Ok(rows.len() as u64)
    }
}
