use super::sql::map_sqlx_error;
use application::{
    UseCaseError,
    audit::AuditContext,
    registration::{AccessPolicy, RegistrationStore},
};
use async_trait::async_trait;
use domain::identity::User;
use sqlx::{Executor, PgPool, Postgres};

pub struct PostgresRegistrationStore {
    pool: PgPool,
}
impl PostgresRegistrationStore {
    pub fn new(database: crate::Database) -> Self {
        Self {
            pool: database.pool,
        }
    }
}

pub(crate) async fn access_policy(
    executor: impl Executor<'_, Database = Postgres>,
) -> Result<AccessPolicy, UseCaseError> {
    let row: Option<(sqlx::types::Json<AccessPolicy>, i64)> =
        sqlx::query_as("SELECT value, version FROM settings WHERE key='access'")
            .fetch_optional(executor)
            .await
            .map_err(map_sqlx_error)?;
    Ok(row
        .map(|(value, version)| AccessPolicy { version, ..value.0 })
        .unwrap_or_default())
}

#[async_trait]
impl RegistrationStore for PostgresRegistrationStore {
    async fn policy(&self) -> Result<AccessPolicy, UseCaseError> {
        access_policy(&self.pool).await
    }
    async fn save_policy(
        &self,
        mut policy: AccessPolicy,
        audit: AuditContext,
    ) -> Result<AccessPolicy, UseCaseError> {
        let mut tx = crate::persistence::begin_authorized_write(&self.pool, &audit).await?;
        crate::locks::acquire(&mut *tx, crate::locks::ACCESS_POLICY, false)
            .await
            .map_err(map_sqlx_error)?;
        let current = access_policy(&mut *tx).await?;
        if current.version != policy.version {
            return Err(UseCaseError::VersionConflict);
        }
        if current != policy {
            policy.version = sqlx::query_scalar("INSERT INTO settings(key,value) VALUES('access',$1) ON CONFLICT(key) DO UPDATE SET value=$1, version=settings.version+1, updated_at=now() RETURNING version")
                .bind(sqlx::types::Json(&policy)).fetch_one(&mut *tx).await.map_err(map_sqlx_error)?;
            crate::audit::record_change(&mut tx, audit, "settings.access", "settings", "access",
                serde_json::json!({"registration_enabled":policy.registration_enabled,"guest_comments_enabled":policy.guest_comments_enabled,"version":policy.version})).await?;
        }
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(policy)
    }
    async fn register(
        &self,
        user: &User,
        password_hash: &str,
        audit: AuditContext,
    ) -> Result<(), UseCaseError> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        super::identity::acquire_identity_lock(&mut tx)
            .await
            .map_err(map_sqlx_error)?;
        crate::locks::acquire(&mut *tx, crate::locks::ACCESS_POLICY, true)
            .await
            .map_err(map_sqlx_error)?;
        if !access_policy(&mut *tx).await?.registration_enabled {
            return Err(UseCaseError::RegistrationClosed);
        }
        let reader: uuid::Uuid = sqlx::query_scalar("SELECT id FROM roles WHERE code='reader'")
            .fetch_one(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        let snapshot = user.snapshot();
        sqlx::query("INSERT INTO users(id,username,email,display_name,password_hash,created_at,updated_at) VALUES($1,$2,$3,$4,$5,$6,$6)")
            .bind(snapshot.id).bind(snapshot.username).bind(snapshot.email).bind(snapshot.display_name)
            .bind(password_hash).bind(snapshot.created_at).execute(&mut *tx).await.map_err(map_sqlx_error)?;
        sqlx::query("INSERT INTO user_roles(user_id,role_id) VALUES($1,$2)")
            .bind(snapshot.id)
            .bind(reader)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        crate::audit::record_change(
            &mut tx,
            audit,
            "user.register",
            "user",
            &snapshot.id.to_string(),
            serde_json::json!({"role":"reader"}),
        )
        .await?;
        tx.commit().await.map_err(map_sqlx_error)
    }
}
