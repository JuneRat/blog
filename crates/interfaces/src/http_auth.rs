//! 认证与管理 API 入站路由。
//!
//! - GET /auth/login?provider=&next= ：发起 OAuth，302 到提供商。
//! - GET /auth/callback/{provider} ：消费 state、签发会话 cookie、303 回跳。
//! - POST /auth/logout ：受保护写（会话 + CSRF 头），撤销会话。
//! - GET /api/admin/v1/me ：会话认证的当前用户信息（含 CSRF token 供 SPA 使用）。
//!
//! Cookie：不透明高熵令牌，HttpOnly + SameSite=Lax（生产可加 Secure）。
//! 会话状态存服务端内存，每次请求重新读取用户与权限。

use std::sync::Arc;

use application::auth::{AuthInteractor, SESSION_COOKIE_NAME};
use application::content::PostInteractor;
use application::error::UseCaseError;
use application::identity::UserInteractor;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;

#[derive(Clone)]
pub struct AuthState {
    pub auth: Arc<AuthInteractor>,
    /// 生产 HTTPS 部署开启（Set-Cookie: Secure）。
    pub secure_cookies: bool,
}

/// 会话 cookie 的 Max-Age（与存储绝对过期对齐的保守值，秒）。
const SESSION_COOKIE_MAX_AGE: u64 = 7 * 24 * 3600;

pub fn auth_router(state: AuthState) -> Router {
    Router::new()
        .route("/auth/login", get(login))
        .route("/auth/callback/{provider}", get(callback))
        .route("/auth/logout", post(logout))
        .with_state(state)
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

async fn login(State(state): State<AuthState>, Query(query): Query<LoginQuery>) -> Response {
    let next = query.next.unwrap_or_else(|| "/".to_string());
    match state.auth.login_start(&query.provider, &next).await {
        Ok(url) => Redirect::temporary(&url).into_response(),
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
) -> Response {
    if let Some(err) = query.error {
        return plain_error(StatusCode::BAD_GATEWAY, &format!("提供商返回错误：{err}"));
    }
    let (Some(code), Some(csrf_state)) = (query.code, query.state) else {
        return plain_error(StatusCode::BAD_REQUEST, "缺少 code 或 state");
    };
    match state
        .auth
        .login_callback(&provider, &code, &csrf_state)
        .await
    {
        Ok(success) => {
            let cookie = format!(
                "{SESSION_COOKIE_NAME}={}; Path=/; HttpOnly; SameSite=Lax; Max-Age={SESSION_COOKIE_MAX_AGE}{}",
                success.token,
                if state.secure_cookies { "; Secure" } else { "" }
            );
            (
                StatusCode::SEE_OTHER,
                [("Set-Cookie", cookie), ("Location", success.next.clone())],
            )
                .into_response()
        }
        Err(e) => auth_error(e),
    }
}

async fn logout(State(state): State<AuthState>, headers: HeaderMap) -> Response {
    let Some(token) = session_token_from_headers(&headers) else {
        return plain_error(StatusCode::UNAUTHORIZED, "未登录");
    };
    // 退出是受保护写：校验会话与 CSRF 头。
    let record = match state.auth.session_record(&token).await {
        Ok(record) => record,
        Err(_) => return plain_error(StatusCode::UNAUTHORIZED, "会话已失效"),
    };
    let provided = headers
        .get("x-csrf-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if provided.is_empty() || provided != record.csrf_token {
        return plain_error(StatusCode::FORBIDDEN, "CSRF 校验失败");
    }
    // Origin 存在时必须同源。
    if let Some(origin) = headers.get("origin").and_then(|v| v.to_str().ok()) {
        let host = headers
            .get("host")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        let origin_host = origin
            .trim_start_matches("https://")
            .trim_start_matches("http://");
        if !origin.eq_ignore_ascii_case(&format!("https://{host}"))
            && !origin.eq_ignore_ascii_case(&format!("http://{host}"))
            && origin_host != host
        {
            return plain_error(StatusCode::FORBIDDEN, "跨源请求被拒绝");
        }
    }
    if let Err(e) = state.auth.logout(&token).await {
        return auth_error(e);
    }
    let clear = format!("{SESSION_COOKIE_NAME}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0");
    (
        StatusCode::SEE_OTHER,
        [("Set-Cookie", clear), ("Location", "/".to_string())],
    )
        .into_response()
}

async fn me(State(state): State<AdminState>, headers: HeaderMap) -> Response {
    let Some(token) = session_token_from_headers(&headers) else {
        return plain_error(StatusCode::UNAUTHORIZED, "未登录");
    };
    let (actor, record) = match (
        state.auth.actor_from_session(&token).await,
        state.auth.session_record(&token).await,
    ) {
        (Ok(a), Ok(r)) => (a, r),
        (Err(e), _) | (_, Err(e)) => return auth_error(e),
    };
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

fn session_token_from_headers(headers: &HeaderMap) -> Option<String> {
    let cookie = headers.get(axum::http::header::COOKIE)?.to_str().ok()?;
    for pair in cookie.split(';') {
        let pair = pair.trim();
        if let Some(value) = pair.strip_prefix(SESSION_COOKIE_NAME) {
            let value = value.strip_prefix('=')?;
            if value.is_empty() {
                return None;
            }
            return Some(value.to_string());
        }
    }
    None
}

fn plain_error(status: StatusCode, message: &str) -> Response {
    (status, message.to_string()).into_response()
}

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
    (status, e.to_string()).into_response()
}
