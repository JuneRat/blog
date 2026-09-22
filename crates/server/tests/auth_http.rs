//! 认证 HTTP 全链路：/auth/login → 假 IdP 回调 → 会话 cookie → /api/admin/v1/me → CSRF 登出。
//! 使用真实 PostgreSQL 与内存会话；外部身份客户端为 fake（不发出网络请求）。

mod common;

use std::sync::{Arc, Mutex};

use application::auth::AuthInteractor;
use application::identity::{Actor, CreateUserCmd, RoleInteractor, UserInteractor};
use application::ports::{
    Clock, ExternalIdentity, ExternalIdentityClient, OAuthAccountStore, OAuthConfigStore,
    ProviderConfig, ProviderKind, SecureRandom, SessionStore,
};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use infrastructure::{
    InMemoryOAuthAttemptStore, InMemorySessionStore, PostgresOAuthAccountStore,
    PostgresOAuthConfigStore, PostgresRbacStore, PostgresUserRepository, SystemClock,
};
use interfaces::http_auth::{AdminState, AuthState, admin_router, auth_router};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

/// 各测试重建同一数据库，必须串行。
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 假 IdP 客户端：不发网络请求；exchange 返回预置外部身份。
struct FakeIdpClient {
    external_id: Mutex<String>,
}

#[async_trait::async_trait]
impl ExternalIdentityClient for FakeIdpClient {
    async fn authorize_url(
        &self,
        _config: &ProviderConfig,
        state: &str,
        _challenge: Option<&str>,
        _nonce: Option<&str>,
        _redirect_uri: &str,
    ) -> Result<String, application::error::UseCaseError> {
        Ok(format!("https://idp.example/authorize?state={state}"))
    }

    async fn exchange(
        &self,
        _config: &ProviderConfig,
        _code: &str,
        _verifier: Option<&str>,
        _nonce: Option<&str>,
        _redirect_uri: &str,
    ) -> Result<ExternalIdentity, application::error::UseCaseError> {
        Ok(ExternalIdentity {
            provider_key: "https://idp.example".into(),
            provider_user_id: self.external_id.lock().unwrap().clone(),
            email: Some("http-user@example.com".into()),
        })
    }
}

struct TestRandom;

impl SecureRandom for TestRandom {
    fn token_hex(&self) -> Result<String, application::error::UseCaseError> {
        Ok(Uuid::now_v7().simple().to_string())
    }
    fn pkce_s256(&self, _verifier: &str) -> Result<String, application::error::UseCaseError> {
        Ok("challenge".into())
    }
}

struct Stack {
    router: axum::Router,
    #[allow(dead_code)]
    pool: PgPool,
}

async fn fresh_stack() -> Stack {
    fresh_stack_with(false).await
}

async fn fresh_stack_with(secure_cookies: bool) -> Stack {
    let pool = common::fresh_database("blog_auth_test").await;

    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let user_repo = Arc::new(PostgresUserRepository::new(pool.clone()));
    let rbac = Arc::new(PostgresRbacStore::new(pool.clone()));
    let roles = Arc::new(RoleInteractor::new(rbac.clone(), user_repo.clone()));
    roles.sync_registry().await.expect("同步权限目录失败");
    let users = Arc::new(UserInteractor::new(user_repo.clone(), rbac, clock.clone()));

    // 用户 + author 角色 + 外部身份绑定。
    let member = users
        .create_user(
            &Actor::bootstrap_cli(),
            CreateUserCmd {
                username: "httpuser".into(),
                email: None,
                display_name: Some("HTTP 用户".into()),
            },
        )
        .await
        .unwrap();
    roles
        .assign_to_username(&Actor::bootstrap_cli(), "httpuser", "author")
        .await
        .unwrap();

    let configs = Arc::new(PostgresOAuthConfigStore::new(pool.clone()));
    configs
        .save(&[ProviderConfig {
            id: "idp".into(),
            name: Some("示例 IdP".into()),
            kind: ProviderKind::Oidc,
            issuer: Some("https://idp.example".into()),
            client_id: "client".into(),
            secret_ref: "IDP_SECRET".into(),
            scopes: vec![],
        }])
        .await
        .unwrap();

    let accounts: Arc<dyn OAuthAccountStore> =
        Arc::new(PostgresOAuthAccountStore::new(pool.clone()));
    accounts
        .bind(member.id, "https://idp.example", "sub-http-42", None)
        .await
        .unwrap();

    let sessions: Arc<dyn SessionStore> = Arc::new(InMemorySessionStore::with_defaults());
    let attempts = Arc::new(InMemoryOAuthAttemptStore::with_defaults());
    let identity_client = Arc::new(FakeIdpClient {
        external_id: Mutex::new("sub-http-42".into()),
    });
    let random: Arc<dyn SecureRandom> = Arc::new(TestRandom);

    let tag_repo: Arc<dyn application::ports::TagRepository> =
        Arc::new(infrastructure::PostgresTagRepository::new(pool.clone()));
    let posts = Arc::new(application::content::PostInteractor::new(
        Arc::new(infrastructure::PostgresPostRepository::new(pool.clone())),
        tag_repo.clone(),
        std::sync::Arc::new(infrastructure::PostgresCategoryRepository::new(
            pool.clone(),
        )),
        std::sync::Arc::new(infrastructure::PostgresSeriesRepository::new(pool.clone())),
        std::sync::Arc::new(infrastructure::SystemClock),
    ));
    let pages = Arc::new(application::page::PageInteractor::new(
        Arc::new(infrastructure::PostgresPageRepository::new(pool.clone())),
        std::sync::Arc::new(infrastructure::SystemClock),
    ));

    let auth = Arc::new(AuthInteractor::new(
        application::auth::AuthDeps {
            sessions: sessions.clone(),
            attempts,
            configs,
            accounts: accounts.clone(),
            identity_client,
            random,
        },
        users.clone(),
        clock,
        "http://127.0.0.1:18099".into(),
    ));
    let passwords = common::password_interactor(user_repo.clone(), sessions);

    let auth_state = AuthState {
        auth: auth.clone(),
        passwords: passwords.clone(),
        secure_cookies,
    };
    let admin_state = AdminState {
        auth,
        users,
        passwords,
        posts,
        pages,
        tags: Arc::new(application::tag::TagInteractor::new(
            tag_repo,
            std::sync::Arc::new(infrastructure::SystemClock),
        )),
        categories: Arc::new(application::category::CategoryInteractor::new(
            std::sync::Arc::new(infrastructure::PostgresCategoryRepository::new(
                pool.clone(),
            )),
            std::sync::Arc::new(infrastructure::SystemClock),
        )),
        series: Arc::new(application::series::SeriesInteractor::new(
            std::sync::Arc::new(infrastructure::PostgresSeriesRepository::new(pool.clone())),
            std::sync::Arc::new(infrastructure::SystemClock),
        )),
        settings: Arc::new(application::settings::SettingsInteractor::new(
            std::sync::Arc::new(infrastructure::PostgresSettingsStore::new(pool.clone())),
            std::sync::Arc::new(infrastructure::SystemClock),
            application::public_site::SiteInfo {
                title: "测试站点".into(),
                description: "测试描述".into(),
            },
        )),
        roles,
        secure_cookies,
    };

    let router = auth_router(auth_state)
        .merge(admin_router(admin_state.clone()))
        .merge(interfaces::http_admin::posts_router(admin_state));
    Stack { router, pool }
}

async fn request(
    router: &axum::Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
) -> (StatusCode, Vec<(String, String)>, String) {
    let mut builder = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = router
        .clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let set_cookies: Vec<(String, String)> = response
        .headers()
        .get_all(axum::http::header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok().map(|s| s.to_string()))
        .map(|s| ("set-cookie".to_string(), s))
        .collect();
    let mut captured = set_cookies;
    for name in [
        axum::http::header::LOCATION,
        axum::http::header::CACHE_CONTROL,
        axum::http::header::WWW_AUTHENTICATE,
    ] {
        if let Some(value) = response.headers().get(&name).and_then(|v| v.to_str().ok()) {
            captured.push((name.as_str().to_string(), value.to_string()));
        }
    }
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let body = String::from_utf8_lossy(&body).to_string();
    (status, captured, body)
}

fn cookie_value(captured: &[(String, String)]) -> Option<String> {
    captured
        .iter()
        .filter(|(k, _)| k == "set-cookie")
        .find_map(|(_, v)| {
            let pair = v.split(';').next()?.trim();
            pair.strip_prefix("blog_session=").map(str::to_string)
        })
}

/// 完成一次带回浏览器绑定的登录；返回（会话 cookie，登录响应头，回调响应头）。
async fn login(
    router: &axum::Router,
    next: &str,
) -> (String, Vec<(String, String)>, Vec<(String, String)>) {
    let (status, login_headers, _) = request(
        router,
        "GET",
        &format!("/auth/login?provider=idp&next={next}"),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::TEMPORARY_REDIRECT);
    let location = login_headers
        .iter()
        .find(|(k, _)| k == "location")
        .map(|(_, v)| v.clone())
        .unwrap();
    let state = location
        .split("state=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap()
        .to_string();
    let binding = login_headers
        .iter()
        .filter(|(k, _)| k == "set-cookie")
        .find_map(|(_, v)| {
            v.split(';')
                .next()?
                .trim()
                .strip_prefix("blog_oauth_state=")
                .map(str::to_string)
        })
        .expect("登录必须下发浏览器绑定 cookie");
    assert_eq!(binding, state, "绑定值与 state 一致（双重提交）");

    let cookie_header = format!("blog_oauth_state={binding}");
    let (status, callback_headers, _) = request(
        router,
        "GET",
        &format!("/auth/callback/idp?code=abc&state={state}"),
        &[("cookie", &cookie_header)],
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER, "回调成功");
    let cookie = cookie_value(&callback_headers).expect("签发会话 cookie");
    (cookie, login_headers, callback_headers)
}

fn set_cookie_named<'a>(headers: &'a [(String, String)], name: &str) -> &'a str {
    headers
        .iter()
        .filter(|(k, _)| k == "set-cookie")
        .map(|(_, v)| v.as_str())
        .find(|v| v.starts_with(&format!("{name}=")))
        .unwrap_or_else(|| panic!("缺少 {name} 的 Set-Cookie：{headers:?}"))
}

#[tokio::test]
async fn full_login_me_logout_round_trip() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;

    // 1-2. 发起登录 → 302 到假 IdP；回调带回绑定 cookie → 303 + 会话 cookie。
    let (cookie, login_headers, callback_headers) = login(&stack.router, "/admin").await;
    let location = login_headers
        .iter()
        .find(|(k, _)| k == "location")
        .map(|(_, v)| v.clone())
        .unwrap();
    assert!(location.starts_with("https://idp.example/authorize"));
    let redirect = callback_headers
        .iter()
        .find(|(k, _)| k == "location")
        .map(|(_, v)| v.clone())
        .unwrap();
    assert_eq!(redirect, "/admin", "回跳受控路径");
    assert!(!cookie.is_empty());

    // 会话 cookie：HttpOnly + SameSite=Lax + Max-Age；非 Secure 部署不带 Secure。
    let session_cookie = set_cookie_named(&callback_headers, "blog_session");
    assert!(session_cookie.contains("HttpOnly"), "{session_cookie}");
    assert!(session_cookie.contains("SameSite=Lax"), "{session_cookie}");
    assert!(session_cookie.contains("Max-Age="), "{session_cookie}");
    assert!(
        !session_cookie.contains("Secure"),
        "非 HTTPS 部署不加 Secure：{session_cookie}"
    );

    // 登录绑定 cookie：短命、HttpOnly、SameSite=Lax，回调后被清除。
    let binding_cookie = set_cookie_named(&login_headers, "blog_oauth_state");
    assert!(binding_cookie.contains("HttpOnly"), "{binding_cookie}");
    assert!(binding_cookie.contains("SameSite=Lax"), "{binding_cookie}");
    assert!(binding_cookie.contains("Max-Age=600"), "{binding_cookie}");
    assert!(!binding_cookie.contains("Secure"), "{binding_cookie}");
    let cleared = set_cookie_named(&callback_headers, "blog_oauth_state");
    assert!(
        cleared.contains("Max-Age=0"),
        "回调必须清除绑定 cookie：{cleared}"
    );

    // 3. /me → 200，含权限与 CSRF token。
    let (status, _, body) = request(
        &stack.router,
        "GET",
        "/api/admin/v1/me",
        &[("cookie", &format!("blog_session={cookie}"))],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "会话认证通过：{body}");
    assert!(body.contains("post.create"), "author 权限并入：{body}");
    let csrf = body
        .split("\"csrf_token\":\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .to_string();

    // 4. 登出缺 CSRF → 403。
    let (status, _, _) = request(
        &stack.router,
        "POST",
        "/auth/logout",
        &[("cookie", &format!("blog_session={cookie}"))],
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "登出必须校验 CSRF");

    // 5. 带 CSRF 登出 → 303；会话失效。
    let (status, headers, _) = request(
        &stack.router,
        "POST",
        "/auth/logout",
        &[
            ("cookie", &format!("blog_session={cookie}")),
            ("x-csrf-token", &csrf),
            ("origin", "http://127.0.0.1:18099"),
            ("host", "127.0.0.1:18099"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(
        headers
            .iter()
            .any(|(k, v)| k == "set-cookie" && v.contains("Max-Age=0")),
        "清除会话 cookie"
    );

    let (status, _, _) = request(
        &stack.router,
        "GET",
        "/api/admin/v1/me",
        &[("cookie", &format!("blog_session={cookie}"))],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "登出后会话失效");
}

#[tokio::test]
async fn unauthenticated_me_is_401_and_cross_origin_logout_rejected() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;

    let (status, _, _) = request(&stack.router, "GET", "/api/admin/v1/me", &[]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // 登录拿会话。
    let (cookie, _, _) = login(&stack.router, "/").await;
    let (_, _, body) = request(
        &stack.router,
        "GET",
        "/api/admin/v1/me",
        &[("cookie", &format!("blog_session={cookie}"))],
    )
    .await;
    let csrf = body
        .split("\"csrf_token\":\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .to_string();

    // 跨源 + 正确 CSRF：仍拒绝。
    let (status, _, _) = request(
        &stack.router,
        "POST",
        "/auth/logout",
        &[
            ("cookie", &format!("blog_session={cookie}")),
            ("x-csrf-token", &csrf),
            ("origin", "https://evil.example"),
            ("host", "127.0.0.1:18099"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "跨源写被拒绝");
}

#[tokio::test]
async fn unknown_provider_login_is_404() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (status, _, body) = request(
        &stack.router,
        "GET",
        "/auth/login?provider=ghost&next=/",
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn callback_without_browser_binding_is_rejected_and_keeps_attempt() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;

    let (status, login_headers, _) =
        request(&stack.router, "GET", "/auth/login?provider=idp&next=/", &[]).await;
    assert_eq!(status, StatusCode::TEMPORARY_REDIRECT);
    let location = login_headers
        .iter()
        .find(|(k, _)| k == "location")
        .map(|(_, v)| v.clone())
        .unwrap();
    let state = location
        .split("state=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap()
        .to_string();
    let binding = set_cookie_named(&login_headers, "blog_oauth_state")
        .split(';')
        .next()
        .unwrap()
        .trim()
        .strip_prefix("blog_oauth_state=")
        .unwrap()
        .to_string();

    // 攻击者把回调 URL 塞给受害者：受害者浏览器没有绑定 cookie → 拒绝。
    let (status, _, _) = request(
        &stack.router,
        "GET",
        &format!("/auth/callback/idp?code=abc&state={state}"),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "缺少浏览器绑定必须拒绝");

    // 尝试未被烧掉：同一浏览器随后仍可完成登录。
    let (status, headers, _) = request(
        &stack.router,
        "GET",
        &format!("/auth/callback/idp?code=abc&state={state}"),
        &[("cookie", &format!("blog_oauth_state={binding}"))],
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(cookie_value(&headers).is_some(), "正常浏览器仍能完成登录");
}

#[tokio::test]
async fn secure_deployment_marks_cookies_secure_and_host_prefixed() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack_with(true).await;

    let (status, login_headers, _) =
        request(&stack.router, "GET", "/auth/login?provider=idp&next=/", &[]).await;
    assert_eq!(status, StatusCode::TEMPORARY_REDIRECT);

    // Secure 部署的绑定 cookie 使用 __Host- 前缀（host-only + Path=/）。
    let binding_cookie = set_cookie_named(&login_headers, "__Host-blog_oauth_state");
    assert!(binding_cookie.contains("Secure"), "{binding_cookie}");
    assert!(binding_cookie.contains("Path=/"), "{binding_cookie}");
    assert!(!binding_cookie.contains("Domain"), "{binding_cookie}");

    let location = login_headers
        .iter()
        .find(|(k, _)| k == "location")
        .map(|(_, v)| v.clone())
        .unwrap();
    let state = location
        .split("state=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap()
        .to_string();
    let binding = binding_cookie
        .split(';')
        .next()
        .unwrap()
        .trim()
        .strip_prefix("__Host-blog_oauth_state=")
        .unwrap()
        .to_string();

    let (status, callback_headers, _) = request(
        &stack.router,
        "GET",
        &format!("/auth/callback/idp?code=abc&state={state}"),
        &[("cookie", &format!("__Host-blog_oauth_state={binding}"))],
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let session_cookie = set_cookie_named(&callback_headers, "blog_session");
    assert!(
        session_cookie.contains("; Secure"),
        "HTTPS 部署的会话 cookie 必须 Secure：{session_cookie}"
    );
}

#[tokio::test]
async fn providers_endpoint_is_public_minimal_and_no_store() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;

    // 匿名可访问，不需要 cookie。
    let (status, headers, body) = request(&stack.router, "GET", "/auth/providers", &[]).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        headers
            .iter()
            .find(|(k, _)| k == "cache-control")
            .map(|(_, v)| v.as_str()),
        Some("no-store"),
        "登录页数据不得缓存"
    );
    assert!(body.contains(r#""id":"idp""#), "{body}");
    assert!(body.contains(r#""name":"示例 IdP""#), "{body}");
    assert!(body.contains(r#""kind":"oidc""#), "{body}");

    // 不得泄漏任何配置细节。
    for leaked in [
        "client",
        "IDP_SECRET",
        "idp.example",
        "secret_ref",
        "issuer",
    ] {
        assert!(!body.contains(leaked), "响应不得包含 {leaked}：{body}");
    }
}
