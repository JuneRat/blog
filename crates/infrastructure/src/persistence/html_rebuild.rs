//! HTML 派生物的有界查询与逐记录事务；批次调度由应用层负责。
use std::sync::Arc;

use application::{
    UseCaseError,
    html_rebuild::{HtmlKind, HtmlRebuildStore, RebuildBatch, RebuildBatchError, RebuildCounts},
    ports::{CommentRenderer, ContentRenderer, MediaContentKind, RenderedContent},
};
use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use super::{
    content::CONTENT_RENDER_VERSION,
    media::{media_ids_for, sync_media_refs},
    sql::map_sqlx_error,
};
use crate::COMMENT_RENDER_VERSION;

pub struct PostgresHtmlRebuildStore {
    pool: PgPool,
    content_renderer: Arc<dyn ContentRenderer>,
    comment_renderer: Arc<dyn CommentRenderer>,
}

impl PostgresHtmlRebuildStore {
    pub fn new(
        database: crate::Database,
        content_renderer: Arc<dyn ContentRenderer>,
        comment_renderer: Arc<dyn CommentRenderer>,
    ) -> Self {
        let pool = database.pool;
        Self {
            pool,
            content_renderer,
            comment_renderer,
        }
    }
}

#[async_trait]
impl HtmlRebuildStore for PostgresHtmlRebuildStore {
    async fn pending(&self) -> Result<RebuildCounts, UseCaseError> {
        // 同一条语句保证三类数量来自同一读取快照，不加载正文。
        let (posts, pages, comments): (i64, i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM posts WHERE content_render_version <> $1),
                    (SELECT count(*) FROM pages WHERE content_render_version <> $1),
                    (SELECT count(*) FROM comments WHERE content_render_version <> $2)",
        )
        .bind(CONTENT_RENDER_VERSION)
        .bind(COMMENT_RENDER_VERSION)
        .fetch_one(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(RebuildCounts {
            posts: posts as u64,
            pages: pages as u64,
            comments: comments as u64,
        })
    }

    async fn rebuild_batch(
        &self,
        kind: HtmlKind,
        after: Option<Uuid>,
        limit: i64,
    ) -> Result<RebuildBatch, RebuildBatchError> {
        if !(1..=1000).contains(&limit) {
            return Err(RebuildBatchError {
                progress: RebuildBatch::default(),
                id: None,
                source: UseCaseError::Invalid("重建批量大小须为 1–1,000".into()),
            });
        }
        let (table, cover, render_version, media_kind) = match kind {
            HtmlKind::Post => (
                "posts",
                "cover_media_id",
                CONTENT_RENDER_VERSION,
                Some(MediaContentKind::Post),
            ),
            HtmlKind::Page => (
                "pages",
                "NULL::uuid",
                CONTENT_RENDER_VERSION,
                Some(MediaContentKind::Page),
            ),
            HtmlKind::Comment => ("comments", "NULL::uuid", COMMENT_RENDER_VERSION, None),
        };
        // 批次只保存 ID；源文逐条读取，内存不会随 batch_size × 正文大小增长。
        let ids: Vec<Uuid> = sqlx::query_scalar(&format!(
            "SELECT id FROM {table}
             WHERE content_render_version <> $1 AND ($2::uuid IS NULL OR id > $2)
             ORDER BY id LIMIT $3"
        ))
        .bind(render_version)
        .bind(after)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| RebuildBatchError {
            progress: RebuildBatch::default(),
            id: None,
            source: map_sqlx_error(error),
        })?;
        let mut progress = RebuildBatch::default();
        for id in ids {
            let attempt: Result<bool, UseCaseError> = async {
                let row: Option<(String, i64, Option<Uuid>)> = sqlx::query_as(&format!(
                    "SELECT content, version, {cover} FROM {table}
                     WHERE id=$1 AND content_render_version<>$2"
                ))
                .bind(id)
                .bind(render_version)
                .fetch_optional(&self.pool)
                .await
                .map_err(map_sqlx_error)?;
                let Some((source, version, cover_media_id)) = row else {
                    // 已删除或由其他保存/重建更新；仍计入本轮已检查并推进游标。
                    return Ok(false);
                };
                let rendered = if kind == HtmlKind::Comment {
                    RenderedContent {
                        content_html: self.comment_renderer.render_comment(&source).await?,
                        media_ids: vec![],
                    }
                } else {
                    self.content_renderer.render_content(&source).await?
                };
                application::rendering_budget::validate_html(&rendered.content_html)
                    .map_err(|error| UseCaseError::Invalid(error.to_string()))?;
                let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
                let changed = sqlx::query(&format!(
                    "UPDATE {table} SET content_html=$2, content_render_version=$3
                     WHERE id=$1 AND content=$4 AND version=$5 AND content_render_version<>$3"
                ))
                .bind(id)
                .bind(&rendered.content_html)
                .bind(render_version)
                .bind(&source)
                .bind(version)
                .execute(&mut *tx)
                .await
                .map_err(map_sqlx_error)?
                .rows_affected();
                if changed == 1 {
                    if let Some(media_kind) = media_kind {
                        sync_media_refs(
                            &mut tx,
                            media_kind,
                            id,
                            &media_ids_for(&rendered.media_ids, cover_media_id),
                        )
                        .await?;
                    }
                    crate::audit::record_change(
                        &mut tx,
                        application::audit::AuditContext::system(),
                        &format!("{kind}.html.rebuild"),
                        &kind.to_string(),
                        &id.to_string(),
                        serde_json::json!({
                            "version": version,
                            "render_version": render_version,
                        }),
                    )
                    .await?;
                }
                tx.commit().await.map_err(map_sqlx_error)?;
                Ok(changed == 1)
            }
            .await;
            match attempt {
                Ok(true) => progress.rebuilt += 1,
                Ok(false) => progress.skipped += 1,
                Err(source) => {
                    return Err(RebuildBatchError {
                        progress,
                        id: Some(id),
                        source,
                    });
                }
            }
            progress.cursor = Some(id);
        }
        Ok(progress)
    }
}
