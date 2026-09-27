//! 在已有业务事务中追加审计；不另开连接或先于业务提交。

use std::net::IpAddr;

use application::error::UseCaseError;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

pub struct AuditEntry<'a> {
    pub actor_id: Option<Uuid>,
    pub ip_address: Option<IpAddr>,
    pub action: &'a str,
    pub target_type: &'a str,
    pub target_id: &'a str,
    /// 调用方仅传允许的摘要；不传密码、token、正文或个人联系信息。
    pub metadata: serde_json::Value,
}

/// Repository writes without an HTTP address still record the real actor (or
/// None for a controlled CLI/system task), never the target user as a fallback.
pub(crate) async fn record_change(
    transaction: &mut Transaction<'_, Postgres>,
    actor_id: Option<Uuid>,
    action: &str,
    target_type: &str,
    target_id: &str,
    metadata: serde_json::Value,
) -> Result<(), UseCaseError> {
    append_audit_log(
        transaction,
        AuditEntry {
            actor_id,
            ip_address: None,
            action,
            target_type,
            target_id,
            metadata,
        },
    )
    .await
}

pub async fn append_audit_log(
    transaction: &mut Transaction<'_, Postgres>,
    entry: AuditEntry<'_>,
) -> Result<(), UseCaseError> {
    if !entry.metadata.is_object() {
        return Err(UseCaseError::Invalid("审计摘要必须是 JSON 对象".into()));
    }
    sqlx::query(
        "INSERT INTO audit_logs (id, actor_id, ip_address, action, target_type, target_id, metadata) \
         VALUES ($1, $2, $3::text::inet, $4, $5, $6, $7)",
    )
    .bind(Uuid::now_v7())
    .bind(entry.actor_id)
    .bind(entry.ip_address.map(|ip| ip.to_string()))
    .bind(entry.action)
    .bind(entry.target_type)
    .bind(entry.target_id)
    .bind(entry.metadata)
    .execute(&mut **transaction)
    .await
    .map_err(|error| UseCaseError::Repository(format!("追加审计失败：{error}")))?;
    Ok(())
}
