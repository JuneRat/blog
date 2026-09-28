//! 分类目录路由与传输 DTO。

use super::support::{VersionBody, deserialize_double_option};
use super::{ADMIN_BODY_LIMIT, AdminAuth};
use crate::http_auth::AdminState;
use crate::http_support::{RequestId, admin_error, no_store};
use application::category::{CategoryDto, CreateCategoryCmd, UpdateCategoryCmd};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router, middleware};
use serde::Deserialize;
use uuid::Uuid;

#[derive(serde::Serialize)]
struct CategoryJson {
    id: Uuid,
    slug: String,
    name: String,
    parent_id: Option<Uuid>,
    description: Option<String>,
    version: i64,
    /// 直接归属的公开文章计数。
    pub_post_count: i64,
}

impl From<&CategoryDto> for CategoryJson {
    fn from(dto: &CategoryDto) -> Self {
        Self {
            id: dto.id,
            slug: dto.slug.clone(),
            name: dto.name.clone(),
            parent_id: dto.parent_id,
            description: dto.description.clone(),
            version: dto.version,
            pub_post_count: dto.public_post_count,
        }
    }
}

#[derive(Deserialize)]
pub struct CreateCategoryBody {
    pub name: String,
    pub slug: String,
    /// 父分类 slug；缺省为根分类。
    pub parent: Option<String>,
    pub description: Option<String>,
}

#[derive(Deserialize, Default)]
pub struct UpdateCategoryBody {
    pub name: String,
    pub description: Option<String>,
    /// 三态：缺省保持现状；null 移到根；slug 移到指定父。
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub parent: Option<Option<String>>,
    pub expected_version: Option<i64>,
}

pub fn categories_router(state: AdminState) -> Router {
    Router::new()
        .route(
            "/api/admin/v1/categories",
            get(list_categories).post(create_category),
        )
        .route(
            "/api/admin/v1/categories/{slug}",
            axum::routing::patch(update_category).delete(delete_category),
        )
        .layer(axum::extract::DefaultBodyLimit::max(ADMIN_BODY_LIMIT))
        .layer(middleware::from_fn(no_store))
        .with_state(state)
}

/// 目录读取：任何已认证会话可读（文章编辑器选择分类需要）。
async fn list_categories(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
) -> Response {
    match state.categories.list(&actor).await {
        Ok(list) => (
            StatusCode::OK,
            Json(list.iter().map(CategoryJson::from).collect::<Vec<_>>()),
        )
            .into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

async fn create_category(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Json(body): Json<CreateCategoryBody>,
) -> Response {
    match state
        .categories
        .create(
            &actor,
            CreateCategoryCmd {
                name: body.name,
                slug: body.slug,
                parent: body.parent,
                description: body.description,
            },
        )
        .await
    {
        Ok(dto) => (StatusCode::CREATED, Json(CategoryJson::from(&dto))).into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

async fn update_category(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    Json(body): Json<UpdateCategoryBody>,
) -> Response {
    match state
        .categories
        .update(
            &actor,
            &slug,
            UpdateCategoryCmd {
                name: body.name,
                description: body.description,
                parent: body.parent,
                expected_version: body.expected_version,
            },
        )
        .await
    {
        Ok(dto) => (StatusCode::OK, Json(CategoryJson::from(&dto))).into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

async fn delete_category(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    body: Option<Json<VersionBody>>,
) -> Response {
    let expected = body.and_then(|Json(b)| b.expected_version);
    match state.categories.delete(&actor, &slug, expected).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}
