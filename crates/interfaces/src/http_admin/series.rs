//! 系列目录、成员与重排路由。

use super::support::{VersionBody, deserialize_double_option};
use super::{ADMIN_BODY_LIMIT, AdminAuth};
use crate::http_auth::AdminState;
use crate::http_support::{RequestId, admin_error, no_store};
use application::series::{CreateSeriesCmd, ReorderSeriesCmd, SeriesDto, UpdateSeriesCmd};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router, middleware};
use serde::Deserialize;
use uuid::Uuid;

#[derive(serde::Serialize, ts_rs::TS)]
#[ts(rename = "SeriesSummary")]
struct SeriesJson {
    id: Uuid,
    slug: String,
    name: String,
    description: Option<String>,
    /// 封面媒体资产 id（None = 无封面）。
    cover_media_id: Option<Uuid>,
    /// 封面站内地址（`/media/{id}`）；None = 无封面。
    cover_url: Option<String>,
    version: i64,
    /// 成员总数（含草稿/私密/回收站——它们保留位置）。
    post_count: i64,
    pub_post_count: i64,
}

impl From<&SeriesDto> for SeriesJson {
    fn from(dto: &SeriesDto) -> Self {
        Self {
            id: dto.id,
            slug: dto.slug.clone(),
            name: dto.name.clone(),
            description: dto.description.clone(),
            cover_media_id: dto.cover_media_id,
            cover_url: dto.cover_media_id.map(application::media::media_url),
            version: dto.version,
            post_count: dto.post_count,
            pub_post_count: dto.public_post_count,
        }
    }
}

#[derive(Deserialize, ts_rs::TS)]
#[ts(rename = "CreateSeriesInput", optional_fields = nullable)]
pub struct CreateSeriesBody {
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
}

#[derive(Deserialize, Default, ts_rs::TS)]
#[ts(rename = "UpdateSeriesInput", optional_fields = nullable)]
pub struct UpdateSeriesBody {
    pub name: String,
    pub description: Option<String>,
    /// 封面三态：缺省不修改；null 移除封面；id 设置封面。
    #[serde(default, deserialize_with = "deserialize_double_option")]
    #[ts(as = "Option<Uuid>", optional = nullable)]
    pub cover_media_id: Option<Option<Uuid>>,
    pub expected_version: Option<i64>,
}

#[derive(Deserialize, ts_rs::TS)]
#[ts(rename = "ReorderSeriesInput", optional_fields = nullable)]
pub struct ReorderBody {
    /// 系列内全部文章 id 按目标顺序（完整排列）。
    pub ordered_post_ids: Vec<Uuid>,
    pub expected_series_version: Option<i64>,
}

pub fn series_router(state: AdminState) -> Router {
    Router::new()
        .route("/api/admin/v1/series", get(list_series).post(create_series))
        .route(
            "/api/admin/v1/series/{slug}",
            axum::routing::patch(update_series).delete(delete_series),
        )
        .route("/api/admin/v1/series/{slug}/reorder", post(reorder_series))
        .route("/api/admin/v1/series/{slug}/members", get(series_members))
        .layer(axum::extract::DefaultBodyLimit::max(ADMIN_BODY_LIMIT))
        .layer(middleware::from_fn(no_store))
        .with_state(state)
}

async fn list_series(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
) -> Response {
    match state.series.list(&actor).await {
        Ok(list) => (
            StatusCode::OK,
            Json(list.iter().map(SeriesJson::from).collect::<Vec<_>>()),
        )
            .into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

async fn create_series(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Json(body): Json<CreateSeriesBody>,
) -> Response {
    match state
        .series
        .create(
            &actor,
            CreateSeriesCmd {
                name: body.name,
                slug: body.slug,
                description: body.description,
            },
        )
        .await
    {
        Ok(dto) => (StatusCode::CREATED, Json(SeriesJson::from(&dto))).into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

async fn update_series(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    Json(body): Json<UpdateSeriesBody>,
) -> Response {
    match state
        .series
        .update(
            &actor,
            &slug,
            UpdateSeriesCmd {
                name: body.name,
                description: body.description,
                cover_media_id: body.cover_media_id,
                expected_version: body.expected_version,
            },
        )
        .await
    {
        Ok(dto) => (StatusCode::OK, Json(SeriesJson::from(&dto))).into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

async fn delete_series(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    body: Option<Json<VersionBody>>,
) -> Response {
    let expected = body.and_then(|Json(b)| b.expected_version);
    match state.series.delete(&actor, &slug, expected).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

/// 管理目录：系列全部成员（含他人草稿/私密——重排会改动它们的位置）。
/// 需 series.manage，且**逐篇核验读取权限**（post.read own / post.read_any）：
/// 目录携带他人草稿的标题与状态；任一成员不可读即整次 403，不回残缺目录。
#[derive(serde::Serialize, ts_rs::TS)]
#[ts(rename = "SeriesMemberRow")]
struct SeriesMemberJson {
    id: Uuid,
    slug: String,
    title: String,
    status: String,
    deleted: bool,
    visibility: String,
    author_id: Uuid,
    position: i32,
}

async fn series_members(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Path(slug): Path<String>,
) -> Response {
    match state.series.members(&actor, &slug).await {
        Ok(members) => (
            StatusCode::OK,
            Json(
                members
                    .iter()
                    .map(|m| SeriesMemberJson {
                        id: m.post_id,
                        slug: m.slug.clone(),
                        title: m.title.clone(),
                        status: m.status.clone(),
                        deleted: m.deleted,
                        visibility: m.visibility.clone(),
                        author_id: m.author_id,
                        position: m.position,
                    })
                    .collect::<Vec<_>>(),
            ),
        )
            .into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

/// 整体重排：series.manage + 逐篇文章授权（own/any）；完整排列契约。
async fn reorder_series(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    Json(body): Json<ReorderBody>,
) -> Response {
    match state
        .series
        .reorder(
            &actor,
            &slug,
            ReorderSeriesCmd {
                ordered_post_ids: body.ordered_post_ids,
                expected_series_version: body.expected_series_version,
            },
        )
        .await
    {
        Ok(dto) => (
            StatusCode::OK,
            Json(crate::http_contract::ReorderSeriesResult::from(dto)),
        )
            .into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

pub(crate) fn export_contract(out: &mut Vec<String>) {
    crate::http_contract::declare::<SeriesJson>(out);
    crate::http_contract::declare::<SeriesMemberJson>(out);
    crate::http_contract::declare::<CreateSeriesBody>(out);
    crate::http_contract::declare::<UpdateSeriesBody>(out);
    crate::http_contract::declare::<ReorderBody>(out);
}
