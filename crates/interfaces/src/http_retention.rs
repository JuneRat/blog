//! Authenticated settings; maintenance execution is deliberately not an HTTP API.
use crate::{
    http_admin::AdminAuth,
    http_auth::AdminState,
    http_support::{RequestId, admin_error, no_store},
};
use application::retention::{RetentionInteractor, RetentionSettings};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, FromRef, State},
    middleware,
    response::{IntoResponse, Response},
    routing::get,
};
use std::sync::Arc;
#[derive(Clone)]
pub struct RetentionState {
    pub retention: Arc<RetentionInteractor>,
    pub admin: AdminState,
}
impl FromRef<RetentionState> for AdminState {
    fn from_ref(state: &RetentionState) -> Self {
        state.admin.clone()
    }
}
pub fn retention_router(state: RetentionState) -> Router {
    Router::new()
        .route("/api/admin/v1/settings/retention", get(read).put(save))
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(middleware::from_fn(no_store))
        .with_state(state)
}
async fn read(State(s): State<RetentionState>, auth: AdminAuth, id: RequestId) -> Response {
    match s.retention.read(&auth.actor).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => admin_error(e, &id),
    }
}
async fn save(
    State(s): State<RetentionState>,
    auth: AdminAuth,
    id: RequestId,
    Json(value): Json<RetentionSettings>,
) -> Response {
    match s.retention.save(&auth.actor, value).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => admin_error(e, &id),
    }
}
