//! 后台列表专用 SQL：只投影展示字段；正文仅参与搜索谓词，不加载写聚合或正文结果。
use super::sql::map_sqlx_error;
use application::content_queries::{
    AdminPageSummary, AdminPostSummary, PageListFilter, PostListFilter,
};
use application::error::UseCaseError;
use application::ports::{AdminPageQuery, AdminPostQuery};
use async_trait::async_trait;
use domain::content::{Visibility, page::PageStatus, post::PostStatus};
use sqlx::{PgPool, Postgres, QueryBuilder, Row};
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
        let mut count = QueryBuilder::new("SELECT count(*) FROM posts");
        post_filters(&mut count, author_id, filter);
        let (total,): (i64,) = count
            .build_query_as()
            .fetch_one(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        let mut list = QueryBuilder::new(
            "SELECT posts.id, posts.slug, posts.title, posts.status, posts.visibility, posts.version, \
             posts.published_at, posts.updated_at, posts.author_id, users.username AS author_username \
             FROM posts JOIN users ON users.id = posts.author_id",
        );
        post_filters(&mut list, author_id, filter);
        list.push(if filter.trash() {
            " ORDER BY posts.deleted_at DESC, posts.id DESC LIMIT "
        } else {
            " ORDER BY posts.updated_at DESC, posts.id DESC LIMIT "
        })
        .push_bind(filter.limit())
        .push(" OFFSET ")
        .push_bind(filter.offset());
        let rows = list
            .build()
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
        let mut count = QueryBuilder::new("SELECT count(*) FROM pages");
        page_filters(&mut count, filter);
        let (total,): (i64,) = count
            .build_query_as()
            .fetch_one(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        let mut list = QueryBuilder::new(
            "SELECT id, slug, title, status, visibility, version, published_at, updated_at FROM pages",
        );
        page_filters(&mut list, filter);
        list.push(if filter.trash() {
            " ORDER BY deleted_at DESC, id DESC LIMIT "
        } else {
            " ORDER BY updated_at DESC, id DESC LIMIT "
        })
        .push_bind(filter.limit())
        .push(" OFFSET ")
        .push_bind(filter.offset());
        let rows = list
            .build()
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

// Build only the requested predicates. In particular, static trash predicates let
// PostgreSQL use partial ordering indexes even after switching to a generic plan.
fn post_filters<'a>(
    query: &mut QueryBuilder<'a, Postgres>,
    author_id: Option<Uuid>,
    filter: &'a PostListFilter,
) {
    query.push(if filter.trash() {
        " WHERE posts.deleted_at IS NOT NULL"
    } else {
        " WHERE posts.deleted_at IS NULL"
    });
    if let Some(id) = author_id {
        query.push(" AND posts.author_id = ").push_bind(id);
    }
    if let Some(status) = filter.status() {
        query
            .push(" AND posts.status = ")
            .push_bind(status.as_str());
    }
    if let Some(visibility) = filter.visibility() {
        query
            .push(" AND posts.visibility = ")
            .push_bind(visibility.as_str());
    }
    if let Some(id) = filter.category_id() {
        query.push(" AND posts.category_id = ").push_bind(id);
    }
    if let Some(q) = filter.q() {
        search_filter(
            query,
            q,
            &[
                "posts.title",
                "posts.slug",
                "posts.content",
                "coalesce(posts.excerpt, '')",
            ],
        );
    }
}

fn page_filters<'a>(query: &mut QueryBuilder<'a, Postgres>, filter: &'a PageListFilter) {
    query.push(if filter.trash() {
        " WHERE pages.deleted_at IS NOT NULL"
    } else {
        " WHERE pages.deleted_at IS NULL"
    });
    if let Some(status) = filter.status() {
        query
            .push(" AND pages.status = ")
            .push_bind(status.as_str());
    }
    if let Some(visibility) = filter.visibility() {
        query
            .push(" AND pages.visibility = ")
            .push_bind(visibility.as_str());
    }
    if let Some(q) = filter.q() {
        search_filter(query, q, &["pages.title", "pages.slug", "pages.content"]);
    }
}

fn search_filter<'a>(query: &mut QueryBuilder<'a, Postgres>, q: &'a str, columns: &[&'static str]) {
    // One trigram candidate index per content type keeps Chinese/case-insensitive
    // literal substring semantics. The original per-field predicate rejects a
    // match spanning concatenated fields (e.g. a query containing a newline).
    // Avoid an unselective full trigram-index scan for short/punctuation-only
    // input. Such input keeps the existing per-field substring scan.
    let mut run = 0;
    let can_use_trigrams = q.chars().any(|ch| {
        run = if ch.is_alphanumeric() { run + 1 } else { 0 };
        run >= 3
    });
    if can_use_trigrams {
        query.push(" AND lower(");
        for (i, column) in columns.iter().enumerate() {
            if i > 0 {
                query.push(" || E'\\n' || ");
            }
            query.push(*column);
        }
        query
            .push(") LIKE lower(")
            .push_bind(substring_pattern(q))
            .push(r") ESCAPE E'\\'");
    }
    query.push(" AND (");
    for (i, column) in columns.iter().enumerate() {
        if i > 0 {
            query.push(" OR ");
        }
        query
            .push("strpos(lower(")
            .push(*column)
            .push("), lower(")
            .push_bind(q)
            .push(")) > 0");
    }
    query.push(")");
}

fn substring_pattern(q: &str) -> String {
    let mut pattern = String::with_capacity(q.len() + 2);
    pattern.push('%');
    for ch in q.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            pattern.push('\\');
        }
        pattern.push(ch);
    }
    pattern.push('%');
    pattern
}
