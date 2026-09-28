use application::{UseCaseError, html_rebuild::*};
use async_trait::async_trait;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};
use uuid::Uuid;

type BatchCall = (
    HtmlKind,
    Option<Uuid>,
    i64,
    Result<RebuildBatch, RebuildBatchError>,
);
struct Store {
    counts: Mutex<VecDeque<Result<RebuildCounts, UseCaseError>>>,
    batches: Mutex<VecDeque<BatchCall>>,
}
impl Store {
    fn new(counts: Vec<Result<RebuildCounts, UseCaseError>>, batches: Vec<BatchCall>) -> Arc<Self> {
        Arc::new(Self {
            counts: Mutex::new(counts.into()),
            batches: Mutex::new(batches.into()),
        })
    }
    fn drained(&self) {
        assert!(self.counts.lock().unwrap().is_empty());
        assert!(self.batches.lock().unwrap().is_empty());
    }
}
#[async_trait]
impl HtmlRebuildStore for Store {
    async fn pending(&self) -> Result<RebuildCounts, UseCaseError> {
        self.counts
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected count read")
    }
    async fn rebuild_batch(
        &self,
        kind: HtmlKind,
        after: Option<Uuid>,
        limit: i64,
    ) -> Result<RebuildBatch, RebuildBatchError> {
        let (expected_kind, expected_after, expected_limit, result) = self
            .batches
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected rebuild batch");
        assert_eq!(
            (kind, after, limit),
            (expected_kind, expected_after, expected_limit)
        );
        result
    }
}
fn counts(posts: u64, pages: u64, comments: u64) -> RebuildCounts {
    RebuildCounts {
        posts,
        pages,
        comments,
    }
}
fn batch(rebuilt: u64, skipped: u64, cursor: u128) -> Result<RebuildBatch, RebuildBatchError> {
    Ok(RebuildBatch {
        rebuilt,
        skipped,
        cursor: Some(Uuid::from_u128(cursor)),
    })
}
fn options(size: i64, max: u32) -> RebuildOptions {
    RebuildOptions {
        batch_size: size,
        max_batches: max,
        dry_run: false,
    }
}

#[tokio::test]
async fn invalid_limits_never_touch_storage() {
    let store = Store::new(vec![], vec![]);
    for (size, max) in [(-1, 1), (0, 1), (1001, 1), (1, 0), (1, 1001)] {
        let error = HtmlRebuildInteractor::new(store.clone())
            .run(options(size, max))
            .await
            .unwrap_err();
        assert_eq!(error.0.batches, 0);
    }
    store.drained();
}

#[tokio::test]
async fn dry_run_counts_all_sources_once_and_never_rebuilds() {
    let store = Store::new(vec![Ok(counts(5, 3, 2))], vec![]);
    let report = HtmlRebuildInteractor::new(store.clone())
        .run(RebuildOptions {
            dry_run: true,
            ..options(1, 1)
        })
        .await
        .unwrap();
    assert_eq!(report.pending, Some(counts(5, 3, 2)));
    assert!(report.rebuilt.is_empty());
    assert!(report.skipped.is_empty());
    assert!(report.dry_run && report.has_more);
    assert_eq!(report.batches, 0);
    store.drained();
}

#[tokio::test]
async fn sources_share_one_budget_and_report_remaining_work() {
    let store = Store::new(
        vec![Ok(counts(1, 1, 1)), Ok(counts(0, 0, 1))],
        vec![
            (HtmlKind::Post, None, 2, batch(1, 0, 1)),
            (HtmlKind::Page, None, 2, batch(1, 0, 2)),
        ],
    );
    let report = HtmlRebuildInteractor::new(store.clone())
        .run(options(2, 2))
        .await
        .unwrap();
    assert_eq!(report.rebuilt, counts(1, 1, 0));
    assert_eq!(report.batches, 2);
    assert_eq!(report.pending, Some(counts(0, 0, 1)));
    assert!(report.has_more);
    store.drained();
}

#[tokio::test]
async fn cursor_advances_past_conflicts_without_retrying_them_in_the_same_run() {
    let store = Store::new(
        vec![Ok(counts(3, 0, 0)), Ok(counts(2, 0, 0))],
        vec![
            (HtmlKind::Post, None, 2, batch(0, 2, 2)),
            (HtmlKind::Post, Some(Uuid::from_u128(2)), 2, batch(1, 0, 3)),
        ],
    );
    let report = HtmlRebuildInteractor::new(store.clone())
        .run(options(2, 10))
        .await
        .unwrap();
    assert_eq!(report.rebuilt, counts(1, 0, 0));
    assert_eq!(report.skipped, counts(2, 0, 0));
    assert_eq!(report.batches, 2);
    assert!(report.has_more);
    store.drained();
}

#[tokio::test]
async fn non_advancing_cursor_does_not_spin() {
    let store = Store::new(
        vec![Ok(counts(1, 0, 0)), Ok(counts(1, 0, 0))],
        vec![(
            HtmlKind::Post,
            None,
            1,
            Ok(RebuildBatch {
                skipped: 1,
                ..Default::default()
            }),
        )],
    );
    let report = HtmlRebuildInteractor::new(store.clone())
        .run(options(1, 100))
        .await
        .unwrap();
    assert_eq!(report.batches, 1);
    assert!(report.has_more);
    store.drained();
}

#[tokio::test]
async fn failure_preserves_prior_and_partial_batch_commits_and_stops_later_sources() {
    let failed_id = Uuid::from_u128(3);
    let store = Store::new(
        vec![Ok(counts(1, 2, 1))],
        vec![
            (HtmlKind::Post, None, 2, batch(1, 0, 1)),
            (
                HtmlKind::Page,
                None,
                2,
                Err(RebuildBatchError {
                    progress: RebuildBatch {
                        rebuilt: 1,
                        skipped: 0,
                        cursor: Some(Uuid::from_u128(2)),
                    },
                    id: Some(failed_id),
                    source: UseCaseError::Render("failed".into()),
                }),
            ),
        ],
    );
    let error = HtmlRebuildInteractor::new(store.clone())
        .run(options(2, 100))
        .await
        .unwrap_err();
    assert_eq!(error.0.rebuilt, counts(1, 1, 0));
    assert_eq!(error.0.batches, 2);
    assert_eq!(error.0.pending, None);
    assert!(error.0.has_more);
    let failure = error.0.failure.as_ref().unwrap();
    assert_eq!(
        (failure.kind, failure.id),
        (Some(HtmlKind::Page), Some(failed_id))
    );
    assert!(error.to_string().contains(&failed_id.to_string()));
    store.drained();
}

#[tokio::test]
async fn failed_final_count_keeps_commits_but_marks_remaining_unknown() {
    let store = Store::new(
        vec![
            Ok(counts(1, 0, 0)),
            Err(UseCaseError::Repository("unavailable".into())),
        ],
        vec![(HtmlKind::Post, None, 2, batch(1, 0, 1))],
    );
    let error = HtmlRebuildInteractor::new(store.clone())
        .run(options(2, 10))
        .await
        .unwrap_err();
    assert_eq!(error.0.rebuilt, counts(1, 0, 0));
    assert_eq!(error.0.pending, None);
    assert_eq!(error.0.failure.as_ref().unwrap().kind, None);
    store.drained();
}

#[tokio::test]
async fn empty_run_never_reads_source_batches() {
    let store = Store::new(vec![Ok(counts(0, 0, 0)), Ok(counts(0, 0, 0))], vec![]);
    let report = HtmlRebuildInteractor::new(store.clone())
        .run(options(1, 1))
        .await
        .unwrap();
    assert_eq!(report.batches, 0);
    assert!(!report.has_more);
    store.drained();
}
