//! PostgreSQL 持久化适配器：连接、迁移与端口实现。
//!
//! - UUID 由应用生成（v7，时间有序利于索引局部性）。
//! - updated_at 由应用写入；SQL 端不依赖 DEFAULT now() 更新。
//! - 写侧使用条件更新（WHERE version = expected）实现乐观并发。

use async_trait::async_trait;
use sqlx::postgres::{PgDatabaseError, PgPoolOptions};
use sqlx::{Executor, PgPool, Row};
use time::OffsetDateTime;
use uuid::Uuid;

use application::error::{ConflictKind, UseCaseError};
use application::ports::{
    AdminUserRow, CategoryDeleteOutcome, CategoryRepository, CategoryWithUsage,
    ClearPasswordOutcome, Clock, HealthCheck, PageRepository, PasswordCredential, PostRepository,
    PublicCategoryRef, PublicCategorySummary, PublicPageDetail, PublicPostDetail,
    PublicPostSummary, PublicSeriesRef, PublicSeriesSummary, PublicTagSummary, PublicUrlEntry,
    PublishedCategoryQuery, PublishedPageQuery, PublishedPostQuery, PublishedSeriesQuery,
    PublishedTagQuery, ReorderOutcome, SaveOutcome, SeriesDeleteOutcome, SeriesMember,
    SeriesRepository, SeriesWithUsage, TagDeleteOutcome, TagRepository, TagWithUsage,
    UserRepository,
};
use domain::content::page::{PageSnapshot, PageStatus};
use domain::content::post::{PostSnapshot, PostStatus, Visibility};
use domain::identity::UserSnapshot;

/// 匿名公开条件（与 docs/database-design.md §5 保持一致）。
const POST_PUBLIC_PREDICATE: &str =
    "p.status = 'published' AND p.visibility = 'public' AND p.deleted_at IS NULL";

/// 页面没有 deleted_at：公开条件只有状态与可见性。
const PAGE_PUBLIC_PREDICATE: &str = "p.status = 'published' AND p.visibility = 'public'";

/// 身份/授权变更的统一排他锁键（docs/identity-and-admin.md §3）。
pub(crate) const IDENTITY_LOCK: (i32, i32) = (2048001, 1);

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

pub async fn connect(url: &str) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(5)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .connect(url)
        .await
}

/// 从目录加载并执行迁移（sqlx 布局 `<version>_<description>.sql`，每条自动包事务）。
/// 默认目录 migrations/postgres；测试可指向同一路径。
pub async fn migrate(
    pool: &PgPool,
    migrations_dir: impl AsRef<std::path::Path>,
) -> Result<(), sqlx::migrate::MigrateError> {
    let migrator = sqlx::migrate::Migrator::new(migrations_dir.as_ref()).await?;
    migrator.run(pool).await
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }
}

/// readiness 探针：真实执行 SELECT 1，连接池不可用时报告不健康。
pub struct PgHealthCheck {
    pool: PgPool,
}

impl PgHealthCheck {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl HealthCheck for PgHealthCheck {
    async fn check(&self) -> bool {
        sqlx::query_scalar::<_, i32>("SELECT 1")
            .fetch_one(&self.pool)
            .await
            .is_ok()
    }
}

// ---------------------------------------------------------------------------
// 错误映射
// ---------------------------------------------------------------------------

/// 唯一约束名 → 结构化冲突原因。
///
/// 这里只做「约束名到枚举」的翻译；是否给某个原因分配独立业务码由接口层决定。
/// 未知约束回 `Unknown`：仍然拒绝写入，只是不给前端编造字段名。
fn unique_conflict_target(error: &PgDatabaseError) -> Option<ConflictKind> {
    match error.constraint()? {
        "posts_slug_key" | "pages_slug_key" => Some(ConflictKind::Slug),
        "users_username_key" => Some(ConflictKind::Username),
        "users_email_key" => Some(ConflictKind::Email),
        "posts_series_position_unique" => Some(ConflictKind::SeriesPosition),
        "oauth_accounts_provider_provider_user_id_key" => Some(ConflictKind::ExternalIdentity),
        "categories_slug_key" | "series_slug_key" | "tags_slug_key" => Some(ConflictKind::Slug),
        "roles_slug_key" => Some(ConflictKind::RoleSlug),
        "permissions_key_key" => Some(ConflictKind::PermissionKey),
        _ => None,
    }
}

fn map_sqlx_error(error: sqlx::Error) -> UseCaseError {
    if let sqlx::Error::Database(db) = &error {
        let pg = db.try_downcast_ref::<PgDatabaseError>();
        if let Some(pg) = pg {
            if pg.code() == "23505" {
                return UseCaseError::Conflict(
                    unique_conflict_target(pg).unwrap_or(ConflictKind::Unknown),
                );
            }
            // 文章关联不存在的标签：用例层已前置校验，这里是并发删除标签的兜底，
            // 翻译成可定位的参数错误而不是裸存储错误。
            if pg.code() == "23503" && pg.constraint() == Some("post_tags_tag_id_fkey") {
                return UseCaseError::Invalid("所选标签不存在或刚被删除".into());
            }
            // 同理：并发删除分类时的 FK 兜底。
            if pg.code() == "23503" && pg.constraint() == Some("posts_category_id_fkey") {
                return UseCaseError::Invalid("所选分类不存在或刚被删除".into());
            }
        }
    }
    UseCaseError::Repository(error.to_string())
}

fn map_row_error(error: sqlx::Error) -> UseCaseError {
    UseCaseError::Repository(format!("行映射失败：{error}"))
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

const USER_COLUMNS: &str =
    "id, username, email, display_name, version, created_at, updated_at, deleted_at";

fn user_from_row(row: &sqlx::postgres::PgRow) -> Result<UserSnapshot, UseCaseError> {
    Ok(UserSnapshot {
        id: row.try_get("id").map_err(map_row_error)?,
        username: row.try_get("username").map_err(map_row_error)?,
        email: row.try_get("email").map_err(map_row_error)?,
        display_name: row.try_get("display_name").map_err(map_row_error)?,
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

// ---------------------------------------------------------------------------
// 文章仓储
// ---------------------------------------------------------------------------

pub struct PostgresPostRepository {
    pool: PgPool,
}

impl PostgresPostRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

const POST_COLUMNS: &str = "id, author_id, category_id, series_id, title, slug, excerpt, content, \
     cover, series_order, status, visibility, published_at, version, created_at, updated_at, deleted_at";

fn post_from_row(row: &sqlx::postgres::PgRow) -> Result<PostSnapshot, UseCaseError> {
    let status: String = row.try_get("status").map_err(map_row_error)?;
    let visibility: String = row.try_get("visibility").map_err(map_row_error)?;
    Ok(PostSnapshot {
        id: row.try_get("id").map_err(map_row_error)?,
        author_id: row.try_get("author_id").map_err(map_row_error)?,
        category_id: row.try_get("category_id").map_err(map_row_error)?,
        series_id: row.try_get("series_id").map_err(map_row_error)?,
        title: row.try_get("title").map_err(map_row_error)?,
        slug: row.try_get("slug").map_err(map_row_error)?,
        excerpt: row.try_get("excerpt").map_err(map_row_error)?,
        content: row.try_get("content").map_err(map_row_error)?,
        cover: row.try_get("cover").map_err(map_row_error)?,
        series_order: row.try_get("series_order").map_err(map_row_error)?,
        status: PostStatus::parse(&status)
            .ok_or_else(|| UseCaseError::Repository(format!("未知文章状态 {status}")))?,
        visibility: Visibility::parse(&visibility)
            .ok_or_else(|| UseCaseError::Repository(format!("未知可见性 {visibility}")))?,
        published_at: row.try_get("published_at").map_err(map_row_error)?,
        version: row.try_get("version").map_err(map_row_error)?,
        created_at: row.try_get("created_at").map_err(map_row_error)?,
        updated_at: row.try_get("updated_at").map_err(map_row_error)?,
        deleted_at: row.try_get("deleted_at").map_err(map_row_error)?,
    })
}

async fn post_lifecycle_miss(
    pool: &PgPool,
    id: Uuid,
    expected_deleted: bool,
) -> Result<SaveOutcome, UseCaseError> {
    let current: Option<(bool,)> =
        sqlx::query_as("SELECT deleted_at IS NOT NULL FROM posts WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await
            .map_err(map_sqlx_error)?;
    Ok(match current {
        Some((deleted,)) if deleted == expected_deleted => SaveOutcome::StaleConflict,
        _ => SaveOutcome::Gone,
    })
}

#[async_trait]
impl PostRepository for PostgresPostRepository {
    async fn find_by_slug(&self, slug: &str) -> Result<Option<PostSnapshot>, UseCaseError> {
        let row = sqlx::query(&format!("SELECT {POST_COLUMNS} FROM posts WHERE slug = $1"))
            .bind(slug)
            .fetch_optional(&self.pool)
            .await
            .map_err(map_sqlx_error)?;
        row.as_ref().map(post_from_row).transpose()
    }

    async fn find_by_id(&self, id: Uuid) -> Result<Option<PostSnapshot>, UseCaseError> {
        let row = sqlx::query(&format!("SELECT {POST_COLUMNS} FROM posts WHERE id = $1"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(map_sqlx_error)?;
        row.as_ref().map(post_from_row).transpose()
    }

    async fn list_by_author(&self, author_id: Uuid) -> Result<Vec<PostSnapshot>, UseCaseError> {
        let rows = sqlx::query(&format!(
            "SELECT {POST_COLUMNS} FROM posts WHERE author_id = $1 AND deleted_at IS NULL ORDER BY updated_at DESC, id DESC"
        ))
        .bind(author_id)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        rows.iter().map(post_from_row).collect()
    }

    async fn list_trash_by_author(
        &self,
        author_id: Uuid,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<PostSnapshot>, i64), UseCaseError> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        let (total,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM posts WHERE author_id = $1 AND deleted_at IS NOT NULL",
        )
        .bind(author_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        let rows = sqlx::query(&format!("SELECT {POST_COLUMNS} FROM posts WHERE author_id = $1 AND deleted_at IS NOT NULL ORDER BY deleted_at DESC, id DESC LIMIT $2 OFFSET $3"))
            .bind(author_id).bind(limit).bind(offset).fetch_all(&mut *tx).await.map_err(map_sqlx_error)?;
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok((
            rows.iter().map(post_from_row).collect::<Result<_, _>>()?,
            total,
        ))
    }

    async fn trash(
        &self,
        id: Uuid,
        expected_version: i64,
        now: OffsetDateTime,
    ) -> Result<SaveOutcome, UseCaseError> {
        let row: Option<(i64,)> = sqlx::query_as("UPDATE posts SET deleted_at = $3, updated_at = $3, version = version + 1 WHERE id = $1 AND version = $2 AND deleted_at IS NULL RETURNING version")
            .bind(id).bind(expected_version).bind(now).fetch_optional(&self.pool).await.map_err(map_sqlx_error)?;
        match row {
            Some((new_version,)) => Ok(SaveOutcome::Saved { new_version }),
            None => post_lifecycle_miss(&self.pool, id, false).await,
        }
    }

    async fn restore(
        &self,
        id: Uuid,
        expected_version: i64,
        now: OffsetDateTime,
    ) -> Result<SaveOutcome, UseCaseError> {
        let row: Option<(i64,)> = sqlx::query_as("UPDATE posts SET deleted_at = NULL, status = CASE WHEN status = 'archived' THEN 'archived' ELSE 'draft' END, updated_at = $3, version = version + 1 WHERE id = $1 AND version = $2 AND deleted_at IS NOT NULL RETURNING version")
            .bind(id).bind(expected_version).bind(now).fetch_optional(&self.pool).await.map_err(map_sqlx_error)?;
        match row {
            Some((new_version,)) => Ok(SaveOutcome::Saved { new_version }),
            None => post_lifecycle_miss(&self.pool, id, true).await,
        }
    }

    async fn purge(&self, id: Uuid, expected_version: i64) -> Result<SaveOutcome, UseCaseError> {
        // 与加入、退出、重排同一锁序：先 series 行，后 post 行。
        // 无锁预读仅用于确定锁目标；锁后再次核对，迁移过的成员重新尝试。
        for _ in 0..5 {
            let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
            let prior: Option<(Option<Uuid>,)> =
                sqlx::query_as("SELECT series_id FROM posts WHERE id = $1")
                    .bind(id)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(map_sqlx_error)?;
            let Some((series_id,)) = prior else {
                return Ok(SaveOutcome::Gone);
            };
            if let Some(series_id) = series_id {
                sqlx::query("SELECT id FROM series WHERE id = $1 FOR UPDATE")
                    .bind(series_id)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(map_sqlx_error)?;
            }
            let locked: Option<(Option<Uuid>, i64, bool)> = sqlx::query_as("SELECT series_id, version, deleted_at IS NOT NULL FROM posts WHERE id = $1 FOR UPDATE")
                .bind(id).fetch_optional(&mut *tx).await.map_err(map_sqlx_error)?;
            let Some((actual_series, version, deleted)) = locked else {
                return Ok(SaveOutcome::Gone);
            };
            if actual_series != series_id {
                tx.rollback().await.map_err(map_sqlx_error)?;
                continue;
            }
            if !deleted {
                return Ok(SaveOutcome::Gone);
            }
            if version != expected_version {
                return Ok(SaveOutcome::StaleConflict);
            }
            sqlx::query("DELETE FROM posts WHERE id = $1")
                .bind(id)
                .execute(&mut *tx)
                .await
                .map_err(map_sqlx_error)?;
            if let Some(series_id) = series_id {
                sqlx::query(
                    "UPDATE series SET version = version + 1, updated_at = now() WHERE id = $1",
                )
                .bind(series_id)
                .execute(&mut *tx)
                .await
                .map_err(map_sqlx_error)?;
            }
            tx.commit().await.map_err(map_sqlx_error)?;
            return Ok(SaveOutcome::Saved {
                new_version: version + 1,
            });
        }
        Ok(SaveOutcome::StaleConflict)
    }

    async fn insert(&self, snapshot: &PostSnapshot, tag_ids: &[Uuid]) -> Result<(), UseCaseError> {
        // 正文与初始标签/系列关系同一事务：半套写入不应对外可见。
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        // 创建即入系列：先锁系列行并递增其版本（加入即改变成员目录，
        // 旧目录上的重排必须失效——与 save/重排共用系列锁协议）。
        if let Some(series_id) = snapshot.series_id {
            bump_series_versions(&mut tx, &[series_id]).await?;
        }
        sqlx::query(
            r#"
            INSERT INTO posts (
                id, author_id, category_id, series_id, title, slug, excerpt, content, cover,
                series_order, status, visibility, published_at, version, created_at, updated_at
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9,
                $10, $11, $12, $13, $14, $15, $16
            )
            "#,
        )
        .bind(snapshot.id)
        .bind(snapshot.author_id)
        .bind(snapshot.category_id)
        .bind(snapshot.series_id)
        .bind(&snapshot.title)
        .bind(&snapshot.slug)
        .bind(&snapshot.excerpt)
        .bind(&snapshot.content)
        .bind(&snapshot.cover)
        .bind(snapshot.series_order)
        .bind(snapshot.status.as_str())
        .bind(snapshot.visibility.as_str())
        .bind(snapshot.published_at)
        .bind(snapshot.version)
        .bind(snapshot.created_at)
        .bind(snapshot.updated_at)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        if !tag_ids.is_empty() {
            insert_post_tags(&mut tx, snapshot.id, tag_ids)
                .await
                .map_err(map_sqlx_error)?;
        }
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(())
    }

    async fn save(
        &self,
        snapshot: &PostSnapshot,
        expected_version: i64,
        now: OffsetDateTime,
        tag_ids: Option<&[Uuid]>,
    ) -> Result<SaveOutcome, UseCaseError> {
        // 正文（或仅标签/系列关系）与 version 递增在同一事务：
        // 观察者不会看到新正文配旧标签（或反之）的混合状态。
        //
        // 系列锁协议（P1 修复）：文章加入/退出/移动系列与整体重排共用同一协议——
        // 锁定顺序恒为「系列行（按 id 序）→ 文章行」，与重排一致（防死锁）；
        // 系列归属或序号变化时递增**相关系列**（旧+新）的 version，
        // 让手持旧目录/旧系列版本的重排立即失效（docs/database-design.md §4）。
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;

        // 1. 读当前文章（无锁快照）：拿旧系列归属与存活状态做三态判定。
        let current: Option<(Option<Uuid>, Option<i32>, i64, bool)> = sqlx::query_as(
            "SELECT series_id, series_order, version, deleted_at IS NULL FROM posts WHERE id = $1",
        )
        .bind(snapshot.id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        let Some((old_series, old_order, current_version, alive)) = current else {
            tx.commit().await.map_err(map_sqlx_error)?;
            return Ok(SaveOutcome::Gone);
        };
        if !alive {
            tx.commit().await.map_err(map_sqlx_error)?;
            return Ok(SaveOutcome::Gone);
        }
        if current_version != expected_version {
            tx.commit().await.map_err(map_sqlx_error)?;
            return Ok(SaveOutcome::StaleConflict);
        }

        // 2. 系列归属/序号变化：按 id 序锁定受影响系列（旧+新去重）并递增版本。
        let series_changed = (old_series, old_order) != (snapshot.series_id, snapshot.series_order);
        if series_changed {
            let mut affected: Vec<Uuid> = [old_series, snapshot.series_id]
                .into_iter()
                .flatten()
                .collect();
            affected.sort();
            affected.dedup();
            bump_series_versions(&mut tx, &affected).await?;
        }

        // 3. 更新文章（此时系列行锁在手，与重排的锁序一致）。
        let updated = sqlx::query(
            r#"
            UPDATE posts SET
                title = $3, slug = $4, excerpt = $5, content = $6, cover = $7,
                series_id = $8, series_order = $9, status = $10, visibility = $11,
                published_at = $12, updated_at = $13, category_id = $14,
                version = version + 1
            WHERE id = $1 AND version = $2 AND deleted_at IS NULL
            RETURNING version
            "#,
        )
        .bind(snapshot.id)
        .bind(expected_version)
        .bind(&snapshot.title)
        .bind(&snapshot.slug)
        .bind(&snapshot.excerpt)
        .bind(&snapshot.content)
        .bind(&snapshot.cover)
        .bind(snapshot.series_id)
        .bind(snapshot.series_order)
        .bind(snapshot.status.as_str())
        .bind(snapshot.visibility.as_str())
        .bind(snapshot.published_at)
        .bind(now)
        .bind(snapshot.category_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;

        let Some(row) = updated else {
            // 版本在步骤 1 之后被并发改写：不再有写入，放弃事务。
            tx.rollback().await.map_err(map_sqlx_error)?;
            return Ok(SaveOutcome::StaleConflict);
        };

        if let Some(tag_ids) = tag_ids {
            // 整体替换：先清空再写入（幂等；空集合 = 解除全部关联）。
            sqlx::query("DELETE FROM post_tags WHERE post_id = $1")
                .bind(snapshot.id)
                .execute(&mut *tx)
                .await
                .map_err(map_sqlx_error)?;
            if !tag_ids.is_empty() {
                insert_post_tags(&mut tx, snapshot.id, tag_ids)
                    .await
                    .map_err(map_sqlx_error)?;
            }
        }
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(SaveOutcome::Saved {
            new_version: row.try_get::<i64, _>(0).map_err(map_row_error)?,
        })
    }

    async fn tags_of(&self, post_id: Uuid) -> Result<Vec<Uuid>, UseCaseError> {
        let rows: Vec<(Uuid,)> =
            sqlx::query_as("SELECT tag_id FROM post_tags WHERE post_id = $1 ORDER BY tag_id")
                .bind(post_id)
                .fetch_all(&self.pool)
                .await
                .map_err(map_sqlx_error)?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }
}

/// 按固定 id 序锁定系列行并递增 version：文章加入/退出/移动系列与整体重排
/// 共用的系列锁协议（锁序恒为「系列（id 序）→ 文章」，防死锁）。
/// `ids` 必须已排序去重。旧系列必存在（FK RESTRICT 挡住被引用删除）；
/// 新系列由用例前置校验，并发删除由 FK 违规翻译兜底。
async fn bump_series_versions(
    tx: &mut sqlx::PgConnection,
    ids: &[Uuid],
) -> Result<(), UseCaseError> {
    sqlx::query("SELECT id FROM series WHERE id = ANY($1::uuid[]) ORDER BY id FOR UPDATE")
        .bind(ids)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
    sqlx::query(
        "UPDATE series SET version = version + 1, updated_at = now()          WHERE id = ANY($1::uuid[])",
    )
    .bind(ids)
    .execute(&mut *tx)
    .await
    .map_err(map_sqlx_error)?;
    Ok(())
}

/// 批量写入文章标签关系。`unnest` 展开保证一条语句完成；
/// DISTINCT 兜住调用方重复 id——post_tags 复合主键本身就是去重语义，
/// 重复提交同一标签不应让整次保存失败。
async fn insert_post_tags(
    tx: &mut sqlx::PgConnection,
    post_id: Uuid,
    tag_ids: &[Uuid],
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO post_tags (post_id, tag_id) \
         SELECT $1, tid FROM (SELECT DISTINCT tid FROM unnest($2::uuid[]) AS tid)",
    )
    .bind(post_id)
    .bind(tag_ids)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 公开只读查询
// ---------------------------------------------------------------------------

pub struct PostgresPublishedPostQuery {
    pool: PgPool,
}

impl PostgresPublishedPostQuery {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl PublishedPostQuery for PostgresPublishedPostQuery {
    async fn list_public(
        &self,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<PublicPostSummary>, UseCaseError> {
        // 端口约束：无论调用方传什么，limit/offset 都被钳制在安全范围。
        let limit = limit.clamp(1, 100);
        let offset = offset.max(0);
        let rows = sqlx::query(&format!(
            r#"
            SELECT p.title, p.slug, p.excerpt, p.published_at,
                   COALESCE(NULLIF(u.display_name, ''), u.username) AS author_display
            FROM posts p
            JOIN users u ON u.id = p.author_id
            WHERE {POST_PUBLIC_PREDICATE}
            ORDER BY p.published_at DESC, p.id DESC
            LIMIT $1 OFFSET $2
            "#
        ))
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;

        rows.iter()
            .map(|row| {
                Ok(PublicPostSummary {
                    title: row.try_get("title").map_err(map_row_error)?,
                    slug: row.try_get("slug").map_err(map_row_error)?,
                    excerpt: row.try_get("excerpt").map_err(map_row_error)?,
                    published_at: row.try_get("published_at").map_err(map_row_error)?,
                    author_display: row.try_get("author_display").map_err(map_row_error)?,
                })
            })
            .collect()
    }

    async fn find_public_by_slug(
        &self,
        slug: &str,
    ) -> Result<Option<PublicPostDetail>, UseCaseError> {
        // 单条语句读正文与标签：同一快照，不会出现新旧混合
        // （docs/content-lifecycle.md §3 的一致性要求）。
        let row = sqlx::query(&format!(
            r#"
            SELECT p.title, p.slug, p.excerpt, p.published_at, p.updated_at, p.content,
                   COALESCE(NULLIF(u.display_name, ''), u.username) AS author_display,
                   u.username AS author_username,
                   (
                       SELECT json_agg(json_build_object('slug', t.slug, 'name', t.name) ORDER BY t.slug)
                       FROM post_tags pt JOIN tags t ON t.id = pt.tag_id
                       WHERE pt.post_id = p.id
                   ) AS tags,
                   c.slug AS category_slug, c.name AS category_name,
                   se.slug AS series_slug, se.name AS series_name, p.series_order
            FROM posts p
            JOIN users u ON u.id = p.author_id
            LEFT JOIN categories c ON c.id = p.category_id
            LEFT JOIN series se ON se.id = p.series_id
            WHERE p.slug = $1 AND {POST_PUBLIC_PREDICATE}
            "#
        ))
        .bind(slug)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_error)?;

        row.map(|row| {
            let tags_json: Option<serde_json::Value> =
                row.try_get("tags").map_err(map_row_error)?;
            Ok(PublicPostDetail {
                title: row.try_get("title").map_err(map_row_error)?,
                slug: row.try_get("slug").map_err(map_row_error)?,
                excerpt: row.try_get("excerpt").map_err(map_row_error)?,
                published_at: row.try_get("published_at").map_err(map_row_error)?,
                updated_at: row.try_get("updated_at").map_err(map_row_error)?,
                author_display: row.try_get("author_display").map_err(map_row_error)?,
                author_username: row.try_get("author_username").map_err(map_row_error)?,
                content: row.try_get("content").map_err(map_row_error)?,
                tags: serde_json::from_value(tags_json.unwrap_or(serde_json::Value::Null))
                    .unwrap_or_default(),
                category: row
                    .try_get::<Option<String>, _>("category_slug")
                    .map_err(map_row_error)?
                    .zip(
                        row.try_get::<Option<String>, _>("category_name")
                            .map_err(map_row_error)?,
                    )
                    .map(|(slug, name)| PublicCategoryRef { slug, name }),
                series: row
                    .try_get::<Option<String>, _>("series_slug")
                    .map_err(map_row_error)?
                    .zip(
                        row.try_get::<Option<String>, _>("series_name")
                            .map_err(map_row_error)?,
                    )
                    .zip(
                        row.try_get::<Option<i32>, _>("series_order")
                            .map_err(map_row_error)?,
                    )
                    .map(|((slug, name), order)| PublicSeriesRef { slug, name, order }),
            })
        })
        .transpose()
    }

    async fn list_public_for_sitemap(
        &self,
        limit: i64,
    ) -> Result<Vec<PublicUrlEntry>, UseCaseError> {
        // 上限取 sitemap 协议的 50,000 条/文件；调用方传更大值也不放大查询。
        let limit = limit.clamp(1, 50_000);
        let rows = sqlx::query(&format!(
            r#"
            SELECT p.slug, p.updated_at
            FROM posts p
            WHERE {POST_PUBLIC_PREDICATE}
            ORDER BY p.updated_at DESC, p.id DESC
            LIMIT $1
            "#
        ))
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        rows.iter()
            .map(|row| {
                Ok(PublicUrlEntry {
                    slug: row.try_get("slug").map_err(map_row_error)?,
                    updated_at: row.try_get("updated_at").map_err(map_row_error)?,
                })
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Page（站点级内容）
// ---------------------------------------------------------------------------

pub struct PostgresPageRepository {
    pool: PgPool,
}

impl PostgresPageRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

const PAGE_COLUMNS: &str = "id, title, slug, content, status, visibility, published_at, version, \
     created_at, updated_at";

fn page_from_row(row: &sqlx::postgres::PgRow) -> Result<PageSnapshot, UseCaseError> {
    let status: String = row.try_get("status").map_err(map_row_error)?;
    let visibility: String = row.try_get("visibility").map_err(map_row_error)?;
    Ok(PageSnapshot {
        id: row.try_get("id").map_err(map_row_error)?,
        title: row.try_get("title").map_err(map_row_error)?,
        slug: row.try_get("slug").map_err(map_row_error)?,
        content: row.try_get("content").map_err(map_row_error)?,
        status: PageStatus::parse(&status)
            .ok_or_else(|| UseCaseError::Repository(format!("未知页面状态 {status}")))?,
        visibility: Visibility::parse(&visibility)
            .ok_or_else(|| UseCaseError::Repository(format!("未知可见性 {visibility}")))?,
        published_at: row.try_get("published_at").map_err(map_row_error)?,
        version: row.try_get("version").map_err(map_row_error)?,
        created_at: row.try_get("created_at").map_err(map_row_error)?,
        updated_at: row.try_get("updated_at").map_err(map_row_error)?,
    })
}

#[async_trait]
impl PageRepository for PostgresPageRepository {
    async fn find_by_slug(&self, slug: &str) -> Result<Option<PageSnapshot>, UseCaseError> {
        let row = sqlx::query(&format!("SELECT {PAGE_COLUMNS} FROM pages WHERE slug = $1"))
            .bind(slug)
            .fetch_optional(&self.pool)
            .await
            .map_err(map_sqlx_error)?;
        row.as_ref().map(page_from_row).transpose()
    }

    async fn find_by_id(&self, id: Uuid) -> Result<Option<PageSnapshot>, UseCaseError> {
        let row = sqlx::query(&format!("SELECT {PAGE_COLUMNS} FROM pages WHERE id = $1"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(map_sqlx_error)?;
        row.as_ref().map(page_from_row).transpose()
    }

    async fn list(&self) -> Result<Vec<PageSnapshot>, UseCaseError> {
        let rows = sqlx::query(&format!(
            "SELECT {PAGE_COLUMNS} FROM pages ORDER BY updated_at DESC, id DESC"
        ))
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        rows.iter().map(page_from_row).collect()
    }

    async fn insert(&self, snapshot: &PageSnapshot) -> Result<(), UseCaseError> {
        sqlx::query(
            r#"
            INSERT INTO pages (
                id, title, slug, content, status, visibility, published_at, version,
                created_at, updated_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
            "#,
        )
        .bind(snapshot.id)
        .bind(&snapshot.title)
        .bind(&snapshot.slug)
        .bind(&snapshot.content)
        .bind(snapshot.status.as_str())
        .bind(snapshot.visibility.as_str())
        .bind(snapshot.published_at)
        .bind(snapshot.version)
        .bind(snapshot.created_at)
        .bind(snapshot.updated_at)
        .execute(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(())
    }

    async fn save(
        &self,
        snapshot: &PageSnapshot,
        expected_version: i64,
        now: OffsetDateTime,
    ) -> Result<SaveOutcome, UseCaseError> {
        let updated = sqlx::query(
            r#"
            UPDATE pages SET
                title = $3, slug = $4, content = $5, status = $6, visibility = $7,
                published_at = $8, updated_at = $9, version = version + 1
            WHERE id = $1 AND version = $2
            RETURNING version
            "#,
        )
        .bind(snapshot.id)
        .bind(expected_version)
        .bind(&snapshot.title)
        .bind(&snapshot.slug)
        .bind(&snapshot.content)
        .bind(snapshot.status.as_str())
        .bind(snapshot.visibility.as_str())
        .bind(snapshot.published_at)
        .bind(now)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_error)?;

        if let Some(row) = updated {
            return Ok(SaveOutcome::Saved {
                new_version: row.try_get::<i64, _>(0).map_err(map_row_error)?,
            });
        }
        // 页面是物理删除：命中不到就是在或不在，不再区分软删除。
        let alive = sqlx::query("SELECT 1 AS alive FROM pages WHERE id = $1")
            .bind(snapshot.id)
            .fetch_optional(&self.pool)
            .await
            .map_err(map_sqlx_error)?;
        match alive {
            Some(_) => Ok(SaveOutcome::StaleConflict),
            None => Ok(SaveOutcome::Gone),
        }
    }
}

pub struct PostgresPublishedPageQuery {
    pool: PgPool,
}

impl PostgresPublishedPageQuery {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl PublishedPageQuery for PostgresPublishedPageQuery {
    async fn find_public_by_slug(
        &self,
        slug: &str,
    ) -> Result<Option<PublicPageDetail>, UseCaseError> {
        let row = sqlx::query(&format!(
            r#"
            SELECT p.title, p.slug, p.published_at, p.updated_at, p.content
            FROM pages p
            WHERE p.slug = $1 AND {PAGE_PUBLIC_PREDICATE}
            "#
        ))
        .bind(slug)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_error)?;

        row.map(|row| {
            Ok(PublicPageDetail {
                title: row.try_get("title").map_err(map_row_error)?,
                slug: row.try_get("slug").map_err(map_row_error)?,
                published_at: row.try_get("published_at").map_err(map_row_error)?,
                updated_at: row.try_get("updated_at").map_err(map_row_error)?,
                content: row.try_get("content").map_err(map_row_error)?,
            })
        })
        .transpose()
    }

    async fn list_public_for_sitemap(
        &self,
        limit: i64,
    ) -> Result<Vec<PublicUrlEntry>, UseCaseError> {
        let limit = limit.clamp(1, 50_000);
        let rows = sqlx::query(&format!(
            r#"
            SELECT p.slug, p.updated_at
            FROM pages p
            WHERE {PAGE_PUBLIC_PREDICATE}
            ORDER BY p.updated_at DESC, p.id DESC
            LIMIT $1
            "#
        ))
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        rows.iter()
            .map(|row| {
                Ok(PublicUrlEntry {
                    slug: row.try_get("slug").map_err(map_row_error)?,
                    updated_at: row.try_get("updated_at").map_err(map_row_error)?,
                })
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// 标签目录与文章关联
// ---------------------------------------------------------------------------

pub struct PostgresTagRepository {
    pool: PgPool,
}

impl PostgresTagRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

const TAG_COLUMNS: &str = "id, name, slug, version, created_at";

fn tag_from_row(row: &sqlx::postgres::PgRow) -> Result<domain::content::TagSnapshot, UseCaseError> {
    Ok(domain::content::TagSnapshot {
        id: row.try_get("id").map_err(map_row_error)?,
        name: row.try_get("name").map_err(map_row_error)?,
        slug: row.try_get("slug").map_err(map_row_error)?,
        version: row.try_get("version").map_err(map_row_error)?,
        created_at: row.try_get("created_at").map_err(map_row_error)?,
    })
}

/// 公开计数子查询：与公开文章谓词同口径（草稿/私密/回收站不计入）。
const TAG_PUBLIC_COUNT: &str = "(SELECT count(*) FROM post_tags pt JOIN posts p ON p.id = pt.post_id \
      WHERE pt.tag_id = t.id AND p.status = 'published' AND p.visibility = 'public' \
        AND p.deleted_at IS NULL)";

#[async_trait]
impl TagRepository for PostgresTagRepository {
    async fn insert(&self, snapshot: &domain::content::TagSnapshot) -> Result<(), UseCaseError> {
        sqlx::query(
            "INSERT INTO tags (id, name, slug, version, created_at) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(snapshot.id)
        .bind(&snapshot.name)
        .bind(&snapshot.slug)
        .bind(snapshot.version)
        .bind(snapshot.created_at)
        .execute(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(())
    }

    async fn find_by_slug(
        &self,
        slug: &str,
    ) -> Result<Option<domain::content::TagSnapshot>, UseCaseError> {
        let row = sqlx::query(&format!("SELECT {TAG_COLUMNS} FROM tags WHERE slug = $1"))
            .bind(slug)
            .fetch_optional(&self.pool)
            .await
            .map_err(map_sqlx_error)?;
        row.as_ref().map(tag_from_row).transpose()
    }

    async fn list(&self) -> Result<Vec<TagWithUsage>, UseCaseError> {
        let rows = sqlx::query(&format!(
            "SELECT {TAG_COLUMNS}, {TAG_PUBLIC_COUNT} AS public_post_count \
             FROM tags t ORDER BY t.slug"
        ))
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;

        rows.iter()
            .map(|row| {
                Ok(TagWithUsage {
                    snapshot: tag_from_row(row)?,
                    public_post_count: row.try_get("public_post_count").map_err(map_row_error)?,
                })
            })
            .collect()
    }

    async fn rename(
        &self,
        id: Uuid,
        new_name: &str,
        expected_version: i64,
    ) -> Result<Option<domain::content::TagSnapshot>, UseCaseError> {
        let row = sqlx::query(
            "UPDATE tags SET name = $3, version = version + 1 \
             WHERE id = $1 AND version = $2 \
             RETURNING id, name, slug, version, created_at",
        )
        .bind(id)
        .bind(expected_version)
        .bind(new_name)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        row.as_ref().map(tag_from_row).transpose()
    }

    async fn delete(
        &self,
        id: Uuid,
        expected_version: i64,
    ) -> Result<TagDeleteOutcome, UseCaseError> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        // 锁住标签行再数引用：并发「把该标签挂到文章」的写入会经
        // post_tags.tag_id 的 FK KEY SHARE 锁与本事务互斥，引用检查因此不被写穿。
        let locked: Option<(i64,)> =
            sqlx::query_as("SELECT version FROM tags WHERE id = $1 FOR UPDATE")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(map_sqlx_error)?;
        let Some((current_version,)) = locked else {
            tx.commit().await.map_err(map_sqlx_error)?;
            return Ok(TagDeleteOutcome::Gone);
        };
        if current_version != expected_version {
            tx.commit().await.map_err(map_sqlx_error)?;
            return Ok(TagDeleteOutcome::StaleVersion);
        }
        let (count,): (i64,) = sqlx::query_as("SELECT count(*) FROM post_tags WHERE tag_id = $1")
            .bind(id)
            .fetch_one(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        if count > 0 {
            tx.commit().await.map_err(map_sqlx_error)?;
            return Ok(TagDeleteOutcome::Referenced { count });
        }
        sqlx::query("DELETE FROM tags WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(TagDeleteOutcome::Deleted)
    }

    async fn existing_ids(&self, ids: &[Uuid]) -> Result<Vec<Uuid>, UseCaseError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows: Vec<(Uuid,)> =
            sqlx::query_as("SELECT id FROM tags WHERE id = ANY($1::uuid[]) ORDER BY id")
                .bind(ids)
                .fetch_all(&self.pool)
                .await
                .map_err(map_sqlx_error)?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    async fn public_count(&self, id: Uuid) -> Result<i64, UseCaseError> {
        // count(*) 恒有一行（可能为 0）。
        let (count,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM post_tags pt JOIN posts p ON p.id = pt.post_id \
             WHERE pt.tag_id = $1 AND p.status = 'published' AND p.visibility = 'public' \
               AND p.deleted_at IS NULL",
        )
        .bind(id)
        .fetch_one(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(count)
    }
}

// ---------------------------------------------------------------------------
// 公开标签页查询
// ---------------------------------------------------------------------------

pub struct PostgresPublishedTagQuery {
    pool: PgPool,
}

impl PostgresPublishedTagQuery {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl PublishedTagQuery for PostgresPublishedTagQuery {
    async fn list_public_tags(&self, limit: i64) -> Result<Vec<PublicTagSummary>, UseCaseError> {
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT slug, name FROM tags ORDER BY slug LIMIT $1")
                .bind(limit.clamp(1, 50))
                .fetch_all(&self.pool)
                .await
                .map_err(map_sqlx_error)?;
        Ok(rows
            .into_iter()
            .map(|(slug, name)| PublicTagSummary { slug, name })
            .collect())
    }
    async fn find_public_by_slug(
        &self,
        slug: &str,
    ) -> Result<Option<PublicTagSummary>, UseCaseError> {
        // 标签目录本身无可见性；未知 slug 与存在与否不区分差异。
        let row: Option<(String, String)> =
            sqlx::query_as("SELECT slug, name FROM tags WHERE slug = $1")
                .bind(slug)
                .fetch_optional(&self.pool)
                .await
                .map_err(map_sqlx_error)?;
        Ok(row.map(|(slug, name)| PublicTagSummary { slug, name }))
    }

    async fn list_public_posts_by_tag(
        &self,
        tag_slug: &str,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<PublicPostSummary>, i64), UseCaseError> {
        let limit = limit.clamp(1, 100);
        let offset = offset.max(0);
        // count(*) OVER() 让总数与页面来自同一快照：分页导航不会显示
        // 「共 N 篇」却翻出第 N+1 篇（或反之）。
        let rows = sqlx::query(&format!(
            r#"
            SELECT p.title, p.slug, p.excerpt, p.published_at,
                   COALESCE(NULLIF(u.display_name, ''), u.username) AS author_display,
                   count(*) OVER() AS total
            FROM tags t
            JOIN post_tags pt ON pt.tag_id = t.id
            JOIN posts p ON p.id = pt.post_id
            JOIN users u ON u.id = p.author_id
            WHERE t.slug = $1 AND {POST_PUBLIC_PREDICATE}
            ORDER BY p.published_at DESC, p.id DESC
            LIMIT $2 OFFSET $3
            "#
        ))
        .bind(tag_slug)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;

        // 空页时窗口函数无行可聚合，总数即 0。
        let total = rows
            .first()
            .map(|row| row.try_get::<i64, _>("total").map_err(map_row_error))
            .transpose()?
            .unwrap_or(0);
        let posts = rows
            .iter()
            .map(|row| {
                Ok(PublicPostSummary {
                    title: row.try_get("title").map_err(map_row_error)?,
                    slug: row.try_get("slug").map_err(map_row_error)?,
                    excerpt: row.try_get("excerpt").map_err(map_row_error)?,
                    published_at: row.try_get("published_at").map_err(map_row_error)?,
                    author_display: row.try_get("author_display").map_err(map_row_error)?,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok((posts, total))
    }

    async fn list_public_directories(&self) -> Result<Vec<PublicUrlEntry>, UseCaseError> {
        // INNER JOIN 直接实现「非空才收录」：没有公开文章的标签根本不出现在结果里。
        // lastmod 取该标签下公开文章的最近更新时间（tags 表没有 updated_at）。
        let rows = sqlx::query(&format!(
            r#"
            SELECT t.slug, max(p.updated_at) AS updated_at
            FROM tags t
            JOIN post_tags pt ON pt.tag_id = t.id
            JOIN posts p ON p.id = pt.post_id
            WHERE {POST_PUBLIC_PREDICATE}
            GROUP BY t.slug
            ORDER BY t.slug
            "#
        ))
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        rows.iter()
            .map(|row| {
                Ok(PublicUrlEntry {
                    slug: row.try_get("slug").map_err(map_row_error)?,
                    updated_at: row.try_get("updated_at").map_err(map_row_error)?,
                })
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// 分类目录：树锁、防环与引用保护
// ---------------------------------------------------------------------------

/// 分类树事务锁：创建/移动/删除统一取得，串行化祖先链检查与写入。
///
/// 不锁读路径（目录读取无锁）；只约束写写并发——两条并发移动若各自
/// 通过了环检查再先后提交，可能拼出环（检查结果在锁外失效）。
pub(crate) const CATEGORY_TREE_LOCK: (i32, i32) = (2048002, 1);

pub struct PostgresCategoryRepository {
    pool: PgPool,
}

impl PostgresCategoryRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

const CATEGORY_COLUMNS: &str =
    "id, name, slug, parent_id, description, version, created_at, updated_at";

fn category_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<domain::content::CategorySnapshot, UseCaseError> {
    Ok(domain::content::CategorySnapshot {
        id: row.try_get("id").map_err(map_row_error)?,
        name: row.try_get("name").map_err(map_row_error)?,
        slug: row.try_get("slug").map_err(map_row_error)?,
        parent_id: row.try_get("parent_id").map_err(map_row_error)?,
        description: row.try_get("description").map_err(map_row_error)?,
        version: row.try_get("version").map_err(map_row_error)?,
        created_at: row.try_get("created_at").map_err(map_row_error)?,
        updated_at: row.try_get("updated_at").map_err(map_row_error)?,
    })
}

/// 公开文章计数子查询（直接归属；与公开分类页同口径）。
const CATEGORY_PUBLIC_COUNT: &str = "(SELECT count(*) FROM posts p WHERE p.category_id = t.id AND p.status = 'published' \
      AND p.visibility = 'public' AND p.deleted_at IS NULL)";

/// 深度受限的祖先链检查：自 parent 向上走，链上出现 self 即成环。
///
/// depth 上限防的是**已损坏数据**（自引用 CHECK 只排除直接自父，历史环会让
/// 无界递归 CTE 永不终止）；正常数据下树锁保证环不会并发产生，链长天然有限。
async fn parent_chain_contains(
    tx: &mut sqlx::PgConnection,
    parent_id: Uuid,
    self_id: Uuid,
) -> Result<bool, UseCaseError> {
    let hit: Option<(i32,)> = sqlx::query_as(
        r#"
        WITH RECURSIVE up(id, parent_id, depth) AS (
            SELECT c.id, c.parent_id, 0 FROM categories c WHERE c.id = $1
            UNION ALL
            -- 向上走祖先链：c 是当前节点的父（up.parent_id = c.id）。
            SELECT c.id, c.parent_id, up.depth + 1
            FROM categories c JOIN up ON up.parent_id = c.id
            WHERE up.depth < 100
        )
        SELECT 1 FROM up WHERE id = $2 LIMIT 1
        "#,
    )
    .bind(parent_id)
    .bind(self_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_sqlx_error)?;
    Ok(hit.is_some())
}

#[async_trait]
impl CategoryRepository for PostgresCategoryRepository {
    async fn insert(
        &self,
        snapshot: &domain::content::CategorySnapshot,
    ) -> Result<(), UseCaseError> {
        // 新节点不可能是自己的祖先，创建本身无环；树锁仍统一取得，
        // 与并发删除父分类互斥（否则插入成功后父已消失，靠 FK 报裸错误）。
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        sqlx::query("SELECT pg_advisory_xact_lock($1::int, $2::int)")
            .bind(CATEGORY_TREE_LOCK.0)
            .bind(CATEGORY_TREE_LOCK.1)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        sqlx::query(
            "INSERT INTO categories (id, name, slug, parent_id, description, version, \
             created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(snapshot.id)
        .bind(&snapshot.name)
        .bind(&snapshot.slug)
        .bind(snapshot.parent_id)
        .bind(&snapshot.description)
        .bind(snapshot.version)
        .bind(snapshot.created_at)
        .bind(snapshot.updated_at)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(())
    }

    async fn find_by_slug(
        &self,
        slug: &str,
    ) -> Result<Option<domain::content::CategorySnapshot>, UseCaseError> {
        let row = sqlx::query(&format!(
            "SELECT {CATEGORY_COLUMNS} FROM categories WHERE slug = $1"
        ))
        .bind(slug)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        row.as_ref().map(category_from_row).transpose()
    }

    async fn list(&self) -> Result<Vec<CategoryWithUsage>, UseCaseError> {
        let rows = sqlx::query(&format!(
            "SELECT {CATEGORY_COLUMNS}, {CATEGORY_PUBLIC_COUNT} AS public_post_count \
             FROM categories t ORDER BY t.slug"
        ))
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        rows.iter()
            .map(|row| {
                Ok(CategoryWithUsage {
                    snapshot: category_from_row(row)?,
                    public_post_count: row.try_get("public_post_count").map_err(map_row_error)?,
                })
            })
            .collect()
    }

    async fn update(
        &self,
        id: Uuid,
        name: &str,
        description: Option<&str>,
        parent_id: Option<Uuid>,
        expected_version: i64,
    ) -> Result<Option<domain::content::CategorySnapshot>, UseCaseError> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        // 树锁内完成「环检查 + 写入」：锁外的检查结果可能被并发移动作废。
        sqlx::query("SELECT pg_advisory_xact_lock($1::int, $2::int)")
            .bind(CATEGORY_TREE_LOCK.0)
            .bind(CATEGORY_TREE_LOCK.1)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;

        let current: Option<(Option<Uuid>, i64)> =
            sqlx::query_as("SELECT parent_id, version FROM categories WHERE id = $1")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(map_sqlx_error)?;
        let Some((current_parent, current_version)) = current else {
            tx.commit().await.map_err(map_sqlx_error)?;
            return Ok(None);
        };
        if current_version != expected_version {
            tx.commit().await.map_err(map_sqlx_error)?;
            return Ok(None);
        }

        if parent_id != current_parent
            && let Some(new_parent) = parent_id
        {
            {
                if new_parent == id {
                    tx.commit().await.map_err(map_sqlx_error)?;
                    return Err(UseCaseError::Invalid("父分类不能是自身".into()));
                }
                // 新父必须存在（给出可定位错误，而非 FK 裸错误）。
                let exists: Option<(Uuid,)> =
                    sqlx::query_as("SELECT id FROM categories WHERE id = $1")
                        .bind(new_parent)
                        .fetch_optional(&mut *tx)
                        .await
                        .map_err(map_sqlx_error)?;
                if exists.is_none() {
                    tx.commit().await.map_err(map_sqlx_error)?;
                    return Err(UseCaseError::Invalid("父分类不存在".into()));
                }
                if parent_chain_contains(&mut tx, new_parent, id).await? {
                    tx.commit().await.map_err(map_sqlx_error)?;
                    return Err(UseCaseError::Invalid(
                        "目标父分类的祖先链包含自身，会形成环".into(),
                    ));
                }
            }
        }

        let row = sqlx::query(
            "UPDATE categories SET name = $3, description = $4, parent_id = $5, \
             version = version + 1, updated_at = now() \
             WHERE id = $1 AND version = $2 \
             RETURNING id, name, slug, parent_id, description, version, created_at, updated_at",
        )
        .bind(id)
        .bind(expected_version)
        .bind(name)
        .bind(description)
        .bind(parent_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        tx.commit().await.map_err(map_sqlx_error)?;
        row.as_ref().map(category_from_row).transpose()
    }

    async fn delete(
        &self,
        id: Uuid,
        expected_version: i64,
    ) -> Result<CategoryDeleteOutcome, UseCaseError> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        sqlx::query("SELECT pg_advisory_xact_lock($1::int, $2::int)")
            .bind(CATEGORY_TREE_LOCK.0)
            .bind(CATEGORY_TREE_LOCK.1)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;

        let current: Option<(i64,)> =
            sqlx::query_as("SELECT version FROM categories WHERE id = $1")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(map_sqlx_error)?;
        let Some((current_version,)) = current else {
            tx.commit().await.map_err(map_sqlx_error)?;
            return Ok(CategoryDeleteOutcome::Gone);
        };
        if current_version != expected_version {
            tx.commit().await.map_err(map_sqlx_error)?;
            return Ok(CategoryDeleteOutcome::StaleVersion);
        }

        // 引用计数不过滤可见性：草稿/私密/回收站同样占用。
        let (posts,): (i64,) = sqlx::query_as("SELECT count(*) FROM posts WHERE category_id = $1")
            .bind(id)
            .fetch_one(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        let (children,): (i64,) =
            sqlx::query_as("SELECT count(*) FROM categories WHERE parent_id = $1")
                .bind(id)
                .fetch_one(&mut *tx)
                .await
                .map_err(map_sqlx_error)?;
        if posts > 0 || children > 0 {
            tx.commit().await.map_err(map_sqlx_error)?;
            return Ok(CategoryDeleteOutcome::Referenced { posts, children });
        }

        sqlx::query("DELETE FROM categories WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(CategoryDeleteOutcome::Deleted)
    }

    async fn existing_id(&self, id: Uuid) -> Result<bool, UseCaseError> {
        let hit: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM categories WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(map_sqlx_error)?;
        Ok(hit.is_some())
    }

    async fn public_count(&self, id: Uuid) -> Result<i64, UseCaseError> {
        let (count,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM posts WHERE category_id = $1 AND status = 'published' \
             AND visibility = 'public' AND deleted_at IS NULL",
        )
        .bind(id)
        .fetch_one(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(count)
    }
}

// ---------------------------------------------------------------------------
// 公开分类页查询
// ---------------------------------------------------------------------------

pub struct PostgresPublishedCategoryQuery {
    pool: PgPool,
}

impl PostgresPublishedCategoryQuery {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl PublishedCategoryQuery for PostgresPublishedCategoryQuery {
    async fn list_public_categories(
        &self,
        limit: i64,
    ) -> Result<Vec<PublicCategorySummary>, UseCaseError> {
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT slug, name FROM categories ORDER BY slug LIMIT $1")
                .bind(limit.clamp(1, 50))
                .fetch_all(&self.pool)
                .await
                .map_err(map_sqlx_error)?;
        Ok(rows
            .into_iter()
            .map(|(slug, name)| PublicCategorySummary { slug, name })
            .collect())
    }
    async fn find_public_by_slug(
        &self,
        slug: &str,
    ) -> Result<Option<PublicCategorySummary>, UseCaseError> {
        let row: Option<(String, String)> =
            sqlx::query_as("SELECT slug, name FROM categories WHERE slug = $1")
                .bind(slug)
                .fetch_optional(&self.pool)
                .await
                .map_err(map_sqlx_error)?;
        Ok(row.map(|(slug, name)| PublicCategorySummary { slug, name }))
    }

    async fn list_public_posts_by_category(
        &self,
        category_slug: &str,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<PublicPostSummary>, i64), UseCaseError> {
        let limit = limit.clamp(1, 100);
        let offset = offset.max(0);
        // 直接归属（不含子树）：父分类不自动成为文章的另一条直接分类关系。
        let rows = sqlx::query(&format!(
            r#"
            SELECT p.title, p.slug, p.excerpt, p.published_at,
                   COALESCE(NULLIF(u.display_name, ''), u.username) AS author_display,
                   count(*) OVER() AS total
            FROM categories c
            JOIN posts p ON p.category_id = c.id
            JOIN users u ON u.id = p.author_id
            WHERE c.slug = $1 AND {POST_PUBLIC_PREDICATE}
            ORDER BY p.published_at DESC, p.id DESC
            LIMIT $2 OFFSET $3
            "#
        ))
        .bind(category_slug)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;

        let total = rows
            .first()
            .map(|row| row.try_get::<i64, _>("total").map_err(map_row_error))
            .transpose()?
            .unwrap_or(0);
        let posts = rows
            .iter()
            .map(|row| {
                Ok(PublicPostSummary {
                    title: row.try_get("title").map_err(map_row_error)?,
                    slug: row.try_get("slug").map_err(map_row_error)?,
                    excerpt: row.try_get("excerpt").map_err(map_row_error)?,
                    published_at: row.try_get("published_at").map_err(map_row_error)?,
                    author_display: row.try_get("author_display").map_err(map_row_error)?,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok((posts, total))
    }

    async fn list_public_directories(&self) -> Result<Vec<PublicUrlEntry>, UseCaseError> {
        // 与标签页同口径：INNER JOIN 只保留有直接归属公开文章的分类，空分类不收。
        // lastmod 取「分类自身改名时间」与「成员文章最近更新时间」的较晚者：
        // 改分类名也会改变公开页展示内容。
        let rows = sqlx::query(&format!(
            r#"
            SELECT c.slug, GREATEST(c.updated_at, max(p.updated_at)) AS updated_at
            FROM categories c
            JOIN posts p ON p.category_id = c.id
            WHERE {POST_PUBLIC_PREDICATE}
            GROUP BY c.id, c.slug, c.updated_at
            ORDER BY c.slug
            "#
        ))
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        rows.iter()
            .map(|row| {
                Ok(PublicUrlEntry {
                    slug: row.try_get("slug").map_err(map_row_error)?,
                    updated_at: row.try_get("updated_at").map_err(map_row_error)?,
                })
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// 系列目录与并发重排
// ---------------------------------------------------------------------------

pub struct PostgresSeriesRepository {
    pool: PgPool,
}

impl PostgresSeriesRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

const SERIES_COLUMNS: &str = "id, name, slug, description, cover, version, created_at, updated_at";

fn series_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<domain::content::SeriesSnapshot, UseCaseError> {
    Ok(domain::content::SeriesSnapshot {
        id: row.try_get("id").map_err(map_row_error)?,
        name: row.try_get("name").map_err(map_row_error)?,
        slug: row.try_get("slug").map_err(map_row_error)?,
        description: row.try_get("description").map_err(map_row_error)?,
        cover: row.try_get("cover").map_err(map_row_error)?,
        version: row.try_get("version").map_err(map_row_error)?,
        created_at: row.try_get("created_at").map_err(map_row_error)?,
        updated_at: row.try_get("updated_at").map_err(map_row_error)?,
    })
}

#[async_trait]
impl SeriesRepository for PostgresSeriesRepository {
    async fn insert(&self, snapshot: &domain::content::SeriesSnapshot) -> Result<(), UseCaseError> {
        sqlx::query(
            "INSERT INTO series (id, name, slug, description, cover, version, created_at, \
             updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(snapshot.id)
        .bind(&snapshot.name)
        .bind(&snapshot.slug)
        .bind(&snapshot.description)
        .bind(&snapshot.cover)
        .bind(snapshot.version)
        .bind(snapshot.created_at)
        .bind(snapshot.updated_at)
        .execute(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(())
    }

    async fn find_by_slug(
        &self,
        slug: &str,
    ) -> Result<Option<domain::content::SeriesSnapshot>, UseCaseError> {
        let row = sqlx::query(&format!(
            "SELECT {SERIES_COLUMNS} FROM series WHERE slug = $1"
        ))
        .bind(slug)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        row.as_ref().map(series_from_row).transpose()
    }

    async fn list(&self) -> Result<Vec<SeriesWithUsage>, UseCaseError> {
        let rows = sqlx::query(
            "SELECT s.id, s.name, s.slug, s.description, s.cover, s.version, s.created_at, \
                    s.updated_at, \
                    (SELECT count(*) FROM posts p WHERE p.series_id = s.id) AS post_count, \
                    (SELECT count(*) FROM posts p WHERE p.series_id = s.id \
                       AND p.status = 'published' AND p.visibility = 'public' \
                       AND p.deleted_at IS NULL) AS public_post_count \
             FROM series s ORDER BY s.slug",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        rows.iter()
            .map(|row| {
                Ok(SeriesWithUsage {
                    snapshot: series_from_row(row)?,
                    post_count: row.try_get("post_count").map_err(map_row_error)?,
                    public_post_count: row.try_get("public_post_count").map_err(map_row_error)?,
                })
            })
            .collect()
    }

    async fn update(
        &self,
        id: Uuid,
        name: &str,
        description: Option<&str>,
        expected_version: i64,
    ) -> Result<Option<domain::content::SeriesSnapshot>, UseCaseError> {
        let row = sqlx::query(
            "UPDATE series SET name = $3, description = $4, version = version + 1, \
             updated_at = now() WHERE id = $1 AND version = $2 \
             RETURNING id, name, slug, description, cover, version, created_at, updated_at",
        )
        .bind(id)
        .bind(expected_version)
        .bind(name)
        .bind(description)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        row.as_ref().map(series_from_row).transpose()
    }

    async fn delete(
        &self,
        id: Uuid,
        expected_version: i64,
    ) -> Result<SeriesDeleteOutcome, UseCaseError> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        // 锁系列行：与「文章加入该系列」的写入（posts 行上的系列引用）互斥，
        // 引用检查与删除之间不会有并发加入。
        let locked: Option<(i64,)> =
            sqlx::query_as("SELECT version FROM series WHERE id = $1 FOR UPDATE")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(map_sqlx_error)?;
        let Some((current_version,)) = locked else {
            tx.commit().await.map_err(map_sqlx_error)?;
            return Ok(SeriesDeleteOutcome::Gone);
        };
        if current_version != expected_version {
            tx.commit().await.map_err(map_sqlx_error)?;
            return Ok(SeriesDeleteOutcome::StaleVersion);
        }
        let (count,): (i64,) = sqlx::query_as("SELECT count(*) FROM posts WHERE series_id = $1")
            .bind(id)
            .fetch_one(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        if count > 0 {
            tx.commit().await.map_err(map_sqlx_error)?;
            return Ok(SeriesDeleteOutcome::Referenced { count });
        }
        sqlx::query("DELETE FROM series WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(SeriesDeleteOutcome::Deleted)
    }

    async fn existing_id(&self, id: Uuid) -> Result<bool, UseCaseError> {
        let hit: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM series WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(map_sqlx_error)?;
        Ok(hit.is_some())
    }

    async fn members_of(&self, series_id: Uuid) -> Result<Vec<SeriesMember>, UseCaseError> {
        let rows = sqlx::query(
            "SELECT id, author_id, slug, title, status, visibility, series_order, deleted_at \
             FROM posts WHERE series_id = $1 \
             ORDER BY series_order, id",
        )
        .bind(series_id)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        rows.iter()
            .map(|row| {
                Ok(SeriesMember {
                    post_id: row.try_get("id").map_err(map_row_error)?,
                    author_id: row.try_get("author_id").map_err(map_row_error)?,
                    slug: row.try_get("slug").map_err(map_row_error)?,
                    title: row.try_get("title").map_err(map_row_error)?,
                    status: row.try_get::<String, _>("status").map_err(map_row_error)?,
                    deleted: row
                        .try_get::<Option<OffsetDateTime>, _>("deleted_at")
                        .map_err(map_row_error)?
                        .is_some(),
                    visibility: row
                        .try_get::<String, _>("visibility")
                        .map_err(map_row_error)?,
                    series_order: row.try_get("series_order").map_err(map_row_error)?,
                })
            })
            .collect()
    }

    /// 重排锁协议（docs/database-design.md §4）：
    /// 1. 锁系列行并校验 series.version（旧目录不得重排）；
    /// 2. 校验成员集合与提交的完整排列一致；
    /// 3. 按固定 id 序锁涉及文章（跨系列移动时两个系列都按 id 序）；
    /// 4. `SET CONSTRAINTS posts_series_position_unique DEFERRED`，
    ///    交换期间允许临时重复，提交时恢复唯一检查；
    /// 5. 更新每篇文章 series_order 并递增 posts.version；递增 series.version。
    async fn reorder(
        &self,
        series_id: Uuid,
        expected_series_version: i64,
        ordered_post_ids: &[Uuid],
    ) -> Result<ReorderOutcome, UseCaseError> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        let locked: Option<(i64,)> =
            sqlx::query_as("SELECT version FROM series WHERE id = $1 FOR UPDATE")
                .bind(series_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(map_sqlx_error)?;
        let Some((current_version,)) = locked else {
            tx.commit().await.map_err(map_sqlx_error)?;
            return Ok(ReorderOutcome::SeriesGone);
        };
        if current_version != expected_series_version {
            tx.commit().await.map_err(map_sqlx_error)?;
            return Ok(ReorderOutcome::StaleSeriesVersion);
        }

        // 固定 id 序锁定成员行，并读取当前成员集合。
        let members: Vec<(Uuid,)> =
            sqlx::query_as("SELECT id FROM posts WHERE series_id = $1 ORDER BY id")
                .bind(series_id)
                .fetch_all(&mut *tx)
                .await
                .map_err(map_sqlx_error)?;
        let mut current: Vec<Uuid> = members.into_iter().map(|(id,)| id).collect();
        let mut given: Vec<Uuid> = ordered_post_ids.to_vec();
        current.sort_unstable();
        given.sort_unstable();
        if current != given {
            tx.commit().await.map_err(map_sqlx_error)?;
            return Ok(ReorderOutcome::MembershipMismatch);
        }

        // 锁成员行（id 序）。VALUES 列表带序号参数在 sqlx 里用 unnest 更稳。
        let locked_posts: Vec<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM posts WHERE id = ANY($1::uuid[]) ORDER BY id \
             FOR UPDATE",
        )
        .bind(&given)
        .fetch_all(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        if locked_posts.len() != given.len() {
            tx.commit().await.map_err(map_sqlx_error)?;
            return Ok(ReorderOutcome::MembershipMismatch);
        }

        // 延后位置唯一约束：交换期间允许临时重复。
        sqlx::query("SET CONSTRAINTS posts_series_position_unique DEFERRED")
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;

        // 按提交顺序写入序号（1 起）；每篇文章 version+1（顺序是文章内容的一部分）。
        for (index, post_id) in ordered_post_ids.iter().enumerate() {
            let order: i32 = (index + 1) as i32;
            sqlx::query(
                "UPDATE posts SET series_order = $2, version = version + 1, updated_at = now() \
                 WHERE id = $1 AND series_id = $3",
            )
            .bind(post_id)
            .bind(order)
            .bind(series_id)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        }

        let bumped: Option<(i64,)> = sqlx::query_as(
            "UPDATE series SET version = version + 1, updated_at = now() \
             WHERE id = $1 RETURNING version",
        )
        .bind(series_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(ReorderOutcome::Reordered {
            new_version: bumped.map(|(v,)| v).unwrap_or(current_version + 1),
        })
    }
}

// ---------------------------------------------------------------------------
// 公开系列页查询
// ---------------------------------------------------------------------------

pub struct PostgresPublishedSeriesQuery {
    pool: PgPool,
}

impl PostgresPublishedSeriesQuery {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl PublishedSeriesQuery for PostgresPublishedSeriesQuery {
    async fn find_public_by_slug(
        &self,
        slug: &str,
    ) -> Result<Option<PublicSeriesSummary>, UseCaseError> {
        let row: Option<(String, String)> =
            sqlx::query_as("SELECT slug, name FROM series WHERE slug = $1")
                .bind(slug)
                .fetch_optional(&self.pool)
                .await
                .map_err(map_sqlx_error)?;
        Ok(row.map(|(slug, name)| PublicSeriesSummary { slug, name }))
    }

    async fn list_public_posts_by_series(
        &self,
        series_slug: &str,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<PublicPostSummary>, i64), UseCaseError> {
        let limit = limit.clamp(1, 100);
        let offset = offset.max(0);
        // 阅读顺序：series_order 升序；草稿/私密/回收站保留位置但不出现，
        // 因此公开页的序号可能留空档（不强制重新编号）。
        let rows = sqlx::query(&format!(
            r#"
            SELECT p.title, p.slug, p.excerpt, p.published_at,
                   COALESCE(NULLIF(u.display_name, ''), u.username) AS author_display,
                   count(*) OVER() AS total
            FROM series s
            JOIN posts p ON p.series_id = s.id
            JOIN users u ON u.id = p.author_id
            WHERE s.slug = $1 AND {POST_PUBLIC_PREDICATE}
            ORDER BY p.series_order ASC, p.id ASC
            LIMIT $2 OFFSET $3
            "#
        ))
        .bind(series_slug)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;

        let total = rows
            .first()
            .map(|row| row.try_get::<i64, _>("total").map_err(map_row_error))
            .transpose()?
            .unwrap_or(0);
        let posts = rows
            .iter()
            .map(|row| {
                Ok(PublicPostSummary {
                    title: row.try_get("title").map_err(map_row_error)?,
                    slug: row.try_get("slug").map_err(map_row_error)?,
                    excerpt: row.try_get("excerpt").map_err(map_row_error)?,
                    published_at: row.try_get("published_at").map_err(map_row_error)?,
                    author_display: row.try_get("author_display").map_err(map_row_error)?,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok((posts, total))
    }

    async fn list_public_directories(&self) -> Result<Vec<PublicUrlEntry>, UseCaseError> {
        // 与分类页同口径：只收录有公开文章的系列；lastmod 取系列改名与成员更新的较晚者。
        let rows = sqlx::query(&format!(
            r#"
            SELECT s.slug, GREATEST(s.updated_at, max(p.updated_at)) AS updated_at
            FROM series s
            JOIN posts p ON p.series_id = s.id
            WHERE {POST_PUBLIC_PREDICATE}
            GROUP BY s.id, s.slug, s.updated_at
            ORDER BY s.slug
            "#
        ))
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        rows.iter()
            .map(|row| {
                Ok(PublicUrlEntry {
                    slug: row.try_get("slug").map_err(map_row_error)?,
                    updated_at: row.try_get("updated_at").map_err(map_row_error)?,
                })
            })
            .collect()
    }
}
