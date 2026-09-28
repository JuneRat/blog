//! 在已有业务事务中追加审计；不另开连接或先于业务提交。

use std::net::IpAddr;

use application::audit::{AuditField, AuditFilter, AuditPage, AuditQueryStore, AuditRecord};
use application::error::UseCaseError;
use async_trait::async_trait;
use sqlx::{PgPool, QueryBuilder, Row};
use sqlx::{Postgres, Transaction};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

pub struct PostgresAuditQuery {
    pool: PgPool,
}
impl PostgresAuditQuery {
    pub fn new(database: crate::Database) -> Self {
        let pool = database.pool;
        Self { pool }
    }
}
#[async_trait]
impl AuditQueryStore for PostgresAuditQuery {
    async fn list(&self, filter: &AuditFilter) -> Result<AuditPage, UseCaseError> {
        let mut sql = QueryBuilder::<Postgres>::new(
            "SELECT a.id,a.created_at,a.actor_id,COALESCE(NULLIF(u.display_name,''),u.username) AS actor_display,\
             host(a.ip_address) AS ip_address,a.action,a.target_type,a.target_id,a.metadata \
             FROM audit_logs a LEFT JOIN users u ON u.id=a.actor_id WHERE true",
        );
        if let Some(action) = &filter.action {
            sql.push(" AND a.action=").push_bind(action);
        }
        if let Some(actor) = filter.actor_id {
            sql.push(" AND a.actor_id=").push_bind(actor);
        }
        if filter.without_actor {
            sql.push(" AND a.actor_id IS NULL");
        }
        if let Some(kind) = &filter.target_type {
            sql.push(" AND a.target_type=").push_bind(kind);
        }
        if let Some(id) = &filter.target_id {
            sql.push(" AND a.target_id=").push_bind(id);
        }
        if let Some(from) = filter.from {
            sql.push(" AND a.created_at>=").push_bind(from);
        }
        if let Some(until) = filter.until {
            sql.push(" AND a.created_at<").push_bind(until);
        }
        if let Some((at, id)) = filter.before {
            sql.push(" AND (a.created_at,a.id)<(")
                .push_bind(at)
                .push(",")
                .push_bind(id)
                .push(")");
        }
        sql.push(" ORDER BY a.created_at DESC,a.id DESC LIMIT ")
            .push_bind(i64::from(filter.limit) + 1);
        let mut rows = sql
            .build()
            .fetch_all(&self.pool)
            .await
            .map_err(query_error)?;
        let has_more = rows.len() > filter.limit as usize;
        rows.truncate(filter.limit as usize);
        let mut items = Vec::with_capacity(rows.len());
        for row in rows {
            let at: OffsetDateTime = row.try_get("created_at").map_err(query_error)?;
            let metadata: serde_json::Value = row.try_get("metadata").map_err(query_error)?;
            let summary = metadata
                .as_object()
                .into_iter()
                .flat_map(|m| m.iter())
                .map(|(key, value)| AuditField {
                    key: key.clone(),
                    value: value
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| value.to_string()),
                })
                .collect();
            items.push(AuditRecord {
                id: row.try_get("id").map_err(query_error)?,
                created_at: at
                    .format(&Rfc3339)
                    .map_err(|e| UseCaseError::Repository(e.to_string()))?,
                actor_id: row.try_get("actor_id").map_err(query_error)?,
                actor_display: row.try_get("actor_display").map_err(query_error)?,
                ip_address: row.try_get("ip_address").map_err(query_error)?,
                action: row.try_get("action").map_err(query_error)?,
                target_type: row.try_get("target_type").map_err(query_error)?,
                target_id: row.try_get("target_id").map_err(query_error)?,
                summary,
            });
        }
        let next_cursor = items
            .last()
            .filter(|_| has_more)
            .map(|item| format!("{}|{}", item.created_at, item.id));
        Ok(AuditPage { items, next_cursor })
    }
}
fn query_error(error: sqlx::Error) -> UseCaseError {
    UseCaseError::Repository(error.to_string())
}

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
    context: application::audit::AuditContext,
    action: &str,
    target_type: &str,
    target_id: &str,
    metadata: serde_json::Value,
) -> Result<(), UseCaseError> {
    append_audit_log(
        transaction,
        AuditEntry {
            actor_id: context.actor_id,
            ip_address: context.ip_address,
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
