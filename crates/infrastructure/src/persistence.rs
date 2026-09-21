//! PostgreSQL 持久化适配器：连接、迁移与端口实现。
//!
//! - UUID 由应用生成（v7，时间有序利于索引局部性）。
//! - updated_at 由应用写入；SQL 端不依赖 DEFAULT now() 更新。
//! - 写侧使用条件更新（WHERE version = expected）实现乐观并发。

use async_trait::async_trait;
use sqlx::postgres::{PgDatabaseError, PgPoolOptions};
use sqlx::{PgPool, Row};
use time::OffsetDateTime;
use uuid::Uuid;

use application::error::UseCaseError;
use application::ports::{
    Clock, HealthCheck, PostRepository, PublicPostDetail, PublicPostSummary, PublishedPostQuery,
    SaveOutcome, UserRepository,
};
use domain::content::post::{PostSnapshot, PostStatus, Visibility};
use domain::identity::UserSnapshot;

/// 匿名公开条件（与 docs/database-design.md §5 保持一致）。
const POST_PUBLIC_PREDICATE: &str =
    "p.status = 'published' AND p.visibility = 'public' AND p.deleted_at IS NULL";

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

/// 唯一约束名 → 冲突字段友好名。
fn unique_conflict_target(error: &PgDatabaseError) -> Option<&'static str> {
    match error.constraint()? {
        "posts_slug_key" | "pages_slug_key" => Some("slug"),
        "users_username_key" => Some("username"),
        "users_email_key" => Some("email"),
        "posts_series_position_unique" => Some("该系列位置"),
        "oauth_accounts_provider_provider_user_id_key" => Some("外部身份"),
        "categories_slug_key" | "series_slug_key" | "tags_slug_key" => Some("slug"),
        "roles_slug_key" | "permissions_key_key" => Some("标识"),
        _ => None,
    }
}

fn map_sqlx_error(error: sqlx::Error) -> UseCaseError {
    if let sqlx::Error::Database(db) = &error {
        let pg = db.try_downcast_ref::<PgDatabaseError>();
        if let Some(pg) = pg
            && pg.code() == "23505"
            && let Some(target) = unique_conflict_target(pg)
        {
            return UseCaseError::Conflict(target.to_string());
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
            "SELECT {POST_COLUMNS} FROM posts WHERE author_id = $1 ORDER BY updated_at DESC, id DESC"
        ))
        .bind(author_id)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        rows.iter().map(post_from_row).collect()
    }

    async fn insert(&self, snapshot: &PostSnapshot) -> Result<(), UseCaseError> {
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
        .execute(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(())
    }

    async fn save(
        &self,
        snapshot: &PostSnapshot,
        expected_version: i64,
        now: OffsetDateTime,
    ) -> Result<SaveOutcome, UseCaseError> {
        let updated = sqlx::query(
            r#"
            UPDATE posts SET
                title = $3, slug = $4, excerpt = $5, content = $6, cover = $7,
                series_id = $8, series_order = $9, status = $10, visibility = $11,
                published_at = $12, updated_at = $13, version = version + 1
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
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_error)?;

        if let Some(row) = updated {
            return Ok(SaveOutcome::Saved {
                new_version: row.try_get::<i64, _>(0).map_err(map_row_error)?,
            });
        }

        // 写入未命中：区分「版本过期」（可重试）与「记录已消失」（不可重试）。
        let current =
            sqlx::query("SELECT version, deleted_at IS NULL AS alive FROM posts WHERE id = $1")
                .bind(snapshot.id)
                .fetch_optional(&self.pool)
                .await
                .map_err(map_sqlx_error)?;
        match current {
            Some(row) if row.try_get::<bool, _>("alive").map_err(map_row_error)? => {
                Ok(SaveOutcome::StaleConflict)
            }
            _ => Ok(SaveOutcome::Gone),
        }
    }
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
        let row = sqlx::query(&format!(
            r#"
            SELECT p.title, p.slug, p.excerpt, p.published_at, p.updated_at, p.content,
                   COALESCE(NULLIF(u.display_name, ''), u.username) AS author_display,
                   u.username AS author_username
            FROM posts p
            JOIN users u ON u.id = p.author_id
            WHERE p.slug = $1 AND {POST_PUBLIC_PREDICATE}
            "#
        ))
        .bind(slug)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_error)?;

        row.map(|row| {
            Ok(PublicPostDetail {
                title: row.try_get("title").map_err(map_row_error)?,
                slug: row.try_get("slug").map_err(map_row_error)?,
                excerpt: row.try_get("excerpt").map_err(map_row_error)?,
                published_at: row.try_get("published_at").map_err(map_row_error)?,
                updated_at: row.try_get("updated_at").map_err(map_row_error)?,
                author_display: row.try_get("author_display").map_err(map_row_error)?,
                author_username: row.try_get("author_username").map_err(map_row_error)?,
                content: row.try_get("content").map_err(map_row_error)?,
            })
        })
        .transpose()
    }
}
