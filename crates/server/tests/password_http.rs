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
    InMemoryLoginThrottle, InMemoryOAuthAttemptStore, InMemorySessionStore,
    PostgresCategoryRepository, PostgresOAuthAccountStore, PostgresOAuthConfigStore,
    PostgresPageRepository, PostgresPostRepository, PostgresRbacStore, PostgresTagRepository,
    PostgresUserRepository, SystemClock, ThrottleConfig,
};
use interfaces::http_auth::{AdminState, AuthState, admin_router, auth_router};
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
    let user_repo = Arc::new(PostgresUserRepository::new(pool.clone()));
    let rbac = Arc::new(PostgresRbacStore::new(pool.clone()));
    let roles = Arc::new(RoleInteractor::new(rbac.clone(), user_repo.clone()));
    roles.sync_registry().await.expect("同步权限目录失败");
    let users = Arc::new(UserInteractor::new(user_repo.clone(), rbac, clock.clone()));

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

    let sessions: Arc<dyn SessionStore> = Arc::new(InMemorySessionStore::with_defaults());
    let accounts: Arc<dyn OAuthAccountStore> =
        Arc::new(PostgresOAuthAccountStore::new(pool.clone()));
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
    let configs: Arc<dyn OAuthConfigStore> = Arc::new(PostgresOAuthConfigStore::new(pool.clone()));
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
    accounts
        .bind(member.id, "https://idp.example", "sub-sun", None)
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
        Arc::new(PostgresTagRepository::new(pool.clone()));
    let posts = Arc::new(application::content::PostInteractor::new(
        Arc::new(PostgresPostRepository::new(pool.clone())),
        tag_repo.clone(),
        Arc::new(PostgresCategoryRepository::new(pool.clone())),
        Arc::new(SystemClock),
    ));
    let pages = Arc::new(PageInteractor::new(
        Arc::new(PostgresPageRepository::new(pool.clone())),
        Arc::new(SystemClock),
    ));

    let auth_state = AuthState {
        auth: auth.clone(),
        passwords: passwords.clone(),
        secure_cookies: false,
    };
    let admin_state = AdminState {
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
            Arc::new(PostgresCategoryRepository::new(pool.clone())),
            Arc::new(SystemClock),
        )),
        roles: roles.clone(),
        secure_cookies: false,
    };
    let router = auth_router(auth_state)
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
    let (status, headers, body) = request(
        &stack.router,
        "POST",
        "/api/admin/v1/me/password",
        &[
            ("cookie", &format!("blog_session={cookie}")),
            ("x-csrf-token", &csrf),
        ],
        Some(serde_json::json!({ "current_password": PASSWORD, "new_password": NEW_PASSWORD })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let rotated = session_cookie(&headers).expect("改密后必须重签会话 cookie");
    assert_ne!(rotated, cookie, "会话必须轮换");

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
/// `blog user passwd` 是**另一个进程**，它调用 `revoke_all_for_user` 只会清空
/// 自己那份空的内存存储，对服务进程内的会话是空操作。真正生效的是数据库里的
/// `users.version`：会话签发时绑定它，校验时比对。这里直接改版本号来模拟
/// 「别处改了身份材料、但没碰到本进程内存」，不借助任何内存撤销。
#[tokio::test]
async fn session_is_invalidated_by_out_of_process_identity_change() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;

    let (_, headers, _) = login(&stack, PASSWORD).await;
    let cookie = session_cookie(&headers).unwrap();
    let (status, _) = me(&stack, &cookie).await;
    assert_eq!(status, StatusCode::OK);

    // 只动数据库：服务进程的内存会话仍在，撤销动作从未在这个进程里发生。
    sqlx::query("UPDATE users SET version = version + 1 WHERE id = $1")
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

/// 角色变更同样递增 users.version：授权变更即时生效，无需等待会话过期。
#[tokio::test]
async fn role_change_invalidates_existing_sessions() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;

    let (_, headers, _) = login(&stack, PASSWORD).await;
    let cookie = session_cookie(&headers).unwrap();
    assert_eq!(me(&stack, &cookie).await.0, StatusCode::OK);

    // 走真实用例（不触碰内存会话存储），只改数据库里的角色与版本号。
    stack
        .roles
        .assign_to_username(&Actor::bootstrap_cli(), "sun", "editor")
        .await
        .unwrap();

    assert_eq!(
        me(&stack, &cookie).await.0,
        StatusCode::UNAUTHORIZED,
        "角色变更后旧会话必须失效"
    );

    // 重新登录即可拿到新权限。
    let (status, headers, _) = login(&stack, PASSWORD).await;
    assert_eq!(status, StatusCode::OK);
    let fresh = session_cookie(&headers).unwrap();
    let (status, body) = me(&stack, &fresh).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p == "post.update_any"),
        "新会话应带上 editor 权限：{body}"
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
