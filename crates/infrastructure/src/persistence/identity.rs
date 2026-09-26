use async_trait::async_trait;
use sqlx::{Executor, PgPool, Row};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::audit::{AuditEntry, append_audit_log};
use application::error::UseCaseError;
use application::ports::{AdminUserRow, ClearPasswordOutcome, PasswordCredential, UserRepository};
use domain::identity::{UserSnapshot, UserStatus};

use super::sql::{map_row_error, map_sqlx_error};

/// 身份/授权变更的统一排他锁键（docs/identity-and-admin.md §3）。
const IDENTITY_LOCK: (i32, i32) = (2048001, 1);

/// 取得身份/授权变更的统一排他锁。
///
/// 角色分配/移除、外部身份绑定解绑、密码清除共用同一把锁：这些操作的
/// 「检查 + 写入」必须在锁内完成，否则跨表不变量（例如「至少保留一种登录方式」）
/// 会被并发写穿——两条路径各自看到「对方还在」，结果一起把它清空。
pub(crate) async fn acquire_identity_lock(
    executor: impl Executor<'_, Database = sqlx::Postgres>,
) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT pg_advisory_xact_lock($1::int, $2::int)")
        .bind(IDENTITY_LOCK.0)
        .bind(IDENTITY_LOCK.1)
        .execute(executor)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 用户仓储
// ---------------------------------------------------------------------------

pub struct PostgresUserRepository {
    pool: PgPool,
}

impl PostgresUserRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

const USER_COLUMNS: &str = "id, username, email, display_name, avatar_media_id, bio, status, auth_version, version, created_at, updated_at, deleted_at";

fn user_from_row(row: &sqlx::postgres::PgRow) -> Result<UserSnapshot, UseCaseError> {
    Ok(UserSnapshot {
        id: row.try_get("id").map_err(map_row_error)?,
        username: row.try_get("username").map_err(map_row_error)?,
        email: row.try_get("email").map_err(map_row_error)?,
        display_name: row.try_get("display_name").map_err(map_row_error)?,
        avatar_media_id: row.try_get("avatar_media_id").map_err(map_row_error)?,
        bio: row.try_get("bio").map_err(map_row_error)?,
        status: user_status(row.try_get("status").map_err(map_row_error)?)?,
        auth_version: row.try_get("auth_version").map_err(map_row_error)?,
        version: row.try_get("version").map_err(map_row_error)?,
        created_at: row.try_get("created_at").map_err(map_row_error)?,
        updated_at: row.try_get("updated_at").map_err(map_row_error)?,
        deleted_at: row.try_get("deleted_at").map_err(map_row_error)?,
    })
}

#[async_trait]
impl UserRepository for PostgresUserRepository {
    async fn insert(&self, aggregate: &domain::identity::User) -> Result<(), UseCaseError> {
        let snapshot = aggregate.snapshot();
        sqlx::query(
            r#"
            INSERT INTO users (id, username, email, display_name, version, created_at, updated_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            "#,
        )
        .bind(snapshot.id)
        .bind(&snapshot.username)
        .bind(&snapshot.email)
        .bind(&snapshot.display_name)
        .bind(snapshot.version)
        .bind(snapshot.created_at)
        .bind(snapshot.updated_at)
        .execute(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(())
    }

    async fn find_by_id(&self, id: Uuid) -> Result<Option<UserSnapshot>, UseCaseError> {
        let row = sqlx::query(&format!("SELECT {USER_COLUMNS} FROM users WHERE id = $1"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(map_sqlx_error)?;
        row.as_ref().map(user_from_row).transpose()
    }

    async fn find_by_username(&self, username: &str) -> Result<Option<UserSnapshot>, UseCaseError> {
        let row = sqlx::query(&format!(
            "SELECT {USER_COLUMNS} FROM users WHERE lower(username) = lower($1)"
        ))
        .bind(username)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        row.as_ref().map(user_from_row).transpose()
    }

    async fn save_profile(
        &self,
        user: &domain::identity::User,
        expected_version: i64,
        now: OffsetDateTime,
    ) -> Result<UserSnapshot, UseCaseError> {
        let snapshot = user.snapshot();
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        let row = sqlx::query(&format!(
            "UPDATE users SET display_name=$2, bio=$3, version=version+1, updated_at=$4 \
             WHERE id=$1 AND version=$5 AND status='active' AND deleted_at IS NULL RETURNING {USER_COLUMNS}"
        ))
        .bind(snapshot.id).bind(snapshot.display_name).bind(snapshot.bio).bind(now).bind(expected_version)
        .fetch_optional(&mut *tx).await.map_err(map_sqlx_error)?
        .ok_or(UseCaseError::VersionConflict)?;
        let result = user_from_row(&row)?;
        append_audit_log(
            &mut tx,
            AuditEntry {
                actor_id: Some(snapshot.id),
                ip_address: None,
                action: "user.profile.update",
                target_type: "user",
                target_id: &snapshot.id.to_string(),
                metadata: serde_json::json!({"version": result.version}),
            },
        )
        .await?;
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(result)
    }

    async fn revoke_authentication(&self, user_id: Uuid) -> Result<(), UseCaseError> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        acquire_identity_lock(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        let changed = sqlx::query("UPDATE users SET auth_version=auth_version+1 WHERE id=$1")
            .bind(user_id)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        if changed.rows_affected() == 0 {
            return Err(UseCaseError::NotFound("用户".into()));
        }
        sqlx::query("DELETE FROM sessions WHERE user_id=$1")
            .bind(user_id)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        tx.commit().await.map_err(map_sqlx_error)
    }

    async fn set_avatar(
        &self,
        user_id: Uuid,
        avatar_media_id: Option<Uuid>,
        now: OffsetDateTime,
    ) -> Result<(), UseCaseError> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        let version: i64 = sqlx::query_scalar(
            "UPDATE users SET avatar_media_id=$2, version=version+1, updated_at=$3 \
             WHERE id=$1 AND status='active' AND deleted_at IS NULL RETURNING version",
        )
        .bind(user_id)
        .bind(avatar_media_id)
        .bind(now)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_error)?
        .ok_or_else(|| UseCaseError::NotFound("用户".into()))?;
        super::media::sync_media_refs(
            &mut tx,
            application::ports::MediaContentKind::User,
            user_id,
            &avatar_media_id.into_iter().collect::<Vec<_>>(),
        )
        .await?;
        append_audit_log(
            &mut tx,
            AuditEntry {
                actor_id: Some(user_id),
                ip_address: None,
                action: "user.avatar.update",
                target_type: "user",
                target_id: &user_id.to_string(),
                metadata: serde_json::json!({"version": version}),
            },
        )
        .await?;
        tx.commit().await.map_err(map_sqlx_error)
    }

    async fn list_admin(&self, limit: i64, offset: i64) -> Result<Vec<AdminUserRow>, UseCaseError> {
        // 登录方式与 RBAC 的最后 Owner 判定保持同一谓词（oauth 或 password_hash），
        // 否则界面会提示「可登录」而后端拒绝，两处定义漂移。
        let rows = sqlx::query(
            "SELECT u.id, u.username, u.email, u.display_name, \
                    u.status, (u.deleted_at IS NOT NULL) AS deleted, \
                    (u.password_hash IS NOT NULL) AS password_enabled, \
                    (SELECT count(*) FROM oauth_accounts oa WHERE oa.user_id = u.id) \
                        AS external_identities \
             FROM users u \
             ORDER BY u.username \
             LIMIT $1 OFFSET $2",
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;

        rows.iter()
            .map(|row| {
                Ok(AdminUserRow {
                    id: row.try_get("id").map_err(map_row_error)?,
                    username: row.try_get("username").map_err(map_row_error)?,
                    email: row.try_get("email").map_err(map_row_error)?,
                    display_name: row.try_get("display_name").map_err(map_row_error)?,
                    status: user_status(row.try_get("status").map_err(map_row_error)?)?,
                    deleted: row.try_get("deleted").map_err(map_row_error)?,
                    password_enabled: row.try_get("password_enabled").map_err(map_row_error)?,
                    external_identities: row
                        .try_get("external_identities")
                        .map_err(map_row_error)?,
                })
            })
            .collect()
    }

    async fn set_password_hash(&self, user_id: Uuid, phc_hash: &str) -> Result<(), UseCaseError> {
        self.change_password_hash(user_id, None, false, Some(phc_hash))
            .await?
            .ok_or_else(|| UseCaseError::NotFound("用户".into()))?;
        Ok(())
    }

    async fn compare_and_set_password_hash(
        &self,
        user_id: Uuid,
        expected: Option<&str>,
        new_hash: &str,
    ) -> Result<Option<i64>, UseCaseError> {
        self.change_password_hash(user_id, expected, true, Some(new_hash))
            .await
    }

    async fn clear_password_hash(&self, user_id: Uuid) -> Result<(), UseCaseError> {
        self.change_password_hash(user_id, None, false, None)
            .await?
            .ok_or_else(|| UseCaseError::NotFound("用户".into()))?;
        Ok(())
    }

    async fn clear_password_hash_guarded(
        &self,
        user_id: Uuid,
    ) -> Result<ClearPasswordOutcome, UseCaseError> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        // 与解绑外部身份同一把排他锁：检查与清除在锁内完成，两条路径不可能同时通过。
        acquire_identity_lock(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;

        let row: Option<(Option<String>,)> = sqlx::query_as(
            "SELECT password_hash FROM users WHERE id = $1 AND status = 'active' AND deleted_at IS NULL FOR UPDATE",
        )
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        let Some((hash,)) = row else {
            return Err(UseCaseError::NotFound("用户".into()));
        };
        if hash.is_none() {
            tx.commit().await.map_err(map_sqlx_error)?;
            return Ok(ClearPasswordOutcome::NoPassword);
        }

        let other: Option<(i32,)> =
            sqlx::query_as("SELECT 1 FROM oauth_accounts WHERE user_id = $1 LIMIT 1")
                .bind(user_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(map_sqlx_error)?;
        if other.is_none() {
            tx.commit().await.map_err(map_sqlx_error)?;
            return Ok(ClearPasswordOutcome::LastLoginMethod);
        }

        sqlx::query(
            "UPDATE users SET password_hash = NULL, version = version + 1, auth_version = auth_version + 1, updated_at = now() \
             WHERE id = $1",
        )
        .bind(user_id)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        sqlx::query("DELETE FROM sessions WHERE user_id=$1")
            .bind(user_id)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(ClearPasswordOutcome::Cleared)
    }

    async fn find_password_credential(
        &self,
        username: &str,
    ) -> Result<Option<PasswordCredential>, UseCaseError> {
        // 软删除用户在查询层排除：登录失败路径因此无法区分「不存在」与「已停用」。
        // auth_version 一并读出：会话签发时绑定，跨进程改密也能让旧会话失效。
        let row = sqlx::query_as::<_, (Uuid, String, i64)>(
            "SELECT id, password_hash, auth_version FROM users \
             WHERE lower(username) = lower($1) AND password_hash IS NOT NULL AND status = 'active' AND deleted_at IS NULL",
        )
        .bind(username)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(row.map(
            |(user_id, password_hash, auth_version)| PasswordCredential {
                user_id,
                password_hash,
                auth_version,
            },
        ))
    }

    async fn password_hash_of(&self, user_id: Uuid) -> Result<Option<String>, UseCaseError> {
        let row = sqlx::query_scalar::<_, Option<String>>(
            "SELECT password_hash FROM users WHERE id = $1 AND status = 'active' AND deleted_at IS NULL",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(row.flatten())
    }
}

fn user_status(value: &str) -> Result<UserStatus, UseCaseError> {
    match value {
        "active" => Ok(UserStatus::Active),
        "disabled" => Ok(UserStatus::Disabled),
        _ => Err(UseCaseError::Repository("无效用户状态".into())),
    }
}

impl PostgresUserRepository {
    async fn change_password_hash(
        &self,
        user_id: Uuid,
        expected: Option<&str>,
        check_expected: bool,
        new_hash: Option<&str>,
    ) -> Result<Option<i64>, UseCaseError> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        acquire_identity_lock(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        let revision = sqlx::query_scalar(
            "UPDATE users SET password_hash=$4, version=version+1, auth_version=auth_version+1, updated_at=now() \
             WHERE id=$1 AND status='active' AND deleted_at IS NULL \
             AND (NOT $2 OR password_hash IS NOT DISTINCT FROM $3) RETURNING auth_version"
        ).bind(user_id).bind(check_expected).bind(expected).bind(new_hash)
            .fetch_optional(&mut *tx).await.map_err(map_sqlx_error)?;
        if revision.is_some() {
            sqlx::query("DELETE FROM sessions WHERE user_id=$1")
                .bind(user_id)
                .execute(&mut *tx)
                .await
                .map_err(map_sqlx_error)?;
        }
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(revision)
    }
}
