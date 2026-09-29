//! 后台列表专用 SQL：只投影展示字段；正文仅参与搜索谓词，不加载写聚合或正文结果。
use super::sql::map_sqlx_error;
use application::content_queries::{
    AdminPageSummary, AdminPostSummary, PageListFilter, PostListFilter,
};
use application::error::UseCaseError;
use application::ports::{AdminPageQuery, AdminPostQuery};
use async_trait::async_trait;
use domain::content::{Visibility, page::PageStatus, post::PostStatus};
use sqlx::{PgPool, Row};
use uuid::Uuid;

pub struct PostgresAdminContentQuery {
    pool: PgPool,
}
impl PostgresAdminContentQuery {
    pub fn new(database: crate::Database) -> Self {
        let pool = database.pool;
        Self { pool }
    }
}

#[async_trait]
impl AdminPostQuery for PostgresAdminContentQuery {
    async fn list(
        &self,
        author_id: Option<Uuid>,
        filter: &PostListFilter,
    ) -> Result<(Vec<AdminPostSummary>, i64), UseCaseError> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        let (total,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM posts
             WHERE ($1::uuid IS NULL OR author_id = $1) AND (deleted_at IS NOT NULL) = $2
             AND ($3::text IS NULL OR status = $3)
             AND ($4::text IS NULL OR visibility = $4)
             AND ($5::text IS NULL OR strpos(lower(title), lower($5)) > 0
                  OR strpos(lower(slug), lower($5)) > 0 OR strpos(lower(content), lower($5)) > 0
                  OR strpos(lower(coalesce(excerpt, '')), lower($5)) > 0)
             AND ($6::uuid IS NULL OR category_id = $6)",
        )
        .bind(author_id)
        .bind(filter.trash())
        .bind(filter.status().map(|status| status.as_str()))
        .bind(filter.visibility().map(Visibility::as_str))
        .bind(filter.q())
        .bind(filter.category_id())
        .fetch_one(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        // 排序字段只由内部布尔值选择，不拼接用户输入。
        let order = if filter.trash() {
            "deleted_at"
        } else {
            "updated_at"
        };
        let rows = sqlx::query(&format!("SELECT posts.id, posts.slug, title, posts.status, visibility, posts.version, published_at, posts.updated_at, author_id, users.username AS author_username
             FROM posts JOIN users ON users.id = posts.author_id WHERE ($1::uuid IS NULL OR author_id = $1) AND (posts.deleted_at IS NOT NULL) = $2
             AND ($3::text IS NULL OR posts.status = $3)
             AND ($4::text IS NULL OR visibility = $4)
             AND ($5::text IS NULL OR strpos(lower(title), lower($5)) > 0
                  OR strpos(lower(slug), lower($5)) > 0 OR strpos(lower(content), lower($5)) > 0
                  OR strpos(lower(coalesce(excerpt, '')), lower($5)) > 0)
             AND ($6::uuid IS NULL OR category_id = $6)
             ORDER BY posts.{order} DESC, posts.id DESC LIMIT $7 OFFSET $8"))
            .bind(author_id)
            .bind(filter.trash())
            .bind(filter.status().map(|status| status.as_str()))
            .bind(filter.visibility().map(Visibility::as_str))
        .bind(filter.q())
        .bind(filter.category_id())
            .bind(filter.limit())
            .bind(filter.offset())
            .fetch_all(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        let items = rows
            .iter()
            .map(|row| {
                Ok(AdminPostSummary {
                    id: row.try_get("id").map_err(map_sqlx_error)?,
                    slug: row.try_get("slug").map_err(map_sqlx_error)?,
                    title: row.try_get("title").map_err(map_sqlx_error)?,
                    status: PostStatus::parse(row.try_get("status").map_err(map_sqlx_error)?)
                        .ok_or_else(|| UseCaseError::Repository("无效的文章状态".into()))?,
                    visibility: Visibility::parse(
                        row.try_get("visibility").map_err(map_sqlx_error)?,
                    )
                    .ok_or_else(|| UseCaseError::Repository("无效的可见性".into()))?,
                    version: row.try_get("version").map_err(map_sqlx_error)?,
                    published_at: row.try_get("published_at").map_err(map_sqlx_error)?,
                    updated_at: row.try_get("updated_at").map_err(map_sqlx_error)?,
                    author_id: row.try_get("author_id").map_err(map_sqlx_error)?,
                    author_username: row.try_get("author_username").map_err(map_sqlx_error)?,
                })
            })
            .collect::<Result<Vec<_>, UseCaseError>>()?;
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok((items, total))
    }
}

#[async_trait]
impl AdminPageQuery for PostgresAdminContentQuery {
    async fn list(
        &self,
        filter: &PageListFilter,
    ) -> Result<(Vec<AdminPageSummary>, i64), UseCaseError> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        let (total,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM pages WHERE (deleted_at IS NOT NULL) = $1
             AND ($2::text IS NULL OR status = $2)
             AND ($3::text IS NULL OR visibility = $3)
             AND ($4::text IS NULL OR strpos(lower(title), lower($4)) > 0
                  OR strpos(lower(slug), lower($4)) > 0 OR strpos(lower(content), lower($4)) > 0)",
        )
        .bind(filter.trash())
        .bind(filter.status().map(|status| status.as_str()))
        .bind(filter.visibility().map(Visibility::as_str))
        .bind(filter.q())
        .fetch_one(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        // 排序字段只由内部布尔值选择，不拼接用户输入。
        let order = if filter.trash() {
            "deleted_at"
        } else {
            "updated_at"
        };
        let rows = sqlx::query(&format!(
            "SELECT id, slug, title, status, visibility, version, published_at, updated_at
             FROM pages WHERE (deleted_at IS NOT NULL) = $1
             AND ($2::text IS NULL OR status = $2)
             AND ($3::text IS NULL OR visibility = $3)
             AND ($4::text IS NULL OR strpos(lower(title), lower($4)) > 0
                  OR strpos(lower(slug), lower($4)) > 0 OR strpos(lower(content), lower($4)) > 0)
             ORDER BY {order} DESC, id DESC LIMIT $5 OFFSET $6"
        ))
        .bind(filter.trash())
        .bind(filter.status().map(|status| status.as_str()))
        .bind(filter.visibility().map(Visibility::as_str))
        .bind(filter.q())
        .bind(filter.limit())
        .bind(filter.offset())
        .fetch_all(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        let items = rows
            .iter()
            .map(|row| {
                Ok(AdminPageSummary {
                    id: row.try_get("id").map_err(map_sqlx_error)?,
                    slug: row.try_get("slug").map_err(map_sqlx_error)?,
                    title: row.try_get("title").map_err(map_sqlx_error)?,
                    status: PageStatus::parse(row.try_get("status").map_err(map_sqlx_error)?)
                        .ok_or_else(|| UseCaseError::Repository("无效的页面状态".into()))?,
                    visibility: Visibility::parse(
                        row.try_get("visibility").map_err(map_sqlx_error)?,
                    )
                    .ok_or_else(|| UseCaseError::Repository("无效的可见性".into()))?,
                    version: row.try_get("version").map_err(map_sqlx_error)?,
                    published_at: row.try_get("published_at").map_err(map_sqlx_error)?,
                    updated_at: row.try_get("updated_at").map_err(map_sqlx_error)?,
                })
            })
            .collect::<Result<Vec<_>, UseCaseError>>()?;
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok((items, total))
    }
}
