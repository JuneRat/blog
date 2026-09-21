//! 认证与管理 API 入站路由。
//!
//! - GET /auth/login?provider=&next= ：发起 OAuth，302 到提供商。
//! - GET /auth/callback/{provider} ：消费 state、签发会话 cookie、303 回跳。
//! - POST /auth/logout ：受保护写（会话 + CSRF 头），撤销会话。
//! - GET /api/admin/v1/me ：会话认证的当前用户信息（含 CSRF token 供 SPA 使用）。
//!
//! Cookie：不透明高熵令牌，HttpOnly + SameSite=Lax（HTTPS 部署加 Secure）；
//! 登录另发短命 `blog_oauth_state`（Secure 部署用 `__Host-` 前缀）绑定浏览器。
//! 会话状态存服务端内存，每次请求重新读取用户与权限。

use std::sync::Arc;

use application::auth::{ATTEMPT_TTL_SECS, AuthInteractor, SESSION_COOKIE_NAME};
use application::content::PostInteractor;
use application::error::UseCaseError;
use application::identity::UserInteractor;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;

use crate::http_support::{
    RequestId, admin_error, cookie_value, ensure_same_origin, no_store, oauth_state_cookie_name,
};

#[derive(Clone)]
pub struct AuthState {
    pub auth: Arc<AuthInteractor>,
    /// 生产 HTTPS 部署开启（Set-Cookie: Secure）。
    pub secure_cookies: bool,
}

/// 会话 cookie 的 Max-Age（与存储绝对过期对齐的保守值，秒）。
const SESSION_COOKIE_MAX_AGE: u64 = 7 * 24 * 3600;

pub fn auth_router(state: AuthState) -> Router {
    let providers_route = Router::new()
        .route("/auth/providers", get(list_providers))
        .layer(middleware::from_fn(no_store))
        .with_state(state.clone());
    Router::new()
        .route("/auth/login", get(login))
        .route("/auth/callback/{provider}", get(callback))
        .route("/auth/logout", post(logout))
        .with_state(state)
        .merge(providers_route)
}

// ---------------------------------------------------------------------------
// 管理端最小 API（后续文章管理等扩展在此 router 下追加）
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct AdminState {
    pub auth: Arc<AuthInteractor>,
    pub users: Arc<UserInteractor>,
    /// 管理写 API 的文章用例（http_admin 模块使用）。
    pub posts: Arc<PostInteractor>,
}

pub fn admin_router(state: AdminState) -> Router {
    Router::new()
        .route("/api/admin/v1/me", get(me))
        .layer(middleware::from_fn(no_store))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// 处理器
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct LoginQuery {
    provider: String,
    #[serde(default)]
    next: Option<String>,
}

/// 公开只读：登录页可用的提供商摘要（id/展示名/类型，匿名可访问）。
async fn list_providers(State(state): State<AuthState>, request_id: RequestId) -> Response {
    match state.auth.list_provider_summaries().await {
        Ok(providers) => (StatusCode::OK, Json(providers)).into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

async fn login(State(state): State<AuthState>, Query(query): Query<LoginQuery>) -> Response {
    let next = query.next.unwrap_or_else(|| "/".to_string());
    match state.auth.login_start(&query.provider, &next).await {
        Ok(start) => {
            // 短命浏览器绑定 cookie：回调必须在同一浏览器回读（防登录 CSRF）。
            let cookie = format!(
                "{}={}; Path=/; HttpOnly; SameSite=Lax; Max-Age={ATTEMPT_TTL_SECS}{}",
                oauth_state_cookie_name(state.secure_cookies),
                start.browser_binding,
                secure_suffix(state.secure_cookies)
            );
            let mut response = Redirect::temporary(&start.authorize_url).into_response();
            response
                .headers_mut()
                .append(header::SET_COOKIE, cookie_header(cookie));
            response
        }
        Err(e) => auth_error(e),
    }
}

#[derive(serde::Deserialize)]
struct CallbackQuery {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

async fn callback(
    State(state): State<AuthState>,
    Path(provider): Path<String>,
    Query(query): Query<CallbackQuery>,
    headers: HeaderMap,
) -> Response {
    let binding_cookie = oauth_state_cookie_name(state.secure_cookies);
    let clear_binding = format!(
        "{binding_cookie}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0{}",
        secure_suffix(state.secure_cookies)
    );

    if let Some(err) = query.error {
        return with_binding_cleared(
            plain_error(StatusCode::BAD_GATEWAY, &format!("提供商返回错误：{err}")),
            clear_binding,
        );
    }
    let (Some(code), Some(csrf_state)) = (query.code, query.state) else {
        return with_binding_cleared(
            plain_error(StatusCode::BAD_REQUEST, "缺少 code 或 state"),
            clear_binding,
        );
    };
    // 浏览器绑定：必须与 state 一致才消费尝试。
    let binding = cookie_value(&headers, binding_cookie);
    match state
        .auth
        .login_callback(&provider, &code, &csrf_state, binding.as_deref())
        .await
    {
        Ok(success) => {
            let cookie = format!(
                "{SESSION_COOKIE_NAME}={}; Path=/; HttpOnly; SameSite=Lax; Max-Age={SESSION_COOKIE_MAX_AGE}{}",
                success.token,
                secure_suffix(state.secure_cookies)
            );
            let mut response =
                (StatusCode::SEE_OTHER, [("Location", success.next.clone())]).into_response();
            response
                .headers_mut()
                .append(header::SET_COOKIE, cookie_header(cookie));
            with_binding_cleared(response, clear_binding)
        }
        Err(e) => with_binding_cleared(auth_error(e), clear_binding),
    }
}

async fn logout(
    State(state): State<AuthState>,
    request_id: RequestId,
    headers: HeaderMap,
) -> Response {
    let Some(token) = cookie_value(&headers, SESSION_COOKIE_NAME) else {
        return admin_error(UseCaseError::Unauthenticated, &request_id);
    };
    // 退出是受保护写：先校验 Origin，再校验会话与 CSRF 头。
    if let Err(e) = ensure_same_origin(&headers) {
        return admin_error(e, &request_id);
    }
    let record = match state.auth.session_record(&token).await {
        Ok(record) => record,
        Err(_) => return admin_error(UseCaseError::Unauthenticated, &request_id),
    };
    let provided = headers
        .get("x-csrf-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if provided.is_empty() || provided != record.csrf_token {
        return admin_error(UseCaseError::Forbidden, &request_id);
    }
    // 会话校验通过即视为已验证身份：补录 actor，退出请求的完成日志也能归属到人。
    request_id.set_actor(record.user_id);
    if let Err(e) = state.auth.logout(&token).await {
        return admin_error(e, &request_id);
    }
    let clear = format!(
        "{SESSION_COOKIE_NAME}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0{}",
        secure_suffix(state.secure_cookies)
    );
    let mut response = (StatusCode::SEE_OTHER, [("Location", "/".to_string())]).into_response();
    response
        .headers_mut()
        .append(header::SET_COOKIE, cookie_header(clear));
    response
}

async fn me(
    State(state): State<AdminState>,
    request_id: RequestId,
    headers: HeaderMap,
) -> Response {
    let Some(token) = cookie_value(&headers, SESSION_COOKIE_NAME) else {
        return admin_error(UseCaseError::Unauthenticated, &request_id);
    };
    let (actor, record) = match (
        state.auth.actor_from_session(&token).await,
        state.auth.session_record(&token).await,
    ) {
        (Ok(a), Ok(r)) => (a, r),
        (Err(e), _) | (_, Err(e)) => return admin_error(e, &request_id),
    };
    request_id.set_actor(actor.user_id.0);
    let _ = &state.users;
    let body = json!({
        "user_id": actor.user_id.0,
        "permissions": actor.permissions().keys().collect::<Vec<_>>(),
        "csrf_token": record.csrf_token,
        "channel": "session",
    });
    (StatusCode::OK, Json(body)).into_response()
}

// ---------------------------------------------------------------------------
// 辅助
// ---------------------------------------------------------------------------

/// Set-Cookie 的 Secure 后缀（HTTPS 部署必须带上）。
fn secure_suffix(secure: bool) -> &'static str {
    if secure { "; Secure" } else { "" }
}

/// cookie 值只含 CSPRNG hex 与常量属性，转换失败属于不可达的内部错误。
fn cookie_header(cookie: String) -> HeaderValue {
    HeaderValue::from_str(&cookie).unwrap_or_else(|_| HeaderValue::from_static(""))
}

/// 在响应上追加“清除浏览器绑定 cookie”的 Set-Cookie。
fn with_binding_cleared(mut response: Response, clear_cookie: String) -> Response {
    response
        .headers_mut()
        .append(header::SET_COOKIE, cookie_header(clear_cookie));
    response
}

fn plain_error(status: StatusCode, message: &str) -> Response {
    (status, message.to_string()).into_response()
}

/// 认证/回调路由的错误响应（浏览器导航为主，保持纯文本）。
/// 内部错误只记日志、回通用文案，不泄漏存储/SQL 细节。
fn auth_error(e: UseCaseError) -> Response {
    let status = match &e {
        UseCaseError::Unauthenticated => StatusCode::UNAUTHORIZED,
        UseCaseError::Invalid(_) | UseCaseError::Conflict(_) => StatusCode::BAD_REQUEST,
        UseCaseError::NotFound(_) => StatusCode::NOT_FOUND,
        UseCaseError::Forbidden => StatusCode::FORBIDDEN,
        UseCaseError::External(_) => StatusCode::BAD_GATEWAY,
        UseCaseError::VersionConflict => StatusCode::CONFLICT,
        UseCaseError::Repository(_) | UseCaseError::Render(_) => {
            tracing::error!(error = %e, "认证路由内部错误");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    let message = match &e {
        UseCaseError::Repository(_) | UseCaseError::Render(_) => "服务器内部错误".to_string(),
        other => other.to_string(),
    };
    (status, message).into_response()
}
