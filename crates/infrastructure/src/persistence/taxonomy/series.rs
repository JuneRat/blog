use super::super::content::{audit_content, lock_content_relations};
use super::super::media::{clear_media_refs, media_ids_for, sync_media_refs};
use super::super::sql::{POST_PUBLIC_PREDICATE, map_row_error, map_sqlx_error};
use application::error::UseCaseError;
use application::ports::{
    MediaContentKind, PublicPostSummary, PublicSeriesSummary, PublicUrlEntry, PublishedSeriesQuery,
    ReorderOutcome, SeriesDeleteOutcome, SeriesMember, SeriesRepository, SeriesWithUsage,
};
use async_trait::async_trait;
use sqlx::{PgPool, Row};
use time::OffsetDateTime;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// 系列目录与并发重排
// ---------------------------------------------------------------------------

pub struct PostgresSeriesRepository {
    pool: PgPool,
}

impl PostgresSeriesRepository {
    pub fn new(database: crate::Database) -> Self {
        let pool = database.pool;
        Self { pool }
    }
}

const SERIES_COLUMNS: &str =
    "id, name, slug, description, cover_media_id, version, created_at, updated_at";

fn series_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<domain::content::SeriesSnapshot, UseCaseError> {
    Ok(domain::content::SeriesSnapshot {
        id: row.try_get("id").map_err(map_row_error)?,
        name: row.try_get("name").map_err(map_row_error)?,
        slug: row.try_get("slug").map_err(map_row_error)?,
        description: row.try_get("description").map_err(map_row_error)?,
        cover_media_id: row.try_get("cover_media_id").map_err(map_row_error)?,
        version: row.try_get("version").map_err(map_row_error)?,
        created_at: row.try_get("created_at").map_err(map_row_error)?,
        updated_at: row.try_get("updated_at").map_err(map_row_error)?,
    })
}

fn series_usage_from_row(row: &sqlx::postgres::PgRow) -> Result<SeriesWithUsage, UseCaseError> {
    Ok(SeriesWithUsage {
        snapshot: series_from_row(row)?,
        post_count: row.try_get("post_count").map_err(map_row_error)?,
        public_post_count: row.try_get("public_post_count").map_err(map_row_error)?,
    })
}

#[async_trait]
impl SeriesRepository for PostgresSeriesRepository {
    async fn insert(
        &self,
        aggregate: &domain::content::Series,
        actor_id: application::audit::AuditContext,
    ) -> Result<(), UseCaseError> {
        let snapshot = aggregate.snapshot();
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        lock_content_relations(&mut tx).await?;
        sqlx::query(
            "INSERT INTO series (id, name, slug, description, cover_media_id, version, created_at, \
             updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(snapshot.id)
        .bind(&snapshot.name)
        .bind(&snapshot.slug)
        .bind(&snapshot.description)
        .bind(snapshot.cover_media_id)
        .bind(snapshot.version)
        .bind(snapshot.created_at)
        .bind(snapshot.updated_at)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        sync_media_refs(
            &mut tx,
            MediaContentKind::Series,
            snapshot.id,
            &media_ids_for(&[], snapshot.cover_media_id),
        )
        .await?;
        audit_content(
            &mut tx,
            actor_id,
            "series.create",
            "series",
            snapshot.id,
            serde_json::json!({"version":snapshot.version}),
        )
        .await?;
        tx.commit().await.map_err(map_sqlx_error)
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
            "SELECT s.id, s.name, s.slug, s.description, s.cover_media_id, s.version, s.created_at, \
                    s.updated_at, \
                    (SELECT count(*) FROM post_series ps JOIN posts p ON p.id=ps.post_id WHERE ps.series_id = s.id) AS post_count, \
                    (SELECT count(*) FROM post_series ps JOIN posts p ON p.id=ps.post_id WHERE ps.series_id = s.id \
                       AND p.status = 'published' AND p.visibility = 'public' \
                       AND p.deleted_at IS NULL AND p.published_at <= now()) AS public_post_count \
             FROM series s ORDER BY s.slug",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        rows.iter().map(series_usage_from_row).collect()
    }

    async fn update(
        &self,
        id: Uuid,
        name: &str,
        description: Option<&str>,
        cover_media_id: Option<Uuid>,
        expected_version: i64,
        actor_id: application::audit::AuditContext,
    ) -> Result<Option<SeriesWithUsage>, UseCaseError> {
        // name/描述/封面与引用行在同一事务：封面替换时旧图必须同时被释放，
        // 否则会出现「列里已换新图、引用表还占着旧图」的幽灵占用。
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        lock_content_relations(&mut tx).await?;
        let row = sqlx::query(
            "UPDATE series s SET name = $3, description = $4, cover_media_id = $5, \
             version = CASE WHEN (name, description, cover_media_id) IS DISTINCT FROM ($3::text, $4::text, $5::uuid) \
                            THEN version + 1 ELSE version END, \
             updated_at = CASE WHEN (name, description, cover_media_id) IS DISTINCT FROM ($3::text, $4::text, $5::uuid) \
                               THEN now() ELSE updated_at END \
             WHERE id = $1 AND version = $2 \
             RETURNING id, name, slug, description, cover_media_id, version, created_at, updated_at, \
               (SELECT count(*) FROM post_series ps WHERE ps.series_id=s.id) AS post_count, \
               (SELECT count(*) FROM post_series ps JOIN posts p ON p.id=ps.post_id \
                WHERE ps.series_id=s.id AND p.status='published' AND p.visibility='public' \
                  AND p.deleted_at IS NULL AND p.published_at<=now()) AS public_post_count",
        )
        .bind(id)
        .bind(expected_version)
        .bind(name)
        .bind(description)
        .bind(cover_media_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        let Some(row) = row else {
            tx.commit().await.map_err(map_sqlx_error)?;
            return Ok(None);
        };
        let result = series_usage_from_row(&row)?;
        let snapshot = &result.snapshot;
        if snapshot.version == expected_version {
            tx.commit().await.map_err(map_sqlx_error)?;
            return Ok(Some(result));
        }
        // 系列封面与引用同事务固化，保留站内使用统计与物理清理保护。
        sync_media_refs(
            &mut tx,
            MediaContentKind::Series,
            snapshot.id,
            &media_ids_for(&[], snapshot.cover_media_id),
        )
        .await?;
        audit_content(
            &mut tx,
            actor_id,
            "series.update",
            "series",
            id,
            serde_json::json!({"version":snapshot.version}),
        )
        .await?;
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(Some(result))
    }

    async fn delete(
        &self,
        id: Uuid,
        expected_version: i64,
        actor_id: application::audit::AuditContext,
    ) -> Result<SeriesDeleteOutcome, UseCaseError> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        lock_content_relations(&mut tx).await?;
        // 关系锁阻止成员并发变化，系列行锁保护版本检查与删除。
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
        let affected: Vec<(Uuid,i64)> = sqlx::query_as("UPDATE posts SET version=version+1,updated_at=now() WHERE id IN (SELECT post_id FROM post_series WHERE series_id=$1) RETURNING id,version")
            .bind(id).fetch_all(&mut *tx).await.map_err(map_sqlx_error)?;
        for (post_id, version) in &affected {
            audit_content(
                &mut tx,
                actor_id,
                "post.series_removed",
                "post",
                *post_id,
                serde_json::json!({"series_id":id,"version":version}),
            )
            .await?;
        }
        // 内容物理删除：其媒体引用必须在同一事务清理（source_id 是多态引用，
        // 没有外键级联兜底）。系列只有封面一种引用来源。
        clear_media_refs(&mut tx, MediaContentKind::Series, id).await?;
        sqlx::query("DELETE FROM series WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        audit_content(
            &mut tx,
            actor_id,
            "series.delete",
            "series",
            id,
            serde_json::json!({"affected_posts":affected.len()}),
        )
        .await?;
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(SeriesDeleteOutcome::Deleted)
    }

    async fn members_of(&self, series_id: Uuid) -> Result<Vec<SeriesMember>, UseCaseError> {
        let rows = sqlx::query(
            "SELECT p.id, p.author_id, p.slug, p.title, p.status, p.visibility, ps.position, p.deleted_at FROM post_series ps JOIN posts p ON p.id=ps.post_id WHERE ps.series_id=$1 ORDER BY ps.position, p.id",
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
                    position: row.try_get("position").map_err(map_row_error)?,
                })
            })
            .collect()
    }

    /// PostgreSQL 实现：关系锁内校验版本与完整成员集合，再按 ID 序锁成员行；仅改变 position 的成员递增文章版本。
    async fn reorder(
        &self,
        series_id: Uuid,
        expected_series_version: i64,
        ordered_post_ids: &[Uuid],
        actor_id: application::audit::AuditContext,
    ) -> Result<ReorderOutcome, UseCaseError> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        lock_content_relations(&mut tx).await?;
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
            sqlx::query_as("SELECT post_id FROM post_series WHERE series_id=$1 ORDER BY post_id")
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

        i32::try_from(ordered_post_ids.len())
            .map_err(|_| UseCaseError::Invalid("系列成员过多".into()))?;
        // The row locks above remain in stable ID order. Both relationship and
        // post changes are set-based; unchanged members get no version/audit bump.
        let changed: Vec<(Uuid, i32, i64)> = sqlx::query_as(
            "WITH desired AS (SELECT post_id,(ordinality-1)::int AS position FROM unnest($1::uuid[]) WITH ORDINALITY AS d(post_id,ordinality)), \
             moved AS (UPDATE post_series ps SET position=d.position FROM desired d WHERE ps.series_id=$2 AND ps.post_id=d.post_id AND ps.position<>d.position RETURNING ps.post_id,ps.position) \
             UPDATE posts p SET version=p.version+1,updated_at=now() FROM moved m WHERE p.id=m.post_id RETURNING p.id,m.position,p.version"
        ).bind(ordered_post_ids).bind(series_id).fetch_all(&mut *tx).await.map_err(map_sqlx_error)?;
        if changed.is_empty() {
            tx.commit().await.map_err(map_sqlx_error)?;
            return Ok(ReorderOutcome::Reordered {
                new_version: current_version,
            });
        }
        let targets: Vec<_> = changed.iter().map(|(id, _, _)| id.to_string()).collect();
        let entries: Vec<_> = changed.iter().zip(&targets).map(|((_,position,version),target)| crate::audit::AuditEntry {
            actor_id: actor_id.actor_id, ip_address: actor_id.ip_address,
            action: "post.series_reordered", target_type: "post", target_id: target,
            metadata: serde_json::json!({"series_id":series_id,"position":position,"version":version}),
        }).collect();
        crate::audit::append_audit_logs(&mut tx, &entries).await?;

        let bumped: Option<(i64,)> = sqlx::query_as(
            "UPDATE series SET version = version + 1, updated_at = now() \
             WHERE id = $1 RETURNING version",
        )
        .bind(series_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        audit_content(
            &mut tx,
            actor_id,
            "series.reorder",
            "series",
            series_id,
            serde_json::json!({"version":current_version+1}),
        )
        .await?;
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
    pub fn new(database: crate::Database) -> Self {
        let pool = database.pool;
        Self { pool }
    }
}

#[async_trait]
impl PublishedSeriesQuery for PostgresPublishedSeriesQuery {
    async fn find_public_by_slug(
        &self,
        slug: &str,
    ) -> Result<Option<PublicSeriesSummary>, UseCaseError> {
        let row: Option<(String, String, Option<Uuid>)> =
            sqlx::query_as("SELECT slug, name, cover_media_id FROM series WHERE slug = $1")
                .bind(slug)
                .fetch_optional(&self.pool)
                .await
                .map_err(map_sqlx_error)?;
        Ok(row.map(|(slug, name, cover_media_id)| PublicSeriesSummary {
            slug,
            name,
            cover_media_id,
        }))
    }

    async fn list_public_posts_by_series(
        &self,
        series_slug: &str,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<PublicPostSummary>, i64), UseCaseError> {
        let limit = limit.clamp(1, 100);
        let offset = offset.max(0);
        // 重复权重按文章 id 稳定排序；非公开成员保留关联但不出现在结果中。
        let rows = sqlx::query(&format!(
            r#"
            WITH matching AS (
            SELECT p.id, ps.position, p.title, p.slug, p.excerpt, p.published_at,
                   COALESCE(NULLIF(u.display_name, ''), u.username) AS author_display,
                   u.avatar_media_id AS author_avatar_media_id
            FROM series s
            JOIN post_series ps ON ps.series_id = s.id
            JOIN posts p ON p.id = ps.post_id
            JOIN users u ON u.id = p.author_id
            WHERE s.slug = $1 AND {POST_PUBLIC_PREDICATE}
            )
            SELECT page.*, totals.total
            FROM (SELECT count(*) AS total FROM matching) totals
            LEFT JOIN (
                SELECT * FROM matching
                ORDER BY position ASC, id ASC
                LIMIT $2 OFFSET $3
            ) page ON true
            ORDER BY page.position ASC, page.id ASC
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
                if row
                    .try_get::<Option<String>, _>("slug")
                    .map_err(map_row_error)?
                    .is_none()
                {
                    return Ok(None);
                }
                Ok(Some(PublicPostSummary {
                    title: row.try_get("title").map_err(map_row_error)?,
                    slug: row.try_get("slug").map_err(map_row_error)?,
                    excerpt: row.try_get("excerpt").map_err(map_row_error)?,
                    published_at: row.try_get("published_at").map_err(map_row_error)?,
                    author_display: row.try_get("author_display").map_err(map_row_error)?,
                    author_avatar_media_id: row
                        .try_get("author_avatar_media_id")
                        .map_err(map_row_error)?,
                }))
            })
            .collect::<Result<Vec<_>, UseCaseError>>()?
            .into_iter()
            .flatten()
            .collect();
        Ok((posts, total))
    }

    async fn list_public_directories(
        &self,
        limit: i64,
    ) -> Result<Vec<PublicUrlEntry>, UseCaseError> {
        // 与分类页同口径：只收录有公开文章的系列；lastmod 取系列改名与成员更新的较晚者。
        let rows = sqlx::query(&format!(
            r#"
            SELECT s.slug, GREATEST(s.updated_at, max(p.updated_at)) AS updated_at
            FROM series s
            JOIN post_series ps ON ps.series_id = s.id
            JOIN posts p ON p.id = ps.post_id
            WHERE {POST_PUBLIC_PREDICATE}
            GROUP BY s.id, s.slug, s.updated_at
            ORDER BY s.slug
            LIMIT $1
            "#
        ))
        .bind(limit.max(0))
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

#[async_trait::async_trait]
impl application::ports::SeriesLookup for PostgresSeriesRepository {
    async fn existing_ids(&self, ids: &[Uuid]) -> Result<Vec<Uuid>, UseCaseError> {
        sqlx::query_scalar("SELECT id FROM series WHERE id = ANY($1::uuid[]) ORDER BY id")
            .bind(ids)
            .fetch_all(&self.pool)
            .await
            .map_err(map_sqlx_error)
    }
}
