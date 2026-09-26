use async_trait::async_trait;
use sqlx::{Executor, PgPool, Row};
use time::OffsetDateTime;
use uuid::Uuid;

use application::error::UseCaseError;
use application::ports::{
    AdminUserRow, ClearPasswordOutcome, MediaContentKind, PasswordCredential, UserRepository,
};
use domain::identity::UserSnapshot;

use super::media::{media_ids_for, sync_media_refs};
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

const USER_COLUMNS: &str = "id, username, email, display_name, avatar_media_id, version, created_at, updated_at, deleted_at";

fn user_from_row(row: &sqlx::postgres::PgRow) -> Result<UserSnapshot, UseCaseError> {
    Ok(UserSnapshot {
        id: row.try_get("id").map_err(map_row_error)?,
        username: row.try_get("username").map_err(map_row_error)?,
        email: row.try_get("email").map_err(map_row_error)?,
        display_name: row.try_get("display_name").map_err(map_row_error)?,
        avatar_media_id: row.try_get("avatar_media_id").map_err(map_row_error)?,
        version: row.try_get("version").map_err(map_row_error)?,
        created_at: row.try_get("created_at").map_err(map_row_error)?,
        updated_at: row.try_get("updated_at").map_err(map_row_error)?,
        deleted_at: row.try_get("deleted_at").map_err(map_row_error)?,
    })
}

#[async_trait]
impl UserRepository for PostgresUserRepository {
    async fn insert(&self, snapshot: &UserSnapshot) -> Result<(), UseCaseError> {
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
            "SELECT {USER_COLUMNS} FROM users WHERE username = $1"
        ))
        .bind(username)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        row.as_ref().map(user_from_row).transpose()
    }

    /// 设置/清除头像：列与引用行在同一事务整体替换。
    ///
    /// 有意不动 `users.version`（会话绑定版本）：换头像不该让本人所有会话失效。
    async fn set_avatar(
        &self,
        user_id: Uuid,
        avatar_media_id: Option<Uuid>,
        now: OffsetDateTime,
    ) -> Result<(), UseCaseError> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        let updated = sqlx::query(
            "UPDATE users SET avatar_media_id = $2, updated_at = $3 \
             WHERE id = $1 AND deleted_at IS NULL RETURNING id",
        )
        .bind(user_id)
        .bind(avatar_media_id)
        .bind(now)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        if updated.is_none() {
            // 用户不存在/已软删除：回滚而不是提交空事务——语义上是失败的写入。
            tx.rollback().await.map_err(map_sqlx_error)?;
            return Err(UseCaseError::NotFound("用户".into()));
        }
        // 引用集合由新头像推导；资产不可用则整次回滚（列与引用都不落库）。
        sync_media_refs(
            &mut tx,
            MediaContentKind::User,
            user_id,
            &media_ids_for(&[], avatar_media_id),
        )
        .await?;
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(())
    }

    async fn list_admin(&self, limit: i64, offset: i64) -> Result<Vec<AdminUserRow>, UseCaseError> {
        // 登录方式与 RBAC 的最后 Owner 判定保持同一谓词（oauth 或 password_hash），
        // 否则界面会提示「可登录」而后端拒绝，两处定义漂移。
        let rows = sqlx::query(
            "SELECT u.id, u.username, u.email, u.display_name, \
                    (u.deleted_at IS NOT NULL) AS deleted, \
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
        // 身份材料变更同步递增 version，与角色/绑定变更保持同一可观察语义。
        let result = sqlx::query(
            "UPDATE users SET password_hash = $2, version = version + 1, updated_at = now() \
             WHERE id = $1 AND deleted_at IS NULL",
        )
        .bind(user_id)
        .bind(phc_hash)
        .execute(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        if result.rows_affected() == 0 {
            return Err(UseCaseError::NotFound("用户".into()));
        }
        Ok(())
    }

    async fn compare_and_set_password_hash(
        &self,
        user_id: Uuid,
        expected: Option<&str>,
        new_hash: &str,
    ) -> Result<Option<i64>, UseCaseError> {
        // 条件更新（compare-and-swap）：期望值不匹配就不写。
        // `expected = None` 表示「当前必须为空」（OAuth 用户设置初始密码）。
        // 这样并发的自助改密/登录升级/管理员重置不会互相覆盖。
        sqlx::query_scalar(
            "UPDATE users SET password_hash = $3, version = version + 1, updated_at = now() \
             WHERE id = $1 AND deleted_at IS NULL \
               AND (($2::text IS NULL AND password_hash IS NULL) OR password_hash = $2) \
             RETURNING version",
        )
        .bind(user_id)
        .bind(expected)
        .bind(new_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_error)
    }

    async fn clear_password_hash(&self, user_id: Uuid) -> Result<(), UseCaseError> {
        let result = sqlx::query(
            "UPDATE users SET password_hash = NULL, version = version + 1, updated_at = now() \
             WHERE id = $1 AND deleted_at IS NULL",
        )
        .bind(user_id)
        .execute(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        if result.rows_affected() == 0 {
            return Err(UseCaseError::NotFound("用户".into()));
        }
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
            "SELECT password_hash FROM users WHERE id = $1 AND deleted_at IS NULL FOR UPDATE",
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
            "UPDATE users SET password_hash = NULL, version = version + 1, updated_at = now() \
             WHERE id = $1",
        )
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
        // version 一并读出：会话签发时绑定，跨进程改密也能让旧会话失效。
        let row = sqlx::query_as::<_, (Uuid, String, i64)>(
            "SELECT id, password_hash, version FROM users \
             WHERE username = $1 AND password_hash IS NOT NULL AND deleted_at IS NULL",
        )
        .bind(username)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(
            row.map(|(user_id, password_hash, version)| PasswordCredential {
                user_id,
                password_hash,
                version,
            }),
        )
    }

    async fn password_hash_of(&self, user_id: Uuid) -> Result<Option<String>, UseCaseError> {
        let row = sqlx::query_scalar::<_, Option<String>>(
            "SELECT password_hash FROM users WHERE id = $1 AND deleted_at IS NULL",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(row.flatten())
    }
}
