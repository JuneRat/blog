//! 后台列表专用 SQL：只投影展示字段，不读取 Markdown、HTML 或关联集合。
use super::sql::map_sqlx_error;
use application::content_queries::{AdminPageSummary, AdminPostSummary, ContentListFilter};
use application::error::UseCaseError;
use application::ports::{AdminPageQuery, AdminPostQuery};
use async_trait::async_trait;
use sqlx::{PgPool, Row};
use uuid::Uuid;

pub struct PostgresAdminContentQuery {
    pool: PgPool,
}
impl PostgresAdminContentQuery {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl AdminPostQuery for PostgresAdminContentQuery {
    async fn list(
        &self,
        author_id: Uuid,
        filter: &ContentListFilter,
    ) -> Result<(Vec<AdminPostSummary>, i64), UseCaseError> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        let (total,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM posts
             WHERE author_id = $1 AND (deleted_at IS NOT NULL) = $2
             AND ($3::text IS NULL OR status = $3)
             AND ($4::text IS NULL OR visibility = $4)",
        )
        .bind(author_id)
        .bind(filter.trash())
        .bind(filter.status())
        .bind(filter.visibility())
        .fetch_one(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        // 排序字段只由内部布尔值选择，不拼接用户输入。
        let order = if filter.trash() {
            "deleted_at"
        } else {
            "updated_at"
        };
        let rows = sqlx::query(&format!("SELECT id, slug, title, status, visibility, version, published_at, updated_at, author_id
             FROM posts WHERE author_id = $1 AND (deleted_at IS NOT NULL) = $2
             AND ($3::text IS NULL OR status = $3)
             AND ($4::text IS NULL OR visibility = $4)
             ORDER BY {order} DESC, id DESC LIMIT $5 OFFSET $6"))
            .bind(author_id)
            .bind(filter.trash())
            .bind(filter.status())
            .bind(filter.visibility())
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
                    status: row.try_get("status").map_err(map_sqlx_error)?,
                    visibility: row.try_get("visibility").map_err(map_sqlx_error)?,
                    version: row.try_get("version").map_err(map_sqlx_error)?,
                    published_at: row.try_get("published_at").map_err(map_sqlx_error)?,
                    updated_at: row.try_get("updated_at").map_err(map_sqlx_error)?,
                    author_id: row.try_get("author_id").map_err(map_sqlx_error)?,
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
        filter: &ContentListFilter,
    ) -> Result<(Vec<AdminPageSummary>, i64), UseCaseError> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        let (total,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM pages WHERE (deleted_at IS NOT NULL) = $1
             AND ($2::text IS NULL OR status = $2)
             AND ($3::text IS NULL OR visibility = $3)",
        )
        .bind(filter.trash())
        .bind(filter.status())
        .bind(filter.visibility())
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
             ORDER BY {order} DESC, id DESC LIMIT $4 OFFSET $5"
        ))
        .bind(filter.trash())
        .bind(filter.status())
        .bind(filter.visibility())
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
                    status: row.try_get("status").map_err(map_sqlx_error)?,
                    visibility: row.try_get("visibility").map_err(map_sqlx_error)?,
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
