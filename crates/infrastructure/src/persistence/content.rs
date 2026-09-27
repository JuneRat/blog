use std::sync::Arc;

use async_trait::async_trait;
use sqlx::{PgPool, Row};
use time::OffsetDateTime;
use uuid::Uuid;

use application::error::UseCaseError;
use application::ports::{
    ContentRenderer, MediaContentKind, PageCommitOutcome, PageDeleteOutcome, PageRepository,
    PostCommitOutcome, PostRecord, PostRepository, PublicCategoryRef, PublicPageDetail,
    PublicPostDetail, PublicPostSummary, PublicUrlEntry, PublishedPageQuery, PublishedPostQuery,
    SaveOutcome,
};
use domain::content::{Page, PageSnapshot, PageStatus};
use domain::content::{Post, PostSnapshot, PostStatus, Visibility};

use super::media::{clear_media_refs, media_ids_for, sync_media_refs};
use super::sql::{PAGE_PUBLIC_PREDICATE, POST_PUBLIC_PREDICATE, map_row_error, map_sqlx_error};
use crate::audit::{AuditEntry, append_audit_log};

/// 渲染或清洗规则变更时递增；启动迁移将重建不匹配的派生内容。
pub const CONTENT_RENDER_VERSION: i32 = 1;

/// 重建持久化派生内容。分批读取、事务外渲染，CAS 防止覆盖同时保存的新正文。
/// HTML 与媒体引用一起更新，不制造编辑版本或改变业务更新时间。
pub async fn rebuild_content_html(
    pool: &PgPool,
    renderer: &dyn ContentRenderer,
) -> Result<usize, UseCaseError> {
    let mut rebuilt = 0;
    for (table, kind, cover) in [
        ("posts", MediaContentKind::Post, "cover_media_id"),
        ("pages", MediaContentKind::Page, "NULL::uuid"),
    ] {
        loop {
            let rows: Vec<(Uuid, String, i64, Option<Uuid>)> = sqlx::query_as(&format!(
                "SELECT id, content, version, {cover} FROM {table} \
                 WHERE content_render_version <> $1 ORDER BY id LIMIT 100"
            ))
            .bind(CONTENT_RENDER_VERSION)
            .fetch_all(pool)
            .await
            .map_err(map_sqlx_error)?;
            if rows.is_empty() {
                break;
            }
            for (id, source, version, cover_media_id) in rows {
                let rendered = renderer.render_content(&source).await?;
                application::rendering_budget::validate_html(&rendered.content_html)
                    .map_err(|e| UseCaseError::Invalid(e.to_string()))?;
                let media_ids = media_ids_for(&rendered.media_ids, cover_media_id);
                let mut tx = pool.begin().await.map_err(map_sqlx_error)?;
                let changed = sqlx::query(&format!(
                    "UPDATE {table} SET content_html = $2, content_render_version = $3 \
                     WHERE id = $1 AND version = $4 AND content = $5 \
                     AND content_render_version <> $3"
                ))
                .bind(id)
                .bind(&rendered.content_html)
                .bind(CONTENT_RENDER_VERSION)
                .bind(version)
                .bind(&source)
                .execute(&mut *tx)
                .await
                .map_err(map_sqlx_error)?
                .rows_affected();
                if changed == 1 {
                    sync_media_refs(&mut tx, kind, id, &media_ids).await?;
                    audit_content(
                        &mut tx,
                        None,
                        &format!("{}.html.rebuild", kind.as_str()),
                        kind.as_str(),
                        id,
                        serde_json::json!({"version": version, "render_version": CONTENT_RENDER_VERSION}),
                    ).await?;
                }
                tx.commit().await.map_err(map_sqlx_error)?;
                rebuilt += changed as usize;
            }
        }
    }
    Ok(rebuilt)
}

// ---------------------------------------------------------------------------
// 文章仓储
// ---------------------------------------------------------------------------

pub struct PostgresPostRepository {
    pool: PgPool,
    renderer: Arc<dyn ContentRenderer>,
}

impl PostgresPostRepository {
    pub fn new(pool: PgPool, renderer: Arc<dyn ContentRenderer>) -> Self {
        Self { pool, renderer }
    }

    /// 调用方持有文章行锁/本次 INSERT，关联写入同样遵循该锁协议。
    /// 必须在 commit 之前读取，返回值因此属于本次业务提交。
    async fn record_in_transaction(
        &self,
        tx: &mut sqlx::PgConnection,
        id: Uuid,
    ) -> Result<PostRecord, UseCaseError> {
        let row = sqlx::query(&format!(
            "SELECT {POST_COLUMNS}, {POST_TAG_IDS} FROM posts WHERE id = $1"
        ))
        .bind(id)
        .fetch_one(tx)
        .await
        .map_err(map_sqlx_error)?;
        post_record_from_row(&row)
    }

    async fn insert_record(
        &self,
        snapshot: &PostSnapshot,
        tag_ids: &[Uuid],
        actor_id: Option<Uuid>,
    ) -> Result<PostRecord, UseCaseError> {
        let rendered = render_content(&*self.renderer, &snapshot.content).await?;
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        lock_content_relations(&mut tx).await?;
        sqlx::query("INSERT INTO posts (id, author_id, category_id, title, slug, excerpt, content, cover_media_id, status, visibility, published_at, version, created_at, updated_at, deleted_at, content_html, content_render_version) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17)")
            .bind(snapshot.id).bind(snapshot.author_id).bind(snapshot.category_id).bind(&snapshot.title)
            .bind(&snapshot.slug).bind(&snapshot.excerpt).bind(&snapshot.content).bind(snapshot.cover_media_id)
            .bind(snapshot.status.as_str()).bind(snapshot.visibility.as_str()).bind(snapshot.published_at)
            .bind(snapshot.version).bind(snapshot.created_at).bind(snapshot.updated_at).bind(snapshot.deleted_at)
            .bind(&rendered.content_html).bind(CONTENT_RENDER_VERSION)
            .execute(&mut *tx).await.map_err(map_sqlx_error)?;
        insert_post_tags(&mut tx, snapshot.id, tag_ids)
            .await
            .map_err(map_sqlx_error)?;
        replace_post_series(&mut tx, snapshot.id, &[], &snapshot.series).await?;
        sync_media_refs(
            &mut tx,
            MediaContentKind::Post,
            snapshot.id,
            &media_ids_for(&rendered.media_ids, snapshot.cover_media_id),
        )
        .await?;
        audit_content(
            &mut tx,
            actor_id,
            "post.create",
            "post",
            snapshot.id,
            serde_json::json!({"version": snapshot.version}),
        )
        .await?;
        let record = self.record_in_transaction(&mut tx, snapshot.id).await?;
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(record)
    }

    async fn save_record(
        &self,
        snapshot: &PostSnapshot,
        expected_version: i64,
        now: OffsetDateTime,
        tag_ids: Option<&[Uuid]>,
        actor_id: Option<Uuid>,
    ) -> Result<PostCommitOutcome, UseCaseError> {
        let rendered = render_content(&*self.renderer, &snapshot.content).await?;
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        lock_content_relations(&mut tx).await?;
        let row = sqlx::query(&format!(
            "SELECT {POST_COLUMNS} FROM posts WHERE id=$1 FOR UPDATE"
        ))
        .bind(snapshot.id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        let Some(row) = row else {
            return Ok(PostCommitOutcome::Gone);
        };
        let previous = post_from_row(&row)?;
        if previous.deleted_at.is_some() {
            return Ok(PostCommitOutcome::Gone);
        }
        if previous.version != expected_version {
            return Ok(PostCommitOutcome::StaleConflict);
        }
        sqlx::query("UPDATE posts SET title=$3, slug=$4, excerpt=$5, content=$6, category_id=$7, cover_media_id=$8, status=$9, visibility=$10, published_at=$11, updated_at=$12, version=version+1, content_html=$13, content_render_version=$14 WHERE id=$1 AND version=$2 AND deleted_at IS NULL")
            .bind(snapshot.id).bind(expected_version).bind(&snapshot.title).bind(&snapshot.slug)
            .bind(&snapshot.excerpt).bind(&snapshot.content).bind(snapshot.category_id).bind(snapshot.cover_media_id)
            .bind(snapshot.status.as_str()).bind(snapshot.visibility.as_str()).bind(snapshot.published_at)
            .bind(now).bind(&rendered.content_html).bind(CONTENT_RENDER_VERSION)
            .execute(&mut *tx).await.map_err(map_sqlx_error)?;
        if let Some(ids) = tag_ids {
            sqlx::query("DELETE FROM post_tags WHERE post_id=$1")
                .bind(snapshot.id)
                .execute(&mut *tx)
                .await
                .map_err(map_sqlx_error)?;
            insert_post_tags(&mut tx, snapshot.id, ids)
                .await
                .map_err(map_sqlx_error)?;
        }
        replace_post_series(&mut tx, snapshot.id, &previous.series, &snapshot.series).await?;
        sync_media_refs(
            &mut tx,
            MediaContentKind::Post,
            snapshot.id,
            &media_ids_for(&rendered.media_ids, snapshot.cover_media_id),
        )
        .await?;
        audit_content(&mut tx, actor_id, "post.update", "post", snapshot.id, serde_json::json!({"version": expected_version+1, "previous_status": previous.status.as_str(), "status": snapshot.status.as_str(), "published_at": snapshot.published_at})).await?;
        let record = self.record_in_transaction(&mut tx, snapshot.id).await?;
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(PostCommitOutcome::Saved(Box::new(record)))
    }
}

const POST_COLUMNS: &str = "id, author_id, category_id, title, slug, excerpt, content, cover_media_id, status, visibility, published_at, version, created_at, updated_at, deleted_at,
    COALESCE((SELECT jsonb_agg(jsonb_build_object('series_id', series_id, 'position', position) ORDER BY series_id) FROM post_series WHERE post_id=posts.id), '[]'::jsonb) AS series";

const POST_TAG_IDS: &str = "ARRAY(SELECT tag_id FROM post_tags WHERE post_id = posts.id \
    ORDER BY tag_id) AS tag_ids";

fn post_record_from_row(row: &sqlx::postgres::PgRow) -> Result<PostRecord, UseCaseError> {
    Ok(PostRecord {
        snapshot: post_from_row(row)?,
        tag_ids: row.try_get("tag_ids").map_err(map_row_error)?,
    })
}

fn post_from_row(row: &sqlx::postgres::PgRow) -> Result<PostSnapshot, UseCaseError> {
    let status: String = row.try_get("status").map_err(map_row_error)?;
    let visibility: String = row.try_get("visibility").map_err(map_row_error)?;
    Ok(PostSnapshot {
        id: row.try_get("id").map_err(map_row_error)?,
        author_id: row.try_get("author_id").map_err(map_row_error)?,
        category_id: row.try_get("category_id").map_err(map_row_error)?,
        series: row
            .try_get::<sqlx::types::Json<Vec<application::content::SeriesPlacement>>, _>("series")
            .map_err(map_row_error)?
            .0
            .into_iter()
            .map(Into::into)
            .collect(),
        title: row.try_get("title").map_err(map_row_error)?,
        slug: row.try_get("slug").map_err(map_row_error)?,
        excerpt: row.try_get("excerpt").map_err(map_row_error)?,
        content: row.try_get("content").map_err(map_row_error)?,
        cover_media_id: row.try_get("cover_media_id").map_err(map_row_error)?,
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
    async fn find_record_by_id(&self, id: Uuid) -> Result<Option<PostRecord>, UseCaseError> {
        let row = sqlx::query(&format!(
            "SELECT {POST_COLUMNS}, {POST_TAG_IDS} FROM posts WHERE id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        row.as_ref().map(post_record_from_row).transpose()
    }

    async fn insert_post(
        &self,
        post: &Post,
        tag_ids: &[Uuid],
        actor_id: Option<Uuid>,
    ) -> Result<PostRecord, UseCaseError> {
        self.insert_record(&post.snapshot(), tag_ids, actor_id)
            .await
    }

    async fn commit_post(
        &self,
        post: &Post,
        expected_version: i64,
        now: OffsetDateTime,
        tag_ids: Option<&[Uuid]>,
        actor_id: Option<Uuid>,
    ) -> Result<PostCommitOutcome, UseCaseError> {
        self.save_record(&post.snapshot(), expected_version, now, tag_ids, actor_id)
            .await
    }

    async fn commit_lifecycle(
        &self,
        post: &Post,
        expected_version: i64,
        now: OffsetDateTime,
        actor_id: Option<Uuid>,
    ) -> Result<PostCommitOutcome, UseCaseError> {
        let snapshot = post.snapshot();
        let expected_deleted = snapshot.deleted_at.is_none();
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        // 状态转换已由领域决定；存储只保护 CAS 与原回收站状态前提。
        // 不改系列归属/序号，因而不需要取得系列锁，也不覆盖当前标签。
        let updated: Option<(i64,)> = sqlx::query_as(
            "UPDATE posts SET status = $3, deleted_at = $4, updated_at = $5, \
             version = version + 1 WHERE id = $1 AND version = $2 \
             AND (deleted_at IS NOT NULL) = $6 RETURNING version",
        )
        .bind(snapshot.id)
        .bind(expected_version)
        .bind(snapshot.status.as_str())
        .bind(snapshot.deleted_at)
        .bind(now)
        .bind(expected_deleted)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        let outcome = if updated.is_some() {
            audit_content(
                &mut tx,
                actor_id,
                if expected_deleted {
                    "post.restore"
                } else {
                    "post.trash"
                },
                "post",
                snapshot.id,
                serde_json::json!({"version": expected_version+1}),
            )
            .await?;
            PostCommitOutcome::Saved(Box::new(
                self.record_in_transaction(&mut tx, snapshot.id).await?,
            ))
        } else {
            let current: Option<(bool,)> =
                sqlx::query_as("SELECT deleted_at IS NOT NULL FROM posts WHERE id = $1")
                    .bind(snapshot.id)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(map_sqlx_error)?;
            match current {
                Some((deleted,)) if deleted == expected_deleted => PostCommitOutcome::StaleConflict,
                _ => PostCommitOutcome::Gone,
            }
        };
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(outcome)
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
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
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

    async fn purge(
        &self,
        id: Uuid,
        expected_version: i64,
        actor_id: Option<Uuid>,
    ) -> Result<SaveOutcome, UseCaseError> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        lock_content_relations(&mut tx).await?;
        let row = sqlx::query(&format!(
            "SELECT {POST_COLUMNS} FROM posts WHERE id=$1 FOR UPDATE"
        ))
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        let Some(row) = row else {
            return Ok(SaveOutcome::Gone);
        };
        let previous = post_from_row(&row)?;
        if previous.deleted_at.is_none() {
            return Ok(SaveOutcome::Gone);
        }
        if previous.version != expected_version {
            return Ok(SaveOutcome::StaleConflict);
        }
        replace_post_series(&mut tx, id, &previous.series, &[]).await?;
        // 单条 DELETE 清理整棵评论树，避免自引用约束把父评论逐条删除卡住。
        sqlx::query("DELETE FROM comments WHERE post_id=$1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        clear_media_refs(&mut tx, MediaContentKind::Post, id).await?;
        sqlx::query("DELETE FROM posts WHERE id=$1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        audit_content(
            &mut tx,
            actor_id,
            "post.purge",
            "post",
            id,
            serde_json::json!({"version": expected_version}),
        )
        .await?;
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(SaveOutcome::Saved {
            new_version: expected_version + 1,
        })
    }
}

/// 短事务串行化内容关系修改（正文渲染在事务外），统一加入/重排/目录删除的锁序。
/// 自托管博客写入量小；读请求不受此锁影响。
pub(super) async fn lock_content_relations(
    tx: &mut sqlx::PgConnection,
) -> Result<(), UseCaseError> {
    sqlx::query("SELECT pg_advisory_xact_lock(1129270868, 1)")
        .execute(tx)
        .await
        .map_err(map_sqlx_error)?;
    Ok(())
}

pub(super) async fn audit_content(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    actor_id: Option<Uuid>,
    action: &str,
    target_type: &str,
    id: Uuid,
    metadata: serde_json::Value,
) -> Result<(), UseCaseError> {
    append_audit_log(
        tx,
        AuditEntry {
            actor_id,
            ip_address: None,
            action,
            target_type,
            target_id: &id.to_string(),
            metadata,
        },
    )
    .await
}

async fn render_content(
    renderer: &dyn ContentRenderer,
    source: &str,
) -> Result<application::ports::RenderedContent, UseCaseError> {
    domain::content::budget::validate_source(source)
        .map_err(|e| UseCaseError::Invalid(e.to_string()))?;
    let rendered = renderer.render_content(source).await?;
    application::rendering_budget::validate_html(&rendered.content_html)
        .map_err(|e| UseCaseError::Invalid(e.to_string()))?;
    Ok(rendered)
}

async fn replace_post_series(
    tx: &mut sqlx::PgConnection,
    post_id: Uuid,
    previous: &[domain::content::SeriesPlacement],
    next: &[domain::content::SeriesPlacement],
) -> Result<(), UseCaseError> {
    if previous == next {
        return Ok(());
    }
    let affected: Vec<Uuid> = previous
        .iter()
        .chain(next)
        .filter(|p| {
            previous.iter().find(|x| x.series_id == p.series_id)
                != next.iter().find(|x| x.series_id == p.series_id)
        })
        .map(|p| p.series_id)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    bump_series_versions(tx, &affected).await?;
    sqlx::query("DELETE FROM post_series WHERE post_id=$1")
        .bind(post_id)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
    let ids: Vec<Uuid> = next.iter().map(|p| p.series_id).collect();
    let positions: Vec<i32> = next.iter().map(|p| p.position).collect();
    sqlx::query("INSERT INTO post_series (post_id, series_id, position) SELECT $1, series_id, position FROM unnest($2::uuid[], $3::int[]) AS placement(series_id, position)")
        .bind(post_id).bind(&ids).bind(&positions).execute(tx).await.map_err(map_sqlx_error)?;
    Ok(())
}

/// 调用方持有内容关系事务锁；关联变化递增受影响系列的版本。
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
                   COALESCE(NULLIF(u.display_name, ''), u.username) AS author_display,
                   u.avatar_media_id AS author_avatar_media_id
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
                    author_avatar_media_id: row
                        .try_get("author_avatar_media_id")
                        .map_err(map_row_error)?,
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
            SELECT p.title, p.slug, p.excerpt, p.published_at, p.updated_at, p.content_html,
                   p.cover_media_id,
                   COALESCE(NULLIF(u.display_name, ''), u.username) AS author_display,
                   u.avatar_media_id AS author_avatar_media_id,
                   u.username AS author_username,
                   (
                       SELECT json_agg(json_build_object('slug', t.slug, 'name', t.name) ORDER BY t.slug)
                       FROM post_tags pt JOIN tags t ON t.id = pt.tag_id
                       WHERE pt.post_id = p.id
                   ) AS tags,
                   c.slug AS category_slug, c.name AS category_name,
                   (SELECT COALESCE(jsonb_agg(jsonb_build_object('slug', se.slug, 'name', se.name, 'position', ps.position) ORDER BY se.slug), '[]'::jsonb)
                    FROM post_series ps JOIN series se ON se.id=ps.series_id WHERE ps.post_id=p.id) AS series
            FROM posts p
            JOIN users u ON u.id = p.author_id
            LEFT JOIN categories c ON c.id = p.category_id
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
                author_avatar_media_id: row
                    .try_get("author_avatar_media_id")
                    .map_err(map_row_error)?,
                author_username: row.try_get("author_username").map_err(map_row_error)?,
                content_html: row.try_get("content_html").map_err(map_row_error)?,
                cover_media_id: row.try_get("cover_media_id").map_err(map_row_error)?,
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
                    .try_get::<sqlx::types::Json<Vec<application::ports::PublicSeriesRef>>, _>(
                        "series",
                    )
                    .map_err(map_row_error)?
                    .0,
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
    renderer: Arc<dyn ContentRenderer>,
}

impl PostgresPageRepository {
    pub fn new(pool: PgPool, renderer: Arc<dyn ContentRenderer>) -> Self {
        Self { pool, renderer }
    }

    async fn record_in_transaction(
        &self,
        tx: &mut sqlx::PgConnection,
        id: Uuid,
    ) -> Result<PageSnapshot, UseCaseError> {
        let row = sqlx::query(&format!("SELECT {PAGE_COLUMNS} FROM pages WHERE id=$1"))
            .bind(id)
            .fetch_one(tx)
            .await
            .map_err(map_sqlx_error)?;
        page_from_row(&row)
    }
}

const PAGE_COLUMNS: &str = "id, title, slug, content, status, visibility, published_at, version, \
     created_at, updated_at, deleted_at";

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
        deleted_at: row.try_get("deleted_at").map_err(map_row_error)?,
    })
}

#[async_trait]
impl PageRepository for PostgresPageRepository {
    async fn insert_page(
        &self,
        page: &Page,
        actor_id: Option<Uuid>,
    ) -> Result<PageSnapshot, UseCaseError> {
        let s = page.snapshot();
        let rendered = render_content(&*self.renderer, &s.content).await?;
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        sqlx::query("INSERT INTO pages (id,title,slug,content,status,visibility,published_at,version,created_at,updated_at,deleted_at,content_html,content_render_version) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)")
            .bind(s.id).bind(&s.title).bind(&s.slug).bind(&s.content).bind(s.status.as_str()).bind(s.visibility.as_str())
            .bind(s.published_at).bind(s.version).bind(s.created_at).bind(s.updated_at).bind(s.deleted_at).bind(&rendered.content_html).bind(CONTENT_RENDER_VERSION)
            .execute(&mut *tx).await.map_err(map_sqlx_error)?;
        sync_media_refs(&mut tx, MediaContentKind::Page, s.id, &rendered.media_ids).await?;
        audit_content(
            &mut tx,
            actor_id,
            "page.create",
            "page",
            s.id,
            serde_json::json!({"version": s.version}),
        )
        .await?;
        let record = self.record_in_transaction(&mut tx, s.id).await?;
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(record)
    }

    async fn commit_page(
        &self,
        page: &Page,
        expected_version: i64,
        now: OffsetDateTime,
        actor_id: Option<Uuid>,
    ) -> Result<PageCommitOutcome, UseCaseError> {
        let s = page.snapshot();
        let rendered = render_content(&*self.renderer, &s.content).await?;
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        let prior: Option<(i64, String)> = sqlx::query_as(
            "SELECT version, status FROM pages WHERE id=$1 AND deleted_at IS NULL FOR UPDATE",
        )
        .bind(s.id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        let Some((version, status)) = prior else {
            return Ok(PageCommitOutcome::Gone);
        };
        if version != expected_version {
            return Ok(PageCommitOutcome::StaleConflict);
        }
        sqlx::query("UPDATE pages SET title=$3,slug=$4,content=$5,status=$6,visibility=$7,published_at=$8,updated_at=$9,version=version+1,content_html=$10,content_render_version=$11 WHERE id=$1 AND version=$2 AND deleted_at IS NULL")
            .bind(s.id).bind(expected_version).bind(&s.title).bind(&s.slug).bind(&s.content).bind(s.status.as_str()).bind(s.visibility.as_str())
            .bind(s.published_at).bind(now).bind(&rendered.content_html).bind(CONTENT_RENDER_VERSION)
            .execute(&mut *tx).await.map_err(map_sqlx_error)?;
        sync_media_refs(&mut tx, MediaContentKind::Page, s.id, &rendered.media_ids).await?;
        audit_content(&mut tx, actor_id, "page.update", "page", s.id, serde_json::json!({"version": expected_version+1, "previous_status": status, "status": s.status.as_str(), "published_at": s.published_at})).await?;
        let record = self.record_in_transaction(&mut tx, s.id).await?;
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(PageCommitOutcome::Saved(record))
    }

    async fn commit_lifecycle(
        &self,
        page: &Page,
        expected_version: i64,
        now: OffsetDateTime,
        actor_id: Option<Uuid>,
    ) -> Result<PageCommitOutcome, UseCaseError> {
        let s = page.snapshot();
        let expected_deleted = s.deleted_at.is_none();
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        let updated=sqlx::query("UPDATE pages SET status=$3,deleted_at=$4,updated_at=$5,version=version+1 WHERE id=$1 AND version=$2 AND (deleted_at IS NOT NULL)=$6")
            .bind(s.id).bind(expected_version).bind(s.status.as_str()).bind(s.deleted_at).bind(now).bind(expected_deleted)
            .execute(&mut *tx).await.map_err(map_sqlx_error)?.rows_affected();
        if updated == 0 {
            let current: Option<(bool,)> =
                sqlx::query_as("SELECT deleted_at IS NOT NULL FROM pages WHERE id=$1")
                    .bind(s.id)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(map_sqlx_error)?;
            return Ok(match current {
                Some((deleted,)) if deleted == expected_deleted => PageCommitOutcome::StaleConflict,
                _ => PageCommitOutcome::Gone,
            });
        }
        audit_content(
            &mut tx,
            actor_id,
            if expected_deleted {
                "page.restore"
            } else {
                "page.trash"
            },
            "page",
            s.id,
            serde_json::json!({"version": expected_version+1}),
        )
        .await?;
        let record = self.record_in_transaction(&mut tx, s.id).await?;
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(PageCommitOutcome::Saved(record))
    }

    async fn find_by_id(&self, id: Uuid) -> Result<Option<PageSnapshot>, UseCaseError> {
        let row = sqlx::query(&format!("SELECT {PAGE_COLUMNS} FROM pages WHERE id=$1"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(map_sqlx_error)?;
        row.as_ref().map(page_from_row).transpose()
    }

    async fn list(&self) -> Result<Vec<PageSnapshot>, UseCaseError> {
        let rows=sqlx::query(&format!("SELECT {PAGE_COLUMNS} FROM pages WHERE deleted_at IS NULL ORDER BY updated_at DESC,id DESC")).fetch_all(&self.pool).await.map_err(map_sqlx_error)?;
        rows.iter().map(page_from_row).collect()
    }

    async fn list_trash(
        &self,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<PageSnapshot>, i64), UseCaseError> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        let (total,): (i64,) =
            sqlx::query_as("SELECT count(*) FROM pages WHERE deleted_at IS NOT NULL")
                .fetch_one(&mut *tx)
                .await
                .map_err(map_sqlx_error)?;
        let rows=sqlx::query(&format!("SELECT {PAGE_COLUMNS} FROM pages WHERE deleted_at IS NOT NULL ORDER BY deleted_at DESC,id DESC LIMIT $1 OFFSET $2")).bind(limit.clamp(1,100)).bind(offset.max(0)).fetch_all(&mut *tx).await.map_err(map_sqlx_error)?;
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok((
            rows.iter().map(page_from_row).collect::<Result<_, _>>()?,
            total,
        ))
    }

    async fn purge(
        &self,
        id: Uuid,
        expected_version: i64,
        actor_id: Option<Uuid>,
    ) -> Result<PageDeleteOutcome, UseCaseError> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        let prior: Option<(i64,)> = sqlx::query_as(
            "SELECT version FROM pages WHERE id=$1 AND deleted_at IS NOT NULL FOR UPDATE",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        let Some((version,)) = prior else {
            return Ok(PageDeleteOutcome::Gone);
        };
        if version != expected_version {
            return Ok(PageDeleteOutcome::StaleVersion);
        }
        clear_media_refs(&mut tx, MediaContentKind::Page, id).await?;
        sqlx::query("DELETE FROM pages WHERE id=$1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        audit_content(
            &mut tx,
            actor_id,
            "page.purge",
            "page",
            id,
            serde_json::json!({"version":expected_version}),
        )
        .await?;
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(PageDeleteOutcome::Deleted)
    }
}

/// 每批到期发布最多处理 limit 条/类型。多进程用 SKIP LOCKED 领取；取消、编辑、删除
/// 与此 UPDATE 争用同一行锁，只有仍满足预约条件的当前记录会发布。
pub async fn publish_due_content(
    pool: &PgPool,
    now: OffsetDateTime,
    limit: i64,
) -> Result<usize, UseCaseError> {
    let mut tx = pool.begin().await.map_err(map_sqlx_error)?;
    let mut count = 0;
    for (table, kind) in [("posts", "post"), ("pages", "page")] {
        let rows: Vec<(Uuid, i64)> = sqlx::query_as(&format!(
            "WITH due AS (
                SELECT id FROM {table}
                WHERE status = 'scheduled' AND deleted_at IS NULL AND published_at <= $1
                ORDER BY published_at, id LIMIT $2 FOR UPDATE SKIP LOCKED
             )
             UPDATE {table} p SET status = 'published', version = p.version + 1, updated_at = $1
             FROM due
             WHERE p.id = due.id AND p.status = 'scheduled'
               AND p.deleted_at IS NULL AND p.published_at <= $1
             RETURNING p.id, p.version"
        ))
        .bind(now)
        .bind(limit.clamp(1, 1000))
        .fetch_all(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        for (id, version) in rows {
            audit_content(
                &mut tx,
                None,
                &format!("{kind}.publish_due"),
                kind,
                id,
                serde_json::json!({"version":version}),
            )
            .await?;
            count += 1;
        }
    }
    tx.commit().await.map_err(map_sqlx_error)?;
    Ok(count)
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
            SELECT p.title, p.slug, p.published_at, p.updated_at, p.content_html
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
                content_html: row.try_get("content_html").map_err(map_row_error)?,
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
