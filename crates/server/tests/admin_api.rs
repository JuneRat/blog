//! 管理写 API 集成测试：会话认证 + CSRF + Origin + own/any 授权 + 乐观并发。
//! 假 IdP 登录拿会话，走 JSON API 全流程。

mod common;

use std::sync::{Arc, Mutex};

use application::auth::{AuthDeps, AuthInteractor};
use application::content::PostInteractor;
use application::identity::{Actor, CreateUserCmd, RoleInteractor, UserInteractor};
use application::ports::{
    Clock, ExternalIdentity, ExternalIdentityClient, OAuthAccountStore, OAuthConfigStore,
    ProviderConfig, ProviderKind, SecureRandom,
};
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use infrastructure::{
    InMemoryOAuthAttemptStore, InMemorySessionStore, PostgresOAuthAccountStore,
    PostgresOAuthConfigStore, PostgresPostRepository, PostgresRbacStore, PostgresUserRepository,
    SystemClock,
};
use interfaces::http_admin::posts_router;
use interfaces::http_auth::{AdminState, AuthState, admin_router, auth_router};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct FakeIdpClient {
    external_id: Mutex<String>,
}

#[async_trait]
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
            email: None,
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
    idp: Arc<FakeIdpClient>,
    roles: Arc<RoleInteractor>,
    #[allow(dead_code)]
    pool: PgPool,
}

async fn fresh_stack() -> Stack {
    let pool = common::fresh_database("blog_admin_test").await;

    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let user_repo = Arc::new(PostgresUserRepository::new(pool.clone()));
    let rbac = Arc::new(PostgresRbacStore::new(pool.clone()));
    let roles = Arc::new(RoleInteractor::new(rbac.clone(), user_repo.clone()));
    roles.sync_registry().await.expect("同步权限目录失败");
    let users = Arc::new(UserInteractor::new(user_repo.clone(), rbac, clock.clone()));

    // author / author2 / editor / stranger 四个用户，各自绑定外部身份。
    let mut ids = std::collections::HashMap::new();
    for (username, role) in [
        ("author", Some("author")),
        ("author2", Some("author")),
        ("editor", Some("editor")),
        ("stranger", None),
    ] {
        let user = users
            .create_user(
                &Actor::bootstrap_cli(),
                CreateUserCmd {
                    username: username.into(),
                    email: None,
                    display_name: Some(username.into()),
                },
            )
            .await
            .unwrap();
        if let Some(role) = role {
            roles
                .assign_to_username(&Actor::bootstrap_cli(), username, role)
                .await
                .unwrap();
        }
        ids.insert(username.to_string(), user.id);
    }

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
    for (username, uid) in &ids {
        let external = format!("sub-{username}");
        accounts
            .bind(*uid, "https://idp.example", &external, None)
            .await
            .unwrap();
    }

    let idp = Arc::new(FakeIdpClient {
        external_id: Mutex::new("sub-author".into()),
    });
    let posts = Arc::new(PostInteractor::new(
        Arc::new(PostgresPostRepository::new(pool.clone())),
        clock.clone(),
    ));

    let auth = Arc::new(AuthInteractor::new(
        AuthDeps {
            sessions: Arc::new(InMemorySessionStore::with_defaults()),
            attempts: Arc::new(InMemoryOAuthAttemptStore::with_defaults()),
            configs,
            accounts,
            identity_client: idp.clone(),
            random: Arc::new(TestRandom),
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
        posts,
    };

    let router = auth_router(auth_state)
        .merge(admin_router(admin_state.clone()))
        .merge(posts_router(admin_state));
    Stack {
        router,
        idp,
        roles,
        pool,
    }
}

/// 以指定用户登录，返回 (cookie, csrf)。
/// 走完整浏览器绑定：login 下发的 `blog_oauth_state` cookie 必须带回 callback。
async fn login_as(router: &axum::Router, idp: &FakeIdpClient, username: &str) -> (String, String) {
    *idp.external_id.lock().unwrap() = format!("sub-{username}");
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/auth/login?provider=idp&next=/")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let location = response
        .headers()
        .get(axum::http::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap()
        .to_string();
    let state = location
        .split("state=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap()
        .to_string();
    let binding = response
        .headers()
        .get_all(axum::http::header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(|v| {
            v.split(';')
                .next()?
                .trim()
                .strip_prefix("blog_oauth_state=")
                .map(str::to_string)
        })
        .expect("登录必须下发浏览器绑定 cookie");
    assert_eq!(binding, state, "绑定值与 state 一致（双重提交）");

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/auth/callback/idp?code=x&state={state}"))
                .header("cookie", format!("blog_oauth_state={binding}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let cookie = response
        .headers()
        .get_all(axum::http::header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(|v| {
            v.split(';')
                .next()?
                .trim()
                .strip_prefix("blog_session=")
                .map(str::to_string)
        })
        .expect("会话 cookie");

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/admin/v1/me")
                .header("cookie", format!("blog_session={cookie}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = String::from_utf8_lossy(&response.into_body().collect().await.unwrap().to_bytes())
        .to_string();
    let csrf = body
        .split("\"csrf_token\":\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .to_string();
    (cookie, csrf)
}

async fn api(
    router: &axum::Router,
    method: &str,
    uri: &str,
    cookie: Option<&str>,
    csrf: Option<&str>,
    body: Option<&str>,
) -> (StatusCode, String) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(cookie) = cookie {
        builder = builder.header("cookie", format!("blog_session={cookie}"));
    }
    if let Some(csrf) = csrf {
        builder = builder.header("x-csrf-token", csrf);
    }
    let request = match body {
        Some(json) => builder
            .header("content-type", "application/json")
            .body(Body::from(json.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).to_string())
}

#[tokio::test]
async fn author_full_crud_round_trip() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (cookie, csrf) = login_as(&stack.router, &stack.idp, "author").await;

    // 创建草稿。
    let (status, body) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/posts",
        Some(&cookie),
        Some(&csrf),
        Some(r##"{"slug":"admin-post","title":"管理端文章","content":"# 你好\n正文","excerpt":"摘要"}"##),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(body.contains("\"slug\":\"admin-post\""));
    assert!(body.contains("\"version\":1"));

    // 读取自己的草稿。
    let (status, body) = api(
        &stack.router,
        "GET",
        "/api/admin/v1/posts/admin-post",
        Some(&cookie),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("\"status\":\"draft\""));

    // 编辑（带 expected_version）。
    let (status, body) = api(
        &stack.router,
        "PATCH",
        "/api/admin/v1/posts/admin-post",
        Some(&cookie),
        Some(&csrf),
        Some(r#"{"title":"管理端文章（改）","expected_version":1}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("\"version\":2"));

    // 发布。
    let (status, body) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/posts/admin-post/publish",
        Some(&cookie),
        Some(&csrf),
        Some(r#"{"expected_version":2}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("\"status\":\"published\""));

    // 重复发布幂等（版本不变）。
    let (status, body) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/posts/admin-post/publish",
        Some(&cookie),
        Some(&csrf),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("\"version\":3"), "幂等发布不递增版本");

    // 过期版本编辑 → 409。
    let (status, body) = api(
        &stack.router,
        "PATCH",
        "/api/admin/v1/posts/admin-post",
        Some(&cookie),
        Some(&csrf),
        Some(r#"{"title":"基于旧版本","expected_version":1}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "版本冲突：{body}");

    // 撤回。
    let (status, body) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/posts/admin-post/unpublish",
        Some(&cookie),
        Some(&csrf),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("\"status\":\"draft\""));

    // 列表（默认本人）。
    let (status, body) = api(
        &stack.router,
        "GET",
        "/api/admin/v1/posts",
        Some(&cookie),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("admin-post"));
}

#[tokio::test]
async fn csrf_and_session_are_enforced() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (cookie, csrf) = login_as(&stack.router, &stack.idp, "author").await;

    // 无会话 → 401。
    let (status, _) = api(
        &stack.router,
        "GET",
        "/api/admin/v1/posts/x",
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // 写方法缺 CSRF → 400。
    let (status, _) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/posts",
        Some(&cookie),
        None,
        Some(r#"{"title":"x","content":"y"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "写请求必须带 CSRF");

    // CSRF 错误值 → 400。
    let (status, _) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/posts",
        Some(&cookie),
        Some("wrong-csrf"),
        Some(r#"{"title":"x","content":"y"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // 读方法不需要 CSRF。
    let (status, _) = api(
        &stack.router,
        "GET",
        "/api/admin/v1/posts",
        Some(&cookie),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let _ = csrf;
}

#[tokio::test]
async fn own_any_authorization_matrix() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (author_cookie, author_csrf) = login_as(&stack.router, &stack.idp, "author").await;

    // author 建文章。
    let (status, _) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/posts",
        Some(&author_cookie),
        Some(&author_csrf),
        Some(r#"{"slug":"matrix-post","title":"越权矩阵","content":"内容"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // author2 有 own 权限但不是本人：读/改他人文章必须 403（测的是“不是本人”，不是“无权限”）。
    let (author2_cookie, author2_csrf) = login_as(&stack.router, &stack.idp, "author2").await;
    let (status, body) = api(
        &stack.router,
        "GET",
        "/api/admin/v1/posts/matrix-post",
        Some(&author2_cookie),
        None,
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "own 权限不得跨作者读取：{body}"
    );
    let (status, body) = api(
        &stack.router,
        "PATCH",
        "/api/admin/v1/posts/matrix-post",
        Some(&author2_cookie),
        Some(&author2_csrf),
        Some(r#"{"title":"越权编辑他人文章"}"#),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "own 权限不得跨作者编辑：{body}"
    );
    let (status, body) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/posts/matrix-post/publish",
        Some(&author2_cookie),
        Some(&author2_csrf),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "own 权限不得跨作者发布：{body}"
    );

    // stranger（无角色）：读/改/发都 403。
    let (stranger_cookie, stranger_csrf) = login_as(&stack.router, &stack.idp, "stranger").await;
    for (method, uri, with_csrf) in [
        ("GET", "/api/admin/v1/posts/matrix-post", false),
        ("PATCH", "/api/admin/v1/posts/matrix-post", true),
        ("POST", "/api/admin/v1/posts/matrix-post/publish", true),
    ] {
        let (status, body) = api(
            &stack.router,
            method,
            uri,
            Some(&stranger_cookie),
            if with_csrf {
                Some(stranger_csrf.as_str())
            } else {
                None
            },
            if method == "PATCH" {
                Some(r#"{"title":"越权"}"#)
            } else {
                Some("{}")
            },
        )
        .await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "{method} 无权限应 403：{body}"
        );
    }

    // editor（any 权限）：可读、可改、可发他人文章。
    let (editor_cookie, editor_csrf) = login_as(&stack.router, &stack.idp, "editor").await;
    let (status, body) = api(
        &stack.router,
        "GET",
        "/api/admin/v1/posts/matrix-post",
        Some(&editor_cookie),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "read_any 可读他人草稿：{body}");

    let (status, body) = api(
        &stack.router,
        "PATCH",
        "/api/admin/v1/posts/matrix-post",
        Some(&editor_cookie),
        Some(&editor_csrf),
        Some(r#"{"title":"编辑改写他人文章"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "update_any 可改他人文章：{body}");

    let (status, _) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/posts/matrix-post/publish",
        Some(&editor_cookie),
        Some(&editor_csrf),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "publish_any 可发他人文章");

    // editor 无 post.create：不能建文章。
    let (status, _) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/posts",
        Some(&editor_cookie),
        Some(&editor_csrf),
        Some(r#"{"title":"x","content":"y"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "editor 无 post.create");

    // author 读他人文章列表 → 403（无 read_any）。
    let (status, _) = api(
        &stack.router,
        "GET",
        "/api/admin/v1/posts?author=editor",
        Some(&author_cookie),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "无 read_any 不能列他人文章");
}

#[tokio::test]
async fn validation_and_no_store_headers() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (cookie, csrf) = login_as(&stack.router, &stack.idp, "author").await;

    // 非法 visibility → 400。
    let (status, body) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/posts",
        Some(&cookie),
        Some(&csrf),
        Some(r#"{"title":"x","content":"y","visibility":"secret"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // 发布空标题草稿 → 400（领域校验）。
    let (_, _) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/posts",
        Some(&cookie),
        Some(&csrf),
        Some(r#"{"slug":"empty-title","title":"","content":""}"#),
    )
    .await;
    let (status, body) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/posts/empty-title/publish",
        Some(&cookie),
        Some(&csrf),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "发布前校验内容：{body}");

    // 响应带 no-store。
    let response = stack
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/admin/v1/posts")
                .header("cookie", format!("blog_session={cookie}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response
            .headers()
            .get(axum::http::header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("no-store"),
        "管理 API 不缓存"
    );
}

#[tokio::test]
async fn admin_writes_validate_origin() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (cookie, csrf) = login_as(&stack.router, &stack.idp, "author").await;

    let post_with_origin = |origin: &'static str| {
        let cookie = cookie.clone();
        let csrf = csrf.clone();
        let router = stack.router.clone();
        async move {
            let response = router
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/admin/v1/posts")
                        .header("cookie", format!("blog_session={cookie}"))
                        .header("x-csrf-token", csrf)
                        .header("content-type", "application/json")
                        .header("origin", origin)
                        .header("host", "127.0.0.1:18099")
                        .body(Body::from(r#"{"title":"来源校验","content":"x"}"#))
                        .unwrap(),
                )
                .await
                .unwrap();
            response.status()
        }
    };

    assert_eq!(
        post_with_origin("http://127.0.0.1:18099").await,
        StatusCode::CREATED
    );
    assert_eq!(
        post_with_origin("https://evil.example").await,
        StatusCode::FORBIDDEN,
        "跨源写请求被拒绝"
    );
}

#[tokio::test]
async fn me_is_no_store_and_errors_share_json_contract() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;

    // 未认证 /me：401 + WWW-Authenticate + JSON 错误体 + no-store。
    let response = stack
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/admin/v1/me")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response
            .headers()
            .get(axum::http::header::WWW_AUTHENTICATE)
            .and_then(|v| v.to_str().ok()),
        Some("Session")
    );
    assert_eq!(
        response
            .headers()
            .get(axum::http::header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("no-store"),
        "带 CSRF token 的响应不得缓存"
    );
    assert_eq!(
        response
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.starts_with("application/json")),
        Some(true),
        "错误契约与其他管理端点一致（JSON）"
    );
    let body = String::from_utf8_lossy(&response.into_body().collect().await.unwrap().to_bytes())
        .to_string();
    assert!(body.contains("\"error\""), "{body}");

    // 已认证 /me 也带 no-store。
    let (cookie, _) = login_as(&stack.router, &stack.idp, "author").await;
    let response = stack
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/admin/v1/me")
                .header("cookie", format!("blog_session={cookie}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(axum::http::header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("no-store")
    );
}

#[tokio::test]
async fn foreign_cookie_does_not_shadow_session() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (cookie, _) = login_as(&stack.router, &stack.idp, "author").await;

    // 一个前缀相同、排在前面且为空的干扰 cookie 不得让正常会话被判未登录。
    let (status, body) = api(
        &stack.router,
        "GET",
        "/api/admin/v1/posts",
        Some(&cookie),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let response = stack
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/admin/v1/me")
                .header(
                    "cookie",
                    format!("blog_session_extra=1; blog_session=; blog_session={cookie}"),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "前缀干扰 cookie 不应提前中止解析"
    );
}

#[tokio::test]
async fn revoked_role_takes_effect_on_existing_session() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;

    // author 建草稿；editor 凭 read_any 能读到他人工况。
    let (author_cookie, author_csrf) = login_as(&stack.router, &stack.idp, "author").await;
    let (status, body) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/posts",
        Some(&author_cookie),
        Some(&author_csrf),
        Some(r#"{"slug":"revoke-post","title":"撤权测试","content":"正文"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (editor_cookie, _) = login_as(&stack.router, &stack.idp, "editor").await;
    let (status, body) = api(
        &stack.router,
        "GET",
        "/api/admin/v1/posts/revoke-post",
        Some(&editor_cookie),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "撤权前可读：{body}");

    // 撤权（同进程内真实 DB 变更，会话存储未动）。
    stack
        .roles
        .remove_from_username(&Actor::bootstrap_cli(), "editor", "editor")
        .await
        .unwrap();

    // 旧 cookie 仍在服务端会话存储中，但每次请求重读权限 → 立即失效。
    let (status, body) = api(
        &stack.router,
        "GET",
        "/api/admin/v1/posts/revoke-post",
        Some(&editor_cookie),
        None,
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "撤权后旧会话必须立即失去权限：{body}"
    );

    let (status, _, body) = request_me(&stack.router, &editor_cookie).await;
    assert_eq!(status, StatusCode::OK, "/me 仍是有效会话：{body}");
    assert!(
        !body.contains("post.read_any"),
        "撤权后 /me 不应再返回 any 权限：{body}"
    );
}

#[tokio::test]
async fn soft_deleted_user_session_is_rejected() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (cookie, _) = login_as(&stack.router, &stack.idp, "author").await;

    // 账号软删除（§5：软删除后旧 Cookie 不得继续可用）。
    sqlx::query("UPDATE users SET deleted_at = now() WHERE username = 'author'")
        .execute(&stack.pool)
        .await
        .unwrap();

    let (status, _, body) = request_me(&stack.router, &cookie).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "软删除后旧会话失效：{body}"
    );
}

/// 读取 /me 并返回 (状态, 头, 体)。
async fn request_me(
    router: &axum::Router,
    cookie: &str,
) -> (StatusCode, axum::http::HeaderMap, String) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/admin/v1/me")
                .header("cookie", format!("blog_session={cookie}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = String::from_utf8_lossy(&response.into_body().collect().await.unwrap().to_bytes())
        .to_string();
    (status, headers, body)
}

#[tokio::test]
async fn internal_errors_return_generic_body_without_leaking_storage_details() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (cookie, csrf) = login_as(&stack.router, &stack.idp, "author").await;

    // 制造真实存储错误：文章表被改名（INSERT 将失败）。
    sqlx::query("ALTER TABLE posts RENAME TO posts_broken")
        .execute(&stack.pool)
        .await
        .unwrap();

    let (status, body) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/posts",
        Some(&cookie),
        Some(&csrf),
        Some(r#"{"title":"x","content":"y"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert!(body.contains("服务器内部错误"), "回通用文案：{body}");
    assert!(
        !body.contains("relation") && !body.contains("does not exist"),
        "不得回显 SQL/存储细节：{body}"
    );
}
