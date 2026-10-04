use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use application::{
    UseCaseError,
    ports::Clock,
    publishing::{
        PublicationObserver, PublicationResult, PublishDueInteractor, ScheduledPublicationStore,
    },
    retention::{
        RetentionBatch, RetentionCleanupStore, RetentionMaintenance, RetentionObserver,
        RetentionResult,
    },
};
use async_trait::async_trait;
use time::OffsetDateTime;

struct FixedClock;
impl Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH
    }
}

struct Publications(Mutex<VecDeque<Result<usize, UseCaseError>>>);
#[async_trait]
impl ScheduledPublicationStore for Publications {
    async fn publish_batch(&self, now: OffsetDateTime, limit: i64) -> Result<usize, UseCaseError> {
        assert_eq!(now, OffsetDateTime::UNIX_EPOCH);
        assert_eq!(limit, 100);
        self.0
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected extra publication batch")
    }
}

#[tokio::test]
async fn publishing_drains_full_batches_from_either_content_type() {
    // One table may fill its batch, or both may fill it in the same transaction.
    let store = Arc::new(Publications(Mutex::new(VecDeque::from([
        Ok(100),
        Ok(200),
        Ok(3),
    ]))));
    let publisher = PublishDueInteractor::new(store.clone(), Arc::new(FixedClock));
    assert_eq!(publisher.run().await.unwrap(), 303);
    assert!(store.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn publishing_propagates_failure_and_next_run_resumes_remaining_work() {
    let store = Arc::new(Publications(Mutex::new(VecDeque::from([
        Ok(100),
        Err(UseCaseError::Repository("unavailable".into())),
        Ok(2),
    ]))));
    let publisher = PublishDueInteractor::new(store, Arc::new(FixedClock));
    assert!(matches!(
        publisher.run().await,
        Err(UseCaseError::Repository(_))
    ));
    assert_eq!(publisher.run().await.unwrap(), 2);
}

struct Cleanup {
    results: Mutex<VecDeque<Result<RetentionBatch, UseCaseError>>>,
    calls: Mutex<Vec<(i64, bool)>>,
}
impl Cleanup {
    fn new(results: Vec<Result<RetentionBatch, UseCaseError>>) -> Arc<Self> {
        Arc::new(Self {
            results: Mutex::new(results.into()),
            calls: Mutex::new(Vec::new()),
        })
    }
}
#[async_trait]
impl RetentionCleanupStore for Cleanup {
    async fn cleanup_batch(
        &self,
        batch_size: i64,
        dry_run: bool,
    ) -> Result<RetentionBatch, UseCaseError> {
        self.calls.lock().unwrap().push((batch_size, dry_run));
        self.results
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected extra cleanup batch")
    }
}
fn batch(
    comment_ips: i64,
    audit_logs: i64,
    has_more: bool,
) -> Result<RetentionBatch, UseCaseError> {
    Ok(RetentionBatch {
        comment_ips,
        audit_logs,
        has_more,
    })
}

#[tokio::test]
async fn invalid_retention_limits_never_reach_storage() {
    let store = Cleanup::new(vec![]);
    let maintenance = RetentionMaintenance::new(store.clone());
    for (size, max) in [(0, 1), (10_001, 1), (1, 0), (1, 1001)] {
        assert!(matches!(
            maintenance.run(size, max, false).await,
            Err(UseCaseError::Invalid(_))
        ));
    }
    assert!(store.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn dry_run_counts_once_without_claiming_committed_batches() {
    let store = Cleanup::new(vec![batch(120, 80, true)]);
    let result = RetentionMaintenance::new(store.clone())
        .run(10, 100, true)
        .await
        .unwrap();
    assert_eq!(
        (result.comment_ips, result.audit_logs, result.batches),
        (120, 80, 0)
    );
    assert!(result.dry_run);
    assert!(!result.has_more);
    assert_eq!(*store.calls.lock().unwrap(), [(10, true)]);
}

#[tokio::test]
async fn cleanup_stops_at_batch_limit_and_reports_remaining_work() {
    let store = Cleanup::new(vec![batch(10, 3, true), batch(10, 1, true)]);
    let result = RetentionMaintenance::new(store)
        .run(10, 2, false)
        .await
        .unwrap();
    assert_eq!(
        (result.comment_ips, result.audit_logs, result.batches),
        (20, 4, 2)
    );
    assert!(result.has_more);
    assert!(!result.dry_run);
}

#[tokio::test]
async fn locked_remaining_rows_do_not_cause_a_busy_loop() {
    let store = Cleanup::new(vec![batch(0, 0, true)]);
    let result = RetentionMaintenance::new(store)
        .run(10, 100, false)
        .await
        .unwrap();
    assert_eq!(result.batches, 1);
    assert!(result.has_more);
}

#[tokio::test]
async fn cleanup_finishes_when_drained_and_propagates_failures() {
    let store = Cleanup::new(vec![batch(10, 0, true), batch(2, 1, false)]);
    let result = RetentionMaintenance::new(store)
        .run(10, 100, false)
        .await
        .unwrap();
    assert_eq!(
        (result.comment_ips, result.audit_logs, result.batches),
        (12, 1, 2)
    );
    assert!(!result.has_more);
    let store = Cleanup::new(vec![
        batch(10, 0, true),
        Err(UseCaseError::Repository("audit failed".into())),
    ]);
    assert!(matches!(
        RetentionMaintenance::new(store).run(10, 100, false).await,
        Err(UseCaseError::Repository(_))
    ));
}

#[derive(Default)]
struct PublicationProgress(Mutex<Vec<PublicationResult>>);
impl PublicationObserver for PublicationProgress {
    fn progress(&self, report: &PublicationResult) {
        self.0.lock().unwrap().push(report.clone());
    }
}
#[derive(Default)]
struct RetentionProgress(Mutex<Vec<RetentionResult>>);
impl RetentionObserver for RetentionProgress {
    fn progress(&self, report: &RetentionResult) {
        self.0.lock().unwrap().push(report.clone());
    }
}

#[tokio::test]
async fn bounded_publication_progress_preserves_only_confirmed_batches_on_failure() {
    let store = Arc::new(Publications(Mutex::new(VecDeque::from([
        Ok(200),
        Err(UseCaseError::Repository(
            "second transaction rolled back".into(),
        )),
        Ok(4),
    ]))));
    let publisher = PublishDueInteractor::new(store.clone(), Arc::new(FixedClock));
    let progress = PublicationProgress::default();
    assert!(publisher.run_with_progress(100, &progress).await.is_err());
    let confirmed = progress.0.lock().unwrap().last().unwrap().clone();
    assert_eq!(
        (confirmed.published, confirmed.batches, confirmed.has_more),
        (200, 1, true)
    );
    assert_eq!(progress.0.lock().unwrap().len(), 2);
    let resumed = publisher
        .run_with_progress(100, &PublicationProgress::default())
        .await
        .unwrap();
    assert_eq!(
        (resumed.published, resumed.batches, resumed.has_more),
        (4, 1, false)
    );
    let limited = PublishDueInteractor::new(
        Arc::new(Publications(Mutex::new(VecDeque::from([Ok(100)])))),
        Arc::new(FixedClock),
    )
    .run_with_progress(1, &PublicationProgress::default())
    .await
    .unwrap();
    assert_eq!(
        (limited.published, limited.batches, limited.has_more),
        (100, 1, true)
    );
    let empty = Arc::new(Publications(Mutex::new(VecDeque::new())));
    assert!(
        PublishDueInteractor::new(empty.clone(), Arc::new(FixedClock))
            .run_with_progress(0, &PublicationProgress::default())
            .await
            .is_err()
    );
    assert!(empty.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn retention_progress_preserves_committed_work_when_later_batch_fails() {
    let store = Cleanup::new(vec![
        batch(10, 3, true),
        Err(UseCaseError::Repository("rolled back".into())),
    ]);
    let progress = RetentionProgress::default();
    assert!(
        RetentionMaintenance::new(store)
            .run_with_progress(10, 100, false, &progress)
            .await
            .is_err()
    );
    let reports = progress.0.lock().unwrap();
    assert_eq!(reports.len(), 2);
    assert_eq!(
        (
            reports[0].comment_ips,
            reports[0].audit_logs,
            reports[0].batches
        ),
        (0, 0, 0)
    );
    assert_eq!(
        (
            reports[1].comment_ips,
            reports[1].audit_logs,
            reports[1].batches
        ),
        (10, 3, 1)
    );
    assert!(reports[1].has_more);
}
