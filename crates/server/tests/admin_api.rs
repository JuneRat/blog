//! 管理写 API 集成测试：会话认证 + CSRF + own/any 授权 + 乐观并发。
//! 假 IdP 登录拿会话，走 JSON API 全流程。

use std::sync::{Arc, Mutex};

use application::auth::{AuthDeps, AuthInteractor};
use application::content::PostInteractor;
use application::identity::{CreateUserCmd, RoleInteractor, UserInteractor};
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
    SystemClock, connect, migrate,
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
    #[allow(dead_code)]
    pool: PgPool,
}

async fn fresh_stack() -> Stack {
    let admin = connect("postgres://blog:blog@127.0.0.1:5432/postgres")
        .await
        .expect("连接管理库失败");
    sqlx::raw_sql("DROP DATABASE IF EXISTS blog_admin_test WITH (FORCE)")
        .execute(&admin)
        .await
        .unwrap();
    sqlx::raw_sql("CREATE DATABASE blog_admin_test")
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;

    let pool = connect("postgres://blog:blog@127.0.0.1:5432/blog_admin_test")
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

    // author / editor / stranger 三个用户，各自绑定外部身份。
    let mut ids = std::collections::HashMap::new();
    for (username, role) in [
        ("author", Some("author")),
        ("editor", Some("editor")),
        ("stranger", None),
    ] {
        let user = users
            .create_user(CreateUserCmd {
                username: username.into(),
                email: None,
                display_name: Some(username.into()),
            })
            .await
            .unwrap();
        if let Some(role) = role {
            roles.assign_to_username(username, role).await.unwrap();
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
    Stack { router, idp, pool }
}

/// 以指定用户登录，返回 (cookie, csrf)。
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
    let state = location.split("state=").nth(1).unwrap().to_string();

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/auth/callback/idp?code=x&state={state}"))
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
