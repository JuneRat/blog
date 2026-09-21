//! 认证/管理 HTTP 的共用辅助：cookie 解析、CSRF/Origin 校验、统一错误契约、请求编号与 no-store。
//!
//! 错误契约：JSON `{"error": ..., "code": ..., "request_id": ...}`；401 附 `WWW-Authenticate: Session`；
//! `Repository`/`Render` 等内部错误只记日志、响应通用文案（不泄漏 SQL 与内部细节）。
//! `code` 是业务码：同一状态码可能对应不同业务原因（slug 占用与版本冲突都是 409），
//! 前端必须能区分，不能只按状态码分支。
//! 管理响应一律 `Cache-Control: no-store`（含带 CSRF token 的 `/me`）。
//!
//! 请求编号：`request_context` 覆盖全站，每请求生成 UUIDv7，回写 `x-request-id` 响应头，
//! 并输出一条含 method/path/status/耗时/已验证 actor 的完成日志（不含 query、Cookie、token、正文）。

use std::sync::{Arc, OnceLock};

use application::error::UseCaseError;
use application::ports::OAUTH_STATE_COOKIE;
use axum::Json;
use axum::extract::{FromRequestParts, Request};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use tracing::Instrument as _;
use uuid::Uuid;

/// 请求编号的响应头名称（HTTP/2 线上为小写；读取方大小写不敏感）。
pub const REQUEST_ID_HEADER: &str = "x-request-id";

/// 每请求上下文：编号 + 经验证身份的 actor 记录位。
///
/// 中间件生成编号并注入请求扩展；身份由**认证成功后的代码**调用 [`RequestId::set_actor`] 补录，
/// 匿名或未通过认证的请求保持为空。actor 只来自服务端验证过的会话，绝不取自请求参数或 header。
#[derive(Clone, Debug)]
pub struct RequestId {
    id: Arc<str>,
    actor_id: Arc<OnceLock<String>>,
}

impl RequestId {
    /// 生成新的请求编号（全站中间件与未挂中间件的单测使用）。
    pub fn generate() -> Self {
        Self {
            id: Uuid::now_v7().to_string().into(),
            actor_id: Arc::new(OnceLock::new()),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.id
    }

    /// 由已验证身份补录；首次写入生效，重复调用忽略。
    pub fn set_actor(&self, user_id: Uuid) {
        let _ = self.actor_id.set(user_id.to_string());
    }

    fn actor_id(&self) -> Option<String> {
        self.actor_id.get().cloned()
    }
}

impl<S: Send + Sync> FromRequestParts<S> for RequestId {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        // 全站中间件总会注入；只装配子路由的单测里退化为新编号，不改变处理结果。
        Ok(parts
            .extensions
            .get::<RequestId>()
            .cloned()
            .unwrap_or_else(RequestId::generate))
    }
}

/// 全站最外层中间件：分配编号、建立请求 span、记录完成日志、回写响应头。
///
/// span 只是上下文，不等于结果记录：这里在 handler 返回后**显式**输出一条完成日志，
/// 因此 401/403/404/409 也会留下记录。正常请求与预期的 4xx 记 `info`，只有 5xx 升为 `warn`。
/// 日志只含 method（无 query）、path、status、耗时与已验证 actor，不含 Cookie/token/正文。
pub async fn request_context(mut req: Request, next: Next) -> Response {
    let request_id = RequestId::generate();
    let id = request_id.as_str().to_string();
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    // 认证提取器与用例边界通过请求扩展回读同一个上下文。
    req.extensions_mut().insert(request_id.clone());

    let span = tracing::info_span!(
        "http_request",
        request_id = %id,
        method = %method,
        path = %path,
    );
    let started = std::time::Instant::now();
    let mut response = next.run(req).instrument(span.clone()).await;

    let status = response.status();
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let actor_id = request_id.actor_id();
    // 在 span 作用域内记录，保证日志行带 request_id 上下文。
    span.in_scope(|| match (status.is_server_error(), actor_id.as_deref()) {
        (true, Some(actor)) => {
            tracing::warn!(
                status = status.as_u16(),
                elapsed_ms,
                actor_id = %actor,
                "请求完成"
            )
        }
        (true, None) => tracing::warn!(status = status.as_u16(), elapsed_ms, "请求完成"),
        (false, Some(actor)) => {
            tracing::info!(
                status = status.as_u16(),
                elapsed_ms,
                actor_id = %actor,
                "请求完成"
            )
        }
        (false, None) => tracing::info!(status = status.as_u16(), elapsed_ms, "请求完成"),
    });

    // 无论成败（含提取器提前拒绝）都回写编号：客户端据此报障。
    if let Ok(value) = HeaderValue::from_str(&id) {
        response.headers_mut().insert(REQUEST_ID_HEADER, value);
    }
    response
}

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

/// 业务错误码：与 HTTP 状态码分离，供客户端做精确分支。
///
/// 关键用例是 409：`version_conflict` 可以用最新 version 重试覆盖，
/// `conflict`（slug/username 已占用）重试无用。新增错误变体时必须同时给出码。
pub fn admin_error_code(e: &UseCaseError) -> &'static str {
    match e {
        UseCaseError::Unauthenticated => "unauthenticated",
        UseCaseError::Invalid(_) => "invalid_request",
        UseCaseError::VersionConflict => "version_conflict",
        UseCaseError::Conflict(_) => "conflict",
        UseCaseError::NotFound(_) => "not_found",
        UseCaseError::Forbidden => "forbidden",
        UseCaseError::External(_) => "external_error",
        UseCaseError::Repository(_) | UseCaseError::Render(_) => "internal_error",
    }
}

/// 统一管理端错误响应（JSON，携带业务码与请求编号）。
pub fn admin_error(e: UseCaseError, request_id: &RequestId) -> Response {
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
    let mut response = (
        status,
        Json(serde_json::json!({
            "error": message,
            "code": admin_error_code(&e),
            "request_id": request_id.as_str(),
        })),
    )
        .into_response();
    if status == StatusCode::UNAUTHORIZED {
        response.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            axum::http::HeaderValue::from_static("Session"),
        );
    }
    response
}
