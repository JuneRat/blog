//! 管理 API：会话认证 + CSRF/Origin 的文章与页面写端点。
//!
//! - 全部响应 `Cache-Control: no-store`；请求体上限 2 MiB。
//! - 认证/CSRF/Origin 由 `AdminAuth` 提取器统一执行（共用实现见 `http_support`）：
//!   读方法仅需会话，写方法（POST/PATCH/PUT/DELETE）额外校验 `X-CSRF-Token`
//!   与同源 `Origin`。
//! - 错误契约统一为 JSON：401 带 `WWW-Authenticate: Session`，内部错误只回通用文案。
//! - 权限由应用层用例执行（文章 own/any；页面站点级）；本层不做业务判断。

use uuid::Uuid;

use application::category::{CategoryDto, CreateCategoryCmd, UpdateCategoryCmd};
use application::content::{CreatePostCmd, EditPostCmd, PostDto, PostVisibility};
use application::error::UseCaseError;
use application::identity::Actor;
use application::page::{CreatePageCmd, DeletePageCmd, EditPageCmd, PageDto};
use application::ports::SESSION_COOKIE;
use application::series::{CreateSeriesCmd, ReorderSeriesCmd, SeriesDto, UpdateSeriesCmd};
use application::settings::{SaveSiteSettingsCmd, SaveThemeSettingsCmd, SiteSettingsView};
use application::tag::{CreateTagCmd, TagDto};
use axum::extract::{FromRef, Path, Query, State};
use axum::http::{StatusCode, request::Parts};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

use crate::http_auth::AdminState;
use crate::http_support::{RequestId, admin_error, cookie_value, ensure_same_origin, no_store};

/// 请求体上限（Markdown 正文足够）。
pub const ADMIN_BODY_LIMIT: usize = 2 * 1024 * 1024;

/// 站点设置请求体上限：标题 + 描述远小于 1 KiB，超限输入在解析前就拒绝。
const SETTINGS_BODY_LIMIT: usize = 16 * 1024;

// ---------------------------------------------------------------------------
// 响应 DTO（时间统一格式化为字符串）
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
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
    series_id: Option<Uuid>,
    series_order: Option<i32>,
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
            published_at: dto
                .published_at
                .map(application::public_site::format_datetime),
            updated_at: application::public_site::format_datetime(dto.updated_at),
            author_id: dto.author_id,
            tag_ids: dto.tag_ids.clone(),
            category_id: dto.category_id,
            series_id: dto.series_id,
            series_order: dto.series_order,
        }
    }
}

/// 单篇详情：在摘要之上附 Markdown 源文与摘要（后台编辑需要）。
/// 列表接口保持摘要形态，避免把全部正文塞进列表响应。
#[derive(serde::Serialize)]
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

// ---------------------------------------------------------------------------
// 认证 + CSRF 提取器
// ---------------------------------------------------------------------------

/// 已认证的管理调用（含 CSRF/Origin 校验结果）。
pub struct AdminAuth {
    pub actor: Actor,
}

impl<S> axum::extract::FromRequestParts<S> for AdminAuth
where
    S: Send + Sync,
    AdminState: axum::extract::FromRef<S>,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let admin = AdminState::from_ref(state);
        // 提前拒绝（未登录/CSRF/跨源）也必须带上请求编号，故先从扩展取上下文。
        let request_id = parts
            .extensions
            .get::<RequestId>()
            .cloned()
            .unwrap_or_else(RequestId::generate);
        let Some(token) = cookie_value(&parts.headers, SESSION_COOKIE) else {
            return Err(admin_error(UseCaseError::Unauthenticated, &request_id));
        };
        let record = admin
            .auth
            .session_record(&token)
            .await
            .map_err(|e| admin_error(e, &request_id))?;

        // 写方法校验 CSRF + Origin（读方法不产生副作用）。
        let write_method = !matches!(parts.method.as_str(), "GET" | "HEAD" | "OPTIONS");
        if write_method {
            ensure_same_origin(&parts.headers).map_err(|e| admin_error(e, &request_id))?;
            let provided = parts
                .headers
                .get("x-csrf-token")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default();
            if provided.is_empty() || provided != record.csrf_token {
                return Err(admin_error(
                    UseCaseError::Invalid("CSRF 校验失败".into()),
                    &request_id,
                ));
            }
        }

        let actor = admin
            .auth
            .actor_from_session(&token)
            .await
            .map_err(|e| admin_error(e, &request_id))?;
        // 身份已由会话验证：只有这里可以补录 actor，完成日志才带上它。
        request_id.set_actor(actor.user_id.0);
        Ok(Self { actor })
    }
}

/// 三态字段的反序列化：缺失 → None（不修改）；JSON null → Some(None)（清空）；
/// 值 → Some(Some(v))。serde 对 Option<Option<T>> 会把 null 折叠成 None，
/// 必须显式包一层才能区分「清空」与「不触碰」。
pub fn deserialize_double_option<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: serde::Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Ok(Some(Option::<T>::deserialize(deserializer)?))
}

// ---------------------------------------------------------------------------
// 请求体
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
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
    pub tag_ids: Vec<Uuid>,
    /// 初始分类 id（存在性由用例校验）。
    #[serde(default)]
    pub category_id: Option<Uuid>,
    /// 初始系列与序号。
    #[serde(default)]
    pub series: Option<SeriesBody>,
}

#[derive(Deserialize, Default)]
pub struct EditPostBody {
    pub new_slug: Option<String>,
    pub title: Option<String>,
    pub excerpt: Option<String>,
    pub content: Option<String>,
    pub visibility: Option<String>,
    /// Some 表示整体替换标签集合（[] = 清空）；缺省不触碰。
    pub tag_ids: Option<Vec<Uuid>>,
    /// 三态：缺省不修改；null 清空分类；id 设置分类。
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub category_id: Option<Option<Uuid>>,
    /// 三态：缺省不修改；null 退出系列；对象设置系列与序号。
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub series: Option<Option<SeriesBody>>,
    pub expected_version: Option<i64>,
}

/// 文章的系列归属载荷（同事务保存）。
#[derive(Deserialize, Clone, Copy)]
pub struct SeriesBody {
    pub id: Uuid,
    pub order: i32,
}

#[derive(Deserialize, Default)]
pub struct VersionBody {
    pub expected_version: Option<i64>,
}

#[derive(Deserialize, Default)]
pub struct ListQuery {
    pub author: Option<String>,
    pub page: Option<i64>,
}

// ---------------------------------------------------------------------------
// 路由
// ---------------------------------------------------------------------------

pub fn posts_router(state: AdminState) -> Router {
    Router::new()
        .route("/api/admin/v1/posts", get(list_posts).post(create_post))
        .route("/api/admin/v1/post-trash", get(list_trash))
        .route("/api/admin/v1/posts/{slug}", get(get_post).patch(edit_post))
        .route("/api/admin/v1/posts/{slug}/trash", post(trash_post))
        .route("/api/admin/v1/posts/{slug}/restore", post(restore_post))
        .route("/api/admin/v1/posts/{slug}/purge", post(purge_post))
        .route("/api/admin/v1/posts/{slug}/publish", post(publish_post))
        .route("/api/admin/v1/posts/{slug}/unpublish", post(unpublish_post))
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
                series: body.series.map(|s| (s.id, s.order)),
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
    Path(slug): Path<String>,
) -> Response {
    match state.posts.find(&actor, &slug).await {
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
    // 先授权再解析用户名：否则任何已登录用户可用 403/404 差异探测用户是否存在。
    let author_id = match query.author.as_deref() {
        Some(username) if !username.is_empty() => {
            if !actor.has_permission("post.read_any") {
                return admin_error(UseCaseError::Forbidden, &request_id);
            }
            match state.users.actor_for_username(username).await {
                Ok(who) => who.user_id,
                Err(e) => return admin_error(e, &request_id),
            }
        }
        _ => actor.user_id,
    };
    match state.posts.list_by_author(&actor, author_id).await {
        Ok(list) => (
            StatusCode::OK,
            Json(list.iter().map(PostJson::from).collect::<Vec<_>>()),
        )
            .into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

async fn list_trash(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Query(query): Query<ListQuery>,
) -> Response {
    let author_id = match query.author.as_deref() {
        Some(username) if !username.is_empty() => {
            if !actor.has_permission("post.read_any") {
                return admin_error(UseCaseError::Forbidden, &request_id);
            }
            match state.users.actor_for_username(username).await {
                Ok(who) => who.user_id,
                Err(e) => return admin_error(e, &request_id),
            }
        }
        _ => actor.user_id,
    };
    match state
        .posts
        .list_trash(&actor, author_id, query.page.unwrap_or(1))
        .await
    {
        Ok(page) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "items": page.items.iter().map(PostJson::from).collect::<Vec<_>>(),
                "total": page.total, "page": page.page, "per_page": page.per_page
            })),
        )
            .into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

async fn trash_post(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    body: Option<Json<VersionBody>>,
) -> Response {
    match state
        .posts
        .trash(&actor, &slug, body.and_then(|Json(b)| b.expected_version))
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
    Path(slug): Path<String>,
    body: Option<Json<VersionBody>>,
) -> Response {
    match state
        .posts
        .restore(&actor, &slug, body.and_then(|Json(b)| b.expected_version))
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
    Path(slug): Path<String>,
    body: Option<Json<VersionBody>>,
) -> Response {
    match state
        .posts
        .purge(&actor, &slug, body.and_then(|Json(b)| b.expected_version))
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
    Path(slug): Path<String>,
    Json(body): Json<EditPostBody>,
) -> Response {
    let visibility = match body.visibility.as_deref() {
        Some(v) => match parse_visibility(Some(v)) {
            Ok(parsed) => Some(parsed),
            Err(e) => return admin_error(e, &request_id),
        },
        None => None,
    };
    match state
        .posts
        .edit(
            &actor,
            EditPostCmd {
                target_slug: slug,
                new_slug: body.new_slug,
                title: body.title,
                excerpt: body.excerpt,
                content: body.content,
                visibility,
                tag_ids: body.tag_ids,
                category_id: body.category_id,
                series: body.series.map(|opt| opt.map(|s| (s.id, s.order))),
                expected_version: body.expected_version,
            },
        )
        .await
    {
        Ok(dto) => (StatusCode::OK, Json(PostDetailJson::from(dto))).into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

async fn publish_post(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    body: Option<Json<VersionBody>>,
) -> Response {
    let expected = body.and_then(|Json(b)| b.expected_version);
    match state.posts.publish(&actor, &slug, expected).await {
        Ok(dto) => (StatusCode::OK, Json(PostDetailJson::from(dto))).into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

async fn unpublish_post(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    body: Option<Json<VersionBody>>,
) -> Response {
    let expected = body.and_then(|Json(b)| b.expected_version);
    match state.posts.withdraw(&actor, &slug, expected).await {
        Ok(dto) => (StatusCode::OK, Json(PostDetailJson::from(dto))).into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

// ---------------------------------------------------------------------------
// 辅助
// ---------------------------------------------------------------------------

fn parse_visibility(value: Option<&str>) -> Result<PostVisibility, UseCaseError> {
    match value {
        None => Ok(PostVisibility::Public),
        Some("public") => Ok(PostVisibility::Public),
        Some("private") => Ok(PostVisibility::Private),
        Some(other) => Err(UseCaseError::Invalid(format!(
            "visibility 只支持 public/private，收到 {other}"
        ))),
    }
}

// ---------------------------------------------------------------------------
// 页面（Page）：站点级 page.* 权限，无作者归属
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
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
            published_at: dto
                .published_at
                .map(application::public_site::format_datetime),
            updated_at: application::public_site::format_datetime(dto.updated_at),
        }
    }
}

/// 页面详情：摘要 + Markdown 源文（后台编辑数据源）。
#[derive(serde::Serialize)]
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

#[derive(Deserialize)]
pub struct CreatePageBody {
    pub slug: Option<String>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub content: String,
    pub visibility: Option<String>,
}

#[derive(Deserialize, Default)]
pub struct EditPageBody {
    pub new_slug: Option<String>,
    pub title: Option<String>,
    pub content: Option<String>,
    pub visibility: Option<String>,
    pub expected_version: Option<i64>,
}

#[derive(Deserialize)]
pub struct DeletePageBody {
    pub expected_id: Uuid,
    pub expected_version: i64,
}

pub fn pages_router(state: AdminState) -> Router {
    Router::new()
        .route("/api/admin/v1/pages", get(list_pages).post(create_page))
        .route(
            "/api/admin/v1/pages/{slug}",
            get(get_page).patch(edit_page).delete(delete_page),
        )
        .route("/api/admin/v1/pages/{slug}/publish", post(publish_page))
        .route("/api/admin/v1/pages/{slug}/unpublish", post(unpublish_page))
        .layer(axum::extract::DefaultBodyLimit::max(ADMIN_BODY_LIMIT))
        .layer(middleware::from_fn(no_store))
        .with_state(state)
}

async fn delete_page(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    Json(body): Json<DeletePageBody>,
) -> Response {
    match state
        .pages
        .delete(
            &actor,
            DeletePageCmd {
                slug,
                expected_id: body.expected_id,
                expected_version: body.expected_version,
            },
        )
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => admin_error(e, &request_id),
    }
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
    Path(slug): Path<String>,
) -> Response {
    match state.pages.find(&actor, &slug).await {
        Ok(dto) => (StatusCode::OK, Json(PageDetailJson::from(dto))).into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

async fn list_pages(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
) -> Response {
    match state.pages.list(&actor).await {
        Ok(list) => (
            StatusCode::OK,
            Json(list.iter().map(PageJson::from).collect::<Vec<_>>()),
        )
            .into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

async fn edit_page(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    Json(body): Json<EditPageBody>,
) -> Response {
    let visibility = match body.visibility.as_deref() {
        Some(v) => match parse_visibility(Some(v)) {
            Ok(parsed) => Some(parsed),
            Err(e) => return admin_error(e, &request_id),
        },
        None => None,
    };
    match state
        .pages
        .edit(
            &actor,
            EditPageCmd {
                target_slug: slug,
                new_slug: body.new_slug,
                title: body.title,
                content: body.content,
                visibility,
                expected_version: body.expected_version,
            },
        )
        .await
    {
        Ok(dto) => (StatusCode::OK, Json(PageDetailJson::from(dto))).into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

async fn publish_page(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    body: Option<Json<VersionBody>>,
) -> Response {
    let expected = body.and_then(|Json(b)| b.expected_version);
    match state.pages.publish(&actor, &slug, expected).await {
        Ok(dto) => (StatusCode::OK, Json(PageDetailJson::from(dto))).into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

async fn unpublish_page(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    body: Option<Json<VersionBody>>,
) -> Response {
    let expected = body.and_then(|Json(b)| b.expected_version);
    match state.pages.withdraw(&actor, &slug, expected).await {
        Ok(dto) => (StatusCode::OK, Json(PageDetailJson::from(dto))).into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

// ---------------------------------------------------------------------------
// 标签目录：管理（tag.manage）+ 目录读取（编辑器选择器共用）
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// 分类目录：管理（category.manage）+ 目录读取（编辑器选择器共用）
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// 系列目录：管理（series.manage）+ 重排 + 目录读取
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
struct SeriesJson {
    id: Uuid,
    slug: String,
    name: String,
    description: Option<String>,
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
            version: dto.version,
            post_count: dto.post_count,
            pub_post_count: dto.public_post_count,
        }
    }
}

#[derive(Deserialize)]
pub struct CreateSeriesBody {
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
}

#[derive(Deserialize, Default)]
pub struct UpdateSeriesBody {
    pub name: String,
    pub description: Option<String>,
    pub expected_version: Option<i64>,
}

#[derive(Deserialize)]
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
#[derive(serde::Serialize)]
struct SeriesMemberJson {
    id: Uuid,
    slug: String,
    title: String,
    status: String,
    deleted: bool,
    visibility: String,
    author_id: Uuid,
    series_order: i32,
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
                        series_order: m.series_order,
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
        Ok(dto) => (StatusCode::OK, Json(dto)).into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

// ---------------------------------------------------------------------------
// 站点设置：settings.manage；只注册 site 分组
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
struct SiteSettingsJson {
    title: String,
    description: String,
    /// "database"（settings.site 行）或 "fallback"（环境变量/默认值，version=0）。
    source: &'static str,
    version: i64,
}

impl From<&SiteSettingsView> for SiteSettingsJson {
    fn from(view: &SiteSettingsView) -> Self {
        Self {
            title: view.title.clone(),
            description: view.description.clone(),
            source: match view.source {
                application::settings::SiteSettingsSource::Database => "database",
                application::settings::SiteSettingsSource::Fallback => "fallback",
            },
            version: view.version,
        }
    }
}

#[derive(Deserialize, Default)]
pub struct SaveSiteSettingsBody {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub expected_version: Option<i64>,
}

/// 站点设置路由：site 与 theme 分组。oauth 等受保护分组
/// 使用专用权限（oauth.manage）与独立入口，不暴露在本 API 面上，
/// 未知分组（含猜测 `/settings/oauth`）由路由层直接 404。
pub fn settings_router(state: AdminState) -> Router {
    Router::new()
        .route(
            "/api/admin/v1/settings/site",
            get(get_site_settings).put(put_site_settings),
        )
        .route(
            "/api/admin/v1/settings/theme",
            get(get_theme_settings).put(put_theme_settings),
        )
        .layer(axum::extract::DefaultBodyLimit::max(SETTINGS_BODY_LIMIT))
        .layer(middleware::from_fn(no_store))
        .with_state(state)
}

#[derive(Deserialize)]
pub struct SaveThemeSettingsBody {
    pub slug: String,
    pub expected_version: Option<i64>,
}

async fn get_theme_settings(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
) -> Response {
    match state.settings.theme_view(&actor).await {
        Ok(view) => (StatusCode::OK, Json(view)).into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

async fn put_theme_settings(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Json(body): Json<SaveThemeSettingsBody>,
) -> Response {
    match state
        .settings
        .save_theme(
            &actor,
            SaveThemeSettingsCmd {
                slug: body.slug,
                expected_version: body.expected_version,
            },
        )
        .await
    {
        Ok(view) => (StatusCode::OK, Json(view)).into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

async fn get_site_settings(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
) -> Response {
    match state.settings.site_view(&actor).await {
        Ok(view) => (StatusCode::OK, Json(SiteSettingsJson::from(&view))).into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

/// 全量替换 site 分组（title/description 必填；PUT 语义）。
/// 读写都要求 `settings.manage`；expected_version 过期是 409 version_conflict。
async fn put_site_settings(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Json(body): Json<SaveSiteSettingsBody>,
) -> Response {
    match state
        .settings
        .save_site(
            &actor,
            SaveSiteSettingsCmd {
                title: body.title,
                description: body.description,
                expected_version: body.expected_version,
            },
        )
        .await
    {
        Ok(view) => (StatusCode::OK, Json(SiteSettingsJson::from(&view))).into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}
