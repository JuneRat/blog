//! Explicit HTML maintenance admission and progress; execution belongs to the runtime adapter.
use crate::{
    http_admin::AdminAuth,
    http_auth::AdminState,
    http_contract::{HtmlRebuildJob, HtmlRebuildView},
    http_support::{RequestId, admin_error, no_store},
};
use application::html_rebuild_admin::HtmlRebuildAdminInteractor;
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, FromRef, State},
    http::StatusCode,
    middleware,
    response::{IntoResponse, Response},
    routing::get,
};
use std::sync::Arc;

#[derive(Clone)]
pub struct HtmlRebuildState {
    pub rebuild: Arc<HtmlRebuildAdminInteractor>,
    pub admin: AdminState,
}

impl FromRef<HtmlRebuildState> for AdminState {
    fn from_ref(state: &HtmlRebuildState) -> Self {
        state.admin.clone()
    }
}

pub fn html_rebuild_router(state: HtmlRebuildState) -> Router {
    Router::new()
        .route(
            "/api/admin/v1/maintenance/html-rebuild",
            get(read).post(start),
        )
        .layer(DefaultBodyLimit::max(4 * 1024))
        .layer(middleware::from_fn(no_store))
        .with_state(state)
}

async fn read(State(s): State<HtmlRebuildState>, auth: AdminAuth, id: RequestId) -> Response {
    match s.rebuild.view(&auth.actor).await {
        Ok(view) => Json(HtmlRebuildView::from(view)).into_response(),
        Err(error) => admin_error(error, &id),
    }
}

async fn start(State(s): State<HtmlRebuildState>, auth: AdminAuth, id: RequestId) -> Response {
    match s.rebuild.start(&auth.actor).await {
        Ok(job) => (StatusCode::ACCEPTED, Json(HtmlRebuildJob::from(job))).into_response(),
        Err(error) => admin_error(error, &id),
    }
}
