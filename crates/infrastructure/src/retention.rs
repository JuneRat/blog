//! Bounded retention work, run by a separately authorized maintenance identity.
use crate::audit::{AuditEntry, append_audit_log};
use application::{
    error::UseCaseError,
    retention::{DEFAULT_RETENTION_DAYS, RetentionSettings, RetentionStore, validate_days},
};
use async_trait::async_trait;
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Row, Transaction};

fn db(e: sqlx::Error) -> UseCaseError {
    UseCaseError::Repository(e.to_string())
}
// The first key is shared with comment submission/global policy writes.
async fn lock(tx: &mut Transaction<'_, Postgres>, write: bool) -> Result<(), UseCaseError> {
    for key in [crate::locks::COMMENT_POLICY, crate::locks::AUDIT_POLICY] {
        crate::locks::acquire(&mut **tx, key, !write)
            .await
            .map_err(db)?;
    }
    Ok(())
}
fn days(value: &Value, key: &str) -> Result<i32, UseCaseError> {
    let days = match value.get(key) {
        None => DEFAULT_RETENTION_DAYS,
        Some(v) => v
            .as_i64()
            .and_then(|v| i32::try_from(v).ok())
            .ok_or_else(|| UseCaseError::Invalid(format!("{key} 必须为整数")))?,
    };
    validate_days(days)?;
    Ok(days)
}
async fn read(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<(RetentionSettings, Value, Value), UseCaseError> {
    let rows =
        sqlx::query("SELECT key,value,version FROM settings WHERE key IN ('comments','audit')")
            .fetch_all(&mut **tx)
            .await
            .map_err(db)?;
    let mut settings = RetentionSettings::default();
    let mut comments = json!({});
    let mut audit = json!({});
    for row in rows {
        if row.get::<&str, _>("key") == "comments" {
            comments = row.get("value");
            settings.comment_version = row.get("version");
        } else {
            audit = row.get("value");
            settings.audit_version = row.get("version");
        }
    }
    settings.comment_ip_days = days(&comments, "ip_retention_days")?;
    settings.audit_days = days(&audit, "retention_days")?;
    Ok((settings, comments, audit))
}
pub struct PostgresRetentionStore {
    pool: PgPool,
}
impl PostgresRetentionStore {
    pub fn new(database: crate::Database) -> Self {
        let pool = database.pool;
        Self { pool }
    }
}
#[async_trait]
impl RetentionStore for PostgresRetentionStore {
    async fn read(&self) -> Result<RetentionSettings, UseCaseError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let result = read(&mut tx).await?.0;
        tx.commit().await.map_err(db)?;
        Ok(result)
    }
    async fn save(
        &self,
        value: RetentionSettings,
        actor: application::audit::AuditContext,
    ) -> Result<RetentionSettings, UseCaseError> {
        validate_days(value.comment_ip_days)?;
        validate_days(value.audit_days)?;
        let mut tx = self.pool.begin().await.map_err(db)?;
        lock(&mut tx, true).await?;
        let (current, mut comments, mut audit) = read(&mut tx).await?;
        if value.comment_version != current.comment_version
            || value.audit_version != current.audit_version
        {
            return Err(UseCaseError::VersionConflict);
        }
        let mut result = current.clone();
        for (key, field, old, new, stored) in [
            (
                "comments",
                "ip_retention_days",
                current.comment_ip_days,
                value.comment_ip_days,
                &mut comments,
            ),
            (
                "audit",
                "retention_days",
                current.audit_days,
                value.audit_days,
                &mut audit,
            ),
        ] {
            if old == new {
                continue;
            }
            stored[field] = json!(new);
            let version:i64=sqlx::query_scalar("INSERT INTO settings(key,value) VALUES($1,$2) ON CONFLICT(key) DO UPDATE SET value=$2,version=settings.version+1,updated_at=now() RETURNING version")
                .bind(key).bind(&*stored).fetch_one(&mut *tx).await.map_err(db)?;
            append_audit_log(
                &mut tx,
                AuditEntry {
                    actor_id: actor.actor_id,
                    ip_address: actor.ip_address,
                    action: "settings.retention",
                    target_type: "settings",
                    target_id: key,
                    metadata: json!({"setting":field,"days":new,"version":version}),
                },
            )
            .await?;
            if key == "comments" {
                result.comment_ip_days = new;
                result.comment_version = version;
            } else {
                result.audit_days = new;
                result.audit_version = version;
            }
        }
        tx.commit().await.map_err(db)?;
        Ok(result)
    }
}
/// 独立维护连接执行单个原子批次；不迁移、不初始化权限、不启动发布任务。
pub struct PostgresRetentionCleanupStore {
    pool: PgPool,
    audit: application::audit::AuditContext,
    task_lease: Option<application::tasks::TaskLease>,
}
impl PostgresRetentionCleanupStore {
    pub fn new(database: crate::Database) -> Self {
        let pool = database.pool;
        Self {
            pool,
            audit: application::audit::AuditContext::system(),
            task_lease: None,
        }
    }
    pub fn with_audit(mut self, audit: application::audit::AuditContext) -> Self {
        self.audit = audit;
        self
    }
    pub fn with_task_lease(mut self, lease: application::tasks::TaskLease) -> Self {
        self.audit = lease.audit;
        self.task_lease = Some(lease);
        self
    }
}

#[async_trait]
impl application::retention::RetentionCleanupStore for PostgresRetentionCleanupStore {
    async fn cleanup_batch(
        &self,
        batch_size: i64,
        dry_run: bool,
    ) -> Result<application::retention::RetentionBatch, UseCaseError> {
        let mut result = application::retention::RetentionBatch::default();
        let mut tx = self.pool.begin().await.map_err(db)?;
        if let Some(lease) = &self.task_lease {
            if lease.run.kind != application::tasks::TaskKind::Retention {
                return Err(UseCaseError::Invalid("任务租约类型不匹配".into()));
            }
            crate::tasks::guard_execution(&mut tx, lease).await?;
        } else if !dry_run {
            crate::tasks::guard_writes(&mut tx).await?;
        }
        // Serialize cleaners without granting the audit maintenance role UPDATE
        // merely to use SELECT FOR UPDATE. Writers only append audit rows.
        crate::locks::acquire(&mut *tx, crate::locks::RETENTION_CLEANUP, false)
            .await
            .map_err(db)?;
        lock(&mut tx, false).await?;
        let (policy, _, _) = read(&mut tx).await?;
        if dry_run {
            result.comment_ips=sqlx::query_scalar("SELECT count(*) FROM comments WHERE ip_address IS NOT NULL AND created_at < now()-make_interval(days => $1)").bind(policy.comment_ip_days).fetch_one(&mut *tx).await.map_err(db)?;
            result.audit_logs=sqlx::query_scalar("SELECT count(*) FROM audit_logs WHERE created_at < now()-make_interval(days => $1)").bind(policy.audit_days).fetch_one(&mut *tx).await.map_err(db)?;
            tx.commit().await.map_err(db)?;
            return Ok(result);
        }
        let comments=sqlx::query("WITH expired AS (SELECT id FROM comments WHERE ip_address IS NOT NULL AND created_at < now()-make_interval(days => $1) ORDER BY created_at,id LIMIT $2 FOR UPDATE SKIP LOCKED) UPDATE comments c SET ip_address=NULL FROM expired e WHERE c.id=e.id")
            .bind(policy.comment_ip_days).bind(batch_size).execute(&mut *tx).await.map_err(db)?.rows_affected() as i64;
        let audits=sqlx::query("WITH expired AS (SELECT id FROM audit_logs WHERE created_at < now()-make_interval(days => $1) ORDER BY created_at,id LIMIT $2) DELETE FROM audit_logs a USING expired e WHERE a.id=e.id")
            .bind(policy.audit_days).bind(batch_size).execute(&mut *tx).await.map_err(db)?.rows_affected() as i64;
        if comments + audits > 0 {
            append_audit_log(&mut tx,AuditEntry{actor_id:self.audit.actor_id,ip_address:self.audit.ip_address,action:"maintenance.retention",target_type:"system",target_id:"retention",metadata:json!({"comment_ips":comments,"audit_logs":audits,"comment_ip_days":policy.comment_ip_days,"audit_days":policy.audit_days})}).await?;
        }
        result.has_more=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM comments WHERE ip_address IS NOT NULL AND created_at < now()-make_interval(days => $1)) OR EXISTS(SELECT 1 FROM audit_logs WHERE created_at < now()-make_interval(days => $2))")
            .bind(policy.comment_ip_days).bind(policy.audit_days).fetch_one(&mut *tx).await.map_err(db)?;
        tx.commit().await.map_err(db)?;
        result.comment_ips = comments;
        result.audit_logs = audits;
        Ok(result)
    }
}
