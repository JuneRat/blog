use super::*;
use application::batch::{BatchItemResult, BatchItems, BatchResult, PostBatchAction};
use application::identity::Actor;
use std::collections::{BTreeMap, BTreeSet};

impl PostgresPostRepository {
    pub(super) async fn batch_posts(
        &self,
        actor: &Actor,
        items: &BatchItems,
        action: PostBatchAction,
        now: OffsetDateTime,
    ) -> Result<BatchResult, UseCaseError> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        lock_content_relations(&mut tx).await?;
        if let PostBatchAction::ChangeCategory(Some(id)) = action {
            let category: Option<Uuid> =
                sqlx::query_scalar("SELECT id FROM categories WHERE id=$1 FOR KEY SHARE")
                    .bind(id)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(map_sqlx_error)?;
            if category.is_none() {
                return Err(UseCaseError::NotFound(format!("分类 {id}")));
            }
        }
        let ids = items.sorted_ids();
        let rows = sqlx::query(&format!(
            "SELECT {POST_COLUMNS} FROM posts WHERE id=ANY($1::uuid[]) ORDER BY id FOR UPDATE"
        ))
        .bind(&ids)
        .fetch_all(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        let mut snapshots: BTreeMap<_, _> = rows
            .iter()
            .map(post_from_row)
            .map(|row| row.map(|s| (s.id, s)))
            .collect::<Result<_, _>>()?;
        let mut plans = Vec::with_capacity(items.items().len());
        for item in items.items() {
            let previous = snapshots
                .remove(&item.id)
                .ok_or_else(|| UseCaseError::NotFound(format!("文章 {}", item.id)))?;
            action.authorize(actor, previous.author_id)?;
            if previous.version != item.expected_version {
                return Err(UseCaseError::VersionConflict);
            }
            let mut post = Post::reconstitute(previous.clone())
                .map_err(|e| UseCaseError::DataCorrupt(e.to_string()))?;
            let changed = action.apply(&mut post, now)?;
            plans.push((previous, post.snapshot(), changed));
        }

        if matches!(
            action,
            PostBatchAction::ChangeCategory(_)
                | PostBatchAction::ChangeStatus(
                    application::batch::BatchPostStatus::Published
                        | application::batch::BatchPostStatus::Scheduled(_)
                )
        ) {
            let pending: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM posts WHERE id=ANY($1::uuid[]) AND draft_revision_id IS NOT NULL)")
                .bind(&ids).fetch_one(&mut *tx).await.map_err(map_sqlx_error)?;
            if pending {
                return Err(UseCaseError::Invalid(
                    "包含待发布修改，请先在对应编辑页发布或处理草稿后再批量操作".into(),
                ));
            }
        }

        let purge = matches!(action, PostBatchAction::Purge);
        if purge {
            // One membership change per series per batch; the content lock keeps reorder/save ordered.
            let series: Vec<_> = plans
                .iter()
                .flat_map(|(previous, _, _)| previous.series.iter().map(|s| s.series_id))
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            bump_series_versions(&mut tx, &series).await?;
            // Delete complete trees in one statement, retaining the existing immutable-reply protocol.
            sqlx::query("DELETE FROM comments WHERE post_id=ANY($1::uuid[])")
                .bind(&ids)
                .execute(&mut *tx)
                .await
                .map_err(map_sqlx_error)?;
            for id in &ids {
                clear_media_refs(&mut tx, MediaContentKind::Post, *id).await?;
            }
            let deleted = sqlx::query("DELETE FROM posts WHERE id=ANY($1::uuid[])")
                .bind(&ids)
                .execute(&mut *tx)
                .await
                .map_err(map_sqlx_error)?;
            if deleted.rows_affected() != ids.len() as u64 {
                return Err(UseCaseError::VersionConflict);
            }
        }

        let mut result = BatchResult {
            items: Vec::with_capacity(plans.len()),
            affected: 0,
        };
        let mut changes = Vec::new();
        for (previous, next, changed) in plans {
            let version = if purge {
                None
            } else {
                Some(
                    previous
                        .version
                        .checked_add(i64::from(changed))
                        .ok_or_else(|| UseCaseError::Invalid("版本已达到上限".into()))?,
                )
            };
            if changed && !purge {
                // Record metadata history in the same transaction as batch changes.
                let original = self.record_in_transaction(&mut tx, previous.id).await?;
                let media: Vec<Uuid> = sqlx::query_scalar(
                    "SELECT media_id FROM media_refs WHERE source_type='post' AND source_id=$1",
                )
                .bind(previous.id)
                .fetch_all(&mut *tx)
                .await
                .map_err(map_sqlx_error)?;
                revisions::save(
                    &mut tx,
                    "post",
                    previous.id,
                    previous.version,
                    &RevisionContent::post(&original),
                    &media,
                    &media,
                    actor.audit_context(),
                    previous.updated_at,
                )
                .await?;
                let edited = PostRecord {
                    snapshot: next.clone(),
                    tag_ids: original.tag_ids,
                };
                revisions::save(
                    &mut tx,
                    "post",
                    previous.id,
                    version.expect("non-purge version"),
                    &RevisionContent::post(&edited),
                    &media,
                    &media,
                    actor.audit_context(),
                    now,
                )
                .await?;
                // Source, HTML, tags, series and media references are unchanged by these actions.
                let saved = sqlx::query("UPDATE posts SET status=$3,published_at=$4,deleted_at=$5,category_id=$6,updated_at=$7,version=version+1 WHERE id=$1 AND version=$2")
                    .bind(previous.id).bind(previous.version).bind(next.status.as_str()).bind(next.published_at)
                    .bind(next.deleted_at).bind(next.category_id).bind(now).execute(&mut *tx).await.map_err(map_sqlx_error)?;
                if saved.rows_affected() != 1 {
                    return Err(UseCaseError::VersionConflict);
                }
                revisions::prune(&mut tx, "post", previous.id).await?;
            }
            if changed {
                result.affected += 1;
                changes.push(serde_json::json!({"id":previous.id,"from_version":previous.version,"version":version,"from_status":previous.status.as_str(),"status":next.status.as_str(),"category_id":next.category_id,"published_at":next.published_at.map(application::public_site::api_datetime)}));
            }
            result.items.push(BatchItemResult {
                id: previous.id,
                version,
                changed,
            });
        }
        if result.affected > 0 {
            audit_content(&mut tx, actor.audit_context(), "post.batch", "post_batch", Uuid::now_v7(), serde_json::json!({"action":action.name(),"affected":result.affected,"items":changes})).await?;
        }
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(result)
    }
}
