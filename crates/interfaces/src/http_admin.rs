//! 管理 API：会话认证 + CSRF 的文章写端点。
//!
//! - 全部响应 `Cache-Control: no-store`；请求体上限 2 MiB。
//! - 认证/CSRF 由 `AdminAuth` 提取器统一执行：读方法仅需会话，
//!   写方法（POST/PATCH/PUT/DELETE）额外校验 `X-CSRF-Token`。
//! - 权限由应用层用例执行（own/any）；本层不做业务判断。

use uuid::Uuid;

use application::content::{CreatePostCmd, EditPostCmd, PostDto, PostVisibility};
use application::error::UseCaseError;
use application::identity::Actor;
use axum::extract::{FromRef, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, request::Parts};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

use crate::http_auth::AdminState;

/// 请求体上限（Markdown 正文足够）。
pub const ADMIN_BODY_LIMIT: usize = 2 * 1024 * 1024;

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
}

impl From<PostDto> for PostJson {
    fn from(dto: PostDto) -> Self {
        Self {
            id: dto.id,
            slug: dto.slug,
            title: dto.title,
            status: dto.status.to_string(),
            visibility: dto.visibility.to_string(),
            version: dto.version,
            published_at: dto
                .published_at
                .map(application::public_site::format_datetime),
            updated_at: application::public_site::format_datetime(dto.updated_at),
            author_id: dto.author_id,
        }
    }
}

// ---------------------------------------------------------------------------
// 认证 + CSRF 提取器
// ---------------------------------------------------------------------------

/// 已认证的管理调用（含 CSRF 校验结果）。
pub struct AdminAuth {
    pub actor: Actor,
}

impl AdminAuth {
    fn token_from_headers(headers: &HeaderMap) -> Option<String> {
        let cookie = headers.get(axum::http::header::COOKIE)?.to_str().ok()?;
        for pair in cookie.split(';') {
            let pair = pair.trim();
            if let Some(value) = pair.strip_prefix(application::ports::SESSION_COOKIE) {
                let value = value.strip_prefix('=')?;
                if !value.is_empty() {
                    return Some(value.to_string());
                }
            }
        }
        None
    }
}

impl<S> axum::extract::FromRequestParts<S> for AdminAuth
where
    S: Send + Sync,
    AdminState: axum::extract::FromRef<S>,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let admin = AdminState::from_ref(state);
        let Some(token) = Self::token_from_headers(&parts.headers) else {
            return Err(admin_error(UseCaseError::Unauthenticated));
        };
        let record = admin
            .auth
            .session_record(&token)
            .await
            .map_err(admin_error)?;

        // 写方法校验 CSRF（读方法不产生副作用）。
        let write_method = !matches!(parts.method.as_str(), "GET" | "HEAD" | "OPTIONS");
        if write_method {
            let provided = parts
                .headers
                .get("x-csrf-token")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default();
            if provided.is_empty() || provided != record.csrf_token {
                return Err(admin_error(UseCaseError::Invalid("CSRF 校验失败".into())));
            }
        }

        let actor = admin
            .auth
            .actor_from_session(&token)
            .await
            .map_err(admin_error)?;
        Ok(Self { actor })
    }
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
}

#[derive(Deserialize, Default)]
pub struct EditPostBody {
    pub new_slug: Option<String>,
    pub title: Option<String>,
    pub excerpt: Option<String>,
    pub content: Option<String>,
    pub visibility: Option<String>,
    pub expected_version: Option<i64>,
}

#[derive(Deserialize, Default)]
pub struct VersionBody {
    pub expected_version: Option<i64>,
}

#[derive(Deserialize, Default)]
pub struct ListQuery {
    pub author: Option<String>,
}

// ---------------------------------------------------------------------------
// 路由
// ---------------------------------------------------------------------------

pub fn posts_router(state: AdminState) -> Router {
    Router::new()
        .route("/api/admin/v1/posts", get(list_posts).post(create_post))
        .route("/api/admin/v1/posts/{slug}", get(get_post).patch(edit_post))
        .route("/api/admin/v1/posts/{slug}/publish", post(publish_post))
        .route("/api/admin/v1/posts/{slug}/unpublish", post(unpublish_post))
        .layer(axum::extract::DefaultBodyLimit::max(ADMIN_BODY_LIMIT))
        .layer(middleware::from_fn(no_store))
        .with_state(state)
}

async fn no_store(req: axum::extract::Request, next: Next) -> Response {
    let mut response = next.run(req).await;
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}

// ---------------------------------------------------------------------------
// 处理器
// ---------------------------------------------------------------------------

async fn create_post(
    AdminAuth { actor }: AdminAuth,
    State(state): State<AdminState>,
    Json(body): Json<CreatePostBody>,
) -> Response {
    let visibility = match parse_visibility(body.visibility.as_deref()) {
        Ok(v) => v,
        Err(e) => return admin_error(e),
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
            },
        )
        .await
    {
        Ok(dto) => (StatusCode::CREATED, Json(PostJson::from(dto))).into_response(),
        Err(e) => admin_error(e),
    }
}

async fn get_post(
    AdminAuth { actor }: AdminAuth,
    State(state): State<AdminState>,
    Path(slug): Path<String>,
) -> Response {
    match state.posts.find(&actor, &slug).await {
        Ok(dto) => (StatusCode::OK, Json(PostJson::from(dto))).into_response(),
        Err(e) => admin_error(e),
    }
}

async fn list_posts(
    AdminAuth { actor }: AdminAuth,
    State(state): State<AdminState>,
    Query(query): Query<ListQuery>,
) -> Response {
    let author_id = match query.author.as_deref() {
        Some(username) if !username.is_empty() => {
            match state.users.actor_for_username(username).await {
                Ok(who) => who.user_id,
                Err(e) => return admin_error(e),
            }
        }
        _ => actor.user_id,
    };
    match state.posts.list_by_author(&actor, author_id).await {
        Ok(list) => (
            StatusCode::OK,
            Json(list.into_iter().map(PostJson::from).collect::<Vec<_>>()),
        )
            .into_response(),
        Err(e) => admin_error(e),
    }
}

async fn edit_post(
    AdminAuth { actor }: AdminAuth,
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    Json(body): Json<EditPostBody>,
) -> Response {
    let visibility = match body.visibility.as_deref() {
        Some(v) => match parse_visibility(Some(v)) {
            Ok(parsed) => Some(parsed),
            Err(e) => return admin_error(e),
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
                expected_version: body.expected_version,
            },
        )
        .await
    {
        Ok(dto) => (StatusCode::OK, Json(PostJson::from(dto))).into_response(),
        Err(e) => admin_error(e),
    }
}

async fn publish_post(
    AdminAuth { actor }: AdminAuth,
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    body: Option<Json<VersionBody>>,
) -> Response {
    let expected = body.and_then(|Json(b)| b.expected_version);
    match state.posts.publish(&actor, &slug, expected).await {
        Ok(dto) => (StatusCode::OK, Json(PostJson::from(dto))).into_response(),
        Err(e) => admin_error(e),
    }
}

async fn unpublish_post(
    AdminAuth { actor }: AdminAuth,
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    body: Option<Json<VersionBody>>,
) -> Response {
    let expected = body.and_then(|Json(b)| b.expected_version);
    match state.posts.withdraw(&actor, &slug, expected).await {
        Ok(dto) => (StatusCode::OK, Json(PostJson::from(dto))).into_response(),
        Err(e) => admin_error(e),
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

fn admin_error(e: UseCaseError) -> Response {
    let status = match e {
        UseCaseError::Unauthenticated => StatusCode::UNAUTHORIZED,
        UseCaseError::Invalid(_) => StatusCode::BAD_REQUEST,
        UseCaseError::Conflict(_) => StatusCode::CONFLICT,
        UseCaseError::VersionConflict => StatusCode::CONFLICT,
        UseCaseError::NotFound(_) => StatusCode::NOT_FOUND,
        UseCaseError::Forbidden => StatusCode::FORBIDDEN,
        UseCaseError::External(_) => StatusCode::BAD_GATEWAY,
        UseCaseError::Repository(_) | UseCaseError::Render(_) => {
            tracing::error!(error = %e, "管理 API 内部错误");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    // 401 必须带 WWW-Authenticate 指示会话入口。
    let mut response =
        (status, Json(serde_json::json!({ "error": e.to_string() }))).into_response();
    if status == StatusCode::UNAUTHORIZED {
        response.headers_mut().insert(
            axum::http::header::WWW_AUTHENTICATE,
            axum::http::HeaderValue::from_static("Session"),
        );
    }
    response
}
