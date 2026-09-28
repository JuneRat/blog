//! 页面管理路由与传输 DTO。

use super::support::{ListQuery, ScheduleBody, VersionBody, api_datetime, parse_visibility};
use super::{ADMIN_BODY_LIMIT, AdminAuth};
use crate::http_auth::AdminState;
use crate::http_support::{RequestId, admin_error, no_store};
use application::content_queries::AdminPageSummary;
use application::error::UseCaseError;
use application::page::{CreatePageCmd, DeletePageCmd, EditPageCmd, PageDto};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router, middleware};
use serde::Deserialize;
use uuid::Uuid;

#[derive(serde::Serialize, ts_rs::TS)]
#[ts(rename = "PageSummary")]
struct PageJson {
    id: Uuid,
    slug: String,
    title: String,
    status: String,
    visibility: String,
    version: i64,
    published_at: Option<String>,
    updated_at: String,
}

impl From<&PageDto> for PageJson {
    fn from(dto: &PageDto) -> Self {
        Self {
            id: dto.id,
            slug: dto.slug.clone(),
            title: dto.title.clone(),
            status: dto.status.to_string(),
            visibility: dto.visibility.to_string(),
            version: dto.version,
            published_at: dto.published_at.map(api_datetime),
            updated_at: api_datetime(dto.updated_at),
        }
    }
}

impl From<AdminPageSummary> for PageJson {
    fn from(dto: AdminPageSummary) -> Self {
        Self {
            id: dto.id,
            slug: dto.slug,
            title: dto.title,
            status: dto.status.as_str().to_owned(),
            visibility: dto.visibility.as_str().to_owned(),
            version: dto.version,
            published_at: dto.published_at.map(api_datetime),
            updated_at: api_datetime(dto.updated_at),
        }
    }
}

/// 页面详情：摘要 + Markdown 源文（后台编辑数据源）。
#[derive(serde::Serialize, ts_rs::TS)]
#[ts(rename = "PageDetail")]
struct PageDetailJson {
    #[serde(flatten)]
    summary: PageJson,
    content: String,
}

impl From<PageDto> for PageDetailJson {
    fn from(dto: PageDto) -> Self {
        Self {
            summary: PageJson::from(&dto),
            content: dto.content,
        }
    }
}

#[derive(Deserialize, ts_rs::TS)]
#[ts(rename = "CreatePageInput", optional_fields = nullable)]
pub struct CreatePageBody {
    pub slug: Option<String>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub content: String,
    pub visibility: Option<String>,
}

#[derive(Deserialize, Default, ts_rs::TS)]
#[ts(rename = "EditPageInput", optional_fields = nullable)]
pub struct EditPageBody {
    pub new_slug: Option<String>,
    pub title: Option<String>,
    pub content: Option<String>,
    pub visibility: Option<String>,
    pub expected_version: Option<i64>,
}

#[derive(Deserialize, ts_rs::TS)]
#[ts(rename = "DeletePageInput", optional_fields = nullable)]
pub struct DeletePageBody {
    pub expected_version: i64,
}

pub fn pages_router(state: AdminState) -> Router {
    Router::new()
        .route("/api/admin/v1/pages", get(list_pages).post(create_page))
        .route("/api/admin/v1/pages/{id}", get(get_page).patch(edit_page))
        .route("/api/admin/v1/pages/{id}/publish", post(publish_page))
        .route("/api/admin/v1/pages/{id}/unpublish", post(unpublish_page))
        .route("/api/admin/v1/pages/{id}/schedule", post(schedule_page))
        .route("/api/admin/v1/pages/{id}/archive", post(archive_page))
        .route("/api/admin/v1/pages/{id}/trash", post(trash_page))
        .route("/api/admin/v1/pages/{id}/restore", post(restore_page))
        .route("/api/admin/v1/pages/{id}/purge", post(purge_page))
        .route("/api/admin/v1/page-trash", get(list_page_trash))
        .layer(axum::extract::DefaultBodyLimit::max(ADMIN_BODY_LIMIT))
        .layer(middleware::from_fn(no_store))
        .with_state(state)
}

async fn create_page(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Json(body): Json<CreatePageBody>,
) -> Response {
    let visibility = match parse_visibility(body.visibility.as_deref()) {
        Ok(v) => v,
        Err(e) => return admin_error(e, &request_id),
    };
    match state
        .pages
        .create(
            &actor,
            CreatePageCmd {
                slug: body.slug,
                title: body.title,
                content: body.content,
                visibility,
            },
        )
        .await
    {
        Ok(dto) => (StatusCode::CREATED, Json(PageDetailJson::from(dto))).into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

async fn get_page(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
) -> Response {
    match state.pages.find(&actor, id).await {
        Ok(dto) => (StatusCode::OK, Json(PageDetailJson::from(dto))).into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

async fn list_pages(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Query(query): Query<ListQuery>,
) -> Response {
    match state
        .content_queries
        .pages(&actor, query.request(false))
        .await
    {
        Ok(page) => Json(crate::http_contract::ContentPage::from(
            page.map(PageJson::from),
        ))
        .into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

async fn edit_page(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    Json(body): Json<EditPageBody>,
) -> Response {
    let visibility = match body.visibility.as_deref() {
        Some(v) => match parse_visibility(Some(v)) {
            Ok(parsed) => Some(parsed),
            Err(e) => return admin_error(e, &request_id),
        },
        None => None,
    };
    let cmd = EditPageCmd {
        id,
        new_slug: body.new_slug,
        title: body.title,
        content: body.content,
        visibility,
        expected_version: body.expected_version,
    };
    let result = state.pages.edit(&actor, cmd).await;
    match result {
        Ok(dto) => (StatusCode::OK, Json(PageDetailJson::from(dto))).into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

async fn publish_page(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    body: Option<Json<VersionBody>>,
) -> Response {
    let expected = body.and_then(|Json(b)| b.expected_version);
    match state.pages.publish(&actor, id, expected).await {
        Ok(dto) => (StatusCode::OK, Json(PageDetailJson::from(dto))).into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

async fn unpublish_page(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    body: Option<Json<VersionBody>>,
) -> Response {
    let expected = body.and_then(|Json(b)| b.expected_version);
    match state.pages.withdraw(&actor, id, expected).await {
        Ok(dto) => (StatusCode::OK, Json(PageDetailJson::from(dto))).into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}
async fn schedule_page(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    Json(body): Json<ScheduleBody>,
) -> Response {
    let at = match time::OffsetDateTime::parse(
        &body.published_at,
        &time::format_description::well_known::Rfc3339,
    ) {
        Ok(at) => at,
        Err(_) => {
            return admin_error(
                UseCaseError::Invalid("published_at 须包含时区的 RFC3339 时间".into()),
                &request_id,
            );
        }
    };
    match state
        .pages
        .schedule(&actor, id, at, body.expected_version)
        .await
    {
        Ok(dto) => (StatusCode::OK, Json(PageDetailJson::from(dto))).into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}
async fn archive_page(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    body: Option<Json<VersionBody>>,
) -> Response {
    match state
        .pages
        .archive(&actor, id, body.and_then(|Json(b)| b.expected_version))
        .await
    {
        Ok(dto) => (StatusCode::OK, Json(PageDetailJson::from(dto))).into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

async fn trash_page(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    Json(body): Json<DeletePageBody>,
) -> Response {
    match state
        .pages
        .trash(
            &actor,
            DeletePageCmd {
                id,
                expected_version: body.expected_version,
            },
        )
        .await
    {
        Ok(dto) => (StatusCode::OK, Json(PageDetailJson::from(dto))).into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

async fn restore_page(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    Json(body): Json<DeletePageBody>,
) -> Response {
    match state
        .pages
        .restore(
            &actor,
            DeletePageCmd {
                id,
                expected_version: body.expected_version,
            },
        )
        .await
    {
        Ok(dto) => (StatusCode::OK, Json(PageDetailJson::from(dto))).into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

async fn purge_page(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    Json(body): Json<DeletePageBody>,
) -> Response {
    match state
        .pages
        .purge(
            &actor,
            DeletePageCmd {
                id,
                expected_version: body.expected_version,
            },
        )
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

async fn list_page_trash(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Query(query): Query<ListQuery>,
) -> Response {
    match state
        .content_queries
        .pages(&actor, query.request(true))
        .await
    {
        Ok(page) => Json(crate::http_contract::ContentPage::from(
            page.map(PageJson::from),
        ))
        .into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

pub(crate) fn export_contract(out: &mut Vec<String>) {
    crate::http_contract::declare::<PageJson>(out);
    crate::http_contract::declare::<PageDetailJson>(out);
    crate::http_contract::declare::<CreatePageBody>(out);
    crate::http_contract::declare::<EditPageBody>(out);
    crate::http_contract::declare::<DeletePageBody>(out);
}
