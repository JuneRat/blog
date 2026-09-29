//! 认证与管理 API 入站路由。
//!
//! - GET /auth/login?provider=&next= ：发起 OAuth，302 到提供商。
//! - GET /auth/callback/{provider} ：消费 state、签发会话 cookie、303 回跳。
//! - POST /auth/login/password ：本地密码登录（JSON；失败统一 invalid_credentials）。
//! - POST /auth/logout ：受保护写（会话 + CSRF 头），撤销会话。
//! - GET /api/admin/v1/me ：会话认证的当前用户信息（含 CSRF token 供 SPA 使用）。
//! - POST /api/admin/v1/me/password ：自助改密（会话 + CSRF + 当前密码重新认证）。
//!
//! Cookie：不透明高熵令牌，HttpOnly + SameSite=Lax（HTTPS 使用 __Host-blog_session + Secure）；
//! 登录另发短命 `blog_oauth_state`（Secure 部署用 `__Host-` 前缀）绑定浏览器。
//! 会话持久化到数据库，每次请求重新读取用户与权限。

use std::sync::Arc;

use crate::http_contract::{
    Me, PasswordChangeResult, PasswordLoginResult, Profile, ProviderSummary, SessionChannel,
};
use application::auth::{ATTEMPT_TTL_SECS, AuthInteractor};
use application::content::PostInteractor;
use application::error::UseCaseError;
use application::identity::UserInteractor;
use application::password::PasswordInteractor;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};

use crate::http_admin::AdminAuth;
use crate::http_support::{
    RequestId, admin_error, admin_error_with_status, cookie_value, csrf_token_matches,
    ensure_same_origin, no_store, oauth_state_cookie_name, session_cookie_name,
};

#[derive(Clone)]
pub struct AuthState {
    pub registration: Arc<application::registration::RegistrationInteractor>,
    pub admission: Arc<dyn application::ports::RequestAdmission>,
    pub auth: Arc<AuthInteractor>,
    pub passwords: Arc<PasswordInteractor>,
    /// 生产 HTTPS 部署开启（Set-Cookie: Secure）。
    pub secure_cookies: bool,
}

/// 会话 cookie 的 Max-Age（与存储绝对过期对齐的保守值，秒）。
const SESSION_COOKIE_MAX_AGE: u64 = 7 * 24 * 3600;

/// 密码相关请求体上限：口令是短字符串，4 KiB 足以容纳任何合法请求，
/// 同时把「用超长输入放大 KDF/解析开销」的尝试挡在用例之前。
const PASSWORD_BODY_LIMIT: usize = 4 * 1024;

pub fn auth_router(state: AuthState) -> Router {
    let registration_state = state.clone();
    let providers_route = Router::new()
        .route("/auth/providers", get(list_providers))
        .layer(middleware::from_fn(no_store))
        .with_state(state.clone());
    let password_route = Router::new()
        .route("/auth/login/password", post(password_login))
        .layer(DefaultBodyLimit::max(PASSWORD_BODY_LIMIT))
        .layer(middleware::from_fn(no_store))
        .with_state(state.clone());
    let login_route =
        Router::new()
            .route("/auth/login", get(login))
            .route_layer(middleware::from_fn_with_state(
                (
                    state.admission.clone(),
                    application::ports::PublicRequest::OAuthStart,
                ),
                crate::http_limits::admit,
            ));
    login_route
        .route("/auth/callback/{provider}", get(callback))
        .route("/auth/logout", post(logout))
        .with_state(state)
        .merge(providers_route)
        .merge(password_route)
        .merge(crate::http_registration::public_router(registration_state))
}

// ---------------------------------------------------------------------------
// 管理端最小 API（后续文章管理等扩展在此 router 下追加）
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct AdminState {
    pub content_queries: Arc<application::content_queries::ContentQueries>,
    pub auth: Arc<AuthInteractor>,
    pub users: Arc<UserInteractor>,
    /// 本地密码用例（登录限流、设置/清除、自助改密）。
    pub passwords: Arc<PasswordInteractor>,
    /// 管理写 API 的文章用例（http_admin 模块使用）。
    pub posts: Arc<PostInteractor>,
    /// 管理写 API 的页面用例（站点级 page.* 权限）。
    pub pages: Arc<application::page::PageInteractor>,
    /// 标签目录用例（管理动作 tag.manage；目录读取对已认证会话开放）。
    pub tags: Arc<application::tag::TagInteractor>,
    /// 分类目录用例（管理动作 category.manage；目录读取对已认证会话开放）。
    pub categories: Arc<application::category::CategoryInteractor>,
    /// 系列用例（管理动作 series.manage；重排逐篇核验文章授权）。
    pub series: Arc<application::series::SeriesInteractor>,
    /// 站点设置用例（读/写都要求 settings.manage；覆盖 site/theme 分组，
    /// oauth 等受保护分组不在此 API 面上）。
    pub settings: Arc<application::settings::SettingsInteractor>,
    /// 角色与分配用例（http_identity 模块使用）。
    pub roles: Arc<application::identity::RoleInteractor>,
    /// 媒体用例（http_media 模块使用）：上传、浏览、使用位置与引用保护删除。
    pub media: Arc<application::media::MediaInteractor>,
    /// 与 AuthState 保持一致：改密后重签会话 cookie 需要 Secure 属性。
    pub secure_cookies: bool,
}

pub fn admin_router(state: AdminState) -> Router {
    Router::new()
        .route("/api/admin/v1/me", get(me))
        .route("/api/admin/v1/me/password", post(change_password))
        .route("/api/admin/v1/me/avatar", put(set_own_avatar))
        .layer(DefaultBodyLimit::max(PASSWORD_BODY_LIMIT))
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
        Ok(providers) => (
            StatusCode::OK,
            Json(
                providers
                    .into_iter()
                    .map(ProviderSummary::from)
                    .collect::<Vec<_>>(),
            ),
        )
            .into_response(),
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

#[derive(serde::Deserialize, ts_rs::TS)]
#[ts(rename = "PasswordLoginInput", optional_fields = nullable)]
struct PasswordLoginBody {
    username: String,
    password: String,
    #[serde(default)]
    next: Option<String>,
}

/// 本地密码登录：JSON 请求体 → 会话 cookie。
///
/// 匿名写操作没有可用的 CSRF token，靠同源 Origin 校验 + SameSite=Lax cookie
/// 防登录 CSRF；用户名字段本身不区分存在性，失败统一 `invalid_credentials`。
async fn password_login(
    State(state): State<AuthState>,
    request_id: RequestId,
    headers: HeaderMap,
    client_key: crate::http_client_ip::ClientRateLimitKey,
    client: crate::http_client_ip::ClientAddress,
    Json(body): Json<PasswordLoginBody>,
) -> Response {
    if let Err(e) = ensure_same_origin(&headers) {
        return admin_error(e, &request_id);
    }
    let next = body.next.unwrap_or_else(|| "/admin/".to_string());
    match state
        .passwords
        .login(
            &body.username,
            &body.password,
            client_key.0.as_deref(),
            &next,
            client.0,
        )
        .await
    {
        Ok(login) => {
            request_id.set_actor(login.user_id);
            let mut response = (
                StatusCode::OK,
                Json(PasswordLoginResult {
                    user_id: login.user_id,
                    next: login.next,
                }),
            )
                .into_response();
            response.headers_mut().append(
                header::SET_COOKIE,
                cookie_header(session_cookie(
                    &login.token,
                    state.secure_cookies,
                    SESSION_COOKIE_MAX_AGE,
                )),
            );
            response
        }
        Err(e) => admin_error(e, &request_id),
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
            let cookie =
                session_cookie(&success.token, state.secure_cookies, SESSION_COOKIE_MAX_AGE);
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
    let Some(token) = cookie_value(&headers, session_cookie_name(state.secure_cookies)) else {
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
    if provided.is_empty() || !csrf_token_matches(provided, &record.csrf_token) {
        return admin_error(UseCaseError::Forbidden, &request_id);
    }
    // 会话校验通过即视为已验证身份：补录 actor，退出请求的完成日志也能归属到人。
    request_id.set_actor(record.user_id);
    if let Err(e) = state.auth.logout(&token).await {
        return admin_error(e, &request_id);
    }
    let clear = session_cookie("", state.secure_cookies, 0);
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
    let Some(token) = cookie_value(&headers, session_cookie_name(state.secure_cookies)) else {
        return admin_error(UseCaseError::Unauthenticated, &request_id);
    };
    let (record, actor) = match state.auth.session_actor(&token).await {
        Ok(pair) => pair,
        Err(UseCaseError::Unauthenticated) => {
            // The public comment form also checks /me. Remove a confirmed stale
            // cookie so its next, explicitly anonymous submission can proceed.
            let mut response = admin_error(UseCaseError::Unauthenticated, &request_id);
            response.headers_mut().append(
                header::SET_COOKIE,
                cookie_header(session_cookie("", state.secure_cookies, 0)),
            );
            return response;
        }
        Err(e) => return admin_error(e, &request_id),
    };
    request_id.set_actor(actor.user_id.0);
    // 资料来自 users 行（展示名/头像）：`/me` 是 SPA 唯一的自身资料入口，
    // 软删除账号已在用例层按不存在处理（会话本应已被撤销）。
    let profile = match state.users.profile_of(&actor).await {
        Ok(profile) => profile,
        Err(e) => return admin_error(e, &request_id),
    };
    let time_zone = match state.settings.public_time_zone().await {
        Ok(zone) => zone,
        Err(error) => return admin_error(error, &request_id),
    };
    let body = Me {
        profile: profile.into(),
        permissions: actor.permissions().keys().map(str::to_owned).collect(),
        csrf_token: record.csrf_token,
        channel: SessionChannel::Session,
        time_zone,
    };
    (StatusCode::OK, Json(body)).into_response()
}

#[derive(serde::Deserialize, ts_rs::TS)]
#[ts(rename = "SetAvatarInput", optional_fields = nullable)]
struct SetAvatarBody {
    expected_version: i64,
    /// 缺省或 null = 清除头像；id = 设置头像（PUT 是整值替换，非三态）。
    #[serde(default)]
    avatar_media_id: Option<uuid::Uuid>,
}

/// 自助设置/清除头像：本人即可，无需额外权限；CSRF/Origin 由 `AdminAuth` 统一校验。
///
/// 只递增资料编辑版本，保持 `users.auth_version`，既有会话继续有效。
async fn set_own_avatar(
    State(state): State<AdminState>,
    auth: AdminAuth,
    request_id: RequestId,
    Json(body): Json<SetAvatarBody>,
) -> Response {
    match state
        .users
        .set_own_avatar(&auth.actor, body.avatar_media_id, body.expected_version)
        .await
    {
        Ok(profile) => (StatusCode::OK, Json(Profile::from(profile))).into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

#[derive(serde::Deserialize, ts_rs::TS)]
#[ts(rename = "ChangePasswordInput", optional_fields = nullable)]
struct ChangePasswordBody {
    /// 已启用密码登录时必填：用于重新认证（docs §5）。
    #[serde(default)]
    current_password: Option<String>,
    new_password: String,
}

/// 自助改密：会话 + CSRF + Origin 三重保护，成功后轮换会话 cookie。
async fn change_password(
    State(state): State<AdminState>,
    request_id: RequestId,
    headers: HeaderMap,
    client_key: crate::http_client_ip::ClientRateLimitKey,
    client: crate::http_client_ip::ClientAddress,
    Json(body): Json<ChangePasswordBody>,
) -> Response {
    let Some(token) = cookie_value(&headers, session_cookie_name(state.secure_cookies)) else {
        return admin_error(UseCaseError::Unauthenticated, &request_id);
    };
    if let Err(e) = ensure_same_origin(&headers) {
        return admin_error(e, &request_id);
    }
    // 一次校验同时拿到记录与 Actor：CSRF 用记录的 token，动作授权用 Actor。
    let (record, actor) = match state.auth.session_actor(&token).await {
        Ok(pair) => pair,
        Err(e) => return admin_error(e, &request_id),
    };
    let provided = headers
        .get("x-csrf-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if provided.is_empty() || !csrf_token_matches(provided, &record.csrf_token) {
        return admin_error(UseCaseError::Forbidden, &request_id);
    }
    request_id.set_actor(actor.user_id.0);

    // 重新认证与登录共用失败预算和可信代理解析规则。
    let actor = actor.with_audit_ip(client.0);
    match state
        .passwords
        .change_own_password(
            &actor,
            body.current_password.as_deref(),
            &body.new_password,
            client_key.0.as_deref(),
        )
        .await
    {
        Ok(new_token) => {
            let csrf_token = match state.auth.session_record(&new_token).await {
                Ok(record) => record.csrf_token,
                Err(e) => return admin_error(e, &request_id),
            };
            let mut response = (
                StatusCode::OK,
                Json(PasswordChangeResult {
                    user_id: actor.user_id.0,
                    csrf_token,
                }),
            )
                .into_response();
            response.headers_mut().append(
                header::SET_COOKIE,
                cookie_header(session_cookie(
                    &new_token,
                    state.secure_cookies,
                    SESSION_COOKIE_MAX_AGE,
                )),
            );
            response
        }
        // 已登录状态下「当前密码不对」不是掉线：用 403 而非 401，
        // 否则前端会把用户当成会话失效直接清空登录态。
        Err(e @ UseCaseError::InvalidCredentials) => {
            admin_error_with_status(e, StatusCode::FORBIDDEN, &request_id)
        }
        Err(e) => admin_error(e, &request_id),
    }
}

// ---------------------------------------------------------------------------
// 辅助
// ---------------------------------------------------------------------------

/// 会话 cookie 的完整 Set-Cookie 值；`max_age = 0` 即清除。
fn session_cookie(token: &str, secure: bool, max_age: u64) -> String {
    format!(
        "{}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{}",
        session_cookie_name(secure),
        secure_suffix(secure)
    )
}

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
        UseCaseError::InvalidCredentials => StatusCode::UNAUTHORIZED,
        UseCaseError::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
        UseCaseError::Invalid(_)
        | UseCaseError::Conflict(_)
        | UseCaseError::CategoryInUse { .. } => StatusCode::BAD_REQUEST,
        UseCaseError::NotFound(_) => StatusCode::NOT_FOUND,
        UseCaseError::Forbidden
        | UseCaseError::RegistrationClosed
        | UseCaseError::LastAdminProtected => StatusCode::FORBIDDEN,
        UseCaseError::External(_) => StatusCode::BAD_GATEWAY,
        UseCaseError::VersionConflict => StatusCode::CONFLICT,
        UseCaseError::Repository(_) | UseCaseError::DataCorrupt(_) | UseCaseError::Render(_) => {
            tracing::error!(error = %e, "认证路由内部错误");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    let message = match &e {
        UseCaseError::Repository(_) | UseCaseError::DataCorrupt(_) | UseCaseError::Render(_) => {
            "服务器内部错误".to_string()
        }
        other => other.to_string(),
    };
    let mut response = (status, message).into_response();
    if let UseCaseError::RateLimited { retry_after_secs } = e
        && let Ok(value) = HeaderValue::from_str(&retry_after_secs.to_string())
    {
        response.headers_mut().insert(header::RETRY_AFTER, value);
    }
    response
}

pub(crate) fn export_contract(out: &mut Vec<String>) {
    crate::http_contract::declare::<PasswordLoginBody>(out);
    crate::http_contract::declare::<SetAvatarBody>(out);
    crate::http_contract::declare::<ChangePasswordBody>(out);
}
