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
        actor: uuid::Uuid,
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
        self.store.save(value, actor.user_id.0).await
    }
}
fn authorize(actor: &Actor) -> Result<(), UseCaseError> {
    if !actor.has_permission("settings.manage") {
        return Err(UseCaseError::Forbidden);
    }
    Ok(())
}
