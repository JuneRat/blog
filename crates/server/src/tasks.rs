//! Persistent task supervision belongs to HTTP assembly, outside listener tasks.
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use application::{
    UseCaseError,
    audit::AuditContext,
    html_rebuild::{
        HtmlRebuildInteractor, HtmlRebuildStore, RebuildObserver, RebuildOptions, RebuildReport,
    },
    publishing::{PublicationObserver, PublicationResult, PublishDueInteractor},
    retention::{RetentionMaintenance, RetentionObserver, RetentionResult},
    tasks::{
        TaskAdmin, TaskExecutionStore, TaskFilter, TaskKind, TaskLease, TaskListQuery, TaskReport,
        TaskRun, TaskSchedule, TaskScheduleInput, TaskStartInput, TaskStatus, TaskStore,
        TaskTrigger, TaskView,
    },
};
use infrastructure::{
    Database, PostgresHtmlRebuildStore, RenderingRuntime, tasks::PostgresTaskStore,
};
use interfaces::observability::{TaskHealth, TaskSchedulerCheck};
use tokio::{sync::watch, task::JoinHandle, time::Instant};
use uuid::Uuid;

const FAILURE_MESSAGE: &str = "任务执行失败，请检查服务日志，修复原因后重试";
const LEASE_SECONDS: i64 = 60;

pub struct TaskRuntime {
    pub pool: Database,
    maintenance: Option<Database>,
    pub store: Arc<PostgresTaskStore>,
    rendering: Arc<RenderingRuntime>,
    telemetry: interfaces::observability::Telemetry,
    recovery_mode: bool,
    publication_log: Mutex<crate::logging::PublicationLog>,
}

impl TaskRuntime {
    pub fn new(
        pool: Database,
        maintenance: Option<Database>,
        rendering: Arc<RenderingRuntime>,
        telemetry: interfaces::observability::Telemetry,
        recovery_mode: bool,
    ) -> Self {
        Self {
            store: Arc::new(PostgresTaskStore::new(pool.clone())),
            pool,
            maintenance,
            rendering,
            telemetry,
            recovery_mode,
            publication_log: Mutex::new(Default::default()),
        }
    }
    async fn writable(&self) -> Result<bool, UseCaseError> {
        Ok(!self.recovery_mode
            && !self
                .pool
                .is_recovery_isolated()
                .await
                .map_err(|e| UseCaseError::Repository(e.to_string()))?)
    }
    async fn retention_available(&self) -> bool {
        let Some(pool) = &self.maintenance else {
            return false;
        };
        match infrastructure::tasks::supports_retention_execution(pool).await {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(%error, "清理任务数据库能力检查失败");
                false
            }
        }
    }

    async fn sample_health(&self) -> Result<(), UseCaseError> {
        let snapshot = self.store.health_snapshot().await?;
        for kind in snapshot.kinds {
            self.telemetry.task_health(TaskHealth {
                kind: kind.kind,
                queued: kind.queued,
                running: kind.running,
                expired: kind.expired,
                due_wait_seconds: kind.due_wait_seconds,
                last_success_timestamp: kind.last_success_timestamp,
                consecutive_failures: kind.consecutive_failures,
                schedule_enabled: kind.schedule_enabled,
                schedule_interval_seconds: kind.schedule_interval_seconds,
                schedule_next_run_timestamp: kind.schedule_next_run_timestamp,
            });
        }
        self.telemetry
            .task_health_snapshot_success(snapshot.observed_at);
        Ok(())
    }

    async fn recover_expired(&self) -> Result<(), UseCaseError> {
        for (kind, count) in self.store.recover_expired_by_kind().await? {
            self.telemetry.task_lease_expirations(kind, count);
        }
        Ok(())
    }
}

pub struct WebsiteTasks {
    runtime: Arc<TaskRuntime>,
    supervisor: Arc<TaskSupervisor>,
}
impl WebsiteTasks {
    pub fn new(runtime: Arc<TaskRuntime>, supervisor: Arc<TaskSupervisor>) -> Self {
        Self {
            runtime,
            supervisor,
        }
    }
    async fn require_available(&self, kind: Option<TaskKind>) -> Result<(), UseCaseError> {
        if !self.supervisor.is_open() || !self.runtime.writable().await? {
            return Err(UseCaseError::Invalid("当前环境不能执行后台任务".into()));
        }
        if kind == Some(TaskKind::Retention) && !self.runtime.retention_available().await {
            return Err(UseCaseError::Invalid(
                "当前数据库权限不能执行清理任务".into(),
            ));
        }
        Ok(())
    }
}
#[async_trait::async_trait]
impl TaskAdmin for WebsiteTasks {
    async fn view(&self, query: TaskListQuery) -> Result<TaskView, UseCaseError> {
        let filter = TaskFilter::try_from(query)?;
        let available = self.supervisor.is_open() && self.runtime.writable().await?;
        let (schedules, latest, runs) = tokio::try_join!(
            self.runtime.store.schedules(),
            self.runtime.store.latest(),
            self.runtime.store.list(filter)
        )?;
        let running = latest
            .iter()
            .any(|run| run.kind == TaskKind::HtmlRebuild && run.status == TaskStatus::Running);
        let pending_html = if running {
            None
        } else {
            Some(
                PostgresHtmlRebuildStore::new(
                    self.runtime.pool.clone(),
                    self.runtime.rendering.clone(),
                    self.runtime.rendering.clone(),
                )
                .pending()
                .await?,
            )
        };
        Ok(TaskView {
            available,
            retention_available: available && self.runtime.retention_available().await,
            pending_html,
            schedules,
            latest,
            runs,
        })
    }
    async fn enqueue(
        &self,
        input: TaskStartInput,
        audit: AuditContext,
    ) -> Result<TaskRun, UseCaseError> {
        self.require_available(Some(input.kind)).await?;
        let run_at = input.resolve_run_at(time::OffsetDateTime::now_utc())?;
        let trigger = if input.run_at.is_some() {
            TaskTrigger::Once
        } else {
            TaskTrigger::Manual
        };
        self.runtime
            .store
            .enqueue(input.kind, run_at, trigger, None, audit)
            .await
    }
    async fn retry(&self, id: Uuid, audit: AuditContext) -> Result<TaskRun, UseCaseError> {
        let run = self
            .runtime
            .store
            .get(id)
            .await?
            .ok_or_else(|| UseCaseError::NotFound("任务".into()))?;
        self.require_available(Some(run.kind)).await?;
        self.runtime.store.retry(id, audit).await
    }
    async fn cancel(&self, id: Uuid, audit: AuditContext) -> Result<TaskRun, UseCaseError> {
        self.require_available(None).await?;
        let run = self.runtime.store.cancel(id, audit).await?;
        self.runtime
            .telemetry
            .task_finished(run.kind, TaskStatus::Cancelled, None);
        Ok(run)
    }
    async fn save_retention_schedule(
        &self,
        input: TaskScheduleInput,
        audit: AuditContext,
    ) -> Result<TaskSchedule, UseCaseError> {
        self.require_available(input.enabled.then_some(TaskKind::Retention))
            .await?;
        self.runtime
            .store
            .save_retention_schedule(input, audit)
            .await
    }
}

#[derive(Default)]
struct Lifecycle {
    closed: bool,
    handle: Option<JoinHandle<()>>,
}
pub struct TaskSupervisor {
    lifecycle: Mutex<Lifecycle>,
    runtime: watch::Sender<Option<Arc<TaskRuntime>>>,
    stopping: watch::Sender<bool>,
}
impl Default for TaskSupervisor {
    fn default() -> Self {
        Self {
            lifecycle: Mutex::new(Default::default()),
            runtime: watch::channel(None).0,
            stopping: watch::channel(false).0,
        }
    }
}
impl TaskSupervisor {
    pub fn activate(&self, runtime: Arc<TaskRuntime>) {
        let state = self.lifecycle.lock().expect("task lifecycle lock");
        if !state.closed {
            self.runtime.send_replace(Some(runtime));
        }
    }
    pub fn start(&self) {
        let mut state = self.lifecycle.lock().expect("task lifecycle lock");
        if !state.closed && state.handle.is_none() {
            state.handle = Some(tokio::spawn(supervise(
                self.runtime.subscribe(),
                self.stopping.subscribe(),
            )));
        }
    }
    pub fn is_open(&self) -> bool {
        let state = self.lifecycle.lock().expect("task lifecycle lock");
        !state.closed && !state.handle.as_ref().is_some_and(JoinHandle::is_finished)
    }
    pub fn close(&self) {
        self.lifecycle.lock().expect("task lifecycle lock").closed = true;
        self.stopping.send_replace(true);
        if let Some(runtime) = self.runtime.borrow().as_ref() {
            runtime.telemetry.task_scheduler_stopped();
        }
    }
    pub async fn shutdown(&self, deadline: Instant) {
        self.close();
        let worker = self
            .lifecycle
            .lock()
            .expect("task lifecycle lock")
            .handle
            .take();
        if let Some(mut worker) = worker
            && tokio::time::timeout_at(deadline, &mut worker)
                .await
                .is_err()
        {
            worker.abort();
            let _ = worker.await;
        }
    }
    pub async fn close_maintenance_pool(&self) {
        let runtime = self.runtime.send_replace(None);
        if let Some(runtime) = runtime
            && let Some(pool) = &runtime.maintenance
        {
            pool.close().await;
        }
    }
}

struct ManagedWorker {
    lease: TaskLease,
    runtime: Arc<TaskRuntime>,
    progress: watch::Receiver<TaskReport>,
    handle: JoinHandle<()>,
    started: std::time::Instant,
}
impl Drop for ManagedWorker {
    fn drop(&mut self) {
        self.handle.abort();
    }
}
async fn stopped(receiver: &mut watch::Receiver<bool>) {
    let _ = receiver.wait_for(|stop| *stop).await;
}
struct BusinessWorker(JoinHandle<(TaskStatus, TaskReport)>);
impl Drop for BusinessWorker {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn supervise(
    runtime: watch::Receiver<Option<Arc<TaskRuntime>>>,
    mut stopping: watch::Receiver<bool>,
) {
    let mut workers = HashMap::<&'static str, ManagedWorker>::new();
    let worker_id = Uuid::now_v7();
    let mut seeded = false;
    let mut health_sample_due = Instant::now();
    let mut ticks = tokio::time::interval(Duration::from_secs(1));
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! { biased; _ = stopped(&mut stopping) => break, _ = ticks.tick() => {} }
        let Some(runtime) = runtime.borrow().clone() else {
            continue;
        };
        let worker_stopping = stopping.clone();
        let step = async {
            if !runtime.writable().await? {
                return Ok::<_, UseCaseError>(false);
            }
            if !seeded {
                runtime.store.seed_schedules().await?;
                seeded = true;
            }
            let finished = workers
                .iter()
                .filter(|(_, worker)| worker.handle.is_finished())
                .map(|(kind, _)| *kind)
                .collect::<Vec<_>>();
            for kind in finished {
                let mut worker = workers.remove(kind).expect("finished worker");
                if let Err(error) = (&mut worker.handle).await {
                    tracing::error!(%error, task_id = %worker.lease.run.id, "后台任务意外退出");
                    let mut report = worker.progress.borrow().clone();
                    report.error = Some(FAILURE_MESSAGE.into());
                    let changed = worker
                        .runtime
                        .store
                        .finish(&worker.lease, TaskStatus::Failed, &report)
                        .await?;
                    if changed {
                        worker.runtime.telemetry.task_finished(
                            worker.lease.run.kind,
                            TaskStatus::Failed,
                            Some(worker.started.elapsed()),
                        );
                    }
                }
            }
            runtime.recover_expired().await?;
            runtime.store.tick_schedules().await?;
            let mut allowed = vec![TaskKind::HtmlRebuild, TaskKind::PublishDue];
            if runtime.retention_available().await {
                allowed.push(TaskKind::Retention);
            }
            allowed.retain(|kind| !workers.contains_key(kind.as_str()));
            while !allowed.is_empty() {
                let Some(lease) = runtime
                    .store
                    .claim(&allowed, worker_id, LEASE_SECONDS)
                    .await?
                else {
                    break;
                };
                allowed.retain(|kind| *kind != lease.run.kind);
                let (updates, progress) = watch::channel(lease.run.report.clone());
                let handle = tokio::spawn(execute_lease(
                    runtime.clone(),
                    lease.clone(),
                    updates,
                    worker_stopping.clone(),
                ));
                workers.insert(
                    lease.run.kind.as_str(),
                    ManagedWorker {
                        lease,
                        runtime: runtime.clone(),
                        progress,
                        handle,
                        started: std::time::Instant::now(),
                    },
                );
            }
            Ok(true)
        };
        let checked =
            tokio::select! { biased; _ = stopped(&mut stopping) => break, result = step => result };
        let successful_check = checked.is_ok();
        let result = match checked {
            Ok(true) => TaskSchedulerCheck::Success,
            Ok(false) => TaskSchedulerCheck::Unavailable,
            Err(error) => {
                tracing::warn!(%error, "后台任务调度失败，下次检查重试");
                TaskSchedulerCheck::Error
            }
        };
        runtime
            .telemetry
            .task_scheduler_check(result, time::OffsetDateTime::now_utc().unix_timestamp());
        if successful_check && Instant::now() >= health_sample_due {
            let sampled = tokio::select! { biased; _ = stopped(&mut stopping) => break, result = runtime.sample_health() => result };
            if let Err(error) = sampled {
                tracing::warn!(%error, "后台任务健康快照暂不可用");
            }
            health_sample_due = Instant::now() + Duration::from_secs(5);
        }
    }
    // The owner enforces the HTTP shutdown deadline; Drop aborts any remaining
    // worker if this supervisor is cancelled while waiting for a database write.
    for worker in workers.values_mut() {
        let _ = (&mut worker.handle).await;
    }
}

async fn execute_lease(
    runtime: Arc<TaskRuntime>,
    lease: TaskLease,
    updates: watch::Sender<TaskReport>,
    mut stopping: watch::Receiver<bool>,
) {
    let started = std::time::Instant::now();
    let due_since = lease.run.created_at.max(lease.run.run_at);
    let wait = lease
        .run
        .started_at
        .map(|claimed| (claimed - due_since).as_seconds_f64().max(0.0))
        .unwrap_or(0.0);
    runtime
        .telemetry
        .task_started(lease.run.kind, Duration::from_secs_f64(wait));
    let mut progress = updates.subscribe();
    let business_runtime = runtime.clone();
    let business_lease = lease.clone();
    let mut business = BusinessWorker(tokio::spawn(async move {
        run_business(&business_runtime, &business_lease, updates).await
    }));
    let mut joined = false;
    let mut progress_pending = false;
    // Keep a conservative local deadline between successful database renewals.
    // Transient report/heartbeat lock contention must not cancel an otherwise
    // valid business transaction; its own fence still checks database ownership.
    let lease_window = Duration::from_secs(LEASE_SECONDS as u64 - 2);
    let mut lease_deadline = Box::pin(tokio::time::sleep(lease_window));
    let mut heartbeat = tokio::time::interval_at(
        Instant::now() + Duration::from_secs(10),
        Duration::from_secs(10),
    );
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let (status, mut report) = loop {
        tokio::select! {
            biased;
            _ = stopped(&mut stopping) => break (TaskStatus::Interrupted, progress.borrow().clone()),
            _ = &mut lease_deadline => break (TaskStatus::Interrupted, progress.borrow().clone()),
            result = &mut business.0 => {
                joined = true;
                break match result {
                    Ok(result) => result,
                    Err(error) => { tracing::error!(%error, task_id = %lease.run.id, "任务业务执行意外退出"); let mut report = progress.borrow().clone(); report.error = Some(FAILURE_MESSAGE.into()); (TaskStatus::Failed, report) }
                };
            }
            _ = heartbeat.tick() => {
                let renewed = tokio::select! { biased; _ = stopped(&mut stopping) => Ok(false), _ = &mut lease_deadline => Ok(false), result = runtime.store.renew(&lease, LEASE_SECONDS) => result };
                match renewed {
                    Ok(true) => {
                        lease_deadline.as_mut().reset(Instant::now() + lease_window);
                        if progress_pending { progress.mark_changed(); }
                    }
                    Err(error @ UseCaseError::Repository(_)) => tracing::warn!(%error, task_id = %lease.run.id, "任务续租暂不可用，在租约期限内重试"),
                    result => { if let Err(error) = result { tracing::error!(%error, task_id = %lease.run.id, "任务续租被拒绝"); } break (TaskStatus::Interrupted, progress.borrow().clone()); }
                }
            }
            _ = progress.changed() => {
                let report = progress.borrow_and_update().clone();
                progress_pending = true;
                let saved = tokio::select! { biased; _ = stopped(&mut stopping) => Ok(false), _ = &mut lease_deadline => Ok(false), result = runtime.store.progress(&lease, &report) => result };
                match saved {
                    Ok(true) => progress_pending = false,
                    Err(error @ UseCaseError::Repository(_)) => tracing::warn!(%error, task_id = %lease.run.id, "任务进度暂不能保存，将合并后重试"),
                    result => { if let Err(error) = result { tracing::error!(%error, task_id = %lease.run.id, "任务进度写入被拒绝"); } break (TaskStatus::Interrupted, report); }
                }
            }
        }
    };
    if !joined {
        business.0.abort();
        let _ = (&mut business.0).await;
    }
    if status == TaskStatus::Interrupted {
        // Progress can advance while a report write waits on the business lock.
        // Read after joining so the interrupted result retains every confirmed
        // batch and no observer can overwrite the terminal snapshot afterward.
        report = progress.borrow().clone();
        report.error = Some("任务执行已中断；已提交操作保留，请检查后重试".into());
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
    // Dropping an sqlx transaction queues its rollback; a server-side statement
    // can retain the task-row lock until its SQL deadline. Retry only this fenced,
    // idempotent terminal write, within the remaining lease and owner shutdown
    // deadline. Never retry business operations or revive expired ownership.
    loop {
        let result = tokio::select! {
            biased;
            _ = &mut lease_deadline => { tracing::warn!(task_id = %lease.run.id, "保存结果期限已到，租约到期后恢复为中断"); return; }
            result = runtime.store.finish(&lease, status, &report) => result,
        };
        match result {
            Ok(changed) => {
                if changed {
                    runtime.telemetry.task_finished(
                        lease.run.kind,
                        status,
                        Some(started.elapsed()),
                    );
                }
                return;
            }
            Err(error @ UseCaseError::Repository(_)) => {
                tracing::warn!(%error, task_id = %lease.run.id, "任务结束记录暂不能保存，在剩余期限内重试");
                tokio::select! { _ = &mut lease_deadline => return, _ = tokio::time::sleep(Duration::from_millis(250)) => {} }
            }
            Err(error) => {
                tracing::error!(%error, task_id = %lease.run.id, "任务结束写入被拒绝");
                return;
            }
        }
    }
}

struct Observer(watch::Sender<TaskReport>);
impl RebuildObserver for Observer {
    fn progress(&self, report: &RebuildReport) {
        self.0.send_replace(TaskReport {
            html: Some(safe_html(report)),
            ..Default::default()
        });
    }
}
impl RetentionObserver for Observer {
    fn progress(&self, report: &RetentionResult) {
        self.0.send_replace(TaskReport {
            retention: Some(report.clone()),
            ..Default::default()
        });
    }
}
impl PublicationObserver for Observer {
    fn progress(&self, report: &PublicationResult) {
        self.0.send_replace(TaskReport {
            publication: Some(report.clone()),
            ..Default::default()
        });
    }
}
fn safe_html(report: &RebuildReport) -> RebuildReport {
    let mut report = report.clone();
    if let Some(failure) = &mut report.failure {
        failure.message = FAILURE_MESSAGE.into();
    }
    report
}
async fn run_business(
    runtime: &TaskRuntime,
    lease: &TaskLease,
    updates: watch::Sender<TaskReport>,
) -> (TaskStatus, TaskReport) {
    let observer = Observer(updates);
    let result = match lease.run.kind {
        TaskKind::HtmlRebuild => {
            let store = PostgresHtmlRebuildStore::new(
                runtime.pool.clone(),
                runtime.rendering.clone(),
                runtime.rendering.clone(),
            )
            .with_audit(lease.audit)
            .with_task_lease(lease.clone());
            match HtmlRebuildInteractor::new(Arc::new(store))
                .run_with_progress(RebuildOptions::default(), &observer)
                .await
            {
                Ok(report) => Ok(TaskReport {
                    html: Some(safe_html(&report)),
                    ..Default::default()
                }),
                Err(error) => {
                    RebuildObserver::progress(&observer, &error.0);
                    Err(UseCaseError::Repository(error.to_string()))
                }
            }
        }
        TaskKind::Retention => {
            let pool = runtime
                .maintenance
                .as_ref()
                .expect("retention capability checked");
            let store = infrastructure::retention::PostgresRetentionCleanupStore::new(pool.clone())
                .with_task_lease(lease.clone());
            RetentionMaintenance::new(Arc::new(store))
                .run_with_progress(100, 100, false, &observer)
                .await
                .map(|report| TaskReport {
                    retention: Some(report),
                    ..Default::default()
                })
        }
        TaskKind::PublishDue => {
            let store =
                infrastructure::PostgresScheduledPublicationStore::new(runtime.pool.clone())
                    .with_audit(lease.audit)
                    .with_task_lease(lease.clone());
            let start = std::time::Instant::now();
            let result =
                PublishDueInteractor::new(Arc::new(store), Arc::new(infrastructure::SystemClock))
                    .run_with_progress(100, &observer)
                    .await;
            let elapsed = start.elapsed();
            runtime.telemetry.publication_run(elapsed, result.is_ok());
            let counts = result
                .as_ref()
                .map(|report| report.published as usize)
                .map_err(|error| UseCaseError::Repository(error.to_string()));
            runtime
                .publication_log
                .lock()
                .expect("publication log lock")
                .record(&counts, elapsed, runtime.pool.pool_snapshot());
            result.map(|report| TaskReport {
                publication: Some(report),
                ..Default::default()
            })
        }
    };
    match result {
        Ok(report) => (TaskStatus::Completed, report),
        Err(error) => {
            tracing::error!(%error, task_id = %lease.run.id, kind = lease.run.kind.as_str(), "后台任务执行失败");
            let mut report = observer.0.borrow().clone();
            report.error = Some(FAILURE_MESSAGE.into());
            (TaskStatus::Failed, report)
        }
    }
}

pub async fn maintenance_pool(
    config: &crate::config::DeploymentConfig,
    runtime_url: &str,
    pool: &Database,
) -> Option<Database> {
    let url = match config.maintenance_url() {
        Ok(url) => url,
        Err(_) => {
            tracing::warn!("清理数据库配置无效，禁用后台清理执行");
            return None;
        }
    };
    if url == runtime_url {
        return Some(pool.clone());
    }
    let mut policy = match config.http_database_pool() {
        Ok(policy) => policy,
        Err(_) => return None,
    };
    policy.max_connections = 2;
    policy.min_connections = 0;
    match infrastructure::connect_with_config(&url, &policy).await {
        Ok(pool) => Some(pool),
        Err(_) => {
            tracing::warn!("清理数据库不可用，禁用后台清理执行");
            None
        }
    }
}

#[cfg(test)]
#[path = "tasks_tests.rs"]
mod database_tests;

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn supervisor_waits_for_installation_and_shutdown_is_idempotent() {
        let supervisor = TaskSupervisor::default();
        supervisor.start();
        supervisor.start();
        tokio::task::yield_now().await;
        assert!(supervisor.is_open());
        supervisor
            .shutdown(Instant::now() + Duration::from_secs(1))
            .await;
        assert!(!supervisor.is_open());
        supervisor.start();
        supervisor
            .shutdown(Instant::now() + Duration::from_secs(1))
            .await;
        assert!(supervisor.lifecycle.lock().unwrap().handle.is_none());
    }
    #[test]
    fn html_failures_do_not_expose_diagnostics() {
        let report = RebuildReport {
            failure: Some(application::html_rebuild::RebuildFailure {
                kind: None,
                id: None,
                message: "postgres password secret".into(),
            }),
            ..Default::default()
        };
        assert_eq!(safe_html(&report).failure.unwrap().message, FAILURE_MESSAGE);
        assert_eq!(report.failure.unwrap().message, "postgres password secret");
    }
}
