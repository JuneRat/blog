//! Own one bounded HTML rebuild worker for the lifetime of the HTTP process.
//! Database work and rendering use the same application ports as the CLI.

use std::sync::{Arc, Mutex};

use application::{
    UseCaseError,
    audit::AuditContext,
    html_rebuild::{
        HtmlRebuildInteractor, HtmlRebuildStore, RebuildFailure, RebuildObserver, RebuildOptions,
        RebuildReport,
    },
    html_rebuild_admin::{HtmlRebuildJob, HtmlRebuildJobStatus, HtmlRebuildJobs, HtmlRebuildView},
};
use infrastructure::{Database, PostgresHtmlRebuildStore, RenderingRuntime};
use tokio::task::JoinHandle;
use uuid::Uuid;

const FAILURE_MESSAGE: &str = "HTML 重建失败，请检查服务日志，修复原因后重试";

#[derive(Default)]
struct Progress {
    closed: bool,
    job: Option<HtmlRebuildJob>,
}

#[derive(Default)]
pub struct HtmlRebuildCoordinator {
    progress: Arc<Mutex<Progress>>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl HtmlRebuildCoordinator {
    /// All admissions and handle inspection lock progress before worker.
    fn snapshot(&self) -> (bool, Option<HtmlRebuildJob>) {
        let mut progress = self.progress.lock().expect("HTML rebuild progress lock");
        let worker = self.worker.lock().expect("HTML rebuild worker lock");
        Self::detect_unexpected_exit(&mut progress, worker.as_ref());
        (progress.closed, progress.job.clone())
    }

    fn detect_unexpected_exit(progress: &mut Progress, worker: Option<&JoinHandle<()>>) {
        if worker.is_some_and(JoinHandle::is_finished)
            && let Some(job) = progress.job.as_mut()
            && job.status == HtmlRebuildJobStatus::Running
        {
            job.status = HtmlRebuildJobStatus::Failed;
            job.report.pending = None;
            job.report.has_more = true;
            job.report.failure = Some(RebuildFailure {
                kind: None,
                id: None,
                message: FAILURE_MESSAGE.into(),
            });
            tracing::error!(job_id = %job.id, "HTML 重建任务意外退出");
        }
    }

    fn start(&self, rebuild: HtmlRebuildInteractor) -> Result<HtmlRebuildJob, UseCaseError> {
        let mut progress = self.progress.lock().expect("HTML rebuild progress lock");
        let mut worker = self.worker.lock().expect("HTML rebuild worker lock");
        Self::detect_unexpected_exit(&mut progress, worker.as_ref());
        if progress.closed {
            return Err(UseCaseError::Invalid(
                "服务正在关闭，暂时不能启动 HTML 重建".into(),
            ));
        }
        if let Some(job) = &progress.job
            && job.status == HtmlRebuildJobStatus::Running
        {
            return Ok(job.clone());
        }
        let job = HtmlRebuildJob {
            id: Uuid::now_v7(),
            status: HtmlRebuildJobStatus::Running,
            report: RebuildReport {
                has_more: true,
                ..Default::default()
            },
        };
        progress.job = Some(job.clone());
        let observer = JobObserver {
            progress: self.progress.clone(),
            id: job.id,
        };
        // Keep this handle outside the HTTP listener JoinSet: normal job completion
        // must never signal that the HTTP process should shut down.
        *worker = Some(tokio::spawn(async move {
            let result = rebuild
                .run_with_progress(RebuildOptions::default(), &observer)
                .await;
            let (status, report) = match result {
                Ok(report) => (HtmlRebuildJobStatus::Completed, report),
                Err(error) => {
                    tracing::error!(job_id = %observer.id, %error, "后台 HTML 重建失败");
                    (HtmlRebuildJobStatus::Failed, *error.0)
                }
            };
            let mut progress = observer
                .progress
                .lock()
                .expect("HTML rebuild progress lock");
            if let Some(job) = progress.job.as_mut()
                && job.id == observer.id
                && job.status == HtmlRebuildJobStatus::Running
            {
                job.status = status;
                job.report = safe_report(&report);
            }
        }));
        Ok(job)
    }

    /// Close admissions before draining HTTP, including a late installer activation.
    pub fn close(&self) {
        self.progress
            .lock()
            .expect("HTML rebuild progress lock")
            .closed = true;
    }

    /// Cancel and join the owned worker before closing its database pool. Committed
    /// records survive cancellation; an in-flight transaction rolls back on drop.
    pub async fn shutdown(&self) {
        let worker = {
            let mut progress = self.progress.lock().expect("HTML rebuild progress lock");
            progress.closed = true;
            self.worker.lock().expect("HTML rebuild worker lock").take()
        };
        if let Some(worker) = worker {
            worker.abort();
            if let Err(error) = worker.await
                && !error.is_cancelled()
            {
                tracing::error!(%error, "关闭 HTML 重建任务失败");
            }
        }
        let mut progress = self.progress.lock().expect("HTML rebuild progress lock");
        if let Some(job) = progress.job.as_mut()
            && job.status == HtmlRebuildJobStatus::Running
        {
            job.status = HtmlRebuildJobStatus::Interrupted;
            job.report.pending = None;
            job.report.has_more = true;
        }
    }
}

fn safe_report(report: &RebuildReport) -> RebuildReport {
    let mut report = report.clone();
    if let Some(failure) = &mut report.failure {
        // Repository/render errors can contain database details or source text.
        failure.message = FAILURE_MESSAGE.into();
    }
    report
}

struct JobObserver {
    progress: Arc<Mutex<Progress>>,
    id: Uuid,
}

impl RebuildObserver for JobObserver {
    fn progress(&self, report: &RebuildReport) {
        let mut progress = self.progress.lock().expect("HTML rebuild progress lock");
        if let Some(job) = progress.job.as_mut()
            && job.id == self.id
            && job.status == HtmlRebuildJobStatus::Running
        {
            job.report = safe_report(report);
        }
    }
}

pub struct WebsiteHtmlRebuildJobs {
    pool: Database,
    runtime: Arc<RenderingRuntime>,
    coordinator: Arc<HtmlRebuildCoordinator>,
    recovery_mode: bool,
}

impl WebsiteHtmlRebuildJobs {
    pub fn new(
        pool: Database,
        runtime: Arc<RenderingRuntime>,
        coordinator: Arc<HtmlRebuildCoordinator>,
        recovery_mode: bool,
    ) -> Self {
        Self {
            pool,
            runtime,
            coordinator,
            recovery_mode,
        }
    }

    fn store(&self, audit: AuditContext) -> PostgresHtmlRebuildStore {
        PostgresHtmlRebuildStore::new(
            self.pool.clone(),
            self.runtime.clone(),
            self.runtime.clone(),
        )
        .with_audit(audit)
    }

    async fn available(&self) -> Result<bool, UseCaseError> {
        Ok(!self.recovery_mode && !self.pool.is_recovery_isolated().await?)
    }
}

#[async_trait::async_trait]
impl HtmlRebuildJobs for WebsiteHtmlRebuildJobs {
    async fn view(&self) -> Result<HtmlRebuildView, UseCaseError> {
        let available = self.available().await?;
        let (closed, job) = self.coordinator.snapshot();
        if job
            .as_ref()
            .is_some_and(|job| job.status == HtmlRebuildJobStatus::Running)
        {
            return Ok(HtmlRebuildView {
                pending: None,
                job,
                available: available && !closed,
            });
        }
        let pending = self.store(AuditContext::system()).pending().await?;
        // A POST may have started while the read-only count was in flight.
        let (closed, job) = self.coordinator.snapshot();
        let running = job
            .as_ref()
            .is_some_and(|job| job.status == HtmlRebuildJobStatus::Running);
        Ok(HtmlRebuildView {
            pending: (!running).then_some(pending),
            job,
            available: available && !closed,
        })
    }

    async fn start(&self, audit: AuditContext) -> Result<HtmlRebuildJob, UseCaseError> {
        if !self.available().await? {
            return Err(UseCaseError::Invalid("恢复隔离期间禁止 HTML 重建".into()));
        }
        self.coordinator
            .start(HtmlRebuildInteractor::new(Arc::new(self.store(audit))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use application::html_rebuild::{HtmlKind, RebuildBatch, RebuildBatchError, RebuildCounts};
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use tokio::sync::Semaphore;

    #[derive(Clone, Copy)]
    enum Outcome {
        Success,
        Failure,
        Panic,
    }

    struct Store {
        posts: AtomicU64,
        calls: AtomicUsize,
        entered: Semaphore,
        release: Semaphore,
        dropped: Arc<AtomicUsize>,
        outcome: Outcome,
        block: bool,
        failure_id: Uuid,
    }

    impl Store {
        fn new(outcome: Outcome, block: bool) -> Arc<Self> {
            Arc::new(Self {
                posts: AtomicU64::new(101),
                calls: AtomicUsize::new(0),
                entered: Semaphore::new(0),
                release: Semaphore::new(0),
                dropped: Arc::new(AtomicUsize::new(0)),
                outcome,
                block,
                failure_id: Uuid::now_v7(),
            })
        }

        fn interactor(self: &Arc<Self>) -> HtmlRebuildInteractor {
            HtmlRebuildInteractor::new(self.clone())
        }
    }

    struct OnDrop(Arc<AtomicUsize>);
    impl Drop for OnDrop {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[async_trait::async_trait]
    impl HtmlRebuildStore for Store {
        async fn pending(&self) -> Result<RebuildCounts, UseCaseError> {
            Ok(RebuildCounts {
                posts: self.posts.load(Ordering::SeqCst),
                ..Default::default()
            })
        }

        async fn rebuild_batch(
            &self,
            kind: HtmlKind,
            _: Option<Uuid>,
            _: i64,
        ) -> Result<RebuildBatch, RebuildBatchError> {
            if kind != HtmlKind::Post {
                return Ok(RebuildBatch::default());
            }
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if call > 1 {
                return Ok(RebuildBatch::default());
            }
            if call == 1 {
                let _drop = OnDrop(self.dropped.clone());
                self.entered.add_permits(1);
                if self.block {
                    self.release.acquire().await.expect("release").forget();
                }
                if matches!(self.outcome, Outcome::Panic) {
                    panic!("worker panicked");
                }
                self.posts.fetch_sub(1, Ordering::SeqCst);
                if matches!(self.outcome, Outcome::Failure) {
                    return Err(RebuildBatchError {
                        progress: RebuildBatch {
                            rebuilt: 1,
                            ..Default::default()
                        },
                        id: Some(self.failure_id),
                        source: UseCaseError::Repository(
                            "postgres://private-password source-content".into(),
                        ),
                    });
                }
            } else {
                self.posts.fetch_sub(100, Ordering::SeqCst);
            }
            Ok(RebuildBatch {
                rebuilt: if call == 0 { 100 } else { 1 },
                cursor: Some(Uuid::now_v7()),
                skipped: 0,
            })
        }
    }

    async fn terminal(coordinator: &HtmlRebuildCoordinator) -> HtmlRebuildJob {
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let job = coordinator.snapshot().1.expect("job");
                if job.status != HtmlRebuildJobStatus::Running {
                    return job;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("worker should stop")
    }

    async fn entered(store: &Store) {
        tokio::time::timeout(std::time::Duration::from_secs(2), store.entered.acquire())
            .await
            .expect("worker should enter second batch")
            .unwrap()
            .forget();
    }

    #[tokio::test]
    async fn concurrent_admissions_share_a_job_and_report_confirmed_progress() {
        let coordinator = Arc::new(HtmlRebuildCoordinator::default());
        let store = Store::new(Outcome::Success, true);
        let initial = coordinator.start(store.interactor()).unwrap();
        entered(&store).await;
        let mut starts = tokio::task::JoinSet::new();
        for _ in 0..16 {
            let coordinator = coordinator.clone();
            let store = store.clone();
            starts.spawn(async move { coordinator.start(store.interactor()).unwrap() });
        }
        while let Some(result) = starts.join_next().await {
            assert_eq!(result.unwrap().id, initial.id);
        }
        let running = coordinator.snapshot().1.unwrap();
        assert_eq!(running.report.rebuilt.posts, 100);
        assert_eq!(running.report.batches, 1);
        assert!(running.report.pending.is_none());
        assert_eq!(store.calls.load(Ordering::SeqCst), 2);
        store.release.add_permits(1);
        let completed = terminal(&coordinator).await;
        assert_eq!(completed.status, HtmlRebuildJobStatus::Completed);
        assert_eq!(completed.report.rebuilt.posts, 101);
        assert_eq!(completed.report.pending, Some(RebuildCounts::default()));
        assert!(!completed.report.has_more);
        coordinator.shutdown().await;
    }

    #[tokio::test]
    async fn partial_failure_preserves_counts_and_identity_but_hides_internal_details() {
        let coordinator = HtmlRebuildCoordinator::default();
        let store = Store::new(Outcome::Failure, false);
        let initial = coordinator.start(store.interactor()).unwrap();
        let failed = terminal(&coordinator).await;
        assert_eq!(failed.status, HtmlRebuildJobStatus::Failed);
        assert_eq!(failed.report.rebuilt.posts, 101);
        assert_eq!(failed.report.batches, 2);
        assert!(failed.report.pending.is_none());
        let failure = failed.report.failure.unwrap();
        assert_eq!(failure.kind, Some(HtmlKind::Post));
        assert_eq!(failure.id, Some(store.failure_id));
        assert_eq!(failure.message, FAILURE_MESSAGE);
        let next_store = Store::new(Outcome::Success, false);
        assert_ne!(
            coordinator.start(next_store.interactor()).unwrap().id,
            initial.id
        );
        assert_eq!(
            terminal(&coordinator).await.status,
            HtmlRebuildJobStatus::Completed
        );
        coordinator.shutdown().await;
    }

    #[tokio::test]
    async fn a_panicked_worker_becomes_failed_and_can_be_retried() {
        let coordinator = HtmlRebuildCoordinator::default();
        let store = Store::new(Outcome::Panic, false);
        let initial = coordinator.start(store.interactor()).unwrap();
        let failed = terminal(&coordinator).await;
        assert_eq!(failed.status, HtmlRebuildJobStatus::Failed);
        assert_eq!(failed.report.rebuilt.posts, 100);
        assert!(failed.report.pending.is_none());
        assert_eq!(failed.report.failure.unwrap().message, FAILURE_MESSAGE);
        assert_ne!(
            coordinator
                .start(Store::new(Outcome::Success, false).interactor())
                .unwrap()
                .id,
            initial.id
        );
        assert_eq!(
            terminal(&coordinator).await.status,
            HtmlRebuildJobStatus::Completed
        );
        coordinator.shutdown().await;
    }

    #[tokio::test]
    async fn shutdown_joins_cancellation_and_keeps_admission_closed() {
        let coordinator = HtmlRebuildCoordinator::default();
        let store = Store::new(Outcome::Success, true);
        coordinator.start(store.interactor()).unwrap();
        entered(&store).await;
        coordinator.close();
        assert!(coordinator.start(store.interactor()).is_err());
        coordinator.shutdown().await;
        let (closed, job) = coordinator.snapshot();
        assert!(closed);
        let interrupted = job.unwrap();
        assert_eq!(interrupted.status, HtmlRebuildJobStatus::Interrupted);
        assert_eq!(interrupted.report.rebuilt.posts, 100);
        assert!(interrupted.report.pending.is_none());
        assert_eq!(store.dropped.load(Ordering::SeqCst), 1);
        assert!(
            coordinator
                .start(Store::new(Outcome::Success, false).interactor())
                .is_err()
        );
        coordinator.shutdown().await;
    }

    #[test]
    fn observer_snapshots_also_hide_failure_details() {
        let progress = Arc::new(Mutex::new(Progress::default()));
        let id = Uuid::now_v7();
        progress.lock().unwrap().job = Some(HtmlRebuildJob {
            id,
            status: HtmlRebuildJobStatus::Running,
            report: RebuildReport::default(),
        });
        JobObserver {
            progress: progress.clone(),
            id,
        }
        .progress(&RebuildReport {
            failure: Some(RebuildFailure {
                kind: None,
                id: None,
                message: "secret SQL".into(),
            }),
            ..Default::default()
        });
        assert_eq!(
            progress
                .lock()
                .unwrap()
                .job
                .as_ref()
                .unwrap()
                .report
                .failure
                .as_ref()
                .unwrap()
                .message,
            FAILURE_MESSAGE
        );
    }
}
