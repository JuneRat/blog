//! Order authenticated writes against cross-process identity revocation.
//! Acquire identity before any business lock; retain it through commit/rollback.
use application::{UseCaseError, audit::AuditContext, identity::ActorChannel};
use sqlx::{PgConnection, PgPool, Postgres, Transaction};

use super::sql::map_sqlx_error;

pub(crate) async fn begin_authorized_write<'a>(
    pool: &'a PgPool,
    context: &AuditContext,
) -> Result<Transaction<'a, Postgres>, UseCaseError> {
    let mut tx = pool.begin().await.map_err(map_sqlx_error)?;
    if context.authorization.is_some() {
        sqlx::query("SET TRANSACTION ISOLATION LEVEL READ COMMITTED")
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        crate::locks::acquire(&mut *tx, crate::locks::IDENTITY, true)
            .await
            .map_err(map_sqlx_error)?;
        revalidate_write(&mut tx, context).await?;
    }
    Ok(tx)
}

/// Caller already holds the shared/exclusive transaction identity lock.
/// Authentication must be read after lock acquisition (READ COMMITTED).
pub(crate) async fn revalidate_write(
    conn: &mut PgConnection,
    context: &AuditContext,
) -> Result<(), UseCaseError> {
    let Some(expected) = &context.authorization else {
        return Ok(());
    };
    let actor_id = context.actor_id.ok_or(UseCaseError::Forbidden)?;
    let version: Option<i64> = sqlx::query_scalar(
        "SELECT auth_version FROM users WHERE id=$1 AND status='active' AND deleted_at IS NULL",
    )
    .bind(actor_id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_sqlx_error)?;
    if version != Some(expected.auth_version) {
        return Err(match expected.channel {
            ActorChannel::Session => UseCaseError::Unauthenticated,
            ActorChannel::ControlledCli => UseCaseError::Forbidden,
        });
    }
    let current = crate::rbac::PostgresRbacStore::permissions_for(conn, actor_id).await?;
    // Keeping the complete permission set used by the use case makes this
    // conservative: a removed permission rejects the old request even when
    // it was unrelated to this action. Added permissions do not invalidate it.
    if !current.contains_all(&expected.permissions) {
        return Err(UseCaseError::Forbidden);
    }
    Ok(())
}
