use async_trait::async_trait;
use sqlx::{PgPool, Row};
use time::OffsetDateTime;
use uuid::Uuid;

use application::error::UseCaseError;
use application::ports::{
    MediaContentKind, MediaDeleteOutcome, MediaRepository, MediaUsageRow, MediaWithUsage,
};
use domain::media::{MediaSnapshot, MediaStatus};

use super::sql::{map_row_error, map_sqlx_error};

/// 某份内容真正引用的媒体集合：**正文渲染出的图片 ∪ 封面**。
///
/// 封面是独立字段而不是正文的一部分，因此需要在已提取的正文图片集合之外加入封面；
/// 但两者必须走同一张引用表——公开来源判定与删除保护都以 `content_media_refs`
/// 为唯一判据，封面如果只存在列里，就会出现「正文引用受保护、封面引用可被删除」
/// 的第二套规则。
///
/// 集合去重并按 id 排序：`sync_media_refs` 的 `FOR SHARE` 校验按 id 序加锁，
/// 排序是锁序协议的一部分（防死锁），同时让重复引用（正文与封面同一张图）只计一次。
pub(crate) fn media_ids_for(body_media_ids: &[Uuid], cover_media_id: Option<Uuid>) -> Vec<Uuid> {
    let mut ids = body_media_ids.to_vec();
    if let Some(cover) = cover_media_id
        && !ids.contains(&cover)
    {
        ids.push(cover);
        ids.sort_unstable();
    }
    ids
}

/// 用内容推导出的引用集合，在同一事务内整体替换某个内容的媒体引用。
///
/// `media_ids` 由调用方在**开启事务之前**用 [`media_ids_for`]（正文渲染结果 ∪ 封面）
/// 算好：正文图片已在受控渲染任务中提取，此处只合并封面。
/// 集合仍然只由即将写入的内容推导，因此不存在「内容与引用关系漂移」的写入路径。
///
/// 并发协议（与 `MediaRepository::begin_delete` 配对）：
/// 先按 id 序对涉及媒体行取 `FOR SHARE` 并确认全部为 `ready`，再改引用行。
/// 删除流程对同一媒体行取 `FOR UPDATE`，因此两者不会交错成
/// 「引用已写入、文件已进入回收」的破图结果。
pub(crate) async fn sync_media_refs(
    tx: &mut sqlx::PgConnection,
    kind: MediaContentKind,
    content_id: Uuid,
    ids: &[Uuid],
) -> Result<(), UseCaseError> {
    if !ids.is_empty() {
        let ready: Vec<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM media_assets \
             WHERE id = ANY($1::uuid[]) AND status = 'ready' ORDER BY id FOR SHARE",
        )
        .bind(ids)
        .fetch_all(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        let ready: Vec<Uuid> = ready.into_iter().map(|(id,)| id).collect();
        if ready != ids {
            let missing: Vec<Uuid> = ids
                .iter()
                .filter(|id| !ready.contains(id))
                .copied()
                .collect();
            return Err(UseCaseError::Invalid(format!(
                "内容引用了不存在或已不可用的图片：{missing:?}"
            )));
        }
    }
    sqlx::query("DELETE FROM content_media_refs WHERE content_type = $1 AND content_id = $2")
        .bind(kind.as_str())
        .bind(content_id)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
    if ids.is_empty() {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO content_media_refs (media_id, content_type, content_id) \
         SELECT mid, $2, $3 FROM unnest($1::uuid[]) AS mid",
    )
    .bind(ids)
    .bind(kind.as_str())
    .bind(content_id)
    .execute(&mut *tx)
    .await
    .map_err(map_sqlx_error)?;
    Ok(())
}

/// 内容**物理删除**时清理其媒体引用。
///
/// `content_media_refs.content_id` 是刻意的多态引用（Post/Page 共用一张表），
/// 没有外键兜底，因此删除内容的同一事务必须显式清理。
pub(super) async fn clear_media_refs(
    tx: &mut sqlx::PgConnection,
    kind: MediaContentKind,
    content_id: Uuid,
) -> Result<(), UseCaseError> {
    sqlx::query("DELETE FROM content_media_refs WHERE content_type = $1 AND content_id = $2")
        .bind(kind.as_str())
        .bind(content_id)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 媒体仓储
// ---------------------------------------------------------------------------

/// 单条引用是否构成「公开来源」的 SQL 谓词；`refs` 是 `content_media_refs` 的别名。
///
/// 与公开文章/页面的可见性谓词必须逐字一致（docs/content-lifecycle.md §1）：
/// 撤回、改为 private 或移入回收站后，这里立刻不再成立，匿名读取随之停止。
const REF_IS_PUBLIC: &str = "((refs.content_type = 'post' AND EXISTS ( \
        SELECT 1 FROM posts mp WHERE mp.id = refs.content_id \
          AND mp.status = 'published' AND mp.visibility = 'public' AND mp.deleted_at IS NULL)) \
    OR (refs.content_type = 'page' AND EXISTS ( \
        SELECT 1 FROM pages gp WHERE gp.id = refs.content_id \
          AND gp.status = 'published' AND gp.visibility = 'public')) \
    OR (refs.content_type = 'series' AND EXISTS ( \
        SELECT 1 FROM series sp WHERE sp.id = refs.content_id)) \
    OR (refs.content_type = 'user' AND EXISTS ( \
        SELECT 1 FROM users au WHERE au.id = refs.content_id AND au.deleted_at IS NULL)) \
    OR (refs.content_type = 'site'))";

const MEDIA_COLUMNS: &str = "m.id, m.owner_id, m.storage_key, m.original_name, m.mime, m.byte_size, \
     m.width, m.height, m.checksum_sha256, m.status, m.version, m.created_at, m.updated_at";

/// `RETURNING` 不能带表别名，列清单单独维护（与 `MEDIA_COLUMNS` 同序同集合）。
const MEDIA_RETURNING_COLUMNS: &str = "id, owner_id, storage_key, original_name, mime, byte_size, width, height, checksum_sha256, \
     status, version, created_at, updated_at";

/// 媒体库行：元数据 + 上传者展示名 + 两个引用计数。
const MEDIA_VIEW_COLUMNS: &str = "m.id, m.owner_id, m.storage_key, m.original_name, m.mime, \
     m.byte_size, m.width, m.height, m.checksum_sha256, m.status, m.version, m.created_at, \
     m.updated_at, \
     COALESCE(NULLIF(u.display_name, ''), u.username) AS owner_display, \
     (SELECT count(*) FROM content_media_refs refs WHERE refs.media_id = m.id) AS ref_count, \
     (SELECT count(*) FROM content_media_refs refs WHERE refs.media_id = m.id \
        AND REPLACE_PUBLIC) AS public_ref_count";

pub struct PostgresMediaRepository {
    pool: PgPool,
}

impl PostgresMediaRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn media_view_columns() -> String {
    MEDIA_VIEW_COLUMNS.replace("REPLACE_PUBLIC", REF_IS_PUBLIC)
}

fn media_from_row(row: &sqlx::postgres::PgRow) -> Result<MediaSnapshot, UseCaseError> {
    let status: String = row.try_get("status").map_err(map_row_error)?;
    Ok(MediaSnapshot {
        id: row.try_get("id").map_err(map_row_error)?,
        owner_id: row.try_get("owner_id").map_err(map_row_error)?,
        storage_key: row.try_get("storage_key").map_err(map_row_error)?,
        original_name: row.try_get("original_name").map_err(map_row_error)?,
        mime: row.try_get("mime").map_err(map_row_error)?,
        byte_size: row.try_get("byte_size").map_err(map_row_error)?,
        width: row.try_get("width").map_err(map_row_error)?,
        height: row.try_get("height").map_err(map_row_error)?,
        checksum_sha256: row.try_get("checksum_sha256").map_err(map_row_error)?,
        status: MediaStatus::parse(&status)
            .ok_or_else(|| UseCaseError::Repository(format!("未知媒体状态 {status}")))?,
        version: row.try_get("version").map_err(map_row_error)?,
        created_at: row.try_get("created_at").map_err(map_row_error)?,
        updated_at: row.try_get("updated_at").map_err(map_row_error)?,
    })
}

fn media_view_from_row(row: &sqlx::postgres::PgRow) -> Result<MediaWithUsage, UseCaseError> {
    Ok(MediaWithUsage {
        snapshot: media_from_row(row)?,
        owner_display: row.try_get("owner_display").map_err(map_row_error)?,
        reference_count: row.try_get("ref_count").map_err(map_row_error)?,
        public_reference_count: row.try_get("public_ref_count").map_err(map_row_error)?,
    })
}

#[async_trait]
impl MediaRepository for PostgresMediaRepository {
    async fn insert_staged(&self, snapshot: &MediaSnapshot) -> Result<(), UseCaseError> {
        sqlx::query(
            r#"
            INSERT INTO media_assets (
                id, owner_id, storage_key, original_name, mime, byte_size, width, height,
                checksum_sha256, status, version, created_at, updated_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
            "#,
        )
        .bind(snapshot.id)
        .bind(snapshot.owner_id)
        .bind(&snapshot.storage_key)
        .bind(&snapshot.original_name)
        .bind(&snapshot.mime)
        .bind(snapshot.byte_size)
        .bind(snapshot.width)
        .bind(snapshot.height)
        .bind(&snapshot.checksum_sha256)
        .bind(snapshot.status.as_str())
        .bind(snapshot.version)
        .bind(snapshot.created_at)
        .bind(snapshot.updated_at)
        .execute(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(())
    }

    async fn mark_ready(&self, id: Uuid, now: OffsetDateTime) -> Result<bool, UseCaseError> {
        // 只允许 staged → ready：并发回收已把行推进到 deleted 时不得复活。
        let updated = sqlx::query(
            "UPDATE media_assets SET status = 'ready', version = version + 1, updated_at = $2 \
             WHERE id = $1 AND status = 'staged' RETURNING id",
        )
        .bind(id)
        .bind(now)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(updated.is_some())
    }

    async fn find_by_id(&self, id: Uuid) -> Result<Option<MediaSnapshot>, UseCaseError> {
        let row = sqlx::query(&format!(
            "SELECT {MEDIA_COLUMNS} FROM media_assets m WHERE m.id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        row.as_ref().map(media_from_row).transpose()
    }

    async fn find_view(&self, id: Uuid) -> Result<Option<MediaWithUsage>, UseCaseError> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        let row = sqlx::query(&format!(
            "SELECT {} FROM media_assets m JOIN users u ON u.id = m.owner_id \
             WHERE m.id = $1 AND m.status = 'ready'",
            media_view_columns()
        ))
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        tx.commit().await.map_err(map_sqlx_error)?;
        row.as_ref().map(media_view_from_row).transpose()
    }

    async fn list(
        &self,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<MediaWithUsage>, i64), UseCaseError> {
        // 计数与当页在同一事务：分页响应里的 total 与条目不会来自两个时刻。
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        let (total,): (i64,) =
            sqlx::query_as("SELECT count(*) FROM media_assets WHERE status = 'ready'")
                .fetch_one(&mut *tx)
                .await
                .map_err(map_sqlx_error)?;
        let rows = sqlx::query(&format!(
            "SELECT {} FROM media_assets m JOIN users u ON u.id = m.owner_id \
             WHERE m.status = 'ready' ORDER BY m.created_at DESC, m.id DESC LIMIT $1 OFFSET $2",
            media_view_columns()
        ))
        .bind(limit)
        .bind(offset)
        .fetch_all(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        tx.commit().await.map_err(map_sqlx_error)?;
        let items = rows
            .iter()
            .map(media_view_from_row)
            .collect::<Result<Vec<_>, _>>()?;
        Ok((items, total))
    }

    async fn usage_of(&self, id: Uuid) -> Result<Vec<MediaUsageRow>, UseCaseError> {
        let rows = sqlx::query(&format!(
            "SELECT refs.content_type, refs.content_id, mp.author_id, \
                COALESCE(mp.slug, gp.slug, sp.slug, au.username, '') AS slug, \
                COALESCE(mp.title, gp.title, sp.name, NULLIF(au.display_name, ''), au.username, \
                    CASE WHEN refs.content_type = 'site' THEN '站点设置' END, '') AS title, \
                COALESCE(mp.status, gp.status, \
                    CASE WHEN sp.id IS NOT NULL THEN 'published' END, \
                    CASE WHEN au.id IS NOT NULL OR refs.content_type = 'site' \
                        THEN 'active' END, '') AS content_status, \
                COALESCE(mp.visibility, gp.visibility, \
                    CASE WHEN sp.id IS NOT NULL THEN 'public' END, \
                    CASE WHEN au.id IS NOT NULL OR refs.content_type = 'site' \
                        THEN 'public' END, '') AS content_visibility, \
                COALESCE(mp.deleted_at IS NOT NULL, au.deleted_at IS NOT NULL, false) \
                    AS content_deleted, \
                ({REF_IS_PUBLIC}) AS is_public \
             FROM content_media_refs refs \
             LEFT JOIN posts mp ON refs.content_type = 'post' AND mp.id = refs.content_id \
             LEFT JOIN pages gp ON refs.content_type = 'page' AND gp.id = refs.content_id \
             LEFT JOIN series sp ON refs.content_type = 'series' AND sp.id = refs.content_id \
             LEFT JOIN users au ON refs.content_type = 'user' AND au.id = refs.content_id \
             WHERE refs.media_id = $1 \
             ORDER BY refs.content_type, slug"
        ))
        .bind(id)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        rows.iter()
            .map(|row| {
                let kind: String = row.try_get("content_type").map_err(map_row_error)?;
                Ok(MediaUsageRow {
                    kind: MediaContentKind::parse(&kind).ok_or_else(|| {
                        UseCaseError::Repository(format!("未知引用来源类型 {kind}"))
                    })?,
                    content_id: row.try_get("content_id").map_err(map_row_error)?,
                    // Page 无作者：LEFT JOIN 未命中时自然为 NULL。
                    author_id: row.try_get("author_id").map_err(map_row_error)?,
                    slug: row.try_get("slug").map_err(map_row_error)?,
                    title: row.try_get("title").map_err(map_row_error)?,
                    status: row.try_get("content_status").map_err(map_row_error)?,
                    visibility: row.try_get("content_visibility").map_err(map_row_error)?,
                    deleted: row.try_get("content_deleted").map_err(map_row_error)?,
                    public: row.try_get("is_public").map_err(map_row_error)?,
                })
            })
            .collect()
    }

    async fn has_public_reference(&self, id: Uuid) -> Result<bool, UseCaseError> {
        let (exists,): (bool,) = sqlx::query_as(&format!(
            "SELECT EXISTS (SELECT 1 FROM content_media_refs refs \
             WHERE refs.media_id = $1 AND {REF_IS_PUBLIC})"
        ))
        .bind(id)
        .fetch_one(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(exists)
    }

    async fn begin_delete(
        &self,
        id: Uuid,
        expected_version: i64,
        now: OffsetDateTime,
    ) -> Result<MediaDeleteOutcome, UseCaseError> {
        // 行锁内完成「读状态 → 校验引用 → 迁移状态」：锁与内容保存的
        // `FOR SHARE` 配对，因此不会出现引用写入与回收并发交错的中间态。
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        let current: Option<(String, i64)> =
            sqlx::query_as("SELECT status, version FROM media_assets WHERE id = $1 FOR UPDATE")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(map_sqlx_error)?;
        let Some((status, version)) = current else {
            tx.commit().await.map_err(map_sqlx_error)?;
            return Ok(MediaDeleteOutcome::Gone);
        };
        match status.as_str() {
            // 已删除或从未就绪：对调用方等同于不存在。
            "deleted" | "staged" => {
                tx.commit().await.map_err(map_sqlx_error)?;
                return Ok(MediaDeleteOutcome::Gone);
            }
            // 上次回收中途失败：重放直接进入删除流程，不再要求版本匹配。
            "pending_deletion" => {
                tx.commit().await.map_err(map_sqlx_error)?;
                return Ok(MediaDeleteOutcome::Marked);
            }
            "ready" => {}
            other => {
                return Err(UseCaseError::Repository(format!("未知媒体状态 {other}")));
            }
        }
        if version != expected_version {
            tx.commit().await.map_err(map_sqlx_error)?;
            return Ok(MediaDeleteOutcome::StaleVersion);
        }
        let (count,): (i64,) =
            sqlx::query_as("SELECT count(*) FROM content_media_refs WHERE media_id = $1")
                .bind(id)
                .fetch_one(&mut *tx)
                .await
                .map_err(map_sqlx_error)?;
        if count > 0 {
            tx.commit().await.map_err(map_sqlx_error)?;
            return Ok(MediaDeleteOutcome::Referenced { count });
        }
        sqlx::query(
            "UPDATE media_assets SET status = 'pending_deletion', version = version + 1, \
             updated_at = $2 WHERE id = $1",
        )
        .bind(id)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(MediaDeleteOutcome::Marked)
    }

    async fn confirm_deleted(&self, id: Uuid, now: OffsetDateTime) -> Result<bool, UseCaseError> {
        let updated = sqlx::query(
            "UPDATE media_assets SET status = 'deleted', version = version + 1, updated_at = $2 \
             WHERE id = $1 AND status = 'pending_deletion' RETURNING id",
        )
        .bind(id)
        .bind(now)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(updated.is_some())
    }

    async fn claim_abandoned_staged(
        &self,
        created_before: OffsetDateTime,
        now: OffsetDateTime,
        limit: i64,
    ) -> Result<Vec<MediaSnapshot>, UseCaseError> {
        // 单语句「认领」：先按宽限期与上限选出候选，再在**外层**重新校验 status。
        //
        // 外层 `status = 'staged'` 是并发正确性的关键：两个回收进程同时执行时，
        // 后到者在行锁上等待，锁释放后重新求值会看到 `pending_deletion` 而跳过，
        // 不会重复认领（也不会重复计入报告）。
        //
        // 与 `mark_ready` 的互斥同理：两者都是 `AND status = 'staged'` 的条件更新，
        // 只有一个能命中——因此不会出现「回收删掉刚就绪资产的文件」。
        let rows = sqlx::query(&format!(
            "UPDATE media_assets SET status = 'pending_deletion', version = version + 1, \
                 updated_at = $2 \
             WHERE status = 'staged' AND id IN ( \
                 SELECT id FROM media_assets \
                 WHERE status = 'staged' AND created_at < $1 \
                 ORDER BY created_at, id LIMIT $3 \
             ) \
             RETURNING {MEDIA_RETURNING_COLUMNS}"
        ))
        .bind(created_before)
        .bind(now)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        rows.iter().map(media_from_row).collect()
    }

    async fn list_pending_deletion(&self, limit: i64) -> Result<Vec<MediaSnapshot>, UseCaseError> {
        let rows = sqlx::query(&format!(
            "SELECT {MEDIA_COLUMNS} FROM media_assets m WHERE m.status = 'pending_deletion' \
             ORDER BY m.created_at, m.id LIMIT $1"
        ))
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        rows.iter().map(media_from_row).collect()
    }
}

/// 附着授权读取面：归属与公开性一条 SQL 得出，公开谓词与匿名读取同一份
/// （`REF_IS_PUBLIC`）。只看 `ready` 资产——其余状态本就不可被引用。
#[async_trait]
impl application::ports::MediaRefGuard for PostgresMediaRepository {
    async fn attachable_status(
        &self,
        id: Uuid,
    ) -> Result<Option<application::ports::MediaAttachStatus>, UseCaseError> {
        let row: Option<(Uuid, bool)> = sqlx::query_as(&format!(
            "SELECT m.owner_id, EXISTS (SELECT 1 FROM content_media_refs refs \
             WHERE refs.media_id = m.id AND {REF_IS_PUBLIC}) \
             FROM media_assets m WHERE m.id = $1 AND m.status = 'ready'"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(row.map(
            |(owner_id, publicly_referenced)| application::ports::MediaAttachStatus {
                owner_id,
                publicly_referenced,
            },
        ))
    }
}
