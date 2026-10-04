//! 到期发布用例，供受控 CLI 和周期调度共用。
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{UseCaseError, ports::Clock};

/// 每次提交同时处理 Post/Page；单类型最多 `limit` 条。
/// 实现必须在同一事务中领取仍到期的内容、更新版本并写入审计。
#[async_trait]
pub trait ScheduledPublicationStore: Send + Sync {
    async fn publish_batch(&self, now: OffsetDateTime, limit: i64) -> Result<usize, UseCaseError>;
}

pub struct PublishDueInteractor {
    store: Arc<dyn ScheduledPublicationStore>,
    clock: Arc<dyn Clock>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct PublicationResult {
    pub published: u64,
    pub batches: u32,
    pub has_more: bool,
}

pub trait PublicationObserver: Send + Sync {
    fn progress(&self, result: &PublicationResult);
}

struct IgnoreProgress;
impl PublicationObserver for IgnoreProgress {
    fn progress(&self, _: &PublicationResult) {}
}

impl PublishDueInteractor {
    pub fn new(store: Arc<dyn ScheduledPublicationStore>, clock: Arc<dyn Clock>) -> Self {
        Self { store, clock }
    }

    /// 处理当前到期积压；错误直接返回，已提交批次保持生效，下一次调用可继续。
    pub async fn run(&self) -> Result<usize, UseCaseError> {
        Ok(self
            .run_with_progress(u32::MAX, &IgnoreProgress)
            .await?
            .published as usize)
    }

    pub async fn run_with_progress(
        &self,
        max_batches: u32,
        observer: &dyn PublicationObserver,
    ) -> Result<PublicationResult, UseCaseError> {
        if max_batches == 0 {
            return Err(UseCaseError::Invalid("发布批次数须为正数".into()));
        }
        const BATCH_SIZE: i64 = 100;
        let mut result = PublicationResult {
            has_more: true,
            ..Default::default()
        };
        observer.progress(&result);
        while result.batches < max_batches {
            let count = self
                .store
                .publish_batch(self.clock.now(), BATCH_SIZE)
                .await?;
            result.published += count as u64;
            result.batches += 1;
            // 任一类型可能填满一批；只有合计不足一批才能确定两者均未填满。
            result.has_more = count >= BATCH_SIZE as usize;
            observer.progress(&result);
            if !result.has_more {
                break;
            }
        }
        Ok(result)
    }
}
