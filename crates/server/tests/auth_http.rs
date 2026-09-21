//! 认证 HTTP 全链路：/auth/login → 假 IdP 回调 → 会话 cookie → /api/admin/v1/me → CSRF 登出。
//! 使用真实 PostgreSQL 与内存会话；外部身份客户端为 fake（不发出网络请求）。

use std::sync::{Arc, Mutex};

use application::auth::AuthInteractor;
use application::identity::{CreateUserCmd, RoleInteractor, UserInteractor};
use application::ports::{
    Clock, ExternalIdentity, ExternalIdentityClient, OAuthAccountStore, OAuthConfigStore,
    ProviderConfig, ProviderKind, SecureRandom, SessionStore,
};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use infrastructure::{
    InMemoryOAuthAttemptStore, InMemorySessionStore, PostgresOAuthAccountStore,
    PostgresOAuthConfigStore, PostgresRbacStore, PostgresUserRepository, SystemClock, connect,
    migrate,
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
    let admin = connect("postgres://blog:blog@127.0.0.1:5432/postgres")
        .await
        .expect("连接管理库失败");
    sqlx::raw_sql("DROP DATABASE IF EXISTS blog_auth_test WITH (FORCE)")
        .execute(&admin)
        .await
        .unwrap();
    sqlx::raw_sql("CREATE DATABASE blog_auth_test")
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;

    let pool = connect("postgres://blog:blog@127.0.0.1:5432/blog_auth_test")
        .await
        .expect("连接测试库失败");
    migrate(&pool, "../../migrations/postgres")
        .await
        .expect("迁移失败");

    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let user_repo = Arc::new(PostgresUserRepository::new(pool.clone()));
    let rbac = Arc::new(PostgresRbacStore::new(pool.clone()));
    let roles = Arc::new(RoleInteractor::new(rbac.clone(), user_repo.clone()));
    roles.sync_registry().await.expect("同步权限目录失败");
    let users = Arc::new(UserInteractor::new(user_repo.clone(), rbac, clock.clone()));

    // 用户 + author 角色 + 外部身份绑定。
    let member = users
        .create_user(CreateUserCmd {
            username: "httpuser".into(),
            email: None,
            display_name: Some("HTTP 用户".into()),
        })
        .await
        .unwrap();
    roles
        .assign_to_username("httpuser", "author")
        .await
        .unwrap();

    let configs = Arc::new(PostgresOAuthConfigStore::new(pool.clone()));
    configs
        .save(&[ProviderConfig {
            id: "idp".into(),
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

    let auth = Arc::new(AuthInteractor::new(
        application::auth::AuthDeps {
            sessions,
            attempts,
            configs,
            accounts,
            identity_client,
            random,
        },
        users.clone(),
        clock,
        "http://127.0.0.1:18099".into(),
    ));

    let auth_state = AuthState {
        auth: auth.clone(),
        secure_cookies: false,
    };
    let admin_state = AdminState {
        auth: auth.clone(),
        users,
    };

    let router = auth_router(auth_state).merge(admin_router(admin_state));
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
    let location = response
        .headers()
        .get(axum::http::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let mut captured = set_cookies;
    if let Some(loc) = location {
        captured.push(("location".to_string(), loc));
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

#[tokio::test]
async fn full_login_me_logout_round_trip() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;

    // 1. 发起登录 → 302 到假 IdP。
    let (status, headers, _) = request(
        &stack.router,
        "GET",
        "/auth/login?provider=idp&next=/admin",
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::TEMPORARY_REDIRECT);
    let location = headers
        .iter()
        .find(|(k, _)| k == "location")
        .map(|(_, v)| v.clone())
        .unwrap();
    assert!(location.starts_with("https://idp.example/authorize"));
    let state = location.split("state=").nth(1).unwrap().to_string();

    // 2. 回调 → 303 + 会话 cookie。
    let (status, headers, _) = request(
        &stack.router,
        "GET",
        &format!("/auth/callback/idp?code=abc&state={state}"),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER, "回调成功");
    let redirect = headers
        .iter()
        .find(|(k, _)| k == "location")
        .map(|(_, v)| v.clone())
        .unwrap();
    assert_eq!(redirect, "/admin", "回跳受控路径");
    let cookie = cookie_value(&headers).expect("签发会话 cookie");
    assert!(!cookie.is_empty());

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
    let (_, headers, _) =
        request(&stack.router, "GET", "/auth/login?provider=idp&next=/", &[]).await;
    let location = headers
        .iter()
        .find(|(k, _)| k == "location")
        .map(|(_, v)| v.clone())
        .unwrap();
    let state = location.split("state=").nth(1).unwrap().to_string();
    let (_, headers, _) = request(
        &stack.router,
        "GET",
        &format!("/auth/callback/idp?code=abc&state={state}"),
        &[],
    )
    .await;
    let cookie = cookie_value(&headers).unwrap();
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
