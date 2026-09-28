//! PostgreSQL RBAC 适配器：权限目录同步、角色分配与授权查询。
//!
//! 身份/角色变更遵守统一事务锁协议（docs/identity-and-admin.md §3）：
//! 排他 pg_advisory_xact_lock(2048001, 1) 内完成变更与 Owner 检查；
//! 角色分配/移除递增 users.version。

use crate::audit::record_change;
use async_trait::async_trait;
use sqlx::{Executor, PgPool, Row};
use uuid::Uuid;

use application::error::UseCaseError;
use application::identity::{BuiltinRoleDef, OWNER_ROLE_SLUG, PermissionDescriptor};
use application::ports::{RbacStore, RoleDto};
use domain::identity::PermissionSet;

/// SQL 谓词中的 `oa` 是 oauth_accounts 行。只匹配当前配置的身份命名空间，
/// 不读取 secret_ref 指向的秘密，也不把第三方在线状态引入事务。
pub(crate) const CONFIGURED_EXTERNAL_IDENTITY: &str = "EXISTS (\
    SELECT 1 FROM settings s, jsonb_array_elements(COALESCE(s.value->'providers', '[]'::jsonb)) p \
    WHERE s.key='oauth' AND oa.provider = CASE p->'kind'->>'type' \
        WHEN 'oidc' THEN p->>'issuer' WHEN 'github' THEN 'github' END)";

pub struct PostgresRbacStore {
    pool: PgPool,
}

impl PostgresRbacStore {
    pub fn new(database: crate::Database) -> Self {
        let pool = database.pool;
        Self { pool }
    }
}

impl PostgresRbacStore {
    fn map_err(error: sqlx::Error) -> UseCaseError {
        UseCaseError::Repository(error.to_string())
    }

    /// 锁内取角色 id：必须复用同一事务连接（pool 只有 5 条连接，
    /// 持锁时再从池里取连接会与并发身份操作互等直至 acquire 超时）。
    async fn role_id_by_slug(
        executor: impl Executor<'_, Database = sqlx::Postgres>,
        slug: &str,
    ) -> Result<Option<Uuid>, UseCaseError> {
        let row: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM roles WHERE code = $1")
            .bind(slug)
            .fetch_optional(executor)
            .await
            .map_err(Self::map_err)?;
        Ok(row.map(|r| r.0))
    }

    /// 未删除、仍持有 owner 角色、且仍有有效登录方式的用户数。
    /// docs §3：可能减少有效 Owner 的操作在排他锁下检查至少保留一个「可登录」Owner。
    ///
    /// 「有效登录方式」= 至少一条匹配当前提供商的 oauth_accounts **或** 本地密码
    /// （`users.password_hash IS NOT NULL`）。两者是对等登录方式，缺一不可，
    /// 否则只用密码的 Owner 会被判成「登不进去」而被移除，站点直接失去 Owner。
    pub(crate) async fn active_owner_count(
        executor: impl Executor<'_, Database = sqlx::Postgres>,
    ) -> Result<i64, UseCaseError> {
        let (count,): (i64,) = sqlx::query_as(&format!(
            "SELECT count(*) \
             FROM user_roles ur \
             JOIN roles r ON r.id = ur.role_id \
             JOIN users u ON u.id = ur.user_id \
             WHERE r.code = 'owner' AND u.status = 'active' AND u.deleted_at IS NULL \
               AND (u.password_hash IS NOT NULL \
                    OR EXISTS (SELECT 1 FROM oauth_accounts oa WHERE oa.user_id = u.id \
                               AND {CONFIGURED_EXTERNAL_IDENTITY}))",
        ))
        .fetch_one(executor)
        .await
        .map_err(Self::map_err)?;
        Ok(count)
    }

    /// 用户是否实际持有某角色（决定移除时是否触发最后 Owner 保护）。
    async fn user_holds_role(
        executor: impl Executor<'_, Database = sqlx::Postgres>,
        user_id: Uuid,
        role_id: Uuid,
    ) -> Result<bool, UseCaseError> {
        let row: Option<(i32,)> =
            sqlx::query_as("SELECT 1 FROM user_roles WHERE user_id = $1 AND role_id = $2")
                .bind(user_id)
                .bind(role_id)
                .fetch_optional(executor)
                .await
                .map_err(Self::map_err)?;
        Ok(row.is_some())
    }

    /// 目标用户是否仍有有效登录方式（已配置的外部身份或本地密码至少其一）。
    ///
    /// 返回 false 会让 `remove_role` 跳过最后 Owner 保护——对，这是有意的：
    /// 「登不进去的 Owner」不构成有效 Owner，可以被清理。因此这个谓词必须
    /// 与 `active_owner_count` 用同一套定义，否则两处判定会互相矛盾。
    pub(crate) async fn user_has_login_method(
        executor: impl Executor<'_, Database = sqlx::Postgres>,
        user_id: Uuid,
    ) -> Result<bool, UseCaseError> {
        let row: Option<(i32,)> = sqlx::query_as(&format!(
            "SELECT 1 FROM users u \
             WHERE u.id = $1 AND u.status = 'active' AND u.deleted_at IS NULL \
               AND (u.password_hash IS NOT NULL \
                    OR EXISTS (SELECT 1 FROM oauth_accounts oa WHERE oa.user_id = u.id \
                               AND {CONFIGURED_EXTERNAL_IDENTITY})) \
             LIMIT 1",
        ))
        .bind(user_id)
        .fetch_optional(executor)
        .await
        .map_err(Self::map_err)?;
        Ok(row.is_some())
    }

    /// 支持在调用方身份事务中复核权限，避免持锁时另取连接。
    pub(crate) async fn permissions_for(
        executor: impl Executor<'_, Database = sqlx::Postgres>,
        user_id: Uuid,
    ) -> Result<PermissionSet, UseCaseError> {
        let keys: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT p.code \
             FROM users u \
             JOIN user_roles ur ON ur.user_id = u.id \
             JOIN roles r ON r.id = ur.role_id \
             JOIN role_permissions rp ON rp.role_id = r.id \
             JOIN permissions p ON p.code = rp.permission_code \
             WHERE u.id = $1 AND u.status = 'active' AND u.deleted_at IS NULL",
        )
        .bind(user_id)
        .fetch_all(executor)
        .await
        .map_err(Self::map_err)?;
        Ok(PermissionSet::from_keys(keys))
    }
}

#[async_trait]
impl RbacStore for PostgresRbacStore {
    async fn sync_permission_registry(
        &self,
        entries: &[PermissionDescriptor],
    ) -> Result<(), UseCaseError> {
        let mut tx = self.pool.begin().await.map_err(Self::map_err)?;
        crate::persistence::acquire_identity_lock(&mut *tx)
            .await
            .map_err(Self::map_err)?;
        let mut changed = 0u64;
        for entry in entries {
            changed += sqlx::query(
                "INSERT INTO permissions (code, name) VALUES ($1, $2) \
                 ON CONFLICT (code) DO UPDATE SET name = EXCLUDED.name \
                 WHERE permissions.name IS DISTINCT FROM EXCLUDED.name",
            )
            .bind(entry.key)
            .bind(entry.name)
            .execute(&mut *tx)
            .await
            .map_err(Self::map_err)?
            .rows_affected();
        }
        if changed > 0 {
            record_change(
                &mut tx,
                application::audit::AuditContext::system(),
                "permissions.sync",
                "system",
                "permissions",
                serde_json::json!({"changed":changed}),
            )
            .await?;
        }
        tx.commit().await.map_err(Self::map_err)
    }

    async fn sync_builtin_roles(&self, defs: &[BuiltinRoleDef]) -> Result<(), UseCaseError> {
        for def in defs {
            let mut tx = self.pool.begin().await.map_err(Self::map_err)?;
            crate::persistence::acquire_identity_lock(&mut *tx)
                .await
                .map_err(Self::map_err)?;

            let existing: Option<(Uuid, String, Option<String>)> =
                sqlx::query_as("SELECT id, name, description FROM roles WHERE code = $1")
                    .bind(def.slug)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(Self::map_err)?;

            let Some((role_id, name, description)) = existing else {
                // 新建内置角色：version 从 1 起（首次创建不算「修改授权集合」）。
                let role_id = Uuid::now_v7();
                sqlx::query(
                    "INSERT INTO roles (id, name, code, description, version, created_at, updated_at) \
                     VALUES ($1, $2, $3, $4, 1, now(), now())",
                )
                .bind(role_id)
                .bind(def.name)
                .bind(def.slug)
                .bind(def.description)
                .execute(&mut *tx)
                .await
                .map_err(Self::map_err)?;
                sqlx::query(
                    "INSERT INTO role_permissions (role_id, permission_code) \
                     SELECT $1, p.code FROM permissions p WHERE p.code = ANY($2) \
                     ON CONFLICT DO NOTHING",
                )
                .bind(role_id)
                .bind(def.permissions)
                .execute(&mut *tx)
                .await
                .map_err(Self::map_err)?;
                record_change(
                    &mut tx,
                    application::audit::AuditContext::system(),
                    "role.create",
                    "role",
                    &role_id.to_string(),
                    serde_json::json!({
                        "code": def.slug, "version": 1,
                        "permission_count": def.permissions.len(),
                    }),
                )
                .await?;
                tx.commit().await.map_err(Self::map_err)?;
                continue;
            };

            // 已存在：名称/描述/授权集合完全一致时不动任何行（roles.version 不漂移）。
            let current: Vec<(String,)> = sqlx::query_as(
                "SELECT p.code FROM role_permissions rp \
                 JOIN permissions p ON p.code = rp.permission_code \
                 WHERE rp.role_id = $1",
            )
            .bind(role_id)
            .fetch_all(&mut *tx)
            .await
            .map_err(Self::map_err)?;
            let mut current_keys: Vec<&str> = current.iter().map(|k| k.0.as_str()).collect();
            let mut desired: Vec<&str> = def.permissions.to_vec();
            current_keys.sort_unstable();
            desired.sort_unstable();

            let unchanged = name == def.name
                && description.as_deref() == Some(def.description)
                && current_keys == desired;
            if unchanged {
                tx.commit().await.map_err(Self::map_err)?;
                continue;
            }

            // 有变化才对齐授权集合并递增 roles.version。
            sqlx::query(
                "UPDATE roles SET name = $2, description = $3, version = version + 1, updated_at = now() \
                 WHERE id = $1",
            )
            .bind(role_id)
            .bind(def.name)
            .bind(def.description)
            .execute(&mut *tx)
            .await
            .map_err(Self::map_err)?;

            sqlx::query(
                "DELETE FROM role_permissions rp \
                 WHERE rp.role_id = $1 \
                 AND rp.permission_code NOT IN (SELECT code FROM permissions WHERE code = ANY($2))",
            )
            .bind(role_id)
            .bind(def.permissions)
            .execute(&mut *tx)
            .await
            .map_err(Self::map_err)?;

            sqlx::query(
                "INSERT INTO role_permissions (role_id, permission_code) \
                 SELECT $1, p.code FROM permissions p WHERE p.code = ANY($2) \
                 ON CONFLICT DO NOTHING",
            )
            .bind(role_id)
            .bind(def.permissions)
            .execute(&mut *tx)
            .await
            .map_err(Self::map_err)?;

            let version: i64 = sqlx::query_scalar("SELECT version FROM roles WHERE id=$1")
                .bind(role_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(Self::map_err)?;
            record_change(
                &mut tx,
                application::audit::AuditContext::system(),
                "role.sync",
                "role",
                &role_id.to_string(),
                serde_json::json!({
                    "code": def.slug, "version": version,
                    "permission_count": def.permissions.len(),
                }),
            )
            .await?;
            tx.commit().await.map_err(Self::map_err)?;
        }
        Ok(())
    }

    async fn permissions_of_user(&self, user_id: Uuid) -> Result<PermissionSet, UseCaseError> {
        Self::permissions_for(&self.pool, user_id).await
    }

    async fn permissions_of_role(&self, role_slug: &str) -> Result<PermissionSet, UseCaseError> {
        let role_id = Self::role_id_by_slug(&self.pool, role_slug)
            .await?
            .ok_or_else(|| UseCaseError::NotFound(format!("角色 {role_slug}")))?;
        let keys: Vec<(String,)> = sqlx::query_as(
            "SELECT p.code FROM role_permissions rp \
             JOIN permissions p ON p.code = rp.permission_code \
             WHERE rp.role_id = $1",
        )
        .bind(role_id)
        .fetch_all(&self.pool)
        .await
        .map_err(Self::map_err)?;
        Ok(PermissionSet::from_keys(keys.into_iter().map(|k| k.0)))
    }

    async fn assign_role(
        &self,
        user_id: Uuid,
        role_slug: &str,
        audit_actor: application::audit::AuditContext,
    ) -> Result<(), UseCaseError> {
        let mut tx = self.pool.begin().await.map_err(Self::map_err)?;
        crate::persistence::acquire_identity_lock(&mut *tx)
            .await
            .map_err(Self::map_err)?;

        // 锁取得后复用同一事务连接重新读取（docs §3）。
        let role_id = Self::role_id_by_slug(&mut *tx, role_slug)
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
            let version: i64 = sqlx::query_scalar("SELECT version FROM users WHERE id=$1")
                .bind(user_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(Self::map_err)?;
            record_change(
                &mut tx,
                audit_actor,
                "user.role.assign",
                "user",
                &user_id.to_string(),
                serde_json::json!({"role_id":role_id,"role":role_slug,"version":version}),
            )
            .await?;
        }
        tx.commit().await.map_err(Self::map_err)
    }

    async fn remove_role(
        &self,
        user_id: Uuid,
        role_slug: &str,
        audit_actor: application::audit::AuditContext,
    ) -> Result<(), UseCaseError> {
        let mut tx = self.pool.begin().await.map_err(Self::map_err)?;
        crate::persistence::acquire_identity_lock(&mut *tx)
            .await
            .map_err(Self::map_err)?;

        // 锁取得后复用同一事务连接重新读取（docs §3）。
        let role_id = Self::role_id_by_slug(&mut *tx, role_slug)
            .await?
            .ok_or_else(|| UseCaseError::NotFound(format!("角色 {role_slug}")))?;

        let holds_role = Self::user_holds_role(&mut *tx, user_id, role_id).await?;

        // 最后 Owner 保护：目标确实持有 owner 且仍有登录方式时，
        // 于排他锁内复核至少保留一个「可登录」Owner（移除登不进去的 Owner 不受限）。
        if holds_role && role_slug == OWNER_ROLE_SLUG {
            let target_has_login = Self::user_has_login_method(&mut *tx, user_id).await?;
            if target_has_login {
                let owners = Self::active_owner_count(&mut *tx).await?;
                application::identity::policy::ensure_owner_removal_allowed(owners)?;
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
            let version: i64 = sqlx::query_scalar("SELECT version FROM users WHERE id=$1")
                .bind(user_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(Self::map_err)?;
            record_change(
                &mut tx,
                audit_actor,
                "user.role.remove",
                "user",
                &user_id.to_string(),
                serde_json::json!({"role_id":role_id,"role":role_slug,"version":version}),
            )
            .await?;
        }
        tx.commit().await.map_err(Self::map_err)
    }

    async fn list_roles(&self) -> Result<Vec<RoleDto>, UseCaseError> {
        let rows = sqlx::query(
            "SELECT r.code, r.name, r.description, count(rp.permission_code) AS permission_count \
             FROM roles r \
             LEFT JOIN role_permissions rp ON rp.role_id = r.id \
             GROUP BY r.id, r.code, r.name, r.description \
             ORDER BY r.code",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(Self::map_err)?;

        rows.iter()
            .map(|row| {
                Ok(RoleDto {
                    slug: row.try_get("code").map_err(Self::map_err)?,
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
            "SELECT r.code FROM user_roles ur JOIN roles r ON r.id = ur.role_id \
             WHERE ur.user_id = $1 ORDER BY r.code",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(Self::map_err)?;
        Ok(rows.into_iter().map(|r| r.0).collect())
    }

    async fn roles_of_users(&self, user_ids: &[Uuid]) -> Result<Vec<(Uuid, String)>, UseCaseError> {
        // 空 IN 列表直接短路：`= ANY('{}')` 虽合法，但省一次往返更清晰。
        if user_ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows: Vec<(Uuid, String)> = sqlx::query_as(
            "SELECT ur.user_id, r.code FROM user_roles ur JOIN roles r ON r.id = ur.role_id \
             WHERE ur.user_id = ANY($1) ORDER BY ur.user_id, r.code",
        )
        .bind(user_ids)
        .fetch_all(&self.pool)
        .await
        .map_err(Self::map_err)?;
        Ok(rows)
    }

    async fn loginable_owner_count(&self) -> Result<i64, UseCaseError> {
        // 复用 `remove_role` 保护使用的同一私有查询与谓词，避免两处定义漂移。
        Self::active_owner_count(&self.pool).await
    }
}
