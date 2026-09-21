//! 认证/管理 HTTP 的共用辅助：cookie 解析、CSRF/Origin 校验、统一错误契约与 no-store。
//!
//! 错误契约：JSON `{"error": ...}`；401 附 `WWW-Authenticate: Session`；
//! `Repository`/`Render` 等内部错误只记日志、响应通用文案（不泄漏 SQL 与内部细节）。
//! 管理响应一律 `Cache-Control: no-store`（含带 CSRF token 的 `/me`）。

use application::error::UseCaseError;
use application::ports::OAUTH_STATE_COOKIE;
use axum::Json;
use axum::extract::Request;
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

/// 管理响应不缓存（任何缓存层都不得留存会话/CSRF 相关内容）。
pub async fn no_store(req: Request, next: Next) -> Response {
    let mut response = next.run(req).await;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}

/// 从 Cookie 头解析指定名称的值。
/// 名称精确匹配（`blog_session_x` 不算 `blog_session`），
/// 同名空值继续扫描后续 pair，不提前中止（否则同级干扰 cookie 会让正常会话被判未登录）。
pub fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    for pair in raw.split(';') {
        let Some((key, value)) = pair.trim().split_once('=') else {
            continue;
        };
        if key == name && !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

/// OAuth 浏览器绑定 cookie 名：Secure 部署使用 `__Host-` 前缀（host-only + Path=/）。
pub fn oauth_state_cookie_name(secure: bool) -> &'static str {
    if secure {
        "__Host-blog_oauth_state"
    } else {
        OAUTH_STATE_COOKIE
    }
}

/// 写请求 Origin 校验：Origin 存在时必须与 Host 同源。
/// 缺失 Origin 视为非浏览器客户端（CSRF token 仍必须校验），与 docs §5 的
/// “写请求校验 CSRF token 和 Origin”一致。
pub fn ensure_same_origin(headers: &HeaderMap) -> Result<(), UseCaseError> {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return Ok(());
    };
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if host.is_empty() {
        return Err(UseCaseError::Invalid(
            "写请求缺少 Host，无法校验来源".into(),
        ));
    }
    if origin.eq_ignore_ascii_case(&format!("https://{host}"))
        || origin.eq_ignore_ascii_case(&format!("http://{host}"))
    {
        return Ok(());
    }
    Err(UseCaseError::Forbidden)
}

/// 统一管理端错误响应。
pub fn admin_error(e: UseCaseError) -> Response {
    let status = match &e {
        UseCaseError::Unauthenticated => StatusCode::UNAUTHORIZED,
        UseCaseError::Invalid(_) => StatusCode::BAD_REQUEST,
        UseCaseError::Conflict(_) | UseCaseError::VersionConflict => StatusCode::CONFLICT,
        UseCaseError::NotFound(_) => StatusCode::NOT_FOUND,
        UseCaseError::Forbidden => StatusCode::FORBIDDEN,
        UseCaseError::External(_) => StatusCode::BAD_GATEWAY,
        UseCaseError::Repository(_) | UseCaseError::Render(_) => {
            tracing::error!(error = %e, "管理 API 内部错误");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    // 内部错误只回通用文案；其余错误按用例语义回显（不含 SQL/存储细节）。
    let message = match &e {
        UseCaseError::Repository(_) | UseCaseError::Render(_) => "服务器内部错误".to_string(),
        other => other.to_string(),
    };
    let mut response = (status, Json(serde_json::json!({ "error": message }))).into_response();
    if status == StatusCode::UNAUTHORIZED {
        response.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            axum::http::HeaderValue::from_static("Session"),
        );
    }
    response
}
