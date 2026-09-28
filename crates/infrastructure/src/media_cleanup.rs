//! PostgreSQL implementation of the explicit media purge commit protocol.
mod files;
pub use files::{LocalMediaPurgeFiles, LocalMediaPurgePlans};

use application::{UseCaseError, media_cleanup::*};
use async_trait::async_trait;
use serde_json::{Value, json};
use sqlx::{PgConnection, Row, postgres::PgRow};
use uuid::Uuid;

pub struct PostgresMediaPurgeStore {
    database: crate::Database,
    legacy_container: Option<String>,
}
impl PostgresMediaPurgeStore {
    pub fn new(database: crate::Database, legacy_container: Option<String>) -> Self {
        Self {
            database,
            legacy_container,
        }
    }
    async fn identity_on(
        &self,
        connection: &mut PgConnection,
    ) -> Result<DatabaseIdentity, UseCaseError> {
        let row = sqlx::query("SELECT datname,oid::text,shobj_description(oid,'pg_database') AS guard FROM pg_database WHERE datname=current_database()")
            .fetch_one(connection).await.map_err(db)?;
        if row
            .get::<Option<String>, _>("guard")
            .is_some_and(|v| v.starts_with("blog:recovery-isolated:"))
        {
            return Err(invalid(
                "media purge is disabled while the database is recovery-isolated",
            ));
        }
        let options = self.database.pool.connect_options();
        let endpoint = match &self.legacy_container {
            Some(container) => DatabaseEndpoint::Container {
                container: container.clone(),
            },
            None => DatabaseEndpoint::Tcp {
                host: options.get_host().into(),
                port: options.get_port().to_string(),
            },
        };
        Ok(DatabaseIdentity {
            name: row.get("datname"),
            oid: row.get("oid"),
            endpoint,
        })
    }
}

const COLUMNS: &str = "id,path,size,checksum_sha256,version,to_char(deleted_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS deleted_at";
// Explicit foreign keys protect references even if their bookkeeping entry was lost.
const REFERENCED: &str = "SELECT EXISTS(SELECT 1 FROM media_refs WHERE media_id=$1) OR EXISTS(SELECT 1 FROM users WHERE avatar_media_id=$1) OR EXISTS(SELECT 1 FROM posts WHERE cover_media_id=$1) OR EXISTS(SELECT 1 FROM series WHERE cover_media_id=$1) OR EXISTS(SELECT 1 FROM settings WHERE key='site' AND (value->>'logo_media_id')::uuid=$1)";
const RECEIPT: &str = "EXISTS(SELECT 1 FROM audit_logs WHERE action='media.purge' AND target_type='media' AND target_id=$1 AND metadata @> $2::jsonb)";

fn item(row: PgRow) -> PurgeItem {
    PurgeItem {
        id: row.get("id"),
        path: row.get("path"),
        size: row.get("size"),
        sha256: row.get("checksum_sha256"),
        version: row.get("version"),
        deleted_at: row.get("deleted_at"),
    }
}
fn metadata(plan: &VerifiedPlan, item: &PurgeItem) -> Value {
    json!({"operation_id":plan.plan.operation_id,"plan_sha256":plan.sha256,
        "path":item.path,"sha256":item.sha256,"size":item.size,"version":item.version})
}
async fn receipt(
    connection: &mut PgConnection,
    plan: &VerifiedPlan,
    item: &PurgeItem,
) -> Result<bool, UseCaseError> {
    sqlx::query_scalar(&format!("SELECT {RECEIPT}"))
        .bind(item.id.to_string())
        .bind(metadata(plan, item))
        .fetch_one(connection)
        .await
        .map_err(db)
}

#[async_trait]
impl MediaPurgeStore for PostgresMediaPurgeStore {
    async fn identity(&self) -> Result<DatabaseIdentity, UseCaseError> {
        self.identity_on(&mut *self.database.pool.acquire().await.map_err(db)?)
            .await
    }
    async fn candidates(&self, ids: &[Uuid]) -> Result<Vec<PurgeItem>, UseCaseError> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM media WHERE id=ANY($1) ORDER BY id"
        ))
        .bind(ids)
        .fetch_all(&self.database.pool)
        .await
        .map_err(db)?;
        let items: Vec<_> = rows.into_iter().map(item).collect();
        for item in &items {
            if item.deleted_at.is_none() {
                return Err(invalid("move media to trash before planning purge"));
            }
            if sqlx::query_scalar::<_, bool>(REFERENCED)
                .bind(item.id)
                .fetch_one(&self.database.pool)
                .await
                .map_err(db)?
            {
                return Err(invalid("media is referenced"));
            }
        }
        Ok(items)
    }
    async fn has_receipt(
        &self,
        plan: &VerifiedPlan,
        item: &PurgeItem,
    ) -> Result<bool, UseCaseError> {
        receipt(
            &mut *self.database.pool.acquire().await.map_err(db)?,
            plan,
            item,
        )
        .await
    }
    async fn commit(&self, plan: &VerifiedPlan) -> Result<(), UseCaseError> {
        let mut tx = self.database.pool.begin().await.map_err(db)?;
        sqlx::query("SET LOCAL lock_timeout='10s'")
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        if self.identity_on(&mut tx).await? != plan.plan.database {
            return Err(invalid("plan belongs to a different database/endpoint"));
        }
        let mut items: Vec<_> = plan.plan.items.iter().collect();
        items.sort_by_key(|item| item.id);
        for expected in items {
            let row = sqlx::query(&format!(
                "SELECT {COLUMNS} FROM media WHERE id=$1 FOR UPDATE"
            ))
            .bind(expected.id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(db)?;
            let committed = receipt(&mut tx, plan, expected).await?;
            if let Some(row) = row {
                if committed {
                    return Err(invalid("media record reappeared after purge"));
                }
                if item(row) != *expected {
                    return Err(invalid("stale media plan"));
                }
                if sqlx::query_scalar::<_, bool>(REFERENCED)
                    .bind(expected.id)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(db)?
                {
                    return Err(invalid("media is referenced"));
                }
                sqlx::query("DELETE FROM media WHERE id=$1")
                    .bind(expected.id)
                    .execute(&mut *tx)
                    .await
                    .map_err(db)?;
                sqlx::query("INSERT INTO audit_logs(id,action,target_type,target_id,metadata) VALUES($1,'media.purge','media',$2,$3::jsonb || jsonb_build_object('database_role',current_user))")
                    .bind(Uuid::now_v7()).bind(expected.id.to_string()).bind(metadata(plan,expected))
                    .execute(&mut *tx).await.map_err(db)?;
            } else if !committed {
                return Err(invalid("missing media without this plan receipt"));
            }
        }
        tx.commit().await.map_err(db)
    }
    async fn may_remove_file(
        &self,
        plan: &VerifiedPlan,
        item: &PurgeItem,
    ) -> Result<bool, UseCaseError> {
        sqlx::query_scalar(&format!(
            "SELECT {RECEIPT} AND NOT EXISTS(SELECT 1 FROM media WHERE id=$1::uuid OR path=$3)"
        ))
        .bind(item.id.to_string())
        .bind(metadata(plan, item))
        .bind(&item.path)
        .fetch_one(&self.database.pool)
        .await
        .map_err(db)
    }
}
fn db(error: sqlx::Error) -> UseCaseError {
    UseCaseError::Repository(error.to_string())
}
fn invalid(message: &str) -> UseCaseError {
    UseCaseError::Invalid(message.into())
}
