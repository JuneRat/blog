//! 有界 HTML 维护：批次预算、只读预检与部分完成结果；不持有渲染器或事务。
use std::{fmt, sync::Arc};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::UseCaseError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HtmlKind {
    Post,
    Page,
    Comment,
}

impl fmt::Display for HtmlKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Post => "post",
            Self::Page => "page",
            Self::Comment => "comment",
        })
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RebuildCounts {
    pub posts: u64,
    pub pages: u64,
    pub comments: u64,
}

impl RebuildCounts {
    pub fn get(&self, kind: HtmlKind) -> u64 {
        match kind {
            HtmlKind::Post => self.posts,
            HtmlKind::Page => self.pages,
            HtmlKind::Comment => self.comments,
        }
    }

    fn add(&mut self, kind: HtmlKind, count: u64) {
        match kind {
            HtmlKind::Post => self.posts += count,
            HtmlKind::Page => self.pages += count,
            HtmlKind::Comment => self.comments += count,
        }
    }

    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RebuildOptions {
    pub batch_size: i64,
    pub max_batches: u32,
    pub dry_run: bool,
}

impl Default for RebuildOptions {
    fn default() -> Self {
        Self {
            batch_size: 100,
            max_batches: 100,
            dry_run: false,
        }
    }
}

impl RebuildOptions {
    pub fn validate(&self) -> Result<(), UseCaseError> {
        if !(1..=1000).contains(&self.batch_size) || !(1..=1000).contains(&self.max_batches) {
            return Err(UseCaseError::Invalid("批量大小和批次数须为 1–1,000".into()));
        }
        Ok(())
    }
}

/// 游标是本批最后检查的 ID，包括 CAS 未提交的记录；一轮内不反复处理同一条。
#[derive(Debug, Default)]
pub struct RebuildBatch {
    pub rebuilt: u64,
    pub skipped: u64,
    pub cursor: Option<Uuid>,
}

/// 批次逐条原子提交；失败必须带回此前已确认提交的计数。
#[derive(Debug)]
pub struct RebuildBatchError {
    pub progress: RebuildBatch,
    pub id: Option<Uuid>,
    pub source: UseCaseError,
}

#[async_trait]
pub trait HtmlRebuildStore: Send + Sync {
    /// 单次只读快照，分别统计三类记录；不读取源文，不触发渲染或写入。
    async fn pending(&self) -> Result<RebuildCounts, UseCaseError>;

    /// 按 ID 升序检查 after 之后至多 limit 条旧版本记录；渲染期间不占用写事务。
    /// 每条记录按读取的版本条件提交，HTML/引用与审计必须原子生效；
    /// 并发变更计入 skipped，不覆盖新内容。这是提交与进度契约，适配器可选择
    /// 等价的条件写入机制。游标包含已检查但跳过的记录，失败返回已确认的进度。
    async fn rebuild_batch(
        &self,
        kind: HtmlKind,
        after: Option<Uuid>,
        limit: i64,
    ) -> Result<RebuildBatch, RebuildBatchError>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RebuildFailure {
    pub kind: Option<HtmlKind>,
    pub id: Option<Uuid>,
    pub message: String,
}

impl fmt::Display for RebuildFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(kind) = self.kind {
            write!(f, "{kind} ")?;
        }
        if let Some(id) = self.id {
            write!(f, "{id} ")?;
        }
        f.write_str(&self.message)
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct RebuildReport {
    pub rebuilt: RebuildCounts,
    pub skipped: RebuildCounts,
    /// 只读预检或成功结束时的剩余快照；运行中和失败时未知（null），
    /// 不能用最初的读取结果冒充当前数量。
    pub pending: Option<RebuildCounts>,
    /// 实际尝试的批次数，三类来源共用额度，包含失败或空批次。
    pub batches: u32,
    pub has_more: bool,
    pub dry_run: bool,
    pub failure: Option<RebuildFailure>,
}

#[derive(Debug, Clone)]
pub struct RebuildError(pub Box<RebuildReport>);

impl fmt::Display for RebuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("HTML 重建失败：")?;
        if let Some(failure) = &self.0.failure {
            write!(f, "{failure}")?;
        }
        Ok(())
    }
}
impl std::error::Error for RebuildError {}

impl RebuildReport {
    fn fail(
        mut self,
        kind: Option<HtmlKind>,
        id: Option<Uuid>,
        source: UseCaseError,
    ) -> RebuildError {
        self.pending = None;
        self.has_more = true;
        self.failure = Some(RebuildFailure {
            kind,
            id,
            message: source.to_string(),
        });
        RebuildError(Box::new(self))
    }

    fn record(&mut self, kind: HtmlKind, batch: &RebuildBatch) {
        self.rebuilt.add(kind, batch.rebuilt);
        self.skipped.add(kind, batch.skipped);
    }
}

pub struct HtmlRebuildInteractor {
    store: Arc<dyn HtmlRebuildStore>,
}

/// 同步接收已确认提交的进度。需要留存快照时由观察者 clone；
/// 观察者不得阻塞批次调度，也不负责取消或调度后台任务。
pub trait RebuildObserver: Send + Sync {
    fn progress(&self, report: &RebuildReport);
}

struct IgnoreProgress;

impl RebuildObserver for IgnoreProgress {
    fn progress(&self, _: &RebuildReport) {}
}

impl HtmlRebuildInteractor {
    pub fn new(store: Arc<dyn HtmlRebuildStore>) -> Self {
        Self { store }
    }

    pub async fn run(&self, options: RebuildOptions) -> Result<RebuildReport, RebuildError> {
        self.run_with_progress(options, &IgnoreProgress).await
    }

    pub async fn run_with_progress(
        &self,
        options: RebuildOptions,
        observer: &dyn RebuildObserver,
    ) -> Result<RebuildReport, RebuildError> {
        let mut report = RebuildReport {
            dry_run: options.dry_run,
            has_more: true,
            ..Default::default()
        };
        observer.progress(&report);
        if let Err(error) = options.validate() {
            return Err(observed_failure(report, None, None, error, observer));
        }
        let pending = match self.store.pending().await {
            Ok(counts) => counts,
            Err(error) => {
                return Err(observed_failure(report, None, None, error, observer));
            }
        };
        if options.dry_run {
            report.pending = Some(pending);
            report.has_more = !pending.is_empty();
            observer.progress(&report);
            return Ok(report);
        }
        for kind in [HtmlKind::Post, HtmlKind::Page, HtmlKind::Comment] {
            if pending.get(kind) == 0 {
                continue;
            }
            let mut after = None;
            while report.batches < options.max_batches {
                report.batches += 1;
                let batch = match self
                    .store
                    .rebuild_batch(kind, after, options.batch_size)
                    .await
                {
                    Ok(batch) => batch,
                    Err(error) => {
                        report.record(kind, &error.progress);
                        return Err(observed_failure(
                            report,
                            Some(kind),
                            error.id,
                            error.source,
                            observer,
                        ));
                    }
                };
                report.record(kind, &batch);
                observer.progress(&report);
                if batch.rebuilt + batch.skipped < options.batch_size as u64
                    || batch.cursor <= after
                {
                    break;
                }
                after = batch.cursor;
            }
        }
        match self.store.pending().await {
            Ok(counts) => {
                report.pending = Some(counts);
                report.has_more = !counts.is_empty();
                observer.progress(&report);
                Ok(report)
            }
            Err(error) => Err(observed_failure(report, None, None, error, observer)),
        }
    }
}

fn observed_failure(
    report: RebuildReport,
    kind: Option<HtmlKind>,
    id: Option<Uuid>,
    source: UseCaseError,
    observer: &dyn RebuildObserver,
) -> RebuildError {
    let error = report.fail(kind, id, source);
    observer.progress(&error.0);
    error
}
