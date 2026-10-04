use super::*;
use application::revisions::{REVISION_LIMIT, RevisionContent, RevisionSummary};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum WriteMode {
    Edit,
    Publication,
    Lifecycle,
}

pub(super) async fn editing_copy(
    tx: &mut sqlx::PgConnection,
    kind: &str,
    id: Uuid,
) -> Result<Option<(RevisionContent, OffsetDateTime)>, UseCaseError> {
    let row: Option<(sqlx::types::Json<RevisionContent>, OffsetDateTime)> =
        sqlx::query_as(&format!("SELECT r.data,r.created_at FROM {kind}s p JOIN content_revisions r ON r.id=p.draft_revision_id AND r.{kind}_id=p.id WHERE p.id=$1"))
            .bind(id).fetch_optional(tx).await.map_err(map_sqlx_error)?;
    Ok(row.map(|(data, time)| (data.0, time)))
}

pub(super) async fn list(
    pool: &PgPool,
    kind: &str,
    id: Uuid,
) -> Result<Vec<RevisionSummary>, UseCaseError> {
    let rows = sqlx::query(&format!("SELECT id,version,data->>'title' AS title,actor_id,created_at FROM content_revisions WHERE {kind}_id=$1 ORDER BY version DESC LIMIT $2"))
        .bind(id).bind(REVISION_LIMIT).fetch_all(pool).await.map_err(map_sqlx_error)?;
    rows.iter()
        .map(|r| {
            Ok(RevisionSummary {
                id: r.try_get("id").map_err(map_row_error)?,
                version: r.try_get("version").map_err(map_row_error)?,
                title: r.try_get("title").map_err(map_row_error)?,
                actor_id: r.try_get("actor_id").map_err(map_row_error)?,
                created_at: application::public_site::api_datetime(
                    r.try_get("created_at").map_err(map_row_error)?,
                ),
            })
        })
        .collect()
}
pub(super) async fn find(
    pool: &PgPool,
    kind: &str,
    id: Uuid,
    revision: Uuid,
) -> Result<Option<RevisionContent>, UseCaseError> {
    let data: Option<sqlx::types::Json<RevisionContent>> = sqlx::query_scalar(&format!(
        "SELECT data FROM content_revisions WHERE {kind}_id=$1 AND id=$2"
    ))
    .bind(id)
    .bind(revision)
    .fetch_optional(pool)
    .await
    .map_err(map_sqlx_error)?;
    Ok(data.map(|data| data.0))
}

pub(super) async fn prior_media(
    tx: &mut sqlx::PgConnection,
    kind: &str,
    id: Uuid,
) -> Result<Vec<Uuid>, UseCaseError> {
    // Only the current editor/live source grants permission to retain a deleted image.
    sqlx::query_scalar(&format!("SELECT media_id FROM media_refs WHERE source_type=$1 AND source_id=$2 UNION SELECT unnest(r.media_ids) FROM {kind}s p JOIN content_revisions r ON r.id=p.draft_revision_id WHERE p.id=$2"))
        .bind(kind).bind(id).fetch_all(tx).await.map_err(map_sqlx_error)
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn save(
    tx: &mut sqlx::PgConnection,
    kind: &str,
    id: Uuid,
    version: i64,
    content: &RevisionContent,
    media: &[Uuid],
    retained_media: &[Uuid],
    actor: application::audit::AuditContext,
    now: OffsetDateTime,
) -> Result<Uuid, UseCaseError> {
    let new_id = Uuid::now_v7();
    let revision: Option<Uuid> = sqlx::query_scalar(&format!("INSERT INTO content_revisions(id,{kind}_id,version,data,media_ids,actor_id,created_at) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT DO NOTHING RETURNING id"))
        .bind(new_id).bind(id).bind(version).bind(sqlx::types::Json(content)).bind(media).bind(actor.actor_id).bind(now)
        .fetch_optional(&mut *tx).await.map_err(map_sqlx_error)?;
    if let Some(revision) = revision {
        super::super::media::sync_rebuilt_media_refs(
            tx,
            MediaContentKind::Revision,
            revision,
            media,
            retained_media,
        )
        .await?;
        Ok(revision)
    } else {
        sqlx::query_scalar(&format!(
            "SELECT id FROM content_revisions WHERE {kind}_id=$1 AND version=$2"
        ))
        .bind(id)
        .bind(version)
        .fetch_one(tx)
        .await
        .map_err(map_sqlx_error)
    }
}
pub(super) async fn prune(
    tx: &mut sqlx::PgConnection,
    kind: &str,
    id: Uuid,
) -> Result<(), UseCaseError> {
    // The newest revision is always retained, including the active editing copy.
    sqlx::query(&format!("DELETE FROM content_revisions WHERE id IN (SELECT id FROM content_revisions WHERE {kind}_id=$1 ORDER BY (id=(SELECT draft_revision_id FROM {kind}s WHERE id=$1)) DESC NULLS LAST,version DESC OFFSET $2)"))
        .bind(id).bind(REVISION_LIMIT).execute(tx).await.map_err(map_sqlx_error)?;
    Ok(())
}
