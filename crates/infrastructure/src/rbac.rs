//! PostgreSQL RBAC 适配器：权限目录同步、角色分配与授权查询。
//!
//! 身份/角色变更遵守统一事务锁协议（docs/identity-and-admin.md §3）：
//! 排他 pg_advisory_xact_lock(2048001, 1) 内完成变更与 Owner 检查；
//! 角色分配/移除递增 users.version。

use async_trait::async_trait;
use sqlx::{Executor, PgPool, Row};
use uuid::Uuid;

use application::error::UseCaseError;
use application::identity::{BuiltinRoleDef, PermissionDescriptor};
use application::ports::{RbacStore, RoleDto};
use domain::identity::PermissionSet;

/// 身份/授权变更的统一排他锁键（int, int 形式）。
const IDENTITY_LOCK: (i64, i64) = (2048001, 1);

pub struct PostgresRbacStore {
    pool: PgPool,
}

impl PostgresRbacStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl PostgresRbacStore {
    fn map_err(error: sqlx::Error) -> UseCaseError {
        UseCaseError::Repository(error.to_string())
    }

    async fn role_id_by_slug(&self, slug: &str) -> Result<Option<Uuid>, UseCaseError> {
        let row: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM roles WHERE slug = $1")
            .bind(slug)
            .fetch_optional(&self.pool)
            .await
            .map_err(Self::map_err)?;
        Ok(row.map(|r| r.0))
    }

    /// 未删除且仍持有 owner 角色的用户数。
    async fn active_owner_count(
        &self,
        executor: impl Executor<'_, Database = sqlx::Postgres>,
    ) -> Result<i64, UseCaseError> {
        let (count,): (i64,) = sqlx::query_as(
            "SELECT count(*) \
             FROM user_roles ur \
             JOIN roles r ON r.id = ur.role_id \
             JOIN users u ON u.id = ur.user_id \
             WHERE r.slug = 'owner' AND u.deleted_at IS NULL",
        )
        .fetch_one(executor)
        .await
        .map_err(Self::map_err)?;
        Ok(count)
    }
}

#[async_trait]
impl RbacStore for PostgresRbacStore {
    async fn sync_permission_registry(
        &self,
        entries: &[PermissionDescriptor],
    ) -> Result<(), UseCaseError> {
        for entry in entries {
            sqlx::query(
                "INSERT INTO permissions (id, name, key, description) VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (key) DO UPDATE SET name = EXCLUDED.name, description = EXCLUDED.description",
            )
            .bind(Uuid::now_v7())
            .bind(entry.name)
            .bind(entry.key)
            .bind(entry.description)
            .execute(&self.pool)
            .await
            .map_err(Self::map_err)?;
        }
        Ok(())
    }

    async fn sync_builtin_roles(&self, defs: &[BuiltinRoleDef]) -> Result<(), UseCaseError> {
        for def in defs {
            let mut tx = self.pool.begin().await.map_err(Self::map_err)?;

            // 内置 slug 保留：按 slug upsert，不可改名绕过。
            sqlx::query(
                "INSERT INTO roles (id, name, slug, description, version, created_at, updated_at) \
                 VALUES ($1, $2, $3, $4, 1, now(), now()) \
                 ON CONFLICT (slug) DO UPDATE SET name = EXCLUDED.name, description = EXCLUDED.description",
            )
            .bind(Uuid::now_v7())
            .bind(def.name)
            .bind(def.slug)
            .bind(def.description)
            .execute(&mut *tx)
            .await
            .map_err(Self::map_err)?;

            let (role_id,): (Uuid,) = sqlx::query_as("SELECT id FROM roles WHERE slug = $1")
                .bind(def.slug)
                .fetch_one(&mut *tx)
                .await
                .map_err(Self::map_err)?;

            // 对齐授权集合：先删多余，再补缺失；有变化才递增 roles.version。
            sqlx::query(
                "DELETE FROM role_permissions rp \
                 WHERE rp.role_id = $1 \
                 AND rp.permission_id NOT IN (SELECT id FROM permissions WHERE key = ANY($2))",
            )
            .bind(role_id)
            .bind(def.permissions)
            .execute(&mut *tx)
            .await
            .map_err(Self::map_err)?;

            sqlx::query(
                "INSERT INTO role_permissions (role_id, permission_id) \
                 SELECT $1, p.id FROM permissions p WHERE p.key = ANY($2) \
                 ON CONFLICT DO NOTHING",
            )
            .bind(role_id)
            .bind(def.permissions)
            .execute(&mut *tx)
            .await
            .map_err(Self::map_err)?;

            sqlx::query(
                "UPDATE roles SET version = version + 1, updated_at = now() \
                 WHERE id = $1 AND EXISTS (SELECT 1 FROM role_permissions WHERE role_id = $1)",
            )
            .bind(role_id)
            .execute(&mut *tx)
            .await
            .map_err(Self::map_err)?;

            tx.commit().await.map_err(Self::map_err)?;
        }
        Ok(())
    }

    async fn permissions_of_user(&self, user_id: Uuid) -> Result<PermissionSet, UseCaseError> {
        let keys: Vec<(String,)> = sqlx::query_as(
            "SELECT DISTINCT p.key \
             FROM users u \
             JOIN user_roles ur ON ur.user_id = u.id \
             JOIN roles r ON r.id = ur.role_id \
             JOIN role_permissions rp ON rp.role_id = r.id \
             JOIN permissions p ON p.id = rp.permission_id \
             WHERE u.id = $1 AND u.deleted_at IS NULL",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(Self::map_err)?;
        Ok(PermissionSet::from_keys(keys.into_iter().map(|k| k.0)))
    }

    async fn assign_role(&self, user_id: Uuid, role_slug: &str) -> Result<(), UseCaseError> {
        let mut tx = self.pool.begin().await.map_err(Self::map_err)?;
        sqlx::query("SELECT pg_advisory_xact_lock($1::int, $2::int)")
            .bind(IDENTITY_LOCK.0 as i32)
            .bind(IDENTITY_LOCK.1 as i32)
            .execute(&mut *tx)
            .await
            .map_err(Self::map_err)?;

        let role_id = self
            .role_id_by_slug(role_slug)
            .await?
            .ok_or_else(|| UseCaseError::NotFound(format!("角色 {role_slug}")))?;

        let result = sqlx::query(
            "INSERT INTO user_roles (user_id, role_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        )
        .bind(user_id)
        .bind(role_id)
        .execute(&mut *tx)
        .await
        .map_err(Self::map_err)?;

        if result.rows_affected() > 0 {
            sqlx::query("UPDATE users SET version = version + 1, updated_at = now() WHERE id = $1")
                .bind(user_id)
                .execute(&mut *tx)
                .await
                .map_err(Self::map_err)?;
        }
        tx.commit().await.map_err(Self::map_err)
    }

    async fn remove_role(&self, user_id: Uuid, role_slug: &str) -> Result<(), UseCaseError> {
        let mut tx = self.pool.begin().await.map_err(Self::map_err)?;
        sqlx::query("SELECT pg_advisory_xact_lock($1::int, $2::int)")
            .bind(IDENTITY_LOCK.0 as i32)
            .bind(IDENTITY_LOCK.1 as i32)
            .execute(&mut *tx)
            .await
            .map_err(Self::map_err)?;

        let role_id = self
            .role_id_by_slug(role_slug)
            .await?
            .ok_or_else(|| UseCaseError::NotFound(format!("角色 {role_slug}")))?;

        // 最后 Owner 保护：删除前在排他锁内复核剩余数量。
        if role_slug == "owner" {
            let owners = self.active_owner_count(&mut *tx).await?;
            if owners <= 1 {
                return Err(UseCaseError::Forbidden);
            }
        }

        let result = sqlx::query("DELETE FROM user_roles WHERE user_id = $1 AND role_id = $2")
            .bind(user_id)
            .bind(role_id)
            .execute(&mut *tx)
            .await
            .map_err(Self::map_err)?;

        if result.rows_affected() > 0 {
            sqlx::query("UPDATE users SET version = version + 1, updated_at = now() WHERE id = $1")
                .bind(user_id)
                .execute(&mut *tx)
                .await
                .map_err(Self::map_err)?;
        }
        tx.commit().await.map_err(Self::map_err)
    }

    async fn list_roles(&self) -> Result<Vec<RoleDto>, UseCaseError> {
        let rows = sqlx::query(
            "SELECT r.slug, r.name, r.description, count(rp.permission_id) AS permission_count \
             FROM roles r \
             LEFT JOIN role_permissions rp ON rp.role_id = r.id \
             GROUP BY r.id, r.slug, r.name, r.description \
             ORDER BY r.slug",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(Self::map_err)?;

        rows.iter()
            .map(|row| {
                Ok(RoleDto {
                    slug: row.try_get("slug").map_err(Self::map_err)?,
                    name: row.try_get("name").map_err(Self::map_err)?,
                    description: row.try_get("description").map_err(Self::map_err)?,
                    builtin: false, // 由应用层按 BUILTIN_ROLES 标注
                    permission_count: row.try_get("permission_count").map_err(Self::map_err)?,
                })
            })
            .collect()
    }

    async fn roles_of_user(&self, user_id: Uuid) -> Result<Vec<String>, UseCaseError> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT r.slug FROM user_roles ur JOIN roles r ON r.id = ur.role_id \
             WHERE ur.user_id = $1 ORDER BY r.slug",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(Self::map_err)?;
        Ok(rows.into_iter().map(|r| r.0).collect())
    }
}
