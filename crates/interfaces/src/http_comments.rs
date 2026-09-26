//! Comment HTTP boundaries: no cached visibility, session writes reuse AdminAuth.
use crate::{
    http_admin::AdminAuth,
    http_auth::AdminState,
    http_support::{RequestId, admin_error, cookie_value, no_store},
};
use application::{
    comments::{CommentInteractor, CommentPolicy, SubmitComment},
    error::UseCaseError,
    ports::SESSION_COOKIE,
};
use axum::{
    Json, Router,
    extract::{ConnectInfo, DefaultBodyLimit, FromRef, FromRequestParts, Path, Query, State},
    http::{HeaderMap, request::Parts},
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use std::{net::SocketAddr, sync::Arc};
use uuid::Uuid;

#[derive(Clone)]
pub struct CommentState {
    pub comments: Arc<CommentInteractor>,
    pub admin: AdminState,
    pub origin: String,
}
impl FromRef<CommentState> for AdminState {
    fn from_ref(s: &CommentState) -> Self {
        s.admin.clone()
    }
}
struct CommentAuth(Option<application::identity::Actor>);
impl FromRequestParts<CommentState> for CommentAuth {
    type Rejection = Response;
    async fn from_request_parts(parts: &mut Parts, state: &CommentState) -> Result<Self, Response> {
        let id = parts
            .extensions
            .get::<RequestId>()
            .cloned()
            .unwrap_or_else(RequestId::generate);
        if parts.method != axum::http::Method::GET {
            check_origin(&parts.headers, &state.origin).map_err(|e| admin_error(e, &id))?;
        }
        if cookie_value(&parts.headers, SESSION_COOKIE).is_some() {
            Ok(Self(Some(
                AdminAuth::from_request_parts(parts, state).await?.actor,
            )))
        } else {
            Ok(Self(None))
        }
    }
}
fn check_origin(headers: &HeaderMap, origin: &str) -> Result<(), UseCaseError> {
    if headers.get("origin").and_then(|v| v.to_str().ok()) != Some(origin.trim_end_matches('/')) {
        return Err(UseCaseError::Forbidden);
    }
    if headers
        .get("sec-fetch-site")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v != "same-origin")
    {
        return Err(UseCaseError::Forbidden);
    }
    Ok(())
}
pub fn comments_router(state: CommentState) -> Router {
    Router::new()
        .route(
            "/assets/comments.js",
            get(|| async {
                (
                    [("content-type", "text/javascript; charset=utf-8")],
                    include_str!("../assets/comments.js"),
                )
            }),
        )
        .route(
            "/assets/comments.css",
            get(|| async {
                (
                    [("content-type", "text/css; charset=utf-8")],
                    include_str!("../assets/comments.css"),
                )
            }),
        )
        .route(
            "/api/v1/posts/{slug}/comments",
            get(public_list).post(submit),
        )
        .route("/api/admin/v1/comments", get(list))
        .route("/api/admin/v1/comments/{id}", post(moderate))
        .route(
            "/api/admin/v1/comment-settings",
            get(global_policy).put(set_global_policy),
        )
        .route(
            "/api/admin/v1/posts/{id}/comment-settings",
            get(post_policy).put(set_post_policy),
        )
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(middleware::from_fn(no_store))
        .with_state(state)
}
#[derive(Deserialize)]
struct PageQuery {
    #[serde(default = "first_page")]
    page: i64,
    parent_id: Option<Uuid>,
    status: Option<String>,
    post_id: Option<Uuid>,
}
fn first_page() -> i64 {
    1
}
async fn public_list(
    State(s): State<CommentState>,
    Path(slug): Path<String>,
    Query(q): Query<PageQuery>,
    id: RequestId,
) -> Response {
    match s.comments.public_list(&slug, q.parent_id, q.page).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => admin_error(e, &id),
    }
}
async fn submit(
    State(s): State<CommentState>,
    Path(slug): Path<String>,
    id: RequestId,
    auth: CommentAuth,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(cmd): Json<SubmitComment>,
) -> Response {
    // Ignore client-supplied forwarding headers; deployments behind a proxy share
    // its quota until a trusted proxy policy is configured explicitly.
    match s
        .comments
        .submit(&slug, auth.0.as_ref(), &peer.ip().to_string(), cmd)
        .await
    {
        Ok(()) => (
            axum::http::StatusCode::ACCEPTED,
            Json(serde_json::json!({"message":"已提交，等待审核"})),
        )
            .into_response(),
        Err(e) => admin_error(e, &id),
    }
}
async fn list(
    State(s): State<CommentState>,
    auth: AdminAuth,
    Query(q): Query<PageQuery>,
    id: RequestId,
) -> Response {
    match s
        .comments
        .list(&auth.actor, q.status.as_deref(), q.post_id, q.page)
        .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => admin_error(e, &id),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Moderate {
    version: i64,
    status: Option<String>,
    #[serde(default)]
    delete: bool,
}
async fn moderate(
    State(s): State<CommentState>,
    auth: AdminAuth,
    Path(cid): Path<Uuid>,
    id: RequestId,
    Json(cmd): Json<Moderate>,
) -> Response {
    if cmd.delete == cmd.status.is_some() {
        return admin_error(UseCaseError::Invalid("请选择审核状态或删除".into()), &id);
    }
    let action = if cmd.delete {
        application::comments::ModerationAction::DeletePermanently
    } else {
        match application::comments::CommentStatus::parse(cmd.status.as_deref().unwrap_or_default())
        {
            Ok(status) => application::comments::ModerationAction::SetStatus(status),
            Err(e) => return admin_error(UseCaseError::Invalid(e.into()), &id),
        }
    };
    match s
        .comments
        .moderate(&auth.actor, cid, cmd.version, action)
        .await
    {
        Ok(()) => axum::http::StatusCode::NO_CONTENT.into_response(),
        Err(e) => admin_error(e, &id),
    }
}
async fn policy(
    s: CommentState,
    auth: AdminAuth,
    post: Option<Uuid>,
    update: Option<CommentPolicy>,
    id: RequestId,
) -> Response {
    match s.comments.policy(&auth.actor, post, update).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => admin_error(e, &id),
    }
}
async fn global_policy(State(s): State<CommentState>, auth: AdminAuth, id: RequestId) -> Response {
    policy(s, auth, None, None, id).await
}
async fn post_policy(
    State(s): State<CommentState>,
    auth: AdminAuth,
    Path(post): Path<Uuid>,
    id: RequestId,
) -> Response {
    policy(s, auth, Some(post), None, id).await
}
async fn set_global_policy(
    State(s): State<CommentState>,
    auth: AdminAuth,
    id: RequestId,
    Json(update): Json<CommentPolicy>,
) -> Response {
    policy(s, auth, None, Some(update), id).await
}
async fn set_post_policy(
    State(s): State<CommentState>,
    auth: AdminAuth,
    Path(post): Path<Uuid>,
    id: RequestId,
    Json(update): Json<CommentPolicy>,
) -> Response {
    policy(s, auth, Some(post), Some(update), id).await
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn guest_origin_is_required_and_compared_to_configured_origin() {
        let mut h = HeaderMap::new();
        assert!(check_origin(&h, "https://blog.test").is_err());
        h.insert("origin", "https://evil.test".parse().unwrap());
        assert!(check_origin(&h, "https://blog.test").is_err());
        h.insert("origin", "https://blog.test".parse().unwrap());
        assert!(check_origin(&h, "https://blog.test/").is_ok());
        h.insert("sec-fetch-site", "cross-site".parse().unwrap());
        assert!(check_origin(&h, "https://blog.test").is_err());
    }
}
