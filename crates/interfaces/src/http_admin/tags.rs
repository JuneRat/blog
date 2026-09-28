//! 标签目录路由与传输 DTO。

use super::support::VersionBody;
use super::{ADMIN_BODY_LIMIT, AdminAuth};
use crate::http_auth::AdminState;
use crate::http_support::{RequestId, admin_error, no_store};
use application::tag::{CreateTagCmd, TagDto};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router, middleware};
use serde::Deserialize;
use uuid::Uuid;

#[derive(serde::Serialize)]
struct TagJson {
    id: Uuid,
    slug: String,
    name: String,
    version: i64,
    /// 公开文章计数（与公开标签页同口径）。
    public_post_count: i64,
}

impl From<&TagDto> for TagJson {
    fn from(dto: &TagDto) -> Self {
        Self {
            id: dto.id,
            slug: dto.slug.clone(),
            name: dto.name.clone(),
            version: dto.version,
            public_post_count: dto.public_post_count,
        }
    }
}

#[derive(Deserialize)]
pub struct CreateTagBody {
    pub name: String,
    pub slug: String,
}

#[derive(Deserialize, Default)]
pub struct RenameTagBody {
    pub name: String,
    pub expected_version: Option<i64>,
}

pub fn tags_router(state: AdminState) -> Router {
    Router::new()
        .route("/api/admin/v1/tags", get(list_tags).post(create_tag))
        .route(
            "/api/admin/v1/tags/{slug}",
            axum::routing::patch(rename_tag).delete(delete_tag),
        )
        .layer(axum::extract::DefaultBodyLimit::max(ADMIN_BODY_LIMIT))
        .layer(middleware::from_fn(no_store))
        .with_state(state)
}

/// 目录读取：任何已认证会话可读（Author 编辑文章要选标签），
/// 权限边界在写动作上；目录本身是公开数据。
async fn list_tags(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
) -> Response {
    match state.tags.list(&actor).await {
        Ok(list) => (
            StatusCode::OK,
            Json(list.iter().map(TagJson::from).collect::<Vec<_>>()),
        )
            .into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

async fn create_tag(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Json(body): Json<CreateTagBody>,
) -> Response {
    match state
        .tags
        .create(
            &actor,
            CreateTagCmd {
                name: body.name,
                slug: body.slug,
            },
        )
        .await
    {
        Ok(dto) => (StatusCode::CREATED, Json(TagJson::from(&dto))).into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

async fn rename_tag(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    Json(body): Json<RenameTagBody>,
) -> Response {
    match state
        .tags
        .rename(&actor, &slug, body.name, body.expected_version)
        .await
    {
        Ok(dto) => (StatusCode::OK, Json(TagJson::from(&dto))).into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

async fn delete_tag(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    body: Option<Json<VersionBody>>,
) -> Response {
    let expected = body.and_then(|Json(b)| b.expected_version);
    match state.tags.delete(&actor, &slug, expected).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}
