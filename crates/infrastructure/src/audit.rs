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
