//! 媒体 HTTP 入站适配器：后台管理 API 与公开文件读取。
//!
//! - 管理 API 走 `AdminAuth`（会话 + CSRF/Origin），权限由用例判定。
//! - `GET /media/{id}` 是**公开边界**：匿名只在存在公开来源引用时放行；
//!   已认证且持有 `media.read` 时可预览任意可用图片（后台预览）。
//!   两种失败一律 404，不泄漏资产存在性。
//! - 上传是**裸字节体**（不是 multipart）：`POST /api/admin/v1/media?filename=…`
//!   的 body 就是图片本身。请求的 `Content-Type` 与文件名都不参与判定——格式由
//!   文件内容嗅探（`domain::media`），文件名只用于展示。因此不需要 multipart
//!   解析依赖，也从根本上避免「按声明类型放行」。
//! - 上传上限由 `DefaultBodyLimit` 在解析前拦住，避免超大请求进入内存。
//!
//! 缓存：公开引用只能重校验（撤回后下一次请求必须立即停止），因此用 `no-cache`
//! 配 ETag；后台预览是私密响应，一律 `no-store`。**绝不**使用长 max-age，
//! 否则撤回后图片会继续从缓存流出（docs/content-lifecycle.md §5）。

use std::sync::Arc;

use application::auth::AuthInteractor;
use application::error::UseCaseError;
use application::media::{MediaContent, MediaDto, MediaInteractor, MediaUsageDto, UploadMediaCmd};
use application::ports::SESSION_COOKIE;
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
use crate::http_support::{RequestId, admin_error, cookie_value, no_store};

/// 单次上传的请求体上限：与 `domain::media::MAX_IMAGE_BYTES`（10 MiB）留出余量。
pub const MEDIA_BODY_LIMIT: usize = 12 * 1024 * 1024;

// ---------------------------------------------------------------------------
// 响应 DTO
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
struct MediaJson {
    id: Uuid,
    original_name: String,
    mime: String,
    byte_size: i64,
    width: i32,
    height: i32,
    status: String,
    version: i64,
    created_at: String,
    owner_id: Uuid,
    owner_display: String,
    url: String,
    /// 全部引用数（含草稿/私密/回收站）；> 0 时删除会被拒绝。
    reference_count: i64,
    /// 构成公开来源的引用数；> 0 时匿名可读取。
    public_reference_count: i64,
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
            status: dto.status.to_string(),
            version: dto.version,
            created_at: application::public_site::format_datetime(dto.created_at),
            owner_id: dto.owner_id,
            owner_display: dto.owner_display.clone(),
            url: dto.url.clone(),
            reference_count: dto.reference_count,
            public_reference_count: dto.public_reference_count,
        }
    }
}

#[derive(serde::Serialize)]
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
            kind: dto.kind,
            content_id: dto.content_id,
            slug: dto.slug.clone(),
            title: dto.title.clone(),
            status: dto.status.clone(),
            visibility: dto.visibility.clone(),
            deleted: dto.deleted,
            public: dto.public,
        }
    }
}

#[derive(serde::Serialize)]
struct MediaPageJson {
    items: Vec<MediaJson>,
    total: i64,
    page: i64,
    per_page: i64,
}

#[derive(serde::Serialize)]
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
}

/// 上传查询参数：文件名只用于展示，缺省时按「未命名图片」处理。
#[derive(Deserialize, Default)]
struct UploadQuery {
    filename: Option<String>,
}

#[derive(Deserialize)]
struct DeleteMediaBody {
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
    match state.media.list(&auth.actor, query.page.unwrap_or(1)).await {
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

/// 单个资产详情与使用位置（删除前提示、删除被拒后定位引用）。
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
    body: Bytes,
) -> Response {
    let request_id = RequestId::generate();
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

/// 删除：未被任何内容引用时进入回收流程；仍被引用则 409 `media_in_use`。
async fn delete_media(
    State(state): State<AdminState>,
    Path(id): Path<String>,
    auth: AdminAuth,
    request_id: RequestId,
    Json(body): Json<DeleteMediaBody>,
) -> Response {
    let Ok(id) = Uuid::parse_str(&id) else {
        return admin_error(UseCaseError::NotFound("图片".into()), &request_id);
    };
    match state
        .media
        .delete(&auth.actor, id, body.expected_version)
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
    pub auth: Arc<AuthInteractor>,
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
    // 会话无效时按匿名处理：公开引用的图片对已失效的旧 Cookie 仍应可读。
    let viewer = match cookie_value(&headers, SESSION_COOKIE) {
        Some(token) => state.auth.actor_from_session(&token).await.ok(),
        None => None,
    };
    match state.media.read(id, viewer.as_ref()).await {
        Ok(content) => file_response(&content, &headers),
        Err(UseCaseError::NotFound(_)) => media_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "读取媒体文件失败");
            (StatusCode::INTERNAL_SERVER_ERROR, "服务器内部错误").into_response()
        }
    }
}

/// 公开引用的响应可重校验（撤回后下一次请求必须立刻失效）；后台预览不可缓存。
fn file_response(content: &MediaContent, request: &HeaderMap) -> Response {
    let etag = format!("\"{}\"", &content.checksum_sha256[..32]);
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
    if let Ok(value) = HeaderValue::from_str(&content.mime) {
        headers.insert(header::CONTENT_TYPE, value);
    }
    if let Ok(value) = HeaderValue::from_str(etag) {
        headers.insert(header::ETAG, value);
    }
    // 不写 Content-Disposition：浏览器按 MIME 内联展示图片本身。
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(if content.public_reference {
            "no-cache"
        } else {
            "no-store"
        }),
    );
}

fn media_not_found() -> Response {
    // 不存在、未就绪与无权读取共用同一响应：不泄漏资产存在性。
    (StatusCode::NOT_FOUND, "图片不存在或未公开").into_response()
}
