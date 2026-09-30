use super::super::content::{audit_content, lock_content_relations};
use super::super::sql::{POST_PUBLIC_PREDICATE, map_row_error, map_sqlx_error};
use application::error::UseCaseError;
use application::ports::{
    PublicPostSummary, PublicTagSummary, PublicUrlEntry, PublishedTagQuery, TagDeleteOutcome,
    TagRepository, TagWithUsage,
};
use async_trait::async_trait;
use sqlx::{PgPool, Row};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// 标签目录与文章关联
// ---------------------------------------------------------------------------

pub struct PostgresTagRepository {
    pool: PgPool,
}

impl PostgresTagRepository {
    pub fn new(database: crate::Database) -> Self {
        let pool = database.pool;
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

fn tag_usage_from_row(row: &sqlx::postgres::PgRow) -> Result<TagWithUsage, UseCaseError> {
    Ok(TagWithUsage {
        snapshot: tag_from_row(row)?,
        public_post_count: row.try_get("public_post_count").map_err(map_row_error)?,
    })
}

/// 公开计数子查询：与公开文章谓词同口径（草稿/私密/回收站不计入）。
const TAG_PUBLIC_COUNT: &str = "(SELECT count(*) FROM post_tags pt JOIN posts p ON p.id = pt.post_id \
      WHERE pt.tag_id = t.id AND p.status = 'published' AND p.visibility = 'public' \
        AND p.deleted_at IS NULL AND p.published_at <= now())";

#[async_trait]
impl TagRepository for PostgresTagRepository {
    async fn insert(
        &self,
        aggregate: &domain::content::Tag,
        actor_id: application::audit::AuditContext,
    ) -> Result<(), UseCaseError> {
        let snapshot = aggregate.snapshot();
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        sqlx::query(
            "INSERT INTO tags (id, name, slug, version, created_at) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(snapshot.id)
        .bind(&snapshot.name)
        .bind(&snapshot.slug)
        .bind(snapshot.version)
        .bind(snapshot.created_at)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        audit_content(
            &mut tx,
            actor_id,
            "tag.create",
            "tag",
            snapshot.id,
            serde_json::json!({"version": snapshot.version}),
        )
        .await?;
        tx.commit().await.map_err(map_sqlx_error)?;
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

        rows.iter().map(tag_usage_from_row).collect()
    }

    async fn rename(
        &self,
        id: Uuid,
        new_name: &str,
        expected_version: i64,
        actor_id: application::audit::AuditContext,
    ) -> Result<Option<TagWithUsage>, UseCaseError> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        let row = sqlx::query(&format!(
            "UPDATE tags t SET name = $3, \
             version = CASE WHEN name IS DISTINCT FROM $3 THEN version + 1 ELSE version END, \
             updated_at = CASE WHEN name IS DISTINCT FROM $3 THEN now() ELSE updated_at END \
             WHERE id = $1 AND version = $2 \
             RETURNING {TAG_COLUMNS}, {TAG_PUBLIC_COUNT} AS public_post_count"
        ))
        .bind(id)
        .bind(expected_version)
        .bind(new_name)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        let result = row.as_ref().map(tag_usage_from_row).transpose()?;
        if result
            .as_ref()
            .is_some_and(|row| row.snapshot.version != expected_version)
        {
            audit_content(
                &mut tx,
                actor_id,
                "tag.update",
                "tag",
                id,
                serde_json::json!({"version": expected_version+1}),
            )
            .await?;
        }
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(result)
    }

    async fn delete(
        &self,
        id: Uuid,
        expected_version: i64,
        actor_id: application::audit::AuditContext,
    ) -> Result<TagDeleteOutcome, UseCaseError> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        lock_content_relations(&mut tx).await?;
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
        // 删除目录仅移除关联；文章保留并递增版本，旧编辑器不能复活该关联。
        let affected: Vec<(Uuid, i64)> = sqlx::query_as("UPDATE posts SET version=version+1, updated_at=now() WHERE id IN (SELECT post_id FROM post_tags WHERE tag_id=$1) RETURNING id,version")
            .bind(id).fetch_all(&mut *tx).await.map_err(map_sqlx_error)?;
        let targets: Vec<_> = affected.iter().map(|(id, _)| id.to_string()).collect();
        let entries: Vec<_> = affected
            .iter()
            .zip(&targets)
            .map(|((_, version), target)| crate::audit::AuditEntry {
                actor_id: actor_id.actor_id,
                ip_address: actor_id.ip_address,
                action: "post.tag_removed",
                target_type: "post",
                target_id: target,
                metadata: serde_json::json!({"tag_id":id,"version":version}),
            })
            .collect();
        crate::audit::append_audit_logs(&mut tx, &entries).await?;
        sqlx::query("DELETE FROM tags WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        audit_content(
            &mut tx,
            actor_id,
            "tag.delete",
            "tag",
            id,
            serde_json::json!({"affected_posts":affected.len()}),
        )
        .await?;
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
}

// ---------------------------------------------------------------------------
// 公开标签页查询
// ---------------------------------------------------------------------------

pub struct PostgresPublishedTagQuery {
    pool: PgPool,
}

impl PostgresPublishedTagQuery {
    pub fn new(database: crate::Database) -> Self {
        let pool = database.pool;
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
        // 总数与页面来自同一快照，LEFT JOIN 保证空页也保留总数。
        let rows = sqlx::query(&format!(
            r#"
            WITH matching AS (
            SELECT p.id, p.title, p.slug, p.excerpt, p.published_at,
                   COALESCE(NULLIF(u.display_name, ''), u.username) AS author_display,
                   u.avatar_media_id AS author_avatar_media_id
            FROM tags t
            JOIN post_tags pt ON pt.tag_id = t.id
            JOIN posts p ON p.id = pt.post_id
            JOIN users u ON u.id = p.author_id
            WHERE t.slug = $1 AND {POST_PUBLIC_PREDICATE}
            )
            SELECT page.*, totals.total
            FROM (SELECT count(*) AS total FROM matching) totals
            LEFT JOIN (
                SELECT * FROM matching
                ORDER BY published_at DESC, id DESC
                LIMIT $2 OFFSET $3
            ) page ON true
            ORDER BY page.published_at DESC, page.id DESC
            "#
        ))
        .bind(tag_slug)
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
