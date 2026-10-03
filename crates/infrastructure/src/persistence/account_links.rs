use super::{
    acquire_identity_lock,
    sql::{map_row_error, map_sqlx_error},
};
use application::{
    UseCaseError,
    account_links::{AccountLinkStore, LINK_TTL, LinkTarget, invalid_link},
};
use async_trait::async_trait;
use sqlx::{PgPool, Row};
use time::OffsetDateTime;

pub struct PostgresAccountLinkStore {
    pool: PgPool,
}
impl PostgresAccountLinkStore {
    pub fn new(database: crate::Database) -> Self {
        Self {
            pool: database.pool,
        }
    }
}
const VALID: &str = "l.token_hash=$1 AND l.expires_at>$2 AND l.auth_version=u.auth_version AND l.email=u.email AND u.status='active' AND u.deleted_at IS NULL";
#[async_trait]
impl AccountLinkStore for PostgresAccountLinkStore {
    async fn issue(
        &self,
        target: LinkTarget<'_>,
        digest: &str,
        now: OffsetDateTime,
    ) -> Result<Option<String>, UseCaseError> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        acquire_identity_lock(&mut tx)
            .await
            .map_err(map_sqlx_error)?;
        let (row, actor, invitation) = match target {
            LinkTarget::Recovery(email) => {
                let row = sqlx::query("SELECT id,email,auth_version FROM users WHERE lower(email)=lower($1) AND status='active' AND deleted_at IS NULL AND password_hash IS NOT NULL FOR NO KEY UPDATE")
                    .bind(email).fetch_optional(&mut *tx).await.map_err(map_sqlx_error)?;
                (row, application::audit::AuditContext::system(), false)
            }
            LinkTarget::Invitation { user_id, actor } => {
                actor.ensure_write_channel()?;
                super::revalidate_write(&mut tx, &actor.audit_context()).await?;
                let permissions = if actor.channel
                    == application::identity::ActorChannel::ControlledCli
                    && actor.user_id.0.is_nil()
                {
                    actor.permissions().clone()
                } else {
                    crate::rbac::PostgresRbacStore::permissions_for(&mut *tx, actor.user_id.0)
                        .await?
                };
                if !permissions.has("user.manage") {
                    return Err(UseCaseError::Forbidden);
                }
                let is_admin: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM user_roles ur JOIN roles r ON r.id=ur.role_id WHERE ur.user_id=$1 AND r.code='admin')")
                    .bind(user_id).fetch_one(&mut *tx).await.map_err(map_sqlx_error)?;
                if is_admin && !permissions.has("admin.manage") {
                    return Err(UseCaseError::Forbidden);
                }
                let row = sqlx::query("SELECT id,email,auth_version FROM users WHERE id=$1 AND status='active' AND deleted_at IS NULL AND email IS NOT NULL AND password_hash IS NULL FOR NO KEY UPDATE")
                    .bind(user_id).fetch_optional(&mut *tx).await.map_err(map_sqlx_error)?;
                if row.is_none() {
                    return Err(UseCaseError::Invalid(
                        "邀请需要启用的账号、邮箱且尚未设置密码。".into(),
                    ));
                }
                (row, actor.audit_context(), true)
            }
        };
        let Some(row) = row else {
            return Ok(None);
        };
        let id: uuid::Uuid = row.try_get("id").map_err(map_row_error)?;
        let email: String = row.try_get("email").map_err(map_row_error)?;
        let auth_version: i64 = row.try_get("auth_version").map_err(map_row_error)?;
        let issued = sqlx::query("INSERT INTO account_links(user_id,token_hash,email,auth_version,issued_at,expires_at) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(user_id) DO UPDATE SET token_hash=excluded.token_hash,email=excluded.email,auth_version=excluded.auth_version,issued_at=excluded.issued_at,expires_at=excluded.expires_at WHERE account_links.issued_at <= $5 - interval '1 minute'")
            .bind(id).bind(digest).bind(&email).bind(auth_version).bind(now).bind(now+LINK_TTL)
            .execute(&mut *tx).await.map_err(map_sqlx_error)?.rows_affected() == 1;
        if !issued {
            if invitation {
                return Err(UseCaseError::RateLimited {
                    retry_after_secs: 60,
                });
            }
            return Ok(None);
        }
        crate::audit::append_audit_log(
            &mut tx,
            crate::audit::AuditEntry {
                actor_id: actor.actor_id,
                ip_address: actor.ip_address,
                action: if invitation {
                    "user.invitation.request"
                } else {
                    "user.password.recovery.request"
                },
                target_type: "user",
                target_id: &id.to_string(),
                metadata: serde_json::json!({}),
            },
        )
        .await?;
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(Some(email))
    }
    async fn username(
        &self,
        digest: &str,
        now: OffsetDateTime,
    ) -> Result<Option<String>, UseCaseError> {
        sqlx::query_scalar(&format!(
            "SELECT u.username FROM account_links l JOIN users u ON u.id=l.user_id WHERE {VALID}"
        ))
        .bind(digest)
        .bind(now)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_error)
    }
    async fn consume(
        &self,
        digest: &str,
        password_hash: &str,
        now: OffsetDateTime,
        ip: Option<std::net::IpAddr>,
    ) -> Result<(), UseCaseError> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        acquire_identity_lock(&mut tx)
            .await
            .map_err(map_sqlx_error)?;
        let id: uuid::Uuid = sqlx::query_scalar(&format!("SELECT u.id FROM account_links l JOIN users u ON u.id=l.user_id WHERE {VALID} FOR NO KEY UPDATE OF u"))
            .bind(digest).bind(now).fetch_optional(&mut *tx).await.map_err(map_sqlx_error)?.ok_or_else(invalid_link)?;
        sqlx::query("UPDATE users SET password_hash=$2,auth_version=auth_version+1,version=version+1,updated_at=$3 WHERE id=$1")
            .bind(id).bind(password_hash).bind(now).execute(&mut *tx).await.map_err(map_sqlx_error)?;
        sqlx::query("DELETE FROM sessions WHERE user_id=$1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        // Retain issuance timestamp for the one-minute limit after consumption;
        // expiry invalidates the bearer, a later request can replace it.
        sqlx::query("UPDATE account_links SET expires_at=$2 WHERE user_id=$1")
            .bind(id)
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        crate::audit::append_audit_log(
            &mut tx,
            crate::audit::AuditEntry {
                actor_id: Some(id),
                ip_address: ip,
                action: "user.password.recovery.complete",
                target_type: "user",
                target_id: &id.to_string(),
                metadata: serde_json::json!({}),
            },
        )
        .await?;
        tx.commit().await.map_err(map_sqlx_error)
    }
    async fn cancel(&self, digest: &str) -> Result<(), UseCaseError> {
        sqlx::query("UPDATE account_links SET expires_at=issued_at WHERE token_hash=$1")
            .bind(digest)
            .execute(&self.pool)
            .await
            .map_err(map_sqlx_error)?;
        Ok(())
    }
}
