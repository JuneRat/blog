//! Authenticated Markdown preview. Uses the same CSRF and no-store contract as
//! other admin POST requests, without persisting the submitted draft.
use std::sync::Arc;

use application::content_preview::ContentPreview;
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, FromRef, State},
    middleware,
    response::{IntoResponse, Response},
    routing::post,
};
use serde::{Deserialize, Serialize};

use crate::{
    http_admin::{ADMIN_BODY_LIMIT, AdminAuth},
    http_auth::AdminState,
    http_support::{RequestId, admin_error, no_store},
};

#[derive(Clone)]
pub struct ContentPreviewState {
    pub preview: Arc<ContentPreview>,
    pub admin: AdminState,
}

impl FromRef<ContentPreviewState> for AdminState {
    fn from_ref(state: &ContentPreviewState) -> Self {
        state.admin.clone()
    }
}

pub fn content_preview_router(state: ContentPreviewState) -> Router {
    Router::new()
        .route("/api/admin/v1/content-preview", post(preview))
        .layer(DefaultBodyLimit::max(ADMIN_BODY_LIMIT))
        .layer(middleware::from_fn(no_store))
        .with_state(state)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PreviewBody {
    content: String,
}

#[derive(Serialize)]
struct PreviewJson {
    content_html: String,
}

async fn preview(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<ContentPreviewState>,
    Json(body): Json<PreviewBody>,
) -> Response {
    match state.preview.render(&actor, &body.content).await {
        Ok(content_html) => Json(PreviewJson { content_html }).into_response(),
        Err(error) => admin_error(error, &request_id),
    }
}
