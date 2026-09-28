//! Privacy retention settings use the existing comments/audit group versions.
use crate::{error::UseCaseError, identity::Actor};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub const DEFAULT_RETENTION_DAYS: i32 = 180;
pub fn validate_days(days: i32) -> Result<(), UseCaseError> {
    if !(1..=36_500).contains(&days) {
        return Err(UseCaseError::Invalid("保留期须为 1–36,500 天".into()));
    }
    Ok(())
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetentionSettings {
    pub comment_ip_days: i32,
    pub comment_version: i64,
    pub audit_days: i32,
    pub audit_version: i64,
}
impl Default for RetentionSettings {
    fn default() -> Self {
        Self {
            comment_ip_days: DEFAULT_RETENTION_DAYS,
            comment_version: 0,
            audit_days: DEFAULT_RETENTION_DAYS,
            audit_version: 0,
        }
    }
}
#[async_trait]
pub trait RetentionStore: Send + Sync {
    async fn read(&self) -> Result<RetentionSettings, UseCaseError>;
    async fn save(
        &self,
        value: RetentionSettings,
        actor: crate::audit::AuditContext,
    ) -> Result<RetentionSettings, UseCaseError>;
}
pub struct RetentionInteractor {
    store: Arc<dyn RetentionStore>,
}
impl RetentionInteractor {
    pub fn new(store: Arc<dyn RetentionStore>) -> Self {
        Self { store }
    }
    pub async fn read(&self, actor: &Actor) -> Result<RetentionSettings, UseCaseError> {
        authorize(actor)?;
        self.store.read().await
    }
    pub async fn save(
        &self,
        actor: &Actor,
        value: RetentionSettings,
    ) -> Result<RetentionSettings, UseCaseError> {
        authorize(actor)?;
        actor.ensure_write_channel()?;
        validate_days(value.comment_ip_days)?;
        validate_days(value.audit_days)?;
        if value.comment_version < 0 || value.audit_version < 0 {
            return Err(UseCaseError::Invalid("设置版本不能为负数".into()));
        }
        self.store.save(value, actor.audit_context()).await
    }
}
fn authorize(actor: &Actor) -> Result<(), UseCaseError> {
    if !actor.has_permission("settings.manage") {
        return Err(UseCaseError::Forbidden);
    }
    Ok(())
}

/// 每批清理的原子结果；实现须在策略锁内重读当前保留期并同事务记录审计。
#[derive(Debug, Default)]
pub struct RetentionBatch {
    pub comment_ips: i64,
    pub audit_logs: i64,
    pub has_more: bool,
}

#[async_trait]
pub trait RetentionCleanupStore: Send + Sync {
    /// dry_run 仅统计预计总量，不删除数据或追加审计。
    async fn cleanup_batch(
        &self,
        batch_size: i64,
        dry_run: bool,
    ) -> Result<RetentionBatch, UseCaseError>;
}

#[derive(Debug, Default, Serialize)]
pub struct RetentionResult {
    pub comment_ips: i64,
    pub audit_logs: i64,
    pub batches: u32,
    pub has_more: bool,
    pub dry_run: bool,
}

/// 使用独立维护身份装配；执行环境的恢复隔离守卫由部署入口负责。
pub struct RetentionMaintenance {
    store: Arc<dyn RetentionCleanupStore>,
}

impl RetentionMaintenance {
    pub fn new(store: Arc<dyn RetentionCleanupStore>) -> Self {
        Self { store }
    }

    pub async fn run(
        &self,
        batch_size: i64,
        max_batches: u32,
        dry_run: bool,
    ) -> Result<RetentionResult, UseCaseError> {
        if !(1..=10_000).contains(&batch_size) || !(1..=1000).contains(&max_batches) {
            return Err(UseCaseError::Invalid(
                "批量大小须为 1–10,000，批次数须为 1–1,000".into(),
            ));
        }
        let mut result = RetentionResult {
            dry_run,
            ..Default::default()
        };
        for _ in 0..max_batches {
            let batch = self.store.cleanup_batch(batch_size, dry_run).await?;
            result.comment_ips += batch.comment_ips;
            result.audit_logs += batch.audit_logs;
            if dry_run {
                return Ok(result);
            }
            result.batches += 1;
            result.has_more = batch.has_more;
            if !batch.has_more || batch.comment_ips + batch.audit_logs == 0 {
                break;
            }
        }
        Ok(result)
    }
}
