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

use application::error::{ConflictKind, UseCaseError};
use application::ports::{OAUTH_STATE_COOKIE, SESSION_COOKIE};
use axum::Json;
use axum::extract::{FromRequestParts, MatchedPath, Request, State};
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
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|path| path.as_str())
        .unwrap_or("unmatched")
        .to_owned();
    let measurement = req
        .extensions()
        .get::<crate::observability::Telemetry>()
        .map(|metrics| metrics.begin(&method, &route));
    // 认证提取器与用例边界通过请求扩展回读同一个上下文。
    req.extensions_mut().insert(request_id.clone());

    let span = tracing::info_span!(
        "http_request",
        request_id = %id,
        method = %method,
        path = %path,
        route = %route,
    );
    let started = std::time::Instant::now();
    let mut response = crate::http_limits::run(req, next, &request_id)
        .instrument(span)
        .await;

    let status = response.status();
    if let Some(measurement) = measurement {
        measurement.complete(status);
    }
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let actor_id = request_id.actor_id();
    // Completion is emitted outside the handler span, with its own correlation
    // fields: no duplicated span/event fields, including when info spans are filtered.
    match (status.is_server_error(), actor_id.as_deref()) {
        (true, Some(actor)) => tracing::warn!(
            method = %method, path = %path, status = status.as_u16(), elapsed_ms,
            request_id = %id, route = %route, actor_id = %actor, "请求完成"
        ),
        (true, None) => tracing::warn!(
            method = %method, path = %path, status = status.as_u16(), elapsed_ms,
            request_id = %id, route = %route, "请求完成"
        ),
        (false, Some(actor)) => tracing::info!(
            method = %method, path = %path, status = status.as_u16(), elapsed_ms,
            request_id = %id, route = %route, actor_id = %actor, "请求完成"
        ),
        (false, None) => tracing::info!(
            method = %method, path = %path, status = status.as_u16(), elapsed_ms,
            request_id = %id, route = %route, "请求完成"
        ),
    }

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

/// 覆盖公开页面、后台、媒体及错误响应。HTTPS 来自可信公开地址配置，不信任请求头。
pub async fn security_headers(State(https): State<bool>, req: Request, next: Next) -> Response {
    let path = req.uri().path();
    let admin = path == "/admin" || path.starts_with("/admin/");
    let mut response = next.run(req).await;
    let headers = response.headers_mut();
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("same-origin"),
    );
    // 公开主题保留资源加载自由；后台仅加载本站脚本，兼容 antd 内联样式和正文图片预览。
    headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(if admin {
        "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob: https: http:; font-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'"
    } else {
        "object-src 'none'; base-uri 'self'; frame-ancestors 'none'"
    }));
    if https {
        headers.insert(
            header::STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static("max-age=31536000"),
        );
    }
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

/// HTTPS 模式只接受 host-only 会话；不回退到可被同站子域投放的旧名称。
pub fn session_cookie_name(secure: bool) -> &'static str {
    if secure {
        "__Host-blog_session"
    } else {
        SESSION_COOKIE
    }
}

/// CSRF token 的常量时间相等比较。
///
/// `==` 在首个不匹配字节处短路，理论上可经计时逐字节猜 token。这里的
/// token 是 256 位随机值的十六进制，叠加网络抖动实际不可利用；但与其余
/// 密码学谨慎度（会话只存摘要、口令常量时间比较）对齐：折叠异或不因
/// 输入内容提前退出。长度不同直接判否——长度本身不是秘密。
pub fn csrf_token_matches(provided: &str, expected: &str) -> bool {
    let (provided, expected) = (provided.as_bytes(), expected.as_bytes());
    provided.len() == expected.len()
        && provided
            .iter()
            .zip(expected)
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
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

/// 管理端稳定业务码清单（docs/identity-and-admin.md §1）。
///
/// 客户端按 `code` 分支、不按状态码或文案分支。新增错误变体必须同时在此登记，
/// 并补 `every_error_variant_maps_to_a_registered_code` 的样例；映射函数本身是穷尽匹配，
/// 漏掉新变体会直接编译失败。
pub const ADMIN_ERROR_CODES: &[&str] = &[
    "unauthenticated",
    "invalid_credentials",
    "invalid_request",
    "request_timeout",
    "rate_limited",
    "version_conflict",
    "conflict",
    "username_taken",
    "email_taken",
    "category_in_use",
    "last_admin",
    "not_found",
    "forbidden",
    "external_error",
    "internal_error",
];

/// 唯一性冲突 → 业务码。
///
/// slug 占用沿用历史上的 `conflict`（已在文档与测试中冻结），
/// username/email 有明确的界面消费者（账号创建表单定位到具体字段），因此给独立码；
/// 其余原因暂不分配独立码，保持通用 `conflict`。
pub fn conflict_error_code(kind: ConflictKind) -> &'static str {
    match kind {
        ConflictKind::Username => "username_taken",
        ConflictKind::Email => "email_taken",
        _ => "conflict",
    }
}

/// 业务错误码：与 HTTP 状态码分离，供客户端做精确分支。
///
/// 关键用例是 409：`version_conflict` 可以用最新 version 重试覆盖，
/// `conflict`（slug 等唯一性冲突）重试无用；username/email 另有专属码。
/// `last_admin` 用 403：调用者可能持有 `admin.manage`，被拒是因为会失去
/// 最后一个可登录 Admin，前端需要显示与「无权操作」不同的原因。
pub fn admin_error_code(e: &UseCaseError) -> &'static str {
    match e {
        UseCaseError::Unauthenticated => "unauthenticated",
        UseCaseError::InvalidCredentials => "invalid_credentials",
        UseCaseError::Invalid(_) => "invalid_request",
        UseCaseError::RateLimited { .. } => "rate_limited",
        UseCaseError::VersionConflict => "version_conflict",
        UseCaseError::Conflict(kind) => conflict_error_code(*kind),
        UseCaseError::CategoryInUse { .. } => "category_in_use",
        UseCaseError::LastAdminProtected => "last_admin",
        UseCaseError::RegistrationClosed => "registration_closed",
        UseCaseError::NotFound(_) => "not_found",
        UseCaseError::Forbidden => "forbidden",
        UseCaseError::External(_) => "external_error",
        UseCaseError::Repository(_) | UseCaseError::DataCorrupt(_) | UseCaseError::Render(_) => {
            "internal_error"
        }
    }
}

/// 错误的默认 HTTP 状态码。
///
/// 注意 `invalid_credentials` 默认 401：登录失败必须让客户端按「未认证」处理。
/// 个别端点（如已登录状态下的重新认证失败）会用
/// [`admin_error_with_status`] 覆盖为 403，避免前端把用户误判为掉线。
pub fn admin_error_status(e: &UseCaseError) -> StatusCode {
    match e {
        UseCaseError::Unauthenticated | UseCaseError::InvalidCredentials => {
            StatusCode::UNAUTHORIZED
        }
        UseCaseError::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
        UseCaseError::Invalid(_) => StatusCode::BAD_REQUEST,
        UseCaseError::Conflict(_)
        | UseCaseError::VersionConflict
        | UseCaseError::CategoryInUse { .. } => StatusCode::CONFLICT,
        UseCaseError::LastAdminProtected | UseCaseError::RegistrationClosed => {
            StatusCode::FORBIDDEN
        }
        UseCaseError::NotFound(_) => StatusCode::NOT_FOUND,
        UseCaseError::Forbidden => StatusCode::FORBIDDEN,
        UseCaseError::External(_) => StatusCode::BAD_GATEWAY,
        UseCaseError::Repository(_) | UseCaseError::DataCorrupt(_) | UseCaseError::Render(_) => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

/// 统一管理端错误响应（JSON，携带业务码与请求编号）。
pub fn admin_error(e: UseCaseError, request_id: &RequestId) -> Response {
    let status = admin_error_status(&e);
    admin_error_with_status(e, status, request_id)
}

/// 指定状态码的错误响应（业务码仍取自错误的默认映射）。
///
/// 用于同一业务码在不同端点需要不同状态的场景；调用方必须是有意为之。
pub fn admin_error_with_status(
    e: UseCaseError,
    status: StatusCode,
    request_id: &RequestId,
) -> Response {
    if matches!(
        e,
        UseCaseError::Repository(_) | UseCaseError::DataCorrupt(_) | UseCaseError::Render(_)
    ) {
        let error_kind = match &e {
            UseCaseError::DataCorrupt(_) => "data_corrupt",
            UseCaseError::Render(_) => "render",
            _ => "repository",
        };
        tracing::error!(request_id = request_id.as_str(), error_kind, error = %e, "管理 API 内部错误");
    }
    // 内部错误只回通用文案；其余错误按用例语义回显（不含 SQL/存储细节）。
    let message = match &e {
        UseCaseError::Repository(_) | UseCaseError::DataCorrupt(_) | UseCaseError::Render(_) => {
            "服务器内部错误".to_string()
        }
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
    // 只有「会话缺失/失效」才提示认证方案；凭据错误不是会话问题。
    if matches!(e, UseCaseError::Unauthenticated) {
        response.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            axum::http::HeaderValue::from_static("Session"),
        );
    }
    if let UseCaseError::RateLimited { retry_after_secs } = e
        && let Ok(value) = axum::http::HeaderValue::from_str(&retry_after_secs.max(1).to_string())
    {
        response.headers_mut().insert(header::RETRY_AFTER, value);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每个错误变体 → 期望业务码；码值一旦发布即视为契约，改动必须是有意为之。
    fn samples() -> Vec<(UseCaseError, &'static str)> {
        vec![
            (UseCaseError::Unauthenticated, "unauthenticated"),
            (UseCaseError::InvalidCredentials, "invalid_credentials"),
            (UseCaseError::Invalid("x".into()), "invalid_request"),
            (
                UseCaseError::RateLimited {
                    retry_after_secs: 60,
                },
                "rate_limited",
            ),
            (UseCaseError::VersionConflict, "version_conflict"),
            (UseCaseError::Conflict(ConflictKind::Slug), "conflict"),
            (
                UseCaseError::Conflict(ConflictKind::Username),
                "username_taken",
            ),
            (UseCaseError::Conflict(ConflictKind::Email), "email_taken"),
            (UseCaseError::Conflict(ConflictKind::Unknown), "conflict"),
            (
                UseCaseError::CategoryInUse {
                    posts: 2,
                    children: 1,
                },
                "category_in_use",
            ),
            (UseCaseError::LastAdminProtected, "last_admin"),
            (UseCaseError::NotFound("x".into()), "not_found"),
            (UseCaseError::Forbidden, "forbidden"),
            (UseCaseError::External("x".into()), "external_error"),
            (UseCaseError::Repository("x".into()), "internal_error"),
            (UseCaseError::DataCorrupt("x".into()), "internal_error"),
            (UseCaseError::Render("x".into()), "internal_error"),
        ]
    }

    /// 状态码默认映射；端点覆盖状态的行为由各自测试保证。
    fn status_samples() -> Vec<(UseCaseError, StatusCode)> {
        vec![
            (UseCaseError::Unauthenticated, StatusCode::UNAUTHORIZED),
            (UseCaseError::InvalidCredentials, StatusCode::UNAUTHORIZED),
            (
                UseCaseError::RateLimited {
                    retry_after_secs: 60,
                },
                StatusCode::TOO_MANY_REQUESTS,
            ),
            (UseCaseError::Forbidden, StatusCode::FORBIDDEN),
            (UseCaseError::LastAdminProtected, StatusCode::FORBIDDEN),
            (
                UseCaseError::Repository("x".into()),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        ]
    }

    #[test]
    fn error_variants_map_to_expected_status_codes() {
        for (error, expected) in status_samples() {
            assert_eq!(admin_error_status(&error), expected, "{error:?}");
        }
    }

    #[test]
    fn csrf_token_compare_is_exact_and_rejects_length_mismatch() {
        assert!(csrf_token_matches("abcdef", "abcdef"));
        assert!(!csrf_token_matches("abcdef", "abcdeg"));
        // 前缀相同但长度不同：长度不是秘密，直接判否。
        assert!(!csrf_token_matches("abcdef", "abcdef0"));
        assert!(!csrf_token_matches("", "abcdef"));
        assert!(csrf_token_matches("", ""));
    }

    #[test]
    fn every_error_variant_maps_to_a_registered_code() {
        let mut mapped = std::collections::BTreeSet::new();
        for (error, expected) in samples() {
            assert_eq!(
                admin_error_code(&error),
                expected,
                "错误码是稳定契约，改动需同步文档与客户端：{error:?}"
            );
            mapped.insert(expected);
        }
        // Emitted by the HTTP deadline middleware, not by business use cases.
        mapped.insert("request_timeout");
        let registered: std::collections::BTreeSet<&str> =
            ADMIN_ERROR_CODES.iter().copied().collect();
        assert_eq!(
            mapped, registered,
            "ADMIN_ERROR_CODES 与映射结果必须一一对应（漏登记或多余登记都会失败）"
        );
        assert_eq!(
            ADMIN_ERROR_CODES.len(),
            registered.len(),
            "清单内不得有重复码"
        );
    }
}
