use std::sync::Arc;

use async_trait::async_trait;
use sqlx::{PgPool, Row};
use time::OffsetDateTime;
use uuid::Uuid;

use application::error::UseCaseError;
use application::ports::{
    ContentRenderer, MediaContentKind, PageCommitOutcome, PageDeleteOutcome, PageRepository,
    PostCommitOutcome, PostRecord, PostRepository, PublicCategoryRef, PublicPageDetail,
    PublicPostDetail, PublicPostSummary, PublicSeriesRef, PublicUrlEntry, PublishedPageQuery,
    PublishedPostQuery, SaveOutcome,
};
use domain::content::page::{Page, PageSnapshot, PageStatus};
use domain::content::post::{Post, PostSnapshot, PostStatus, Visibility};

use super::media::{clear_media_refs, media_ids_for, sync_media_refs};
use super::sql::{PAGE_PUBLIC_PREDICATE, POST_PUBLIC_PREDICATE, map_row_error, map_sqlx_error};

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
                domain::content::budget::validate_html(&rendered.content_html)
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
                    rebuilt += 1;
                }
                tx.commit().await.map_err(map_sqlx_error)?;
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
    ) -> Result<PostRecord, UseCaseError> {
        // 引用集合在事务外推导：正文图片与封面求并集（提取要完整渲染 + 清洗正文，
        // 不应占用事务）。
        domain::content::budget::validate_source(&snapshot.content)
            .map_err(|e| UseCaseError::Invalid(e.to_string()))?;
        let rendered = self.renderer.render_content(&snapshot.content).await?;
        domain::content::budget::validate_html(&rendered.content_html)
            .map_err(|e| UseCaseError::Invalid(e.to_string()))?;
        let media_ids = media_ids_for(&rendered.media_ids, snapshot.cover_media_id);
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
                id, author_id, category_id, series_id, title, slug, excerpt, content, cover_media_id,
                series_order, status, visibility, published_at, version, created_at, updated_at,
                content_html, content_render_version
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9,
                $10, $11, $12, $13, $14, $15, $16, $17, $18
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
        .bind(snapshot.cover_media_id)
        .bind(snapshot.series_order)
        .bind(snapshot.status.as_str())
        .bind(snapshot.visibility.as_str())
        .bind(snapshot.published_at)
        .bind(snapshot.version)
        .bind(snapshot.created_at)
        .bind(snapshot.updated_at)
        .bind(&rendered.content_html)
        .bind(CONTENT_RENDER_VERSION)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        if !tag_ids.is_empty() {
            insert_post_tags(&mut tx, snapshot.id, tag_ids)
                .await
                .map_err(map_sqlx_error)?;
        }
        // 正文引用与正文同一事务：引用校验失败则整体回滚，不留下半套写入。
        sync_media_refs(&mut tx, MediaContentKind::Post, snapshot.id, &media_ids).await?;
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
    ) -> Result<PostCommitOutcome, UseCaseError> {
        // 引用集合在事务外推导：正文图片与封面求并集（渲染 + 清洗是纯 CPU 工作，
        // 不应占用事务）。封面变化同样反映到引用行，因此替换封面会释放旧图。
        domain::content::budget::validate_source(&snapshot.content)
            .map_err(|e| UseCaseError::Invalid(e.to_string()))?;
        let rendered = self.renderer.render_content(&snapshot.content).await?;
        domain::content::budget::validate_html(&rendered.content_html)
            .map_err(|e| UseCaseError::Invalid(e.to_string()))?;
        let media_ids = media_ids_for(&rendered.media_ids, snapshot.cover_media_id);
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
            return Ok(PostCommitOutcome::Gone);
        };
        if !alive {
            tx.commit().await.map_err(map_sqlx_error)?;
            return Ok(PostCommitOutcome::Gone);
        }
        if current_version != expected_version {
            tx.commit().await.map_err(map_sqlx_error)?;
            return Ok(PostCommitOutcome::StaleConflict);
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
                title = $3, slug = $4, excerpt = $5, content = $6, cover_media_id = $7,
                series_id = $8, series_order = $9, status = $10, visibility = $11,
                published_at = $12, updated_at = $13, category_id = $14,
                version = version + 1, content_html = $15, content_render_version = $16
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
        .bind(snapshot.cover_media_id)
        .bind(snapshot.series_id)
        .bind(snapshot.series_order)
        .bind(snapshot.status.as_str())
        .bind(snapshot.visibility.as_str())
        .bind(snapshot.published_at)
        .bind(now)
        .bind(snapshot.category_id)
        .bind(&rendered.content_html)
        .bind(CONTENT_RENDER_VERSION)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;

        let Some(_row) = updated else {
            // 版本在步骤 1 之后被并发改写：不再有写入，放弃事务。
            tx.rollback().await.map_err(map_sqlx_error)?;
            return Ok(PostCommitOutcome::StaleConflict);
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
        // 引用集合始终由本次写入的正文推导（发布/撤回时正文未变，重写同集合是幂等的）。
        sync_media_refs(&mut tx, MediaContentKind::Post, snapshot.id, &media_ids).await?;
        let record = self.record_in_transaction(&mut tx, snapshot.id).await?;
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(PostCommitOutcome::Saved(Box::new(record)))
    }
}

const POST_COLUMNS: &str = "id, author_id, category_id, series_id, title, slug, excerpt, content, \
     cover_media_id, series_order, status, visibility, published_at, version, created_at, updated_at, deleted_at";

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
        series_id: row.try_get("series_id").map_err(map_row_error)?,
        title: row.try_get("title").map_err(map_row_error)?,
        slug: row.try_get("slug").map_err(map_row_error)?,
        excerpt: row.try_get("excerpt").map_err(map_row_error)?,
        content: row.try_get("content").map_err(map_row_error)?,
        cover_media_id: row.try_get("cover_media_id").map_err(map_row_error)?,
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

    async fn insert_post(&self, post: &Post, tag_ids: &[Uuid]) -> Result<PostRecord, UseCaseError> {
        self.insert_record(&post.snapshot(), tag_ids).await
    }

    async fn commit_post(
        &self,
        post: &Post,
        expected_version: i64,
        now: OffsetDateTime,
        tag_ids: Option<&[Uuid]>,
    ) -> Result<PostCommitOutcome, UseCaseError> {
        self.save_record(&post.snapshot(), expected_version, now, tag_ids)
            .await
    }

    async fn commit_lifecycle(
        &self,
        post: &Post,
        expected_version: i64,
        now: OffsetDateTime,
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
            // 内容物理删除：其媒体引用必须在同一事务清理
            // （content_id 是多态引用，没有外键级联兜底）。
            clear_media_refs(&mut tx, MediaContentKind::Post, id).await?;
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
    renderer: Arc<dyn ContentRenderer>,
}

impl PostgresPageRepository {
    pub fn new(pool: PgPool, renderer: Arc<dyn ContentRenderer>) -> Self {
        Self { pool, renderer }
    }

    async fn insert_record(&self, snapshot: &PageSnapshot) -> Result<(), UseCaseError> {
        // 引用集合在事务外推导：提取要完整渲染 + 清洗正文，不应占用事务。
        // Page 没有封面列，因此引用集合只由正文推导。
        domain::content::budget::validate_source(&snapshot.content)
            .map_err(|e| UseCaseError::Invalid(e.to_string()))?;
        let rendered = self.renderer.render_content(&snapshot.content).await?;
        domain::content::budget::validate_html(&rendered.content_html)
            .map_err(|e| UseCaseError::Invalid(e.to_string()))?;
        let media_ids = media_ids_for(&rendered.media_ids, None);
        // 正文与正文引用同一事务：引用校验失败则整页不落库。
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        sqlx::query(
            r#"
            INSERT INTO pages (
                id, title, slug, content, status, visibility, published_at, version,
                created_at, updated_at, content_html, content_render_version
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
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
        .bind(&rendered.content_html)
        .bind(CONTENT_RENDER_VERSION)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        sync_media_refs(&mut tx, MediaContentKind::Page, snapshot.id, &media_ids).await?;
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(())
    }

    async fn save_record(
        &self,
        snapshot: &PageSnapshot,
        expected_version: i64,
        now: OffsetDateTime,
    ) -> Result<SaveOutcome, UseCaseError> {
        // 引用集合在事务外推导（渲染 + 清洗是纯 CPU 工作，不应占用事务）。
        // Page 没有封面列，因此引用集合只由正文推导。
        domain::content::budget::validate_source(&snapshot.content)
            .map_err(|e| UseCaseError::Invalid(e.to_string()))?;
        let rendered = self.renderer.render_content(&snapshot.content).await?;
        domain::content::budget::validate_html(&rendered.content_html)
            .map_err(|e| UseCaseError::Invalid(e.to_string()))?;
        let media_ids = media_ids_for(&rendered.media_ids, None);
        // 条件更新与引用替换同一事务：观察者不会看到新正文配旧引用。
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        let updated = sqlx::query(
            r#"
            UPDATE pages SET
                title = $3, slug = $4, content = $5, status = $6, visibility = $7,
                published_at = $8, updated_at = $9, version = version + 1,
                content_html = $10, content_render_version = $11
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
        .bind(&rendered.content_html)
        .bind(CONTENT_RENDER_VERSION)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;

        if let Some(row) = updated {
            sync_media_refs(&mut tx, MediaContentKind::Page, snapshot.id, &media_ids).await?;
            let new_version = row.try_get::<i64, _>(0).map_err(map_row_error)?;
            tx.commit().await.map_err(map_sqlx_error)?;
            return Ok(SaveOutcome::Saved { new_version });
        }
        // 页面是物理删除：命中不到就是在或不在，不再区分软删除。
        let alive = sqlx::query("SELECT 1 AS alive FROM pages WHERE id = $1")
            .bind(snapshot.id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        tx.commit().await.map_err(map_sqlx_error)?;
        match alive {
            Some(_) => Ok(SaveOutcome::StaleConflict),
            None => Ok(SaveOutcome::Gone),
        }
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
    async fn insert_page(&self, page: &Page) -> Result<PageSnapshot, UseCaseError> {
        let snapshot = page.snapshot();
        self.insert_record(&snapshot).await?;
        Ok(snapshot)
    }

    async fn commit_page(
        &self,
        page: &Page,
        expected_version: i64,
        now: OffsetDateTime,
    ) -> Result<PageCommitOutcome, UseCaseError> {
        let mut snapshot = page.snapshot();
        Ok(
            match self.save_record(&snapshot, expected_version, now).await? {
                SaveOutcome::Saved { new_version } => {
                    snapshot.version = new_version;
                    snapshot.updated_at = now;
                    PageCommitOutcome::Saved(snapshot)
                }
                SaveOutcome::StaleConflict => PageCommitOutcome::StaleConflict,
                SaveOutcome::Gone => PageCommitOutcome::Gone,
            },
        )
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

    async fn delete(
        &self,
        id: Uuid,
        expected_version: i64,
    ) -> Result<PageDeleteOutcome, UseCaseError> {
        // Page 无回收站：删除即物理删除，其媒体引用必须在同一事务清理。
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        let deleted: Option<(Uuid,)> =
            sqlx::query_as("DELETE FROM pages WHERE id = $1 AND version = $2 RETURNING id")
                .bind(id)
                .bind(expected_version)
                .fetch_optional(&mut *tx)
                .await
                .map_err(map_sqlx_error)?;
        if deleted.is_some() {
            clear_media_refs(&mut tx, MediaContentKind::Page, id).await?;
            tx.commit().await.map_err(map_sqlx_error)?;
            return Ok(PageDeleteOutcome::Deleted);
        }
        let alive: Option<(i64,)> = sqlx::query_as("SELECT version FROM pages WHERE id = $1")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(if alive.is_some() {
            PageDeleteOutcome::StaleVersion
        } else {
            PageDeleteOutcome::Gone
        })
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
