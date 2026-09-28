//! 到期发布用例，供受控 CLI 和周期调度共用。
use std::sync::Arc;

use async_trait::async_trait;
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

impl PublishDueInteractor {
    pub fn new(store: Arc<dyn ScheduledPublicationStore>, clock: Arc<dyn Clock>) -> Self {
        Self { store, clock }
    }

    /// 处理当前到期积压；错误直接返回，已提交批次保持生效，下一次调用可继续。
    pub async fn run(&self) -> Result<usize, UseCaseError> {
        const BATCH_SIZE: i64 = 100;
        let mut total = 0;
        loop {
            let count = self
                .store
                .publish_batch(self.clock.now(), BATCH_SIZE)
                .await?;
            total += count;
            // 任一类型可能填满一批；只有合计不足一批才能确定两者均未填满。
            if count < BATCH_SIZE as usize {
                return Ok(total);
            }
        }
    }
}
