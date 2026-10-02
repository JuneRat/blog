use super::sql::{PAGE_PUBLIC_PREDICATE, POST_PUBLIC_PREDICATE, map_row_error, map_sqlx_error};
use application::{
    UseCaseError,
    discovery::{ArchiveMonth, DiscoveryFilter, DiscoveryPage, DiscoveryRow, PublicDiscoveryQuery},
    ports::PublicPostSummary,
};
use async_trait::async_trait;
use sqlx::{PgPool, Row};

pub struct PostgresPublicDiscoveryQuery {
    pool: PgPool,
}
impl PostgresPublicDiscoveryQuery {
    pub fn new(database: crate::Database) -> Self {
        Self {
            pool: database.pool,
        }
    }
}
const POST_COLUMNS: &str = "p.id,p.title,p.slug,p.excerpt,p.published_at,COALESCE(NULLIF(u.display_name,''),u.username) AS author_display,u.avatar_media_id AS author_avatar_media_id,false AS is_page";
#[async_trait]
impl PublicDiscoveryQuery for PostgresPublicDiscoveryQuery {
    async fn list(
        &self,
        filter: &DiscoveryFilter,
        time_zone: &str,
        limit: i64,
        offset: i64,
    ) -> Result<DiscoveryPage, UseCaseError> {
        filter.validate()?;
        let mut params: Vec<String> = vec![];
        let select = match filter {
            DiscoveryFilter::Search(q) => {
                if q.is_empty() {
                    return Ok(DiscoveryPage::default());
                }
                params.push(q.clone());
                params.push(super::content_queries::substring_pattern(q));
                // Match the existing GIN expressions, then recheck individual
                // fields so a match cannot cross a concatenation boundary.
                format!(
                    "SELECT {POST_COLUMNS} FROM posts p JOIN users u ON u.id=p.author_id WHERE {POST_PUBLIC_PREDICATE} AND lower(p.title || E'\\n' || p.slug || E'\\n' || p.content || E'\\n' || coalesce(p.excerpt,'')) LIKE lower($2) ESCAPE '\\' AND (strpos(lower(p.title),lower($1))>0 OR strpos(lower(p.slug),lower($1))>0 OR strpos(lower(p.content),lower($1))>0 OR strpos(lower(coalesce(p.excerpt,'')),lower($1))>0) UNION ALL SELECT p.id,p.title,p.slug,NULL::text AS excerpt,p.published_at,''::text AS author_display,NULL::uuid AS author_avatar_media_id,true AS is_page FROM pages p WHERE {PAGE_PUBLIC_PREDICATE} AND lower(p.title || E'\\n' || p.slug || E'\\n' || p.content) LIKE lower($2) ESCAPE '\\' AND (strpos(lower(p.title),lower($1))>0 OR strpos(lower(p.slug),lower($1))>0 OR strpos(lower(p.content),lower($1))>0)"
                )
            }
            DiscoveryFilter::Author(name) => {
                params.push(name.to_lowercase());
                format!(
                    "SELECT {POST_COLUMNS} FROM posts p JOIN users u ON u.id=p.author_id WHERE {POST_PUBLIC_PREDICATE} AND lower(u.username)=$1"
                )
            }
            DiscoveryFilter::Archive(month) => {
                let mut select = format!(
                    "SELECT {POST_COLUMNS} FROM posts p JOIN users u ON u.id=p.author_id WHERE {POST_PUBLIC_PREDICATE}"
                );
                if let Some(month) = month {
                    params.push(format!("{month}-01"));
                    params.push(time_zone.to_owned());
                    select.push_str(" AND p.published_at >= ($1::date::timestamp AT TIME ZONE $2) AND p.published_at < (($1::date + interval '1 month') AT TIME ZONE $2)");
                }
                select
            }
        };
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        let count_sql = format!("SELECT count(*) FROM ({select}) candidates");
        let mut count = sqlx::query_scalar::<_, i64>(&count_sql);
        for param in &params {
            count = count.bind(param);
        }
        let total = count.fetch_one(&mut *tx).await.map_err(map_sqlx_error)?;
        let list_sql = format!(
            "SELECT * FROM ({select}) candidates ORDER BY published_at DESC,id DESC,is_page LIMIT ${} OFFSET ${}",
            params.len() + 1,
            params.len() + 2
        );
        let mut query = sqlx::query(&list_sql);
        for param in &params {
            query = query.bind(param);
        }
        let rows = query
            .bind(limit.clamp(1, 100))
            .bind(offset.max(0))
            .fetch_all(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        let items = rows
            .iter()
            .map(|row| {
                Ok(DiscoveryRow {
                    is_page: row.try_get("is_page").map_err(map_row_error)?,
                    post: PublicPostSummary {
                        title: row.try_get("title").map_err(map_row_error)?,
                        slug: row.try_get("slug").map_err(map_row_error)?,
                        excerpt: row.try_get("excerpt").map_err(map_row_error)?,
                        published_at: row.try_get("published_at").map_err(map_row_error)?,
                        author_display: row.try_get("author_display").map_err(map_row_error)?,
                        author_avatar_media_id: row
                            .try_get("author_avatar_media_id")
                            .map_err(map_row_error)?,
                    },
                })
            })
            .collect::<Result<Vec<_>, UseCaseError>>()?;
        let months = if matches!(filter, DiscoveryFilter::Archive(_)) {
            sqlx::query(&format!("SELECT to_char(p.published_at AT TIME ZONE $1,'YYYY-MM') AS month,count(*) AS count FROM posts p WHERE {POST_PUBLIC_PREDICATE} GROUP BY month ORDER BY month DESC LIMIT 120"))
                .bind(time_zone).fetch_all(&mut *tx).await.map_err(map_sqlx_error)?.iter().map(|row| Ok(ArchiveMonth {month:row.try_get("month").map_err(map_row_error)?,count:row.try_get("count").map_err(map_row_error)?})).collect::<Result<Vec<_>,UseCaseError>>()?
        } else {
            vec![]
        };
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(DiscoveryPage {
            items,
            total,
            months,
        })
    }
}
