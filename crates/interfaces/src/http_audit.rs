//! Audit history has a dedicated read permission and no mutation endpoints.
use crate::{
    http_admin::AdminAuth,
    http_auth::AdminState,
    http_support::{RequestId, admin_error, no_store},
};
use application::{
    audit::{AuditInteractor, AuditQuery},
    error::UseCaseError,
};
use axum::{
    Json, Router,
    extract::{FromRef, Query, State, rejection::QueryRejection},
    middleware,
    response::{IntoResponse, Response},
    routing::get,
};
use std::sync::Arc;

#[derive(Clone)]
pub struct AuditState {
    pub audit: Arc<AuditInteractor>,
    pub admin: AdminState,
}
impl FromRef<AuditState> for AdminState {
    fn from_ref(state: &AuditState) -> Self {
        state.admin.clone()
    }
}
pub fn audit_router(state: AuditState) -> Router {
    Router::new()
        .route("/api/admin/v1/audit-logs", get(list))
        .layer(middleware::from_fn(no_store))
        .with_state(state)
}
async fn list(
    State(s): State<AuditState>,
    auth: AdminAuth,
    id: RequestId,
    query: Result<Query<AuditQuery>, QueryRejection>,
) -> Response {
    if !auth.actor.has_permission("audit.read") {
        return admin_error(UseCaseError::Forbidden, &id);
    }
    let query = match query {
        Ok(Query(query)) => query,
        Err(_) => return admin_error(UseCaseError::Invalid("审计筛选参数无效".into()), &id),
    };
    match s.audit.list(&auth.actor, query).await {
        Ok(page) => Json(page).into_response(),
        Err(e) => admin_error(e, &id),
    }
}
