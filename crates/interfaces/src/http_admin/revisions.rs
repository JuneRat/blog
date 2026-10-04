//! History shares the owning content's permissions and version preconditions.
use super::AdminAuth;
use crate::{
    http_auth::AdminState,
    http_contract::SeriesPlacement,
    http_support::{RequestId, admin_error},
};
use axum::{
    Json,
    extract::{Path, State},
    response::{IntoResponse, Response},
};
use uuid::Uuid;

#[derive(serde::Serialize, ts_rs::TS)]
pub struct ContentRevisionSummary {
    pub id: Uuid,
    #[ts(type = "number")]
    pub version: i64,
    pub title: String,
    pub created_at: String,
    pub actor_id: Option<Uuid>,
}
impl From<application::revisions::RevisionSummary> for ContentRevisionSummary {
    fn from(r: application::revisions::RevisionSummary) -> Self {
        Self {
            id: r.id,
            version: r.version,
            title: r.title,
            created_at: r.created_at,
            actor_id: r.actor_id,
        }
    }
}
#[derive(serde::Serialize, ts_rs::TS)]
pub struct ContentRevisionDetail {
    pub slug: String,
    pub title: String,
    pub content: String,
    pub visibility: String,
    pub excerpt: Option<String>,
    pub tag_ids: Vec<Uuid>,
    pub category_id: Option<Uuid>,
    pub series: Vec<SeriesPlacement>,
    pub cover_media_id: Option<Uuid>,
}
impl From<application::revisions::RevisionContent> for ContentRevisionDetail {
    fn from(r: application::revisions::RevisionContent) -> Self {
        Self {
            slug: r.slug,
            title: r.title,
            content: r.content,
            visibility: r.visibility,
            excerpt: r.excerpt,
            tag_ids: r.tag_ids,
            category_id: r.category_id,
            series: r.series.into_iter().map(Into::into).collect(),
            cover_media_id: r.cover_media_id,
        }
    }
}
#[derive(serde::Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct RestoreRevisionInput {
    #[ts(type = "number")]
    pub expected_version: i64,
}

pub(super) async fn posts_list(
    auth: AdminAuth,
    request: RequestId,
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
) -> Response {
    match state.posts.revisions(&auth.actor, id).await {
        Ok(rows) => Json(
            rows.into_iter()
                .map(ContentRevisionSummary::from)
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(error) => admin_error(error, &request),
    }
}
pub(super) async fn posts_detail(
    auth: AdminAuth,
    request: RequestId,
    State(state): State<AdminState>,
    Path((id, revision)): Path<(Uuid, Uuid)>,
) -> Response {
    match state.posts.revision(&auth.actor, id, revision).await {
        Ok(row) => Json(ContentRevisionDetail::from(row)).into_response(),
        Err(error) => admin_error(error, &request),
    }
}
pub(super) async fn posts_restore(
    auth: AdminAuth,
    request: RequestId,
    State(state): State<AdminState>,
    Path((id, revision)): Path<(Uuid, Uuid)>,
    Json(body): Json<RestoreRevisionInput>,
) -> Response {
    match state
        .posts
        .restore_revision(&auth.actor, id, revision, body.expected_version)
        .await
    {
        Ok(row) => Json(super::posts::PostDetailJson::from(row)).into_response(),
        Err(error) => admin_error(error, &request),
    }
}

pub(super) async fn pages_list(
    auth: AdminAuth,
    request: RequestId,
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
) -> Response {
    match state.pages.revisions(&auth.actor, id).await {
        Ok(rows) => Json(
            rows.into_iter()
                .map(ContentRevisionSummary::from)
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(error) => admin_error(error, &request),
    }
}
pub(super) async fn pages_detail(
    auth: AdminAuth,
    request: RequestId,
    State(state): State<AdminState>,
    Path((id, revision)): Path<(Uuid, Uuid)>,
) -> Response {
    match state.pages.revision(&auth.actor, id, revision).await {
        Ok(row) => Json(ContentRevisionDetail::from(row)).into_response(),
        Err(error) => admin_error(error, &request),
    }
}
pub(super) async fn pages_restore(
    auth: AdminAuth,
    request: RequestId,
    State(state): State<AdminState>,
    Path((id, revision)): Path<(Uuid, Uuid)>,
    Json(body): Json<RestoreRevisionInput>,
) -> Response {
    match state
        .pages
        .restore_revision(&auth.actor, id, revision, body.expected_version)
        .await
    {
        Ok(row) => Json(super::pages::PageDetailJson::from(row)).into_response(),
        Err(error) => admin_error(error, &request),
    }
}
pub(super) fn export_contract(out: &mut Vec<String>) {
    crate::http_contract::declare::<ContentRevisionSummary>(out);
    crate::http_contract::declare::<ContentRevisionDetail>(out);
    crate::http_contract::declare::<RestoreRevisionInput>(out);
}
