//! 媒体管理 API 与独立公开的图片读取。上传接受受限图片裸字节体。

use std::sync::Arc;

use application::error::UseCaseError;
use application::media::{MediaContent, MediaDto, MediaInteractor, MediaUsageDto, UploadMediaCmd};
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;
use uuid::Uuid;

use crate::http_admin::AdminAuth;
use crate::http_auth::AdminState;
use crate::http_support::{RequestId, admin_error, no_store};

/// 单次上传的请求体上限：与 `domain::media::MAX_IMAGE_BYTES`（10 MiB）留出余量。
pub const MEDIA_BODY_LIMIT: usize = 12 * 1024 * 1024;

// ---------------------------------------------------------------------------
// 响应 DTO
// ---------------------------------------------------------------------------

#[derive(serde::Serialize, ts_rs::TS)]
#[ts(rename = "MediaAsset")]
struct MediaJson {
    id: Uuid,
    original_name: String,
    mime: String,
    byte_size: i64,
    width: i32,
    height: i32,
    deleted_at: Option<String>,
    version: i64,
    created_at: String,
    owner_id: Option<Uuid>,
    owner_display: String,
    url: String,
    /// 已知引用数；软删除保留引用。
    reference_count: i64,
}

impl From<&MediaDto> for MediaJson {
    fn from(dto: &MediaDto) -> Self {
        Self {
            id: dto.id,
            original_name: dto.original_name.clone(),
            mime: dto.mime.clone(),
            byte_size: dto.byte_size,
            width: dto.width,
            height: dto.height,
            deleted_at: dto.deleted_at.map(application::public_site::api_datetime),
            version: dto.version,
            created_at: application::public_site::api_datetime(dto.created_at),
            owner_id: dto.owner_id,
            owner_display: dto.owner_display.clone(),
            url: dto.url.clone(),
            reference_count: dto.reference_count,
        }
    }
}

#[derive(serde::Serialize, ts_rs::TS)]
#[ts(rename = "MediaReference")]
struct MediaUsageJson {
    kind: &'static str,
    content_id: Uuid,
    slug: String,
    title: String,
    status: String,
    visibility: String,
    deleted: bool,
    public: bool,
}

impl From<&MediaUsageDto> for MediaUsageJson {
    fn from(dto: &MediaUsageDto) -> Self {
        Self {
            kind: dto.source.kind().as_str(),
            content_id: dto.content_id,
            slug: dto.slug.clone(),
            title: dto.title.clone(),
            // Series/Site have no lifecycle status; retain their existing API display values.
            status: match dto.source {
                application::ports::MediaUsageSource::Post(status) => status.as_str(),
                application::ports::MediaUsageSource::Page(status) => status.as_str(),
                application::ports::MediaUsageSource::User(status) => status.as_str(),
                application::ports::MediaUsageSource::Series => "published",
                application::ports::MediaUsageSource::Site => "active",
            }
            .to_owned(),
            visibility: dto.visibility.as_str().to_owned(),
            deleted: dto.deleted,
            public: dto.public,
        }
    }
}

#[derive(serde::Serialize, ts_rs::TS)]
#[ts(rename = "MediaPage")]
struct MediaPageJson {
    items: Vec<MediaJson>,
    total: i64,
    page: i64,
    per_page: i64,
}

#[derive(serde::Serialize, ts_rs::TS)]
#[ts(rename = "MediaUsageView")]
struct MediaUsageViewJson {
    media: MediaJson,
    /// 调用者有权查看的使用位置（按 Post own/any 与 Page 站点权限过滤）。
    references: Vec<MediaUsageJson>,
    /// 存在但调用者无权查看的引用数：计数是全局的，展示必须过滤，
    /// 因此把差额如实返回，界面才能解释「为什么列出的比统计的少」。
    hidden_references: i64,
}

#[derive(Deserialize, Default)]
struct MediaListQuery {
    page: Option<i64>,
    q: Option<String>,
    #[serde(default)]
    trash: bool,
}

/// 上传查询参数：文件名只用于展示，缺省时按「未命名图片」处理。
#[derive(Deserialize, Default)]
struct UploadQuery {
    filename: Option<String>,
}

#[derive(Deserialize, ts_rs::TS)]
#[ts(rename = "MediaVersionInput", optional_fields = nullable)]
struct MediaVersionBody {
    expected_version: i64,
}

// ---------------------------------------------------------------------------
// 后台管理路由
// ---------------------------------------------------------------------------

pub fn media_admin_router(state: AdminState) -> Router {
    let reads = Router::new()
        .route("/api/admin/v1/media", get(list_media))
        .route("/api/admin/v1/media/{id}", get(media_detail))
        .layer(middleware::from_fn(no_store))
        .with_state(state.clone());
    let writes = Router::new()
        .route("/api/admin/v1/media", post(upload_media))
        .route("/api/admin/v1/media/{id}", delete(delete_media))
        .route("/api/admin/v1/media/{id}/restore", post(restore_media))
        // 二进制入口是唯一需要放宽请求体上限的路由；其余管理 API 仍是 2 MiB。
        .layer(DefaultBodyLimit::max(MEDIA_BODY_LIMIT))
        .layer(middleware::from_fn(no_store))
        .with_state(state);
    reads.merge(writes)
}

/// 媒体库分页：按上传时间倒序，显示文件名、大小、上传者与引用状态。
async fn list_media(
    State(state): State<AdminState>,
    auth: AdminAuth,
    Query(query): Query<MediaListQuery>,
    request_id: RequestId,
) -> Response {
    match state
        .media
        .list(
            &auth.actor,
            query.page.unwrap_or(1),
            query.trash,
            query.q.as_deref(),
        )
        .await
    {
        Ok(page) => (
            StatusCode::OK,
            Json(MediaPageJson {
                items: page.items.iter().map(MediaJson::from).collect(),
                total: page.total,
                page: page.page,
                per_page: page.per_page,
            }),
        )
            .into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

/// 单个资产详情与有权查看的使用位置。
async fn media_detail(
    State(state): State<AdminState>,
    Path(id): Path<String>,
    auth: AdminAuth,
    request_id: RequestId,
) -> Response {
    let Ok(id) = Uuid::parse_str(&id) else {
        return admin_error(UseCaseError::NotFound("图片".into()), &request_id);
    };
    match state.media.detail(&auth.actor, id).await {
        Ok(view) => (
            StatusCode::OK,
            Json(MediaUsageViewJson {
                media: MediaJson::from(&view.media),
                references: view.references.iter().map(MediaUsageJson::from).collect(),
                hidden_references: view.hidden_references,
            }),
        )
            .into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

/// 上传：请求体就是图片字节，`?filename=` 只提供展示名。
///
/// 声明类型与文件名都不参与判定；格式、尺寸与大小由用例按文件内容校验。
async fn upload_media(
    State(state): State<AdminState>,
    auth: AdminAuth,
    Query(query): Query<UploadQuery>,
    request_id: RequestId,
    body: Bytes,
) -> Response {
    if body.is_empty() {
        return admin_error(UseCaseError::Invalid("上传内容为空".into()), &request_id);
    }
    let cmd = UploadMediaCmd {
        file_name: query.filename.unwrap_or_default(),
        bytes: body.to_vec(),
    };
    match state.media.upload(&auth.actor, cmd).await {
        Ok(dto) => (StatusCode::CREATED, Json(MediaJson::from(&dto))).into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

/// 软删除：保留对象、链接和引用；只从正常媒体库隐藏。
async fn delete_media(
    State(state): State<AdminState>,
    Path(id): Path<String>,
    auth: AdminAuth,
    request_id: RequestId,
    Json(body): Json<MediaVersionBody>,
) -> Response {
    let Ok(id) = Uuid::parse_str(&id) else {
        return admin_error(UseCaseError::NotFound("图片".into()), &request_id);
    };
    match state
        .media
        .set_deleted(&auth.actor, id, body.expected_version, true)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

async fn restore_media(
    State(state): State<AdminState>,
    Path(id): Path<String>,
    auth: AdminAuth,
    request_id: RequestId,
    Json(body): Json<MediaVersionBody>,
) -> Response {
    let Ok(id) = Uuid::parse_str(&id) else {
        return admin_error(UseCaseError::NotFound("图片".into()), &request_id);
    };
    match state
        .media
        .set_deleted(&auth.actor, id, body.expected_version, false)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

// ---------------------------------------------------------------------------
// 公开文件读取
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct MediaReadState {
    pub media: Arc<MediaInteractor>,
}

pub fn media_read_router(state: MediaReadState) -> Router {
    Router::new()
        .route("/media/{id}", get(read_media))
        .with_state(state)
}

async fn read_media(
    State(state): State<MediaReadState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let Ok(id) = Uuid::parse_str(&id) else {
        return media_not_found();
    };
    match state.media.read(id).await {
        Ok(content) => file_response(&content, &headers),
        Err(UseCaseError::NotFound(_)) => media_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "读取媒体文件失败");
            (StatusCode::INTERNAL_SERVER_ERROR, "服务器内部错误").into_response()
        }
    }
}

/// 地址对应不可变的图片字节，可公开长期缓存；软删除不撤销访问。
fn file_response(content: &MediaContent, request: &HeaderMap) -> Response {
    let etag = format!("\"{}\"", content.checksum_sha256);
    if let Some(value) = request
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        && value.split(',').any(|candidate| candidate.trim() == etag)
    {
        let mut response = StatusCode::NOT_MODIFIED.into_response();
        insert_media_headers(response.headers_mut(), content, &etag);
        return response;
    }
    let mut response = (
        StatusCode::OK,
        [(header::CONTENT_TYPE, content.mime.clone())],
        content.bytes.clone(),
    )
        .into_response();
    insert_media_headers(response.headers_mut(), content, &etag);
    response
}

fn insert_media_headers(headers: &mut HeaderMap, content: &MediaContent, etag: &str) {
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    if let Ok(value) = HeaderValue::from_str(&content.mime) {
        headers.insert(header::CONTENT_TYPE, value);
    }
    if let Ok(value) = HeaderValue::from_str(etag) {
        headers.insert(header::ETAG, value);
    }
    // 不写 Content-Disposition：浏览器按 MIME 内联展示图片本身。
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
}

fn media_not_found() -> Response {
    (StatusCode::NOT_FOUND, "图片不存在").into_response()
}

pub(crate) fn export_contract(out: &mut Vec<String>) {
    crate::http_contract::declare::<MediaJson>(out);
    crate::http_contract::declare::<MediaUsageJson>(out);
    crate::http_contract::declare::<MediaPageJson>(out);
    crate::http_contract::declare::<MediaUsageViewJson>(out);
    crate::http_contract::declare::<MediaVersionBody>(out);
}
