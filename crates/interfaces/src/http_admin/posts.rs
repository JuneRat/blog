//! 文章管理路由与传输 DTO；业务授权由应用用例执行。

use super::support::{
    ListQuery, ScheduleBody, VersionBody, api_datetime, double_option, parse_visibility,
};
use super::{ADMIN_BODY_LIMIT, AdminAuth};
use crate::http_auth::AdminState;
use crate::http_contract::{ContentPage, SeriesPlacement};
use crate::http_support::{RequestId, admin_error, no_store};
use application::content::{CreatePostCmd, EditPostCmd, PostDto};
use application::content_queries::AdminPostSummary;
use application::error::UseCaseError;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router, middleware};
use serde::Deserialize;
use uuid::Uuid;

#[derive(serde::Serialize, ts_rs::TS)]
#[ts(rename = "PostMetadata")]
struct PostJson {
    id: Uuid,
    slug: String,
    title: String,
    status: String,
    visibility: String,
    version: i64,
    published_at: Option<String>,
    updated_at: String,
    author_id: Uuid,
    /// 当前关联标签 id（按 id 升序）；名称由前端结合标签目录解析。
    tag_ids: Vec<Uuid>,
    /// 所属分类 id（None = 未分类）。
    category_id: Option<Uuid>,
    series: Vec<SeriesPlacement>,
    /// 封面媒体资产 id（None = 无封面）。
    cover_media_id: Option<Uuid>,
    /// 封面站内地址（`/media/{id}`）；None = 无封面。
    cover_url: Option<String>,
}

impl From<&PostDto> for PostJson {
    fn from(dto: &PostDto) -> Self {
        Self {
            id: dto.id,
            slug: dto.slug.clone(),
            title: dto.title.clone(),
            status: dto.status.to_string(),
            visibility: dto.visibility.to_string(),
            version: dto.version,
            published_at: dto.published_at.map(api_datetime),
            updated_at: api_datetime(dto.updated_at),
            author_id: dto.author_id,
            tag_ids: dto.tag_ids.clone(),
            category_id: dto.category_id,
            series: dto.series.clone().into_iter().map(Into::into).collect(),
            cover_media_id: dto.cover_media_id,
            cover_url: dto.cover_media_id.map(application::media::media_url),
        }
    }
}

/// 单篇详情：在摘要之上附 Markdown 源文与摘要（后台编辑需要）。
/// 列表通过独立的 PostListJson 输出展示字段。
#[derive(serde::Serialize, ts_rs::TS)]
#[ts(rename = "PostDetail")]
struct PostDetailJson {
    #[serde(flatten)]
    summary: PostJson,
    excerpt: Option<String>,
    content: String,
}

impl From<PostDto> for PostDetailJson {
    fn from(dto: PostDto) -> Self {
        Self {
            summary: PostJson::from(&dto),
            excerpt: dto.excerpt,
            content: dto.content,
        }
    }
}

#[derive(serde::Serialize, ts_rs::TS)]
#[ts(rename = "PostSummary")]
struct PostListJson {
    author_username: String,
    id: Uuid,
    slug: String,
    title: String,
    status: String,
    visibility: String,
    version: i64,
    published_at: Option<String>,
    updated_at: String,
    author_id: Uuid,
}
impl From<AdminPostSummary> for PostListJson {
    fn from(dto: AdminPostSummary) -> Self {
        Self {
            author_username: dto.author_username,
            id: dto.id,
            slug: dto.slug,
            title: dto.title,
            status: dto.status.as_str().to_owned(),
            visibility: dto.visibility.as_str().to_owned(),
            version: dto.version,
            published_at: dto.published_at.map(api_datetime),
            updated_at: api_datetime(dto.updated_at),
            author_id: dto.author_id,
        }
    }
}

// ---------------------------------------------------------------------------
// 请求体
// ---------------------------------------------------------------------------

#[derive(Deserialize, ts_rs::TS)]
#[ts(rename = "CreatePostInput", optional_fields = nullable)]
pub struct CreatePostBody {
    pub slug: Option<String>,
    #[serde(default)]
    pub title: String,
    pub excerpt: Option<String>,
    #[serde(default)]
    pub content: String,
    pub visibility: Option<String>,
    /// 初始标签 id 集合（去重与存在性由用例处理）。
    #[serde(default)]
    #[ts(as = "Option<Vec<Uuid>>", optional)]
    pub tag_ids: Vec<Uuid>,
    /// 初始分类 id（存在性由用例校验）。
    #[serde(default)]
    pub category_id: Option<Uuid>,
    /// 初始系列与序号。
    #[serde(default)]
    #[ts(as = "Option<Vec<SeriesPlacement>>", optional)]
    pub series: Vec<SeriesPlacement>,
    /// 初始封面媒体资产 id（缺省 = 无封面）。
    #[serde(default)]
    pub cover_media_id: Option<Uuid>,
}

#[derive(Deserialize, Default, ts_rs::TS)]
#[ts(rename = "EditPostInput", optional_fields = nullable)]
pub struct EditPostBody {
    pub new_slug: Option<String>,
    pub title: Option<String>,
    pub excerpt: Option<String>,
    pub content: Option<String>,
    pub visibility: Option<String>,
    /// Some 表示整体替换标签集合（[] = 清空）；缺省不触碰。
    pub tag_ids: Option<Vec<Uuid>>,
    /// 三态：缺省不修改；null 清空分类；id 设置分类。
    #[serde(default, with = "double_option")]
    #[ts(as = "Option<Uuid>", optional = nullable)]
    pub category_id: Option<Option<Uuid>>,
    /// 缺省保留，数组整体替换；空数组清空。
    pub series: Option<Vec<SeriesPlacement>>,
    /// 封面三态：缺省不修改；null 移除封面；id 设置封面。
    #[serde(default, with = "double_option")]
    #[ts(as = "Option<Uuid>", optional = nullable)]
    pub cover_media_id: Option<Option<Uuid>>,
    pub expected_version: Option<i64>,
}

pub fn posts_router(state: AdminState) -> Router {
    Router::new()
        .route("/api/admin/v1/posts", get(list_posts).post(create_post))
        .route("/api/admin/v1/post-trash", get(list_trash))
        .route("/api/admin/v1/posts/{id}", get(get_post).patch(edit_post))
        .route("/api/admin/v1/posts/{id}/trash", post(trash_post))
        .route("/api/admin/v1/posts/{id}/restore", post(restore_post))
        .route("/api/admin/v1/posts/{id}/purge", post(purge_post))
        .route("/api/admin/v1/posts/{id}/publish", post(publish_post))
        .route("/api/admin/v1/posts/{id}/unpublish", post(unpublish_post))
        .route("/api/admin/v1/posts/{id}/schedule", post(schedule_post))
        .route("/api/admin/v1/posts/{id}/archive", post(archive_post))
        .layer(axum::extract::DefaultBodyLimit::max(ADMIN_BODY_LIMIT))
        .layer(middleware::from_fn(no_store))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// 处理器
// ---------------------------------------------------------------------------

async fn create_post(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Json(body): Json<CreatePostBody>,
) -> Response {
    let visibility = match parse_visibility(body.visibility.as_deref()) {
        Ok(v) => v,
        Err(e) => return admin_error(e, &request_id),
    };
    match state
        .posts
        .create(
            &actor,
            CreatePostCmd {
                slug: body.slug,
                title: body.title,
                excerpt: body.excerpt,
                content: body.content,
                visibility,
                tag_ids: body.tag_ids,
                category_id: body.category_id,
                series: body.series.into_iter().map(Into::into).collect(),
                cover_media_id: body.cover_media_id,
            },
        )
        .await
    {
        Ok(dto) => (StatusCode::CREATED, Json(PostDetailJson::from(dto))).into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

async fn get_post(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
) -> Response {
    match state.posts.find(&actor, id).await {
        Ok(dto) => (StatusCode::OK, Json(PostDetailJson::from(dto))).into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

async fn list_posts(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Query(query): Query<ListQuery>,
) -> Response {
    let author = query.author.clone();
    match state
        .content_queries
        .posts_by_author(&actor, author.as_deref(), query.request(false))
        .await
    {
        Ok(page) => Json(ContentPage::from(page.map(PostListJson::from))).into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

async fn list_trash(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Query(query): Query<ListQuery>,
) -> Response {
    let author = query.author.clone();
    match state
        .content_queries
        .posts_by_author(&actor, author.as_deref(), query.request(true))
        .await
    {
        Ok(page) => Json(ContentPage::from(page.map(PostListJson::from))).into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

async fn trash_post(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    body: Option<Json<VersionBody>>,
) -> Response {
    match state
        .posts
        .trash(&actor, id, body.and_then(|Json(b)| b.expected_version))
        .await
    {
        Ok(dto) => (StatusCode::OK, Json(PostDetailJson::from(dto))).into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

async fn restore_post(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    body: Option<Json<VersionBody>>,
) -> Response {
    match state
        .posts
        .restore(&actor, id, body.and_then(|Json(b)| b.expected_version))
        .await
    {
        Ok(dto) => (StatusCode::OK, Json(PostDetailJson::from(dto))).into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

async fn purge_post(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    body: Option<Json<VersionBody>>,
) -> Response {
    match state
        .posts
        .purge(&actor, id, body.and_then(|Json(b)| b.expected_version))
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

async fn edit_post(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    Json(body): Json<EditPostBody>,
) -> Response {
    let visibility = match body.visibility.as_deref() {
        Some(v) => match parse_visibility(Some(v)) {
            Ok(parsed) => Some(parsed),
            Err(e) => return admin_error(e, &request_id),
        },
        None => None,
    };
    let cmd = EditPostCmd {
        id,
        new_slug: body.new_slug,
        title: body.title,
        excerpt: body.excerpt,
        content: body.content,
        visibility,
        tag_ids: body.tag_ids,
        category_id: body.category_id,
        series: body
            .series
            .map(|items| items.into_iter().map(Into::into).collect()),
        cover_media_id: body.cover_media_id,
        expected_version: body.expected_version,
    };
    let result = state.posts.edit(&actor, cmd).await;
    match result {
        Ok(dto) => (StatusCode::OK, Json(PostDetailJson::from(dto))).into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

async fn publish_post(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    body: Option<Json<VersionBody>>,
) -> Response {
    let expected = body.and_then(|Json(b)| b.expected_version);
    match state.posts.publish(&actor, id, expected).await {
        Ok(dto) => (StatusCode::OK, Json(PostDetailJson::from(dto))).into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

async fn unpublish_post(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    body: Option<Json<VersionBody>>,
) -> Response {
    let expected = body.and_then(|Json(b)| b.expected_version);
    match state.posts.withdraw(&actor, id, expected).await {
        Ok(dto) => (StatusCode::OK, Json(PostDetailJson::from(dto))).into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}
async fn schedule_post(
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
        .posts
        .schedule(&actor, id, at, body.expected_version)
        .await
    {
        Ok(dto) => (StatusCode::OK, Json(PostDetailJson::from(dto))).into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}
async fn archive_post(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    body: Option<Json<VersionBody>>,
) -> Response {
    match state
        .posts
        .archive(&actor, id, body.and_then(|Json(b)| b.expected_version))
        .await
    {
        Ok(dto) => (StatusCode::OK, Json(PostDetailJson::from(dto))).into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

pub(crate) fn export_contract(out: &mut Vec<String>) {
    crate::http_contract::declare::<PostJson>(out);
    crate::http_contract::declare::<PostDetailJson>(out);
    crate::http_contract::declare::<PostListJson>(out);
    crate::http_contract::declare::<CreatePostBody>(out);
    crate::http_contract::declare::<EditPostBody>(out);
}
