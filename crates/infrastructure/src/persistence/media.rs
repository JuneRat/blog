use async_trait::async_trait;
use sqlx::{PgPool, Row};
use time::OffsetDateTime;
use uuid::Uuid;

use super::sql::{map_row_error, map_sqlx_error};
use crate::audit::{AuditEntry, append_audit_log};
use application::error::UseCaseError;
use application::ports::{
    MediaChangeOutcome, MediaContentKind, MediaRepository, MediaUsageRow, MediaUsageSource,
    MediaWithUsage,
};
use domain::content::{Visibility, page::PageStatus, post::PostStatus};
use domain::identity::UserStatus;
use domain::media::{Media, MediaSnapshot};

/// 同一来源的正文和封面取并集，固定锁序。
pub(crate) fn media_ids_for(body_media_ids: &[Uuid], cover_media_id: Option<Uuid>) -> Vec<Uuid> {
    let mut ids = body_media_ids.to_vec();
    ids.extend(cover_media_id);
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// 调用者持有来源实体的写锁；共享媒体行锁与软删除和独立物理清理互斥。
/// 已软删除媒体可以保留同一来源的历史引用，不能新增到其他来源。
pub(crate) async fn sync_media_refs(
    tx: &mut sqlx::PgConnection,
    kind: MediaContentKind,
    content_id: Uuid,
    ids: &[Uuid],
) -> Result<(), UseCaseError> {
    sync_media_refs_with_history(tx, kind, content_id, ids, &[]).await
}

/// Rebuilding may repair a missing bookkeeping row for an image already present
/// in this source's persisted HTML/cover. That historical relationship survives
/// soft deletion; it does not authorize attaching a new image or another source.
pub(super) async fn sync_rebuilt_media_refs(
    tx: &mut sqlx::PgConnection,
    kind: MediaContentKind,
    content_id: Uuid,
    ids: &[Uuid],
    persisted_ids: &[Uuid],
) -> Result<(), UseCaseError> {
    sync_media_refs_with_history(tx, kind, content_id, ids, persisted_ids).await
}

async fn sync_media_refs_with_history(
    tx: &mut sqlx::PgConnection,
    kind: MediaContentKind,
    content_id: Uuid,
    ids: &[Uuid],
    persisted_ids: &[Uuid],
) -> Result<(), UseCaseError> {
    let ids = media_ids_for(ids, None);
    let media: Vec<(Uuid, Option<OffsetDateTime>)> = sqlx::query_as(
        "SELECT id, deleted_at FROM media WHERE id=ANY($1::uuid[]) ORDER BY id FOR SHARE",
    )
    .bind(&ids)
    .fetch_all(&mut *tx)
    .await
    .map_err(map_sqlx_error)?;
    let previous: Vec<Uuid> =
        sqlx::query_scalar("SELECT media_id FROM media_refs WHERE source_type=$1 AND source_id=$2")
            .bind(kind.as_str())
            .bind(content_id)
            .fetch_all(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
    if media.len() != ids.len()
        || media.iter().any(|(id, deleted)| {
            deleted.is_some() && !previous.contains(id) && !persisted_ids.contains(id)
        })
    {
        return Err(UseCaseError::Invalid(
            "引用了不存在或已移入回收站的图片".into(),
        ));
    }
    sqlx::query("DELETE FROM media_refs WHERE source_type=$1 AND source_id=$2 AND NOT (media_id=ANY($3::uuid[]))")
        .bind(kind.as_str()).bind(content_id).bind(&ids).execute(&mut *tx).await.map_err(map_sqlx_error)?;
    sqlx::query("INSERT INTO media_refs (media_id, source_type, source_id) SELECT mid,$2,$3 FROM unnest($1::uuid[]) AS mid ON CONFLICT DO NOTHING")
        .bind(&ids).bind(kind.as_str()).bind(content_id).execute(&mut *tx).await.map_err(map_sqlx_error)?;
    Ok(())
}

/// 多态来源无外键：物理删除来源时须在同一事务清理引用，软删除时不调用。
pub(super) async fn clear_media_refs(
    tx: &mut sqlx::PgConnection,
    kind: MediaContentKind,
    content_id: Uuid,
) -> Result<(), UseCaseError> {
    sqlx::query("DELETE FROM media_refs WHERE source_type=$1 AND source_id=$2")
        .bind(kind.as_str())
        .bind(content_id)
        .execute(tx)
        .await
        .map_err(map_sqlx_error)?;
    Ok(())
}

// 来源可见性只保护后台使用位置的标题/地址；不参与媒体文件读取。
const SOURCE_IS_PUBLIC: &str = "((refs.source_type='post' AND mp.status='published' AND mp.visibility='public' AND mp.deleted_at IS NULL AND mp.published_at<=now()) \
    OR (refs.source_type='page' AND gp.status='published' AND gp.visibility='public' AND gp.deleted_at IS NULL AND gp.published_at<=now()) \
    OR (refs.source_type='series' AND sp.id IS NOT NULL) OR refs.source_type='site')";
const MEDIA_COLUMNS: &str = "m.id, m.uploaded_by, m.path, m.filename, m.mime_type, m.size, m.width, m.height, m.checksum_sha256, m.version, m.created_at, m.updated_at, m.deleted_at";

fn view_columns() -> String {
    format!(
        "{MEDIA_COLUMNS}, COALESCE(NULLIF(u.display_name,''),u.username,'未知上传者') AS owner_display, \
        (SELECT count(*) FROM media_refs WHERE media_id=m.id) AS ref_count"
    )
}

pub struct PostgresMediaRepository {
    pool: PgPool,
}
impl PostgresMediaRepository {
    pub fn new(database: crate::Database) -> Self {
        let pool = database.pool;
        Self { pool }
    }
}

fn media_from_row(row: &sqlx::postgres::PgRow) -> Result<MediaSnapshot, UseCaseError> {
    let snapshot = MediaSnapshot {
        id: row.try_get("id").map_err(map_row_error)?,
        owner_id: row.try_get("uploaded_by").map_err(map_row_error)?,
        storage_key: row.try_get("path").map_err(map_row_error)?,
        original_name: row.try_get("filename").map_err(map_row_error)?,
        mime: row.try_get("mime_type").map_err(map_row_error)?,
        byte_size: row.try_get("size").map_err(map_row_error)?,
        width: row.try_get("width").map_err(map_row_error)?,
        height: row.try_get("height").map_err(map_row_error)?,
        checksum_sha256: row.try_get("checksum_sha256").map_err(map_row_error)?,
        version: row.try_get("version").map_err(map_row_error)?,
        created_at: row.try_get("created_at").map_err(map_row_error)?,
        updated_at: row.try_get("updated_at").map_err(map_row_error)?,
        deleted_at: row.try_get("deleted_at").map_err(map_row_error)?,
    };
    Media::reconstitute(snapshot)
        .map(|media| media.snapshot())
        .map_err(|e| UseCaseError::DataCorrupt(e.to_string()))
}
fn media_view_from_row(row: &sqlx::postgres::PgRow) -> Result<MediaWithUsage, UseCaseError> {
    Ok(MediaWithUsage {
        snapshot: media_from_row(row)?,
        owner_display: row.try_get("owner_display").map_err(map_row_error)?,
        reference_count: row.try_get("ref_count").map_err(map_row_error)?,
    })
}

#[async_trait]
impl MediaRepository for PostgresMediaRepository {
    async fn insert(
        &self,
        aggregate: &Media,
        actor_id: application::audit::AuditContext,
    ) -> Result<(), UseCaseError> {
        let s = aggregate.snapshot();
        if s.deleted_at.is_some() {
            return Err(UseCaseError::Invalid("不能登记已删除媒体".into()));
        }
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        sqlx::query("INSERT INTO media (id,uploaded_by,path,filename,mime_type,size,width,height,checksum_sha256,version,created_at,updated_at) \
            VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)")
            .bind(s.id).bind(s.owner_id).bind(&s.storage_key).bind(&s.original_name).bind(&s.mime).bind(s.byte_size)
            .bind(s.width).bind(s.height).bind(&s.checksum_sha256).bind(s.version).bind(s.created_at).bind(s.updated_at)
            .execute(&mut *tx).await.map_err(map_sqlx_error)?;
        append_audit_log(&mut tx, AuditEntry { actor_id: actor_id.actor_id, ip_address: actor_id.ip_address, action: "media.upload", target_type: "media", target_id: &s.id.to_string(),
            metadata: serde_json::json!({"version": s.version, "size": s.byte_size, "mime_type": s.mime}) }).await?;
        tx.commit().await.map_err(map_sqlx_error)
    }

    async fn find_by_id(&self, id: Uuid) -> Result<Option<MediaSnapshot>, UseCaseError> {
        let row = sqlx::query(&format!(
            "SELECT {MEDIA_COLUMNS} FROM media m WHERE m.id=$1"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        row.as_ref().map(media_from_row).transpose()
    }
    async fn find_view(&self, id: Uuid) -> Result<Option<MediaWithUsage>, UseCaseError> {
        let row = sqlx::query(&format!(
            "SELECT {} FROM media m LEFT JOIN users u ON u.id=m.uploaded_by WHERE m.id=$1",
            view_columns()
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        row.as_ref().map(media_view_from_row).transpose()
    }
    async fn list(
        &self,
        limit: i64,
        offset: i64,
        trash: bool,
        q: Option<&str>,
    ) -> Result<(Vec<MediaWithUsage>, i64), UseCaseError> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        // Literal predicates allow partial indexes in generic prepared plans too.
        let predicate = if trash {
            "m.deleted_at IS NOT NULL"
        } else {
            "m.deleted_at IS NULL"
        };
        let predicate = format!(
            "{predicate} AND ($1::text IS NULL OR strpos(lower(m.filename), lower($1)) > 0)"
        );
        let total: i64 =
            sqlx::query_scalar(&format!("SELECT count(*) FROM media m WHERE {predicate}"))
                .bind(q)
                .fetch_one(&mut *tx)
                .await
                .map_err(map_sqlx_error)?;
        let rows = sqlx::query(&format!(
            "SELECT {} FROM media m LEFT JOIN users u ON u.id=m.uploaded_by \
            WHERE {predicate} ORDER BY m.created_at DESC,m.id DESC LIMIT $2 OFFSET $3",
            view_columns()
        ))
        .bind(q)
        .bind(limit)
        .bind(offset)
        .fetch_all(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok((
            rows.iter()
                .map(media_view_from_row)
                .collect::<Result<_, _>>()?,
            total,
        ))
    }
    async fn usage_of(&self, id: Uuid) -> Result<Vec<MediaUsageRow>, UseCaseError> {
        let rows = sqlx::query(&format!("SELECT refs.source_type, refs.source_id, mp.author_id, \
            COALESCE(mp.slug,gp.slug,sp.slug,au.username,'') AS slug, \
            COALESCE(mp.title,gp.title,sp.name,NULLIF(au.display_name,''),au.username,CASE WHEN refs.source_type='site' THEN '站点设置' END,'') AS title, \
            COALESCE(mp.status,gp.status,au.status) AS content_status, \
            COALESCE(mp.visibility,gp.visibility,'public') AS content_visibility, \
            CASE refs.source_type WHEN 'post' THEN mp.deleted_at IS NOT NULL WHEN 'page' THEN gp.deleted_at IS NOT NULL WHEN 'user' THEN au.deleted_at IS NOT NULL ELSE false END AS content_deleted, \
            COALESCE({SOURCE_IS_PUBLIC},false) AS is_public \
            FROM media_refs refs \
            LEFT JOIN posts mp ON refs.source_type='post' AND mp.id=refs.source_id \
            LEFT JOIN pages gp ON refs.source_type='page' AND gp.id=refs.source_id \
            LEFT JOIN series sp ON refs.source_type='series' AND sp.id=refs.source_id \
            LEFT JOIN users au ON refs.source_type='user' AND au.id=refs.source_id \
            WHERE refs.media_id=$1 ORDER BY refs.source_type,slug,refs.source_id"))
            .bind(id).fetch_all(&self.pool).await.map_err(map_sqlx_error)?;
        rows.iter()
            .map(|row| {
                let kind: &str = row.try_get("source_type").map_err(map_row_error)?;
                let source =
                    usage_source(kind, row.try_get("content_status").map_err(map_row_error)?)?;
                Ok(MediaUsageRow {
                    source,
                    content_id: row.try_get("source_id").map_err(map_row_error)?,
                    author_id: row.try_get("author_id").map_err(map_row_error)?,
                    slug: row.try_get("slug").map_err(map_row_error)?,
                    title: row.try_get("title").map_err(map_row_error)?,
                    visibility: Visibility::parse(
                        row.try_get("content_visibility").map_err(map_row_error)?,
                    )
                    .ok_or_else(|| UseCaseError::Repository("无效引用可见性".into()))?,
                    deleted: row.try_get("content_deleted").map_err(map_row_error)?,
                    public: row.try_get("is_public").map_err(map_row_error)?,
                })
            })
            .collect()
    }
    async fn set_deleted(
        &self,
        id: Uuid,
        expected_version: i64,
        deleted: bool,
        now: OffsetDateTime,
        actor_id: application::audit::AuditContext,
    ) -> Result<MediaChangeOutcome, UseCaseError> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        let row = sqlx::query(&format!(
            "SELECT {MEDIA_COLUMNS} FROM media m WHERE id=$1 FOR UPDATE"
        ))
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        let Some(row) = row else {
            return Ok(MediaChangeOutcome::Gone);
        };
        let mut media = Media::reconstitute(media_from_row(&row)?)
            .map_err(|e| UseCaseError::DataCorrupt(e.to_string()))?;
        if media.version() != expected_version {
            return Ok(MediaChangeOutcome::StaleVersion);
        }
        if !media.set_deleted(deleted, now) {
            return Ok(MediaChangeOutcome::Unchanged);
        }
        let snapshot = media.snapshot();
        sqlx::query("UPDATE media SET deleted_at=$2,version=$3,updated_at=$4 WHERE id=$1")
            .bind(id)
            .bind(snapshot.deleted_at)
            .bind(snapshot.version)
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        append_audit_log(
            &mut tx,
            AuditEntry {
                actor_id: actor_id.actor_id,
                ip_address: actor_id.ip_address,
                action: if deleted {
                    "media.trash"
                } else {
                    "media.restore"
                },
                target_type: "media",
                target_id: &id.to_string(),
                metadata: serde_json::json!({"version": snapshot.version}),
            },
        )
        .await?;
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(MediaChangeOutcome::Updated)
    }
}

#[async_trait]
impl application::ports::MediaRefGuard for PostgresMediaRepository {
    async fn is_attachable(&self, id: Uuid) -> Result<bool, UseCaseError> {
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM media WHERE id=$1 AND deleted_at IS NULL)")
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .map_err(map_sqlx_error)
    }
}

fn usage_source(kind: &str, status: Option<&str>) -> Result<MediaUsageSource, UseCaseError> {
    let source = match MediaContentKind::parse(kind) {
        Some(MediaContentKind::Post) => status
            .and_then(PostStatus::parse)
            .map(MediaUsageSource::Post),
        Some(MediaContentKind::Page) => status
            .and_then(PageStatus::parse)
            .map(MediaUsageSource::Page),
        Some(MediaContentKind::User) => status
            .and_then(UserStatus::parse)
            .map(MediaUsageSource::User),
        Some(MediaContentKind::Series) => Some(MediaUsageSource::Series),
        Some(MediaContentKind::Site) => Some(MediaUsageSource::Site),
        None => None,
    };
    source.ok_or_else(|| UseCaseError::Repository("无效引用来源或状态".into()))
}
