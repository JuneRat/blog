//! 本地密码 HTTP 全链路：`POST /auth/login/password` → 会话 cookie → `/me` →
//! 自助改密（重新认证 + 会话轮换）；以及限流、跨源与错误契约。
//!
//! 使用真实 PostgreSQL 与真实 Argon2id（生产参数），只把网络/OAuth 部件留在测试外。

mod common;

use std::net::SocketAddr;
use std::sync::Arc;

use application::auth::{AuthDeps, AuthInteractor};
use application::identity::{Actor, CreateUserCmd, RoleInteractor, UserInteractor};
use application::page::PageInteractor;
use application::ports::{
    Clock, ExternalIdentity, ExternalIdentityClient, OAuthAccountStore, OAuthConfigStore,
    ProviderConfig, ProviderKind, SecureRandom, SessionStore,
};
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use axum::middleware;
use http_body_util::BodyExt;
use infrastructure::{
    InMemoryLoginThrottle, InMemoryOAuthAttemptStore, PostgresCategoryRepository,
    PostgresOAuthAccountStore, PostgresOAuthConfigStore, PostgresPageRepository,
    PostgresPostRepository, PostgresRbacStore, PostgresSessionStore, PostgresTagRepository,
    PostgresUserRepository, SystemClock, ThrottleConfig,
};
use interfaces::http_auth::{AdminState, AuthState, admin_router, auth_router};
use interfaces::http_identity::identity_router;
use interfaces::http_support::request_context;
use sqlx::PgPool;
use tower::ServiceExt;

/// 各测试重建同一数据库，必须串行。
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 满足密码策略（≥12 字符且不含用户名 "sun"）。
const PASSWORD: &str = "harbor-lantern-2026";
const NEW_PASSWORD: &str = "quiet-mountain-2027";

/// 密码登录不涉及外部身份服务；给出一个不会被调用的实现。
struct UnusedIdentityClient;

#[async_trait::async_trait]
impl ExternalIdentityClient for UnusedIdentityClient {
    async fn authorize_url(
        &self,
        _config: &ProviderConfig,
        _state: &str,
        _challenge: Option<&str>,
        _nonce: Option<&str>,
        _redirect_uri: &str,
    ) -> Result<String, application::error::UseCaseError> {
        unreachable!("密码用例不发起 OAuth 登录")
    }

    async fn exchange(
        &self,
        _config: &ProviderConfig,
        _code: &str,
        _verifier: Option<&str>,
        _nonce: Option<&str>,
        _redirect_uri: &str,
    ) -> Result<ExternalIdentity, application::error::UseCaseError> {
        unreachable!("密码用例不交换授权码")
    }
}

struct TestRandom;

impl SecureRandom for TestRandom {
    fn token_hex(&self) -> Result<String, application::error::UseCaseError> {
        Ok(uuid::Uuid::now_v7().simple().to_string())
    }
    fn pkce_s256(&self, _verifier: &str) -> Result<String, application::error::UseCaseError> {
        Ok("challenge".into())
    }
}

struct Stack {
    router: axum::Router,
    user_id: uuid::Uuid,
    pool: PgPool,
    roles: Arc<RoleInteractor>,
}

async fn fresh_stack() -> Stack {
    fresh_stack_with(ThrottleConfig::default()).await
}

async fn fresh_stack_with(throttle_config: ThrottleConfig) -> Stack {
    let pool = common::fresh_database("blog_password_test").await;

    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let user_repo = Arc::new(PostgresUserRepository::new(common::database(pool.clone())));
    let rbac = Arc::new(PostgresRbacStore::new(common::database(pool.clone())));
    let roles = Arc::new(RoleInteractor::new(rbac.clone(), user_repo.clone()));
    roles.sync_registry().await.expect("同步权限目录失败");
    let users = Arc::new(UserInteractor::new(
        application::identity::UserStores {
            query: user_repo.clone(),
            profiles: user_repo.clone(),
            accounts: user_repo.clone(),
        },
        rbac,
        clock.clone(),
        common::media_guard(pool.clone()),
    ));

    let member = users
        .create_user(
            &Actor::bootstrap_cli(),
            CreateUserCmd {
                username: "sun".into(),
                email: None,
                display_name: Some("Sun".into()),
            },
        )
        .await
        .unwrap();
    roles
        .assign_to_username(&Actor::bootstrap_cli(), "sun", "author")
        .await
        .unwrap();

    let sessions: Arc<dyn SessionStore> = Arc::new(PostgresSessionStore::with_defaults(
        common::database(pool.clone()),
    ));
    let accounts: Arc<dyn OAuthAccountStore> = Arc::new(PostgresOAuthAccountStore::new(
        common::database(pool.clone()),
    ));
    let passwords = common::password_interactor_with_throttle(
        user_repo.clone(),
        sessions.clone(),
        Arc::new(InMemoryLoginThrottle::new(
            throttle_config,
            Box::new(time::OffsetDateTime::now_utc),
        )),
    );
    // 受控 CLI 设置初始密码（与 `blog user passwd` 同一用例）。
    passwords
        .set_password(&Actor::bootstrap_cli(), "sun", PASSWORD)
        .await
        .unwrap();

    // OAuth 侧装配保持可用（本文件不触发），避免测试栈与生产结构偏离。
    let configs: Arc<dyn OAuthConfigStore> = Arc::new(PostgresOAuthConfigStore::new(
        common::database(pool.clone()),
    ));
    configs
        .save(
            &[ProviderConfig {
                id: "idp".into(),
                name: Some("示例 IdP".into()),
                kind: ProviderKind::Oidc,
                issuer: Some("https://idp.example".into()),
                client_id: "client".into(),
                secret_ref: "IDP_SECRET".into(),
                scopes: vec![],
            }],
            0,
            None.into(),
        )
        .await
        .unwrap();
    accounts
        .bind(
            member.id,
            "https://idp.example",
            "sub-sun",
            None,
            None.into(),
        )
        .await
        .unwrap();

    let auth = Arc::new(AuthInteractor::new(
        AuthDeps {
            sessions: sessions.clone(),
            attempts: Arc::new(InMemoryOAuthAttemptStore::with_defaults()),
            configs,
            accounts: accounts.clone(),
            identity_client: Arc::new(UnusedIdentityClient),
            random: Arc::new(TestRandom),
        },
        users.clone(),
        clock,
        "http://127.0.0.1:18099".into(),
    ));

    let tag_repo: Arc<dyn application::ports::TagRepository> =
        Arc::new(PostgresTagRepository::new(common::database(pool.clone())));
    let posts = Arc::new(application::content::PostInteractor::new(
        Arc::new(PostgresPostRepository::new(
            common::database(pool.clone()),
            Arc::new(infrastructure::RenderingRuntime::default()),
        )),
        tag_repo.clone(),
        Arc::new(PostgresCategoryRepository::new(common::database(
            pool.clone(),
        ))),
        Arc::new(infrastructure::PostgresSeriesRepository::new(
            common::database(pool.clone()),
        )),
        Arc::new(SystemClock),
        common::media_guard(pool.clone()),
    ));
    let pages = Arc::new(PageInteractor::new(
        Arc::new(PostgresPageRepository::new(
            common::database(pool.clone()),
            Arc::new(infrastructure::RenderingRuntime::default()),
        )),
        Arc::new(SystemClock),
    ));

    let auth_state = AuthState {
        auth: auth.clone(),
        passwords: passwords.clone(),
        secure_cookies: false,
    };
    let admin_state = AdminState {
        content_queries: common::content_queries(&pool),
        auth,
        users,
        passwords,
        posts,
        pages,
        tags: Arc::new(application::tag::TagInteractor::new(
            tag_repo,
            Arc::new(SystemClock),
        )),
        categories: Arc::new(application::category::CategoryInteractor::new(
            Arc::new(PostgresCategoryRepository::new(common::database(
                pool.clone(),
            ))),
            Arc::new(SystemClock),
        )),
        series: Arc::new(application::series::SeriesInteractor::new(
            Arc::new(infrastructure::PostgresSeriesRepository::new(
                common::database(pool.clone()),
            )),
            Arc::new(SystemClock),
            common::media_guard(pool.clone()),
        )),
        settings: Arc::new(application::settings::SettingsInteractor::new(
            Arc::new(infrastructure::PostgresSettingsStore::new(
                common::database(pool.clone()),
            )),
            Arc::new(SystemClock),
            application::site_info::SiteInfo {
                time_zone: "UTC".into(),
                title: "测试站点".into(),
                description: "测试描述".into(),
                logo_url: None,
            },
            common::media_guard(pool.clone()),
        )),
        roles: roles.clone(),
        media: common::media_interactor(pool.clone(), common::media_dir("password")),
        secure_cookies: false,
    };
    let router = auth_router(auth_state)
        .merge(identity_router(admin_state.clone()))
        .merge(admin_router(admin_state))
        .layer(middleware::from_fn(request_context));

    Stack {
        router,
        user_id: member.id,
        pool,
        roles,
    }
}

/// 发送请求；`body` 为 JSON 值时带上 `Content-Type: application/json`。
async fn request(
    router: &axum::Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: Option<serde_json::Value>,
) -> (StatusCode, Vec<(String, String)>, serde_json::Value) {
    request_with_client(router, method, uri, headers, body, None).await
}

/// 同 [`request`]，但可注入 `ConnectInfo`（模拟真实 TCP 对端地址）。
///
/// 生产由 `into_make_service_with_connect_info` 注入；`oneshot` 不会，
/// 因此来源地址维度必须靠这里显式构造才测得到。
async fn request_with_client(
    router: &axum::Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: Option<serde_json::Value>,
    client: Option<SocketAddr>,
) -> (StatusCode, Vec<(String, String)>, serde_json::Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    if let Some(addr) = client {
        builder = builder.extension(ConnectInfo(addr));
    }
    let body = match body {
        Some(value) => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(value.to_string())
        }
        None => Body::empty(),
    };
    let response = router
        .clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let mut captured: Vec<(String, String)> = response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .map(|s| ("set-cookie".to_string(), s.to_string()))
        .collect();
    for name in [header::LOCATION, header::CACHE_CONTROL, header::RETRY_AFTER] {
        if let Some(value) = response.headers().get(&name).and_then(|v| v.to_str().ok()) {
            captured.push((name.as_str().to_string(), value.to_string()));
        }
    }
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, captured, json)
}

fn session_cookie(headers: &[(String, String)]) -> Option<String> {
    headers
        .iter()
        .filter(|(k, _)| k == "set-cookie")
        .find_map(|(_, v)| {
            let pair = v.split(';').next()?.trim();
            pair.strip_prefix("blog_session=").map(str::to_string)
        })
}

fn response_header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

/// 密码登录成功并返回（会话 cookie，响应体）。
async fn login(
    stack: &Stack,
    password: &str,
) -> (StatusCode, Vec<(String, String)>, serde_json::Value) {
    request(
        &stack.router,
        "POST",
        "/auth/login/password",
        &[],
        Some(serde_json::json!({
            "username": "sun",
            "password": password,
            "next": "/admin/",
        })),
    )
    .await
}

async fn me(stack: &Stack, cookie: &str) -> (StatusCode, serde_json::Value) {
    let (status, _, body) = request(
        &stack.router,
        "GET",
        "/api/admin/v1/me",
        &[("cookie", &format!("blog_session={cookie}"))],
        None,
    )
    .await;
    (status, body)
}

#[tokio::test]
async fn password_login_issues_session_and_me_resolves_actor() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;

    let (status, headers, body) = login(&stack, PASSWORD).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["user_id"].as_str().unwrap(),
        stack.user_id.to_string(),
        "{body}"
    );
    assert_eq!(body["next"].as_str().unwrap(), "/admin/");
    assert_eq!(
        response_header(&headers, "cache-control"),
        Some("no-store"),
        "登录响应不得缓存"
    );

    let cookie = session_cookie(&headers).expect("登录成功必须签发会话 cookie");
    let set_cookie = headers
        .iter()
        .find(|(k, v)| k == "set-cookie" && v.contains("blog_session="))
        .map(|(_, v)| v.as_str())
        .unwrap();
    assert!(set_cookie.contains("HttpOnly"), "{set_cookie}");
    assert!(set_cookie.contains("SameSite=Lax"), "{set_cookie}");

    let (status, body) = me(&stack, &cookie).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p == "post.create")
    );
}

#[tokio::test]
async fn owner_can_edit_profile_keep_session_and_log_out() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    stack
        .roles
        .assign_to_username(&Actor::bootstrap_cli(), "sun", "owner")
        .await
        .unwrap();
    let (status, headers, _) = login(&stack, PASSWORD).await;
    assert_eq!(status, StatusCode::OK);
    let cookie = session_cookie(&headers).unwrap();
    let (_, profile) = me(&stack, &cookie).await;
    assert!(
        profile["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p == "ownership.manage")
    );
    let csrf = profile["csrf_token"].as_str().unwrap();
    let version = profile["version"].as_i64().unwrap();
    let revision: i64 = sqlx::query_scalar("SELECT auth_version FROM users WHERE id=$1")
        .bind(stack.user_id)
        .fetch_one(&stack.pool)
        .await
        .unwrap();
    let body = serde_json::json!({
        "display_name": "新的展示名", "bio": "个人简介", "expected_version": version,
    });
    let cookie_header = format!("blog_session={cookie}");
    let (status, _, _) = request(
        &stack.router,
        "PUT",
        "/api/admin/v1/me/profile",
        &[("cookie", &cookie_header)],
        Some(body.clone()),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "通用管理写入口拒绝缺失的 CSRF"
    );
    let (status, _, saved) = request_with_client(
        &stack.router,
        "PUT",
        "/api/admin/v1/me/profile",
        &[("cookie", &cookie_header), ("x-csrf-token", csrf)],
        Some(body.clone()),
        Some("198.51.100.24:4444".parse().unwrap()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["version"], version + 1);
    let (status, current) = me(&stack, &cookie).await;
    assert_eq!(status, StatusCode::OK, "资料编辑不撤销会话");
    assert_eq!(current["display_name"], "新的展示名");
    assert_eq!(current["bio"], "个人简介");
    let current_revision: i64 = sqlx::query_scalar("SELECT auth_version FROM users WHERE id=$1")
        .bind(stack.user_id)
        .fetch_one(&stack.pool)
        .await
        .unwrap();
    assert_eq!(current_revision, revision);
    let (status, _, _) = request(
        &stack.router,
        "PUT",
        "/api/admin/v1/me/profile",
        &[("cookie", &cookie_header), ("x-csrf-token", csrf)],
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "旧资料版本拒绝覆盖");
    let audits: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT metadata FROM audit_logs WHERE actor_id=$1 AND action='user.profile.update'",
    )
    .bind(stack.user_id)
    .fetch_all(&stack.pool)
    .await
    .unwrap();
    assert_eq!(audits, vec![serde_json::json!({"version": version + 1})]);
    let ip: Option<String> = sqlx::query_scalar("SELECT host(ip_address) FROM audit_logs WHERE actor_id=$1 AND action='user.profile.update'")
        .bind(stack.user_id).fetch_one(&stack.pool).await.unwrap();
    assert_eq!(ip.as_deref(), Some("198.51.100.24"));
    let (status, _, _) = request(
        &stack.router,
        "POST",
        "/auth/logout",
        &[("cookie", &cookie_header), ("x-csrf-token", csrf)],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(me(&stack, &cookie).await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn wrong_password_and_unknown_user_share_status_and_code() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;

    let (status, headers, body) = login(&stack, "not-the-password").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["code"].as_str().unwrap(), "invalid_credentials");
    assert!(
        session_cookie(&headers).is_none(),
        "失败不得签发会话 cookie"
    );

    let (status, _, unknown) = request(
        &stack.router,
        "POST",
        "/auth/login/password",
        &[],
        Some(serde_json::json!({ "username": "nobody", "password": "not-the-password" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(unknown["code"].as_str().unwrap(), "invalid_credentials");
    assert_eq!(
        body["error"], unknown["error"],
        "未知用户与密码错误必须回同一文案，避免用户名枚举"
    );
}

#[tokio::test]
async fn repeated_failures_are_rate_limited_with_retry_after() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;

    // 默认账号维度阈值 5 次：打满后即使密码正确也拒绝。
    for _ in 0..5 {
        let (status, _, _) = login(&stack, "not-the-password").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    let (status, headers, body) = login(&stack, PASSWORD).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert_eq!(body["code"].as_str().unwrap(), "rate_limited");
    let retry_after = response_header(&headers, "retry-after")
        .expect("限流响应必须带 Retry-After")
        .parse::<u64>()
        .unwrap();
    assert!(retry_after >= 1);
}

#[tokio::test]
async fn cross_origin_password_login_is_rejected() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;

    let (status, _, body) = request(
        &stack.router,
        "POST",
        "/auth/login/password",
        &[
            ("origin", "https://evil.example"),
            ("host", "127.0.0.1:18099"),
        ],
        Some(serde_json::json!({ "username": "sun", "password": PASSWORD })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

#[tokio::test]
async fn malformed_login_body_is_rejected_before_verification() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;

    // 缺少必填字段：JSON 提取器直接拒绝（422），不会进入凭据校验。
    let (status, _, _) = request(
        &stack.router,
        "POST",
        "/auth/login/password",
        &[],
        Some(serde_json::json!({ "username": "sun" })),
    )
    .await;
    assert!(
        matches!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY | StatusCode::BAD_REQUEST
        ),
        "缺字段必须被拒绝，得到 {status}"
    );
}

#[tokio::test]
async fn change_password_requires_csrf_and_current_password_then_rotates_session() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;

    let (_, headers, _) = login(&stack, PASSWORD).await;
    let cookie = session_cookie(&headers).unwrap();
    let (_, body) = me(&stack, &cookie).await;
    let csrf = body["csrf_token"].as_str().unwrap().to_string();

    // 缺少 CSRF → 403。
    let (status, _, _) = request(
        &stack.router,
        "POST",
        "/api/admin/v1/me/password",
        &[("cookie", &format!("blog_session={cookie}"))],
        Some(serde_json::json!({ "current_password": PASSWORD, "new_password": NEW_PASSWORD })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "改密必须校验 CSRF");

    // 当前密码错误 → 403 + invalid_credentials（不是 401，避免前端误判掉线）。
    let (status, _, body) = request(
        &stack.router,
        "POST",
        "/api/admin/v1/me/password",
        &[
            ("cookie", &format!("blog_session={cookie}")),
            ("x-csrf-token", &csrf),
        ],
        Some(serde_json::json!({ "current_password": "wrong-current", "new_password": NEW_PASSWORD })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["code"].as_str().unwrap(), "invalid_credentials");

    // 正确改密 → 200 + 新会话 cookie。
    let (status, headers, body) = request_with_client(
        &stack.router,
        "POST",
        "/api/admin/v1/me/password",
        &[
            ("cookie", &format!("blog_session={cookie}")),
            ("x-csrf-token", &csrf),
        ],
        Some(serde_json::json!({ "current_password": PASSWORD, "new_password": NEW_PASSWORD })),
        Some("[2001:db8::25]:4444".parse().unwrap()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let rotated = session_cookie(&headers).expect("改密后必须重签会话 cookie");
    assert_ne!(rotated, cookie, "会话必须轮换");
    let ip: Option<String> = sqlx::query_scalar("SELECT host(ip_address) FROM audit_logs WHERE actor_id=$1 AND action='user.password.set' ORDER BY created_at DESC,id DESC LIMIT 1")
        .bind(stack.user_id).fetch_one(&stack.pool).await.unwrap();
    assert_eq!(ip.as_deref(), Some("2001:db8::25"));

    // 旧会话已失效，新会话可用。
    let (status, _) = me(&stack, &cookie).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "改密后旧会话必须失效");
    let (status, _) = me(&stack, &rotated).await;
    assert_eq!(status, StatusCode::OK);

    // 新密码可用，旧密码不再可用。
    let (status, _, _) = login(&stack, NEW_PASSWORD).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = login(&stack, PASSWORD).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn change_password_without_current_password_is_rejected() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;

    // 已启用密码登录时，自助改密必须重新提供当前密码。
    let (_, headers, _) = login(&stack, PASSWORD).await;
    let cookie = session_cookie(&headers).unwrap();
    let (_, body) = me(&stack, &cookie).await;
    let csrf = body["csrf_token"].as_str().unwrap().to_string();

    let (status, _, body) = request(
        &stack.router,
        "POST",
        "/api/admin/v1/me/password",
        &[
            ("cookie", &format!("blog_session={cookie}")),
            ("x-csrf-token", &csrf),
        ],
        Some(serde_json::json!({ "new_password": NEW_PASSWORD })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"].as_str().unwrap(), "invalid_request");
    // 凭据未被改动：旧密码仍可登录。
    let (status, _, _) = login(&stack, PASSWORD).await;
    assert_eq!(status, StatusCode::OK);
}

/// 来源地址维度：不同用户名共享同一个来源地址计数，锁定只影响该地址。
///
/// 生产阈值是 50 次；这里把客户端阈值降到 2，避免为跑满阈值做 50 次真实 Argon2 校验。
#[tokio::test]
async fn client_address_dimension_locks_independently_of_username() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack_with(ThrottleConfig {
        client_max_failures: 2,
        ..ThrottleConfig::default()
    })
    .await;
    let client = SocketAddr::from(([203, 0, 113, 9], 5555));
    let other_client = SocketAddr::from(([198, 51, 100, 7], 4444));

    // 两个**不同**用户名的失败：账号维度各记 1（未达 5），来源地址维度累计到 2 → 锁定。
    for username in ["sun", "nobody"] {
        let (status, _, _) = request_with_client(
            &stack.router,
            "POST",
            "/auth/login/password",
            &[],
            Some(serde_json::json!({ "username": username, "password": "wrong-password-value" })),
            Some(client),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{username}");
    }

    // 同一来源地址再用正确密码：被来源地址维度拦下（且在哈希校验之前）。
    let (status, headers, body) = request_with_client(
        &stack.router,
        "POST",
        "/auth/login/password",
        &[],
        Some(serde_json::json!({ "username": "sun", "password": PASSWORD })),
        Some(client),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert_eq!(body["code"].as_str().unwrap(), "rate_limited");
    assert!(response_header(&headers, "retry-after").is_some());

    // 换一个来源地址不受影响：证明限流确实按地址分桶，而不是全局。
    let (status, headers, _) = request_with_client(
        &stack.router,
        "POST",
        "/auth/login/password",
        &[],
        Some(serde_json::json!({ "username": "sun", "password": PASSWORD })),
        Some(other_client),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(session_cookie(&headers).is_some());
}

/// 跨进程改密必须让运行中服务的旧会话失效。
///
/// 认证版本由数据库统一判定；即使没有物理删除会话行，旧快照也不得继续使用。
#[tokio::test]
async fn session_is_invalidated_by_out_of_process_identity_change() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;

    let (_, headers, _) = login(&stack, PASSWORD).await;
    let cookie = session_cookie(&headers).unwrap();
    let (status, _) = me(&stack, &cookie).await;
    assert_eq!(status, StatusCode::OK);

    sqlx::query("UPDATE users SET auth_version = auth_version + 1 WHERE id = $1")
        .bind(stack.user_id)
        .execute(&stack.pool)
        .await
        .unwrap();

    let (status, body) = me(&stack, &cookie).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "身份修订号变化后旧会话必须失效：{body}"
    );
}

#[tokio::test]
async fn trusted_proxy_login_and_password_change_use_client_buckets_without_forwarding_bypass() {
    let _g = SERIAL.lock().await;
    let mut stack = fresh_stack_with(ThrottleConfig {
        client_max_failures: 2,
        ..Default::default()
    })
    .await;
    let proxy: SocketAddr = "127.0.0.1:9000".parse().unwrap();
    stack.router = stack
        .router
        .layer(axum::Extension(interfaces::http_client_ip::TrustedProxies(
            vec![proxy.ip()],
        )));
    for (user, forwarded) in [
        ("sun", "203.0.113.1"),
        ("nobody", "198.51.100.1, 203.0.113.1"),
    ] {
        assert_eq!(
            request_with_client(
                &stack.router,
                "POST",
                "/auth/login/password",
                &[("x-forwarded-for", forwarded)],
                Some(serde_json::json!({"username":user,"password":"wrong-password-value"})),
                Some(proxy)
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED
        );
    }
    let body = || Some(serde_json::json!({"username":"sun","password":PASSWORD}));
    assert_eq!(
        request_with_client(
            &stack.router,
            "POST",
            "/auth/login/password",
            &[("x-forwarded-for", "203.0.113.1")],
            body(),
            Some(proxy)
        )
        .await
        .0,
        StatusCode::TOO_MANY_REQUESTS
    );
    let (status, headers, _) = request_with_client(
        &stack.router,
        "POST",
        "/auth/login/password",
        &[("x-forwarded-for", "2001:db8::2")],
        body(),
        Some(proxy),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let cookie = session_cookie(&headers).unwrap();
    let (_, me) = me(&stack, &cookie).await;
    let csrf = me["csrf_token"].as_str().unwrap();
    let cookie = format!("blog_session={cookie}");
    // Reauthentication uses the same client key; another client is still usable.
    for _ in 0..2 {
        assert_eq!(request_with_client(&stack.router, "POST", "/api/admin/v1/me/password",
            &[("cookie", &cookie), ("x-csrf-token", csrf), ("x-forwarded-for", "203.0.113.3")],
            Some(serde_json::json!({"current_password":"wrong-password-value","new_password":NEW_PASSWORD})), Some(proxy)).await.0,
            StatusCode::FORBIDDEN);
    }
    for (ip, status) in [
        ("203.0.113.3", StatusCode::TOO_MANY_REQUESTS),
        ("203.0.113.4", StatusCode::OK),
    ] {
        assert_eq!(
            request_with_client(
                &stack.router,
                "POST",
                "/api/admin/v1/me/password",
                &[
                    ("cookie", &cookie),
                    ("x-csrf-token", csrf),
                    ("x-forwarded-for", ip)
                ],
                Some(serde_json::json!({"current_password":PASSWORD,"new_password":NEW_PASSWORD})),
                Some(proxy)
            )
            .await
            .0,
            status
        );
    }
    let ip: Option<String> = sqlx::query_scalar("SELECT host(ip_address) FROM audit_logs WHERE actor_id=$1 AND action='user.password.set' ORDER BY created_at DESC,id DESC LIMIT 1")
        .bind(stack.user_id).fetch_one(&stack.pool).await.unwrap();
    assert_eq!(ip.as_deref(), Some("203.0.113.4"));
    // Untrusted peers cannot obtain fresh buckets by forging forwarding headers.
    let stranger = "192.0.2.10:4321".parse().unwrap();
    for (n, expected) in [
        (1, StatusCode::UNAUTHORIZED),
        (2, StatusCode::UNAUTHORIZED),
        (3, StatusCode::TOO_MANY_REQUESTS),
    ] {
        assert_eq!(request_with_client(&stack.router, "POST", "/auth/login/password",
            &[("x-forwarded-for", &format!("203.0.113.{n}"))],
            Some(serde_json::json!({"username":format!("unknown{n}"),"password":"wrong-password-value"})), Some(stranger)).await.0, expected);
    }
    // Missing/invalid chains share the proxy fallback bucket, never omit limiting.
    for (value, expected) in [
        ("bad", StatusCode::UNAUTHORIZED),
        ("127.0.0.1", StatusCode::UNAUTHORIZED),
        ("", StatusCode::TOO_MANY_REQUESTS),
    ] {
        assert_eq!(request_with_client(&stack.router, "POST", "/auth/login/password",
            &[("x-forwarded-for", value)],
            Some(serde_json::json!({"username":"unknown-fallback","password":"wrong-password-value"})), Some(proxy)).await.0, expected);
    }
}

/// 同一个会话逐次读取最新角色权限，角色变更不撤销登录。
#[tokio::test]
async fn role_change_updates_permissions_without_invalidating_session() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;

    let (_, headers, _) = login(&stack, PASSWORD).await;
    let cookie = session_cookie(&headers).unwrap();
    assert_eq!(me(&stack, &cookie).await.0, StatusCode::OK);

    // 走真实用例修改角色，用原 cookie 验证权限立即更新。
    stack
        .roles
        .assign_to_username(&Actor::bootstrap_cli(), "sun", "editor")
        .await
        .unwrap();

    let (status, body) = me(&stack, &cookie).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p == "post.update_any"),
        "原会话应带上 editor 权限：{body}"
    );
    stack
        .roles
        .remove_from_username(&Actor::bootstrap_cli(), "sun", "editor")
        .await
        .unwrap();
    let (status, body) = me(&stack, &cookie).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !body["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p == "post.update_any")
    );
}

/// 并发登录不得突破失败次数上限。
///
/// 旧实现「先检查、再慢慢校验、最后才记失败」在并发下会全部放行：
/// 12 个并发请求会做 12 次 Argon2 猜测。预占模型下最多只放行阈值内的次数。
#[tokio::test]
async fn concurrent_logins_cannot_exceed_the_failure_budget() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;

    let mut set = tokio::task::JoinSet::new();
    for _ in 0..12 {
        let router = stack.router.clone();
        set.spawn(async move {
            let (status, _, _) = request(
                &router,
                "POST",
                "/auth/login/password",
                &[],
                Some(serde_json::json!({
                    "username": "sun",
                    "password": "wrong-password-value",
                })),
            )
            .await;
            status
        });
    }

    let mut unauthorized = 0usize;
    let mut limited = 0usize;
    while let Some(joined) = set.join_next().await {
        match joined.unwrap() {
            StatusCode::UNAUTHORIZED => unauthorized += 1,
            StatusCode::TOO_MANY_REQUESTS => limited += 1,
            other => panic!("意料之外的状态码：{other}"),
        }
    }
    assert!(
        unauthorized <= 5,
        "并发下被校验的尝试不得超过阈值：{unauthorized} 次"
    );
    assert!(limited >= 1, "超出阈值的并发请求必须被限流");
    assert_eq!(unauthorized + limited, 12);
}

/// 重新认证同样受限流保护：被盗会话不能无限次试当前密码。
#[tokio::test]
async fn reauthentication_is_rate_limited() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;

    let (_, headers, _) = login(&stack, PASSWORD).await;
    let cookie = session_cookie(&headers).unwrap();
    let (_, body) = me(&stack, &cookie).await;
    let csrf = body["csrf_token"].as_str().unwrap().to_string();

    let attempt = |wrong: &'static str| {
        let router = stack.router.clone();
        let cookie = cookie.clone();
        let csrf = csrf.clone();
        async move {
            request(
                &router,
                "POST",
                "/api/admin/v1/me/password",
                &[
                    ("cookie", &format!("blog_session={cookie}")),
                    ("x-csrf-token", &csrf),
                ],
                Some(serde_json::json!({
                    "current_password": wrong,
                    "new_password": NEW_PASSWORD,
                })),
            )
            .await
            .0
        }
    };

    // 阈值 5：前 5 次是「当前密码不正确」（403），第 6 次直接限流。
    for _ in 0..5 {
        assert_eq!(attempt("not-the-current-one").await, StatusCode::FORBIDDEN);
    }
    assert_eq!(
        attempt("not-the-current-one").await,
        StatusCode::TOO_MANY_REQUESTS,
        "超过预算的重新认证必须被限流"
    );
}

/// 为账号管理测试建立独立操作者；目标 sun 仍通过真实密码登录。
async fn status_operator(stack: &Stack, username: &str, role: &str) -> (String, serde_json::Value) {
    use application::ports::{AccountAdministration, PasswordCredentialStore, UserQuery};
    let repo = PostgresUserRepository::new(common::database(stack.pool.clone()));
    let user =
        domain::identity::User::new(username, None, None, time::OffsetDateTime::now_utc()).unwrap();
    let id = user.id().0;
    repo.insert(&user, None.into()).await.unwrap();
    repo.set_password_hash(id, "$operator-test-password", None.into())
        .await
        .unwrap();
    stack
        .roles
        .assign_to_username(&Actor::bootstrap_cli(), username, role)
        .await
        .unwrap();
    let version = repo.find_by_id(id).await.unwrap().unwrap().auth_version;
    let cookie = PostgresSessionStore::with_defaults(common::database(stack.pool.clone()))
        .create(id, version)
        .await
        .unwrap();
    let (status, me) = me(stack, &cookie).await;
    assert_eq!(status, StatusCode::OK);
    (format!("blog_session={cookie}"), me)
}

#[tokio::test]
async fn disabling_blocks_password_login_and_enabling_requires_a_new_session() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (cookie, operator) = status_operator(&stack, "owner", "owner").await;
    let csrf = operator["csrf_token"].as_str().unwrap();
    let headers = [("cookie", cookie.as_str()), ("x-csrf-token", csrf)];
    let (_, original_headers, _) = login(&stack, PASSWORD).await;
    let old_cookie = session_cookie(&original_headers).unwrap();
    let (_, before) = me(&stack, &old_cookie).await;
    let path = format!("/api/admin/v1/users/{}/status", stack.user_id);
    let (status, result_headers, disabled) = request_with_client(
        &stack.router,
        "PUT",
        &path,
        &headers,
        Some(serde_json::json!({"status":"disabled","expected_version":before["version"]})),
        Some("198.51.100.25:1234".parse().unwrap()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{disabled}");
    assert_eq!(disabled["status"], "disabled");
    assert_eq!(
        response_header(&result_headers, "cache-control"),
        Some("no-store")
    );
    assert_eq!(me(&stack, &old_cookie).await.0, StatusCode::UNAUTHORIZED);
    let (status, _, rejected_login) = login(&stack, PASSWORD).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(rejected_login["code"], "invalid_credentials");
    let (status, _, list) =
        request(&stack.router, "GET", "/api/admin/v1/users", &headers, None).await;
    assert_eq!(status, StatusCode::OK);
    let target = list
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["id"] == stack.user_id.to_string())
        .unwrap();
    assert_eq!(target["status"], "disabled");
    assert_eq!(target["version"], disabled["version"]);
    assert_eq!(target["password_enabled"], true);
    assert_eq!(target["can_login"], false);
    let (status, _, enabled) = request(
        &stack.router,
        "PUT",
        &path,
        &headers,
        Some(serde_json::json!({"status":"active","expected_version":disabled["version"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{enabled}");
    assert_eq!(enabled["status"], "active");
    assert_eq!(me(&stack, &old_cookie).await.0, StatusCode::UNAUTHORIZED);
    let (status, new_headers, _) = login(&stack, PASSWORD).await;
    assert_eq!(status, StatusCode::OK);
    let new_cookie = session_cookie(&new_headers).unwrap();
    assert_ne!(old_cookie, new_cookie);
    assert_eq!(me(&stack, &new_cookie).await.0, StatusCode::OK);
    let audit: (String, String, serde_json::Value) = sqlx::query_as(
        "SELECT actor_id::text,host(ip_address),metadata FROM audit_logs WHERE action='user.status.update' AND metadata->>'to'='disabled'"
    ).fetch_one(&stack.pool).await.unwrap();
    assert_eq!(audit.0, operator["user_id"].as_str().unwrap());
    assert_eq!(audit.1, "198.51.100.25");
    assert_eq!(audit.2["from"], "active");
}

#[tokio::test]
async fn status_endpoint_enforces_csrf_permissions_owner_guard_and_versions() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (cookie, owner) = status_operator(&stack, "owner", "owner").await;
    let path = format!(
        "/api/admin/v1/users/{}/status",
        owner["user_id"].as_str().unwrap()
    );
    let body = serde_json::json!({"status":"disabled","expected_version":owner["version"]});
    assert_eq!(
        request(&stack.router, "PUT", &path, &[], Some(body.clone()))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(
            &stack.router,
            "PUT",
            &path,
            &[("cookie", &cookie)],
            Some(body.clone())
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let headers = [
        ("cookie", cookie.as_str()),
        ("x-csrf-token", owner["csrf_token"].as_str().unwrap()),
    ];
    let (status, _, response) =
        request(&stack.router, "PUT", &path, &headers, Some(body.clone())).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(response["code"], "last_owner");
    let mut stale = body.clone();
    stale["expected_version"] = serde_json::json!(owner["version"].as_i64().unwrap() - 1);
    let (status, _, response) = request(&stack.router, "PUT", &path, &headers, Some(stale)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(response["code"], "version_conflict");
    let mut invalid = body.clone();
    invalid["expected_version"] = serde_json::json!(0);
    assert_eq!(
        request(&stack.router, "PUT", &path, &headers, Some(invalid))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    for invalid in [
        serde_json::json!({"status":"deleted","expected_version":owner["version"]}),
        serde_json::json!({"status":"disabled"}),
        serde_json::json!({"status":"disabled","expected_version":owner["version"],"actor_id":owner["user_id"]}),
    ] {
        assert_eq!(
            request(&stack.router, "PUT", &path, &headers, Some(invalid))
                .await
                .0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    let mut cross_origin = headers.to_vec();
    cross_origin.extend([
        ("host", "blog.example"),
        ("origin", "https://elsewhere.example"),
    ]);
    assert_eq!(
        request(
            &stack.router,
            "PUT",
            &path,
            &cross_origin,
            Some(body.clone())
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (_, login_headers, _) = login(&stack, PASSWORD).await;
    let author_cookie = session_cookie(&login_headers).unwrap();
    let (_, author) = me(&stack, &author_cookie).await;
    let author_cookie = format!("blog_session={author_cookie}");
    let (status, _, response) = request(
        &stack.router,
        "PUT",
        &path,
        &[
            ("cookie", &author_cookie),
            ("x-csrf-token", author["csrf_token"].as_str().unwrap()),
        ],
        Some(body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(response["code"], "forbidden");
    let (admin_cookie, admin) = status_operator(&stack, "administrator", "admin").await;
    assert_eq!(
        request(
            &stack.router,
            "PUT",
            &path,
            &[
                ("cookie", &admin_cookie),
                ("x-csrf-token", admin["csrf_token"].as_str().unwrap())
            ],
            Some(body.clone()),
        )
        .await
        .0,
        StatusCode::FORBIDDEN,
        "Administrator 不能停用 Owner"
    );
    // 有另一位可登录 Owner 后允许本人停用；下一次请求必须重新认证。
    status_operator(&stack, "other-owner", "owner").await;
    assert_eq!(
        request(&stack.router, "PUT", &path, &headers, Some(body))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        request(&stack.router, "GET", "/api/admin/v1/me", &headers, None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
}
