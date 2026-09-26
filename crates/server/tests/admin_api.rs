//! 管理写 API 集成测试：会话认证 + CSRF + Origin + own/any 授权 + 乐观并发。
//! 假 IdP 登录拿会话，走 JSON API 全流程。

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use application::auth::{AuthDeps, AuthInteractor};
use application::content::PostInteractor;
use application::identity::{Actor, CreateUserCmd, RoleInteractor, UserInteractor};
use application::page::PageInteractor;
use application::ports::{
    Clock, ExternalIdentity, ExternalIdentityClient, OAuthAccountStore, OAuthConfigStore,
    ProviderConfig, ProviderKind, SecureRandom, SessionRecord, SessionStore,
};
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::middleware;
use http_body_util::BodyExt;
use infrastructure::{
    InMemoryOAuthAttemptStore, InMemorySessionStore, PostgresCategoryRepository,
    PostgresOAuthAccountStore, PostgresOAuthConfigStore, PostgresPageRepository,
    PostgresPostRepository, PostgresRbacStore, PostgresUserRepository, SystemClock,
};
use interfaces::http_admin::posts_router;
use interfaces::http_auth::{AdminState, AuthState, admin_router, auth_router};
use interfaces::http_support::request_context;
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
    /// 会话校验次数：`validate` 会刷新 `last_seen_at`，用于断言单请求只校验一次。
    session_validates: Arc<AtomicUsize>,
    #[allow(dead_code)]
    pool: PgPool,
}

/// 计数包装：`validate` 次数可观测，其余委托内存实现。
struct CountingSessionStore {
    inner: InMemorySessionStore,
    validates: Arc<AtomicUsize>,
}

#[async_trait]
impl SessionStore for CountingSessionStore {
    async fn create(
        &self,
        user_id: Uuid,
        user_version: i64,
    ) -> Result<String, application::error::UseCaseError> {
        self.inner.create(user_id, user_version).await
    }

    async fn validate(
        &self,
        token: &str,
    ) -> Result<Option<SessionRecord>, application::error::UseCaseError> {
        self.validates.fetch_add(1, Ordering::SeqCst);
        self.inner.validate(token).await
    }

    async fn revoke(&self, token: &str) -> Result<(), application::error::UseCaseError> {
        self.inner.revoke(token).await
    }

    async fn revoke_all_for_user(
        &self,
        user_id: Uuid,
    ) -> Result<(), application::error::UseCaseError> {
        self.inner.revoke_all_for_user(user_id).await
    }
}

async fn fresh_stack() -> Stack {
    let pool = common::fresh_database("blog_admin_test").await;

    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let user_repo = Arc::new(PostgresUserRepository::new(pool.clone()));
    let rbac = Arc::new(PostgresRbacStore::new(pool.clone()));
    let roles = Arc::new(RoleInteractor::new(rbac.clone(), user_repo.clone()));
    roles.sync_registry().await.expect("同步权限目录失败");
    let users = Arc::new(UserInteractor::new(
        user_repo.clone(),
        rbac,
        clock.clone(),
        common::media_guard(pool.clone()),
    ));

    // author / author2 / editor / stranger / admin / owner：覆盖内容 own/any、
    // 账号管理（user.manage + role.manage）与所有权（ownership.manage）三类边界。
    let mut ids = std::collections::HashMap::new();
    for (username, role) in [
        ("author", Some("author")),
        ("author2", Some("author")),
        ("editor", Some("editor")),
        ("stranger", None),
        ("admin", Some("admin")),
        ("owner", Some("owner")),
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
    let tag_repo: Arc<dyn application::ports::TagRepository> =
        Arc::new(infrastructure::PostgresTagRepository::new(pool.clone()));
    let category_repo: Arc<dyn application::ports::CategoryRepository> =
        Arc::new(PostgresCategoryRepository::new(pool.clone()));
    let series_repo: Arc<dyn application::ports::SeriesRepository> =
        Arc::new(infrastructure::PostgresSeriesRepository::new(pool.clone()));
    let posts = Arc::new(PostInteractor::new(
        Arc::new(PostgresPostRepository::new(
            pool.clone(),
            Arc::new(infrastructure::RenderingRuntime::default()),
        )),
        tag_repo.clone(),
        category_repo.clone(),
        series_repo.clone(),
        clock.clone(),
        common::media_guard(pool.clone()),
    ));
    let pages = Arc::new(PageInteractor::new(
        Arc::new(PostgresPageRepository::new(
            pool.clone(),
            Arc::new(infrastructure::RenderingRuntime::default()),
        )),
        clock.clone(),
    ));
    let tags = Arc::new(application::tag::TagInteractor::new(
        tag_repo,
        clock.clone(),
    ));
    let categories = Arc::new(application::category::CategoryInteractor::new(
        category_repo,
        clock.clone(),
    ));
    let series = Arc::new(application::series::SeriesInteractor::new(
        series_repo,
        clock.clone(),
        common::media_guard(pool.clone()),
    ));
    let settings = Arc::new(application::settings::SettingsInteractor::new(
        Arc::new(infrastructure::PostgresSettingsStore::new(pool.clone())),
        clock.clone(),
        application::public_site::SiteInfo {
            title: "测试站点".into(),
            description: "集成测试".into(),
            logo_url: None,
        },
        common::media_guard(pool.clone()),
    ));

    let session_validates = Arc::new(AtomicUsize::new(0));
    let sessions: Arc<dyn SessionStore> = Arc::new(CountingSessionStore {
        inner: InMemorySessionStore::with_defaults(),
        validates: session_validates.clone(),
    });
    let auth = Arc::new(AuthInteractor::new(
        AuthDeps {
            sessions: sessions.clone(),
            attempts: Arc::new(InMemoryOAuthAttemptStore::with_defaults()),
            configs,
            accounts: accounts.clone(),
            identity_client: idp.clone(),
            random: Arc::new(TestRandom),
        },
        users.clone(),
        clock,
        "http://127.0.0.1:18099".into(),
    ));
    let passwords = common::password_interactor(user_repo.clone(), sessions);

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
        tags,
        categories,
        series,
        settings,
        roles: roles.clone(),
        media: common::media_interactor(pool.clone(), common::media_dir("admin")),
        secure_cookies: false,
    };

    let router = auth_router(auth_state)
        .merge(admin_router(admin_state.clone()))
        .merge(posts_router(admin_state.clone()))
        .merge(interfaces::http_admin::pages_router(admin_state.clone()))
        .merge(interfaces::http_admin::tags_router(admin_state.clone()))
        .merge(interfaces::http_admin::categories_router(
            admin_state.clone(),
        ))
        .merge(interfaces::http_admin::series_router(admin_state.clone()))
        .merge(interfaces::http_comments::comments_router(
            interfaces::http_comments::CommentState {
                comments: Arc::new(application::comments::CommentInteractor::new(Arc::new(
                    infrastructure::comments::PostgresCommentRepository::new(pool.clone()),
                ))),
                admin: admin_state.clone(),
                origin: "http://127.0.0.1:18099".into(),
            },
        ))
        .layer(axum::Extension(axum::extract::ConnectInfo(
            "127.0.0.1:12345".parse::<std::net::SocketAddr>().unwrap(),
        )))
        .merge(interfaces::http_identity::identity_router(admin_state))
        // 与生产装配一致：最外层请求编号/日志中间件。
        .layer(middleware::from_fn(request_context));
    Stack {
        router,
        idp,
        roles,
        session_validates,
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

fn response_id(body: &str) -> Uuid {
    serde_json::from_str::<serde_json::Value>(body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

/// 管理请求只校验一次会话：提取器不再「先取记录、再解析 Actor」各校验一次。
/// 持久存储下每次 validate 都会写一次 `last_seen_at`，两次就是双倍写。
#[tokio::test]
async fn admin_request_validates_session_once() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (cookie, _csrf) = login_as(&stack.router, &stack.idp, "author").await;

    let before = stack.session_validates.load(Ordering::SeqCst);
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
    assert_eq!(
        stack.session_validates.load(Ordering::SeqCst) - before,
        1,
        "一个管理请求只应校验一次会话"
    );
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
    let post_id = response_id(&body);
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(body.contains("\"slug\":\"admin-post\""));
    assert!(body.contains("\"version\":1"));

    // 读取自己的草稿。
    let (status, body) = api(
        &stack.router,
        "GET",
        &format!("/api/admin/v1/posts/{post_id}"),
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
        &format!("/api/admin/v1/posts/{post_id}"),
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
        &format!("/api/admin/v1/posts/{post_id}/publish"),
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
        &format!("/api/admin/v1/posts/{post_id}/publish"),
        Some(&cookie),
        Some(&csrf),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("\"version\":3"), "幂等发布不递增版本");

    // 状态已经是 published，也不能让旧页面取得最新版本号并覆盖旧正文。
    let (status, body) = api(
        &stack.router,
        "POST",
        &format!("/api/admin/v1/posts/{post_id}/publish"),
        Some(&cookie),
        Some(&csrf),
        Some(r#"{"expected_version":1}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "幂等发布也校验版本：{body}");
    assert!(body.contains("\"code\":\"version_conflict\""), "{body}");

    // 过期版本编辑 → 409。
    let (status, body) = api(
        &stack.router,
        "PATCH",
        &format!("/api/admin/v1/posts/{post_id}"),
        Some(&cookie),
        Some(&csrf),
        Some(r#"{"title":"基于旧版本","expected_version":1}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "版本冲突：{body}");
    assert!(
        body.contains("\"code\":\"version_conflict\""),
        "409 必须带可区分的业务码：{body}"
    );

    // 撤回。
    let (status, body) = api(
        &stack.router,
        "POST",
        &format!("/api/admin/v1/posts/{post_id}/unpublish"),
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
async fn duplicate_slug_is_conflict_not_version_conflict() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (cookie, csrf) = login_as(&stack.router, &stack.idp, "author").await;

    let (status, body) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/posts",
        Some(&cookie),
        Some(&csrf),
        Some(r#"{"slug":"dup-slug","title":"第一版","content":"正文"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    // 同一 slug 再建一次：状态同为 409，但业务码必须是 conflict。
    // 否则前端会把它当版本冲突，弹出「内容已在别处修改」并给出重试仍失败的覆盖按钮。
    let (status, body) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/posts",
        Some(&cookie),
        Some(&csrf),
        Some(r#"{"slug":"dup-slug","title":"第二版","content":"正文"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.contains("\"code\":\"conflict\""), "{body}");
    assert!(!body.contains("version_conflict"), "{body}");
    assert!(body.contains("slug"), "冲突文案应指向 slug：{body}");
}

#[tokio::test]
async fn request_id_is_present_on_auth_rejection_and_json_error() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;

    // 无会话 → 401 由认证提取器提前拒绝：响应头仍必须有编号。
    let response = stack
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/admin/v1/posts/{}", Uuid::now_v7()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let rejected_id = response
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .expect("提取器提前拒绝也必须带 x-request-id");
    assert!(Uuid::parse_str(&rejected_id).is_ok(), "{rejected_id}");

    // 有会话的业务错误 → 404 JSON：body 里的 request_id 必须等于响应头。
    let (cookie, _csrf) = login_as(&stack.router, &stack.idp, "author").await;
    let response = stack
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/admin/v1/posts/{}", Uuid::now_v7()))
                .header("cookie", format!("blog_session={cookie}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let header_id = response
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .unwrap()
        .to_string();
    let body = String::from_utf8_lossy(&response.into_body().collect().await.unwrap().to_bytes())
        .to_string();
    assert_ne!(rejected_id, header_id, "每个请求编号唯一");
    assert!(
        body.contains(&format!("\"request_id\":\"{header_id}\"")),
        "错误体编号必须等于响应头：{body}"
    );
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
        &format!("/api/admin/v1/posts/{}", Uuid::now_v7()),
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
    let (status, body) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/posts",
        Some(&author_cookie),
        Some(&author_csrf),
        Some(r#"{"slug":"matrix-post","title":"越权矩阵","content":"内容"}"#),
    )
    .await;
    let post_id = response_id(&body);
    assert_eq!(status, StatusCode::CREATED);

    // author2 有 own 权限但不是本人：读/改他人文章必须 403（测的是“不是本人”，不是“无权限”）。
    let (author2_cookie, author2_csrf) = login_as(&stack.router, &stack.idp, "author2").await;
    let (status, body) = api(
        &stack.router,
        "GET",
        &format!("/api/admin/v1/posts/{post_id}"),
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
        &format!("/api/admin/v1/posts/{post_id}"),
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
        &format!("/api/admin/v1/posts/{post_id}/publish"),
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
        ("GET", &format!("/api/admin/v1/posts/{post_id}"), false),
        ("PATCH", &format!("/api/admin/v1/posts/{post_id}"), true),
        (
            "POST",
            &format!("/api/admin/v1/posts/{post_id}/publish"),
            true,
        ),
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
        &format!("/api/admin/v1/posts/{post_id}"),
        Some(&editor_cookie),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "read_any 可读他人草稿：{body}");

    let (status, body) = api(
        &stack.router,
        "PATCH",
        &format!("/api/admin/v1/posts/{post_id}"),
        Some(&editor_cookie),
        Some(&editor_csrf),
        Some(r#"{"title":"编辑改写他人文章"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "update_any 可改他人文章：{body}");

    let (status, _) = api(
        &stack.router,
        "POST",
        &format!("/api/admin/v1/posts/{post_id}/publish"),
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
    let (_, body) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/posts",
        Some(&cookie),
        Some(&csrf),
        Some(r#"{"slug":"empty-title","title":"","content":""}"#),
    )
    .await;
    let post_id = response_id(&body);
    let (status, body) = api(
        &stack.router,
        "POST",
        &format!("/api/admin/v1/posts/{post_id}/publish"),
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
    let post_id = response_id(&body);
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (editor_cookie, _) = login_as(&stack.router, &stack.idp, "editor").await;
    let (status, body) = api(
        &stack.router,
        "GET",
        &format!("/api/admin/v1/posts/{post_id}"),
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

    // 会话绑定签发时的 users.version；撤权递增版本，旧 cookie 立即被判为未登录。
    // 这比「旧会话仍有效但权限变少」更强：撤权后不留可继续试探的会话。
    let (status, body) = api(
        &stack.router,
        "GET",
        &format!("/api/admin/v1/posts/{post_id}"),
        Some(&editor_cookie),
        None,
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "撤权后旧会话必须立即失效：{body}"
    );

    let (status, _, body) = request_me(&stack.router, &editor_cookie).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "撤权后 /me 不再接受旧会话：{body}"
    );

    // 重新登录：会话有效，但权限已按新角色集合计算（不再有 any 权限）。
    let (editor_cookie, _) = login_as(&stack.router, &stack.idp, "editor").await;
    let (status, _, body) = request_me(&stack.router, &editor_cookie).await;
    assert_eq!(status, StatusCode::OK, "重新登录应拿到有效会话：{body}");
    assert!(
        !body.contains("post.read_any"),
        "撤权后 /me 不应再返回 any 权限：{body}"
    );

    let (status, body) = api(
        &stack.router,
        "GET",
        &format!("/api/admin/v1/posts/{post_id}"),
        Some(&editor_cookie),
        None,
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "撤权并重新登录后仍读不到他人文章：{body}"
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

/// 从账号列表 JSON 里取某个用户名的条目；找不到直接失败（避免断言静默落空）。
fn listed_user(body: &str, username: &str) -> serde_json::Value {
    let users: Vec<serde_json::Value> = serde_json::from_str(body)
        .unwrap_or_else(|e| panic!("账号列表不是 JSON 数组：{e}：{body}"));
    users
        .into_iter()
        .find(|user| user["username"] == username)
        .unwrap_or_else(|| panic!("列表中没有 {username}：{body}"))
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

#[tokio::test]
async fn detail_returns_markdown_body_while_list_stays_summary() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (cookie, csrf) = login_as(&stack.router, &stack.idp, "author").await;

    let (status, body) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/posts",
        Some(&cookie),
        Some(&csrf),
        Some(r##"{"slug":"body-check","title":"正文","excerpt":"摘要","content":"# 标题\n\nBODYMARKER"}"##),
    )
    .await;
    let post_id = response_id(&body);
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(body.contains("BODYMARKER"), "创建响应带正文：{body}");
    assert!(body.contains(r#""excerpt":"摘要""#), "{body}");

    // 详情（后台编辑器数据源）。
    let (status, body) = api(
        &stack.router,
        "GET",
        &format!("/api/admin/v1/posts/{post_id}"),
        Some(&cookie),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body.contains("BODYMARKER"),
        "详情必须包含 Markdown 源文：{body}"
    );

    // 列表保持摘要形态，不携带正文。
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
    assert!(body.contains("body-check"));
    assert!(
        !body.contains("BODYMARKER") && !body.contains("\"content\""),
        "列表不应携带正文：{body}"
    );
}

#[tokio::test]
async fn page_full_crud_round_trip_with_site_level_permissions() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;

    // author 只有文章 own 权限：页面接口必须 403（Page 不套用文章归属规则）。
    let (author_cookie, _) = login_as(&stack.router, &stack.idp, "author").await;
    let (status, body) = api(
        &stack.router,
        "GET",
        "/api/admin/v1/pages",
        Some(&author_cookie),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    // editor 持有站点级 page.*：完整闭环。
    let (cookie, csrf) = login_as(&stack.router, &stack.idp, "editor").await;

    let (status, body) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/pages",
        Some(&cookie),
        Some(&csrf),
        Some(r##"{"slug":"about","title":"关于","content":"# 关于\n正文"}"##),
    )
    .await;
    let page_id = response_id(&body);
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(body.contains("\"slug\":\"about\""), "{body}");
    assert!(body.contains("\"version\":1"), "{body}");
    assert!(body.contains("\"status\":\"draft\""), "{body}");

    // 保留路径：400 且业务码是可校验的 invalid_request。
    let (status, body) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/pages",
        Some(&cookie),
        Some(&csrf),
        Some(r#"{"slug":"admin","title":"伪后台","content":"x"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("\"code\":\"invalid_request\""), "{body}");

    // 详情（含正文）与列表。
    let (status, body) = api(
        &stack.router,
        "GET",
        &format!("/api/admin/v1/pages/{page_id}"),
        Some(&cookie),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("# 关于"), "详情含 Markdown 源文：{body}");

    let (status, body) = api(
        &stack.router,
        "GET",
        "/api/admin/v1/pages",
        Some(&cookie),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("about"), "{body}");

    // 编辑：正确版本 → 2。
    let (status, body) = api(
        &stack.router,
        "PATCH",
        &format!("/api/admin/v1/pages/{page_id}"),
        Some(&cookie),
        Some(&csrf),
        Some(r#"{"title":"关于我们","expected_version":1}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("\"version\":2"), "{body}");

    // 过期版本 → 409 version_conflict（可重试覆盖）。
    let (status, body) = api(
        &stack.router,
        "PATCH",
        &format!("/api/admin/v1/pages/{page_id}"),
        Some(&cookie),
        Some(&csrf),
        Some(r#"{"title":"基于旧版本","expected_version":1}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.contains("\"code\":\"version_conflict\""), "{body}");

    // 发布 → published；重复发布幂等。
    let (status, body) = api(
        &stack.router,
        "POST",
        &format!("/api/admin/v1/pages/{page_id}/publish"),
        Some(&cookie),
        Some(&csrf),
        Some(r#"{"expected_version":2}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("\"status\":\"published\""), "{body}");
    let (status, body) = api(
        &stack.router,
        "POST",
        &format!("/api/admin/v1/pages/{page_id}/publish"),
        Some(&cookie),
        Some(&csrf),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("\"version\":3"), "幂等发布不递增版本：{body}");

    // 撤回 → draft。
    let (status, body) = api(
        &stack.router,
        "POST",
        &format!("/api/admin/v1/pages/{page_id}/unpublish"),
        Some(&cookie),
        Some(&csrf),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("\"status\":\"draft\""), "{body}");
}

// ---------------------------------------------------------------------------
// 用户与角色管理 API（http_identity）：授权边界、最后可登录 Owner、撤权会话失效
// ---------------------------------------------------------------------------

/// 无会话者、以及有会话但无 `user.manage`/`role.manage` 者，账号接口一律拒绝；
/// 有权限者也不能越过委派上限或所有权边界。
#[tokio::test]
async fn account_api_enforces_permission_boundaries() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;

    // 未登录：认证提取器先拒。
    let (status, body) = api(
        &stack.router,
        "GET",
        "/api/admin/v1/users",
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert!(body.contains("\"code\":\"unauthenticated\""), "{body}");

    // 有会话但没有账号管理权限：读列表同样拒绝（不泄漏用户名与邮箱）。
    for who in ["author", "stranger"] {
        let (cookie, csrf) = login_as(&stack.router, &stack.idp, who).await;
        for (method, uri, payload) in [
            ("GET", "/api/admin/v1/users", None),
            ("GET", "/api/admin/v1/roles", None),
            ("POST", "/api/admin/v1/users", Some(r#"{"username":"x"}"#)),
            ("PUT", "/api/admin/v1/users/author/roles/editor", None),
            ("DELETE", "/api/admin/v1/users/author/roles/author", None),
        ] {
            let (status, res) = api(
                &stack.router,
                method,
                uri,
                Some(&cookie),
                Some(&csrf),
                payload,
            )
            .await;
            assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "{who} {method} {uri} 应被拒绝：{res}"
            );
            assert!(res.contains("\"code\":\"forbidden\""), "{res}");
        }
    }

    // admin 持 user.manage + role.manage：可列出账号并创建，但不能越过权限边界。
    let (admin_cookie, admin_csrf) = login_as(&stack.router, &stack.idp, "admin").await;
    let (status, body) = api(
        &stack.router,
        "GET",
        "/api/admin/v1/users",
        Some(&admin_cookie),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("\"username\":\"author\""), "{body}");
    // 列表带可登录标记（界面据此在移除 Owner 前提示），且不含任何凭据材料。
    assert!(body.contains("\"can_login\":true"), "{body}");
    assert!(body.contains("\"roles\":[\"author\"]"), "{body}");
    assert!(
        !body.contains("password_hash") && !body.contains("$argon2"),
        "列表不得泄漏凭据：{body}"
    );

    // editor 的授权集合不是 admin 的子集 → 委派上限拒绝（即使持有 role.manage）。
    let (status, body) = api(
        &stack.router,
        "PUT",
        "/api/admin/v1/users/author/roles/editor",
        Some(&admin_cookie),
        Some(&admin_csrf),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body.contains("\"code\":\"forbidden\""), "{body}");

    // 授予 Owner 需要专门的 ownership.manage。
    let (status, body) = api(
        &stack.router,
        "PUT",
        "/api/admin/v1/users/author/roles/owner",
        Some(&admin_cookie),
        Some(&admin_csrf),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    // admin 可以创建账号，但角色仍需另行分配。
    let (status, body) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/users",
        Some(&admin_cookie),
        Some(&admin_csrf),
        Some(r#"{"username":"drafted","display_name":"待分配"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(body.contains("\"username\":\"drafted\""), "{body}");
}

/// 用户名与邮箱占用必须给出可区分的业务码，创建表单据此把错误定位到字段。
#[tokio::test]
async fn username_and_email_conflicts_reach_the_ui_as_distinct_codes() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (cookie, csrf) = login_as(&stack.router, &stack.idp, "admin").await;

    let (status, body) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/users",
        Some(&cookie),
        Some(&csrf),
        Some(r#"{"username":"newcomer","email":"newcomer@example.com","display_name":"新人"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    // 用户名占用。
    let (status, body) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/users",
        Some(&cookie),
        Some(&csrf),
        Some(r#"{"username":"newcomer","email":"other@example.com"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.contains("\"code\":\"username_taken\""), "{body}");
    assert!(body.contains("username"), "文案应指向用户名：{body}");

    // 邮箱占用（用户名不同）。
    let (status, body) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/users",
        Some(&cookie),
        Some(&csrf),
        Some(r#"{"username":"another","email":"newcomer@example.com"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.contains("\"code\":\"email_taken\""), "{body}");
    assert!(body.contains("email"), "文案应指向邮箱：{body}");

    // 规范化（trim + 小写）后仍是同一个用户名。
    let (status, body) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/users",
        Some(&cookie),
        Some(&csrf),
        Some(r#"{"username":"  NewComer  "}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.contains("\"code\":\"username_taken\""), "{body}");
}

/// 最后一个「可登录」Owner 的 Owner 角色不能被移除；登不进去的 Owner 不构成有效 Owner。
#[tokio::test]
async fn last_loginable_owner_is_protected_with_a_dedicated_code() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;

    // 直接建一个没有任何登录方式的 Owner：它不满足「可登录」，不构成有效 Owner。
    let ghost = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO users (id, username, display_name, version, created_at, updated_at) \
         VALUES ($1, 'ghost', '影子 Owner', 1, now(), now())",
    )
    .bind(ghost)
    .execute(&stack.pool)
    .await
    .unwrap();
    stack
        .roles
        .assign_to_username(&Actor::bootstrap_cli(), "ghost", "owner")
        .await
        .unwrap();

    let (cookie, csrf) = login_as(&stack.router, &stack.idp, "owner").await;

    // 列表把「可登录」暴露给界面，便于事前提示。
    let (status, body) = api(
        &stack.router,
        "GET",
        "/api/admin/v1/users",
        Some(&cookie),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body.contains("\"username\":\"ghost\"") && body.contains("\"can_login\":false"),
        "{body}"
    );
    // 后端给出全局结论：ghost 不可登录 → 不是「最后一个可登录 Owner」；
    // owner 是唯一可登录 Owner → 标记为受保护。
    assert_eq!(
        listed_user(&body, "ghost")["can_login"].as_bool(),
        Some(false)
    );
    assert_eq!(
        listed_user(&body, "ghost")["is_last_loginable_owner"].as_bool(),
        Some(false)
    );
    assert_eq!(
        listed_user(&body, "owner")["is_last_loginable_owner"].as_bool(),
        Some(true)
    );

    // 唯一可登录 Owner 被保护：专属业务码，而不是笼统的 forbidden。
    let (status, body) = api(
        &stack.router,
        "DELETE",
        "/api/admin/v1/users/owner/roles/owner",
        Some(&cookie),
        Some(&csrf),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body.contains("\"code\":\"last_owner\""), "{body}");
    assert!(!body.contains("\"code\":\"forbidden\""), "{body}");

    // 登不进去的 Owner 可以清理：它不减少可用 Owner。
    let (status, body) = api(
        &stack.router,
        "DELETE",
        "/api/admin/v1/users/ghost/roles/owner",
        Some(&cookie),
        Some(&csrf),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    // 出现第二个可登录 Owner 后，原 Owner 的 Owner 角色允许移除。
    let second = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO users (id, username, display_name, version, created_at, updated_at) \
         VALUES ($1, 'owner2', '第二 Owner', 1, now(), now())",
    )
    .bind(second)
    .execute(&stack.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO oauth_accounts (id, user_id, provider, provider_user_id, created_at, updated_at) \
         VALUES ($1, $2, 'https://idp.example', 'sub-owner2', now(), now())",
    )
    .bind(Uuid::now_v7())
    .bind(second)
    .execute(&stack.pool)
    .await
    .unwrap();
    stack
        .roles
        .assign_to_username(&Actor::bootstrap_cli(), "owner2", "owner")
        .await
        .unwrap();

    // 全局计数变为 2：两个 Owner 都不再被标记为「最后一个可登录 Owner」。
    let (status, body) = api(
        &stack.router,
        "GET",
        "/api/admin/v1/users",
        Some(&cookie),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        listed_user(&body, "owner")["is_last_loginable_owner"].as_bool(),
        Some(false)
    );
    assert_eq!(
        listed_user(&body, "owner2")["is_last_loginable_owner"].as_bool(),
        Some(false)
    );

    let (status, body) = api(
        &stack.router,
        "DELETE",
        "/api/admin/v1/users/owner/roles/owner",
        Some(&cookie),
        Some(&csrf),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "有第二个可登录 Owner 时应允许：{body}"
    );
}

/// 回归：`is_last_loginable_owner` 必须按全站计数判定，不能只看当前页。
///
/// 若按页推断，第一页里唯一的 Owner 会被误标成「最后一个可登录 Owner」，
/// 界面随即错误禁用移除——即使另一个可登录 Owner 就在后续页。
#[tokio::test]
async fn last_owner_flag_is_global_across_pages() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;

    // 两个可登录 Owner：`aaa-owner` 排在第一页，`zzz-owner` 落在后续页。
    for (username, external) in [
        ("aaa-owner", "sub-aaa-owner"),
        ("zzz-owner", "sub-zzz-owner"),
    ] {
        let id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO users (id, username, display_name, version, created_at, updated_at) \
             VALUES ($1, $2, $3, 1, now(), now())",
        )
        .bind(id)
        .bind(username)
        .bind(username)
        .execute(&stack.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO oauth_accounts (id, user_id, provider, provider_user_id, created_at, updated_at) \
             VALUES ($1, $2, 'https://idp.example', $3, now(), now())",
        )
        .bind(Uuid::now_v7())
        .bind(id)
        .bind(external)
        .execute(&stack.pool)
        .await
        .unwrap();
        stack
            .roles
            .assign_to_username(&Actor::bootstrap_cli(), username, "owner")
            .await
            .unwrap();
    }

    let (cookie, _) = login_as(&stack.router, &stack.idp, "owner").await;
    let (status, body) = api(
        &stack.router,
        "GET",
        "/api/admin/v1/users?limit=1",
        Some(&cookie),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // 该页只有 aaa-owner；全站还有 owner 与 zzz-owner 两个可登录 Owner。
    assert!(
        !body.contains("zzz-owner"),
        "确认另一个 Owner 在后续页：{body}"
    );
    assert_eq!(
        listed_user(&body, "aaa-owner")["is_last_loginable_owner"].as_bool(),
        Some(false),
        "最后 Owner 必须按全局计数判定，不能按当前页推断：{body}"
    );
}

/// 通过账号 API 改角色会递增 `users.version`，目标用户的旧会话立即失效；
/// 重复分配同一角色是幂等的，不应把用户意外登出。
#[tokio::test]
async fn role_change_through_api_invalidates_the_target_session() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;

    let (author_cookie, _) = login_as(&stack.router, &stack.idp, "author").await;
    let (status, _, body) = request_me(&stack.router, &author_cookie).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // owner 持有全部权限，可分配 editor；admin 因委派上限不行。
    let (owner_cookie, owner_csrf) = login_as(&stack.router, &stack.idp, "owner").await;
    let (status, body) = api(
        &stack.router,
        "PUT",
        "/api/admin/v1/users/author/roles/editor",
        Some(&owner_cookie),
        Some(&owner_csrf),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    // 授权集合变化递增目标用户版本：旧 cookie 立即被判为未登录。
    let (status, _, body) = request_me(&stack.router, &author_cookie).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "改角色后目标用户旧会话必须失效：{body}"
    );

    // 重新登录：拿到新角色的权限。
    let (author_cookie, _) = login_as(&stack.router, &stack.idp, "author").await;
    let (status, _, body) = request_me(&stack.router, &author_cookie).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("post.read_any"), "重新登录应带新权限：{body}");

    // 幂等重复分配：不再递增版本，会话保持有效。
    let (status, _) = api(
        &stack.router,
        "PUT",
        "/api/admin/v1/users/author/roles/editor",
        Some(&owner_cookie),
        Some(&owner_csrf),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _, body) = request_me(&stack.router, &author_cookie).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "重复分配同一角色不应撤销会话：{body}"
    );

    // 移除角色 → 会话再次失效。
    let (status, _) = api(
        &stack.router,
        "DELETE",
        "/api/admin/v1/users/author/roles/editor",
        Some(&owner_cookie),
        Some(&owner_csrf),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _, body) = request_me(&stack.router, &author_cookie).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "移除角色后旧会话必须失效：{body}"
    );
}

/// 对自己改角色同样递增版本：操作成功，但本人当前会话随之下线。
/// 界面据此在成功后重新读 `/me`，而不是继续显示已失效的登录态。
#[tokio::test]
async fn self_role_change_logs_the_actor_out() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;

    let (owner_cookie, owner_csrf) = login_as(&stack.router, &stack.idp, "owner").await;
    // Owner 本就有全部权限，但尚未持有 editor 角色；插入新分配会递增版本。
    let (status, body) = api(
        &stack.router,
        "PUT",
        "/api/admin/v1/users/owner/roles/editor",
        Some(&owner_cookie),
        Some(&owner_csrf),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let (status, _, body) = request_me(&stack.router, &owner_cookie).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "改自己的角色也会让本人会话失效：{body}"
    );
}

// ---------------------------------------------------------------------------
// 标签目录管理 API：权限、CSRF、版本与引用保护
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tag_management_requires_tag_manage_but_catalog_is_readable() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (editor_cookie, editor_csrf) = login_as(&stack.router, &stack.idp, "editor").await;
    let (author_cookie, author_csrf) = login_as(&stack.router, &stack.idp, "author").await;

    // editor（tag.manage）创建标签。
    let (status, body) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/tags",
        Some(&editor_cookie),
        Some(&editor_csrf),
        Some(r#"{"name":"Rust","slug":"rust"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    // author（无 tag.manage）读取目录：200——编辑器选择器需要。
    let (status, body) = api(
        &stack.router,
        "GET",
        "/api/admin/v1/tags",
        Some(&author_cookie),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("\"slug\":\"rust\""), "{body}");

    // author 创建/改名/删除一律 403。
    for (method, uri, body) in [
        (
            "POST",
            "/api/admin/v1/tags",
            Some(r#"{"name":"别的","slug":"other"}"#),
        ),
        (
            "PATCH",
            "/api/admin/v1/tags/rust",
            Some(r#"{"name":"改名"}"#),
        ),
        ("DELETE", "/api/admin/v1/tags/rust", None),
    ] {
        let (status, body_out) = api(
            &stack.router,
            method,
            uri,
            Some(&author_cookie),
            Some(&author_csrf),
            body,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "{method} 应为 403：{body_out}"
        );
        assert!(body_out.contains("\"code\":\"forbidden\""), "{body_out}");
    }

    // 未登录读取目录：401（目录读取不设权限 ≠ 匿名可读）。
    let (status, _) = api(&stack.router, "GET", "/api/admin/v1/tags", None, None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn tag_write_requires_csrf_token() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (editor_cookie, _csrf) = login_as(&stack.router, &stack.idp, "editor").await;

    let (status, body) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/tags",
        Some(&editor_cookie),
        None,
        Some(r#"{"name":"Rust","slug":"rust"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "缺 CSRF 不得写入：{body}");
    assert!(body.contains("CSRF"), "{body}");
}

#[tokio::test]
async fn tag_rename_and_delete_check_version_and_references() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (editor_cookie, editor_csrf) = login_as(&stack.router, &stack.idp, "editor").await;
    let (author_cookie, author_csrf) = login_as(&stack.router, &stack.idp, "author").await;

    // 建标签。
    let (status, body) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/tags",
        Some(&editor_cookie),
        Some(&editor_csrf),
        Some(r#"{"name":"Rust","slug":"rust"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let tag_id: Uuid = body
        .split("\"id\":\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .parse()
        .unwrap();

    // 改名：过期 expected_version → 409 version_conflict。
    let (status, body) = api(
        &stack.router,
        "PATCH",
        "/api/admin/v1/tags/rust",
        Some(&editor_cookie),
        Some(&editor_csrf),
        Some(r#"{"name":"Rust 语言","expected_version":99}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.contains("\"code\":\"version_conflict\""), "{body}");

    // 改名成功：version +1。
    let (status, body) = api(
        &stack.router,
        "PATCH",
        "/api/admin/v1/tags/rust",
        Some(&editor_cookie),
        Some(&editor_csrf),
        Some(r#"{"name":"Rust 语言"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("\"version\":2"), "{body}");

    // 作者建文章挂上标签 → 删除标签被引用保护拒绝（409 tag_in_use，区别于通用 conflict）。
    let (status, body) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/posts",
        Some(&author_cookie),
        Some(&author_csrf),
        Some(&format!(
            r#"{{"slug":"tagged","title":"带标签","content":"正文","tag_ids":["{tag_id}"]}}"#
        )),
    )
    .await;
    let post_id = response_id(&body);
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (status, body) = api(
        &stack.router,
        "DELETE",
        "/api/admin/v1/tags/rust",
        Some(&editor_cookie),
        Some(&editor_csrf),
        Some(r#"{"expected_version":2}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.contains("\"code\":\"tag_in_use\""), "{body}");
    assert!(body.contains("1 篇"), "文案指出引用规模：{body}");

    // 解除关联（清空标签，同事务）后删除成功 → 204。
    let (status, body) = api(
        &stack.router,
        "PATCH",
        &format!("/api/admin/v1/posts/{post_id}"),
        Some(&author_cookie),
        Some(&author_csrf),
        Some(r#"{"tag_ids":[]}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("\"tag_ids\":[]"), "{body}");

    let (status, _) = api(
        &stack.router,
        "DELETE",
        "/api/admin/v1/tags/rust",
        Some(&editor_cookie),
        Some(&editor_csrf),
        Some(r#"{"expected_version":2}"#),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn post_edit_saves_tags_with_content_and_validates_ids() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (editor_cookie, editor_csrf) = login_as(&stack.router, &stack.idp, "editor").await;
    let (author_cookie, author_csrf) = login_as(&stack.router, &stack.idp, "author").await;

    // 两个标签。
    let mut tag_ids = Vec::new();
    for (name, slug) in [("Rust", "rust"), ("随笔", "essay")] {
        let (status, body) = api(
            &stack.router,
            "POST",
            "/api/admin/v1/tags",
            Some(&editor_cookie),
            Some(&editor_csrf),
            Some(&format!(r#"{{"name":"{name}","slug":"{slug}"}}"#)),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        tag_ids.push(
            body.split("\"id\":\"")
                .nth(1)
                .unwrap()
                .split('"')
                .next()
                .unwrap()
                .to_string(),
        );
    }

    // 创建时携带重复 tag_ids：去重后成功，详情返回去重集合。
    let (status, body) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/posts",
        Some(&author_cookie),
        Some(&author_csrf),
        Some(&format!(
            r#"{{"slug":"multi-tag","title":"多标签","content":"正文","tag_ids":["{}","{}","{}"]}}"#,
            tag_ids[0], tag_ids[1], tag_ids[0]
        )),
    )
    .await;
    let post_id = response_id(&body);
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(
        body.matches(&format!("\"{}\"", tag_ids[0])).count(),
        1,
        "重复 id 只出现一次：{body}"
    );
    let version: i64 = body
        .split("\"version\":")
        .nth(1)
        .unwrap()
        .split(',')
        .next()
        .unwrap()
        .parse()
        .unwrap();

    // 未知标签 id：可定位的 400，不落任何写入。
    let ghost = Uuid::now_v7();
    let (status, body) = api(
        &stack.router,
        "PATCH",
        &format!("/api/admin/v1/posts/{post_id}"),
        Some(&author_cookie),
        Some(&author_csrf),
        Some(&format!(r#"{{"tag_ids":["{ghost}"]}}"#)),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("所选标签不存在"), "{body}");

    // 只换标签集合：版本 +1（同事务语义在基础设施测试直证，这里验证 HTTP 契约）。
    let (status, body) = api(
        &stack.router,
        "PATCH",
        &format!("/api/admin/v1/posts/{post_id}"),
        Some(&author_cookie),
        Some(&author_csrf),
        Some(&format!(
            r#"{{"tag_ids":["{}"],"expected_version":{}}}"#,
            tag_ids[1], version
        )),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body.contains(&format!("\"version\":{}", version + 1)),
        "仅标签变化也递增版本：{body}"
    );
    assert!(
        body.contains(&format!("\"tag_ids\":[\"{}\"]", tag_ids[1])),
        "{body}"
    );

    // editor（update_any）也能改他人文章的标签；author2（author 角色）不能。
    let (author2_cookie, author2_csrf) = login_as(&stack.router, &stack.idp, "author2").await;
    let (status, body) = api(
        &stack.router,
        "PATCH",
        &format!("/api/admin/v1/posts/{post_id}"),
        Some(&author2_cookie),
        Some(&author2_csrf),
        Some(r#"{"tag_ids":[]}"#),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "他人文章标签受文章授权保护：{body}"
    );
    let (status, body) = api(
        &stack.router,
        "PATCH",
        &format!("/api/admin/v1/posts/{post_id}"),
        Some(&editor_cookie),
        Some(&editor_csrf),
        Some(r#"{"tag_ids":[]}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "editor 持 update_any 可改：{body}");
    assert!(body.contains("\"tag_ids\":[]"), "{body}");
}

// ---------------------------------------------------------------------------
// 分类管理 API：权限、防环、删除保护与文章关联
// ---------------------------------------------------------------------------

#[tokio::test]
async fn category_management_and_post_association() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (editor_cookie, editor_csrf) = login_as(&stack.router, &stack.idp, "editor").await;
    let (author_cookie, author_csrf) = login_as(&stack.router, &stack.idp, "author").await;

    // author 无 category.manage：创建被拒，但目录可读。
    let (status, body) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/categories",
        Some(&author_cookie),
        Some(&author_csrf),
        Some(r#"{"name":"技术","slug":"tech"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, _) = api(
        &stack.router,
        "GET",
        "/api/admin/v1/categories",
        Some(&author_cookie),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // editor 创建父子两级。
    let (status, body) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/categories",
        Some(&editor_cookie),
        Some(&editor_csrf),
        Some(r#"{"name":"技术","slug":"tech"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let tech_id: Uuid = body
        .split("\"id\":\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let (status, body) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/categories",
        Some(&editor_cookie),
        Some(&editor_csrf),
        Some(r#"{"name":"Rust","slug":"rust","parent":"tech"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let rust_id: Uuid = body
        .split("\"id\":\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .parse()
        .unwrap();

    // 防环：把 tech 移到 rust（其子）之下 → 400，文案含「环」。
    let (status, body) = api(
        &stack.router,
        "PATCH",
        "/api/admin/v1/categories/tech",
        Some(&editor_cookie),
        Some(&editor_csrf),
        Some(r#"{"name":"技术","parent":"rust","expected_version":1}"#),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("环"), "{body}");

    // 文章设置分类（三态：id 设置 / null 清空）；仅分类变化也递增版本。
    let (status, body) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/posts",
        Some(&author_cookie),
        Some(&author_csrf),
        Some(r#"{"slug":"cat-post","title":"分类文章","content":"正文"}"#),
    )
    .await;
    let post_id = response_id(&body);
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let version: i64 = body
        .split("\"version\":")
        .nth(1)
        .unwrap()
        .split(',')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let (status, body) = api(
        &stack.router,
        "PATCH",
        &format!("/api/admin/v1/posts/{post_id}"),
        Some(&author_cookie),
        Some(&author_csrf),
        Some(&format!(
            r#"{{"category_id":"{tech_id}","expected_version":{version}}}"#
        )),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body.contains(&format!("\"category_id\":\"{tech_id}\"")),
        "{body}"
    );
    assert!(
        body.contains(&format!("\"version\":{}", version + 1)),
        "仅分类变化也递增版本：{body}"
    );

    // 未知分类 id：400。
    let ghost = Uuid::now_v7();
    let (status, body) = api(
        &stack.router,
        "PATCH",
        &format!("/api/admin/v1/posts/{post_id}"),
        Some(&author_cookie),
        Some(&author_csrf),
        Some(&format!(r#"{{"category_id":"{ghost}"}}"#)),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // 删除保护：tech 被 1 篇文章引用、有 1 个子分类 → 409 category_in_use。
    let (status, body) = api(
        &stack.router,
        "DELETE",
        "/api/admin/v1/categories/tech",
        Some(&editor_cookie),
        Some(&editor_csrf),
        Some(r#"{"expected_version":1}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.contains("\"code\":\"category_in_use\""), "{body}");

    // 清空文章分类并删除子分类后可删。
    let (status, body) = api(
        &stack.router,
        "PATCH",
        &format!("/api/admin/v1/posts/{post_id}"),
        Some(&author_cookie),
        Some(&author_csrf),
        Some(r#"{"category_id":null}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("\"category_id\":null"), "{body}");
    let (status, _) = api(
        &stack.router,
        "DELETE",
        "/api/admin/v1/categories/rust",
        Some(&editor_cookie),
        Some(&editor_csrf),
        Some(r#"{"expected_version":1}"#),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = api(
        &stack.router,
        "DELETE",
        "/api/admin/v1/categories/tech",
        Some(&editor_cookie),
        Some(&editor_csrf),
        Some(r#"{"expected_version":1}"#),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let _ = rust_id;
}

// ---------------------------------------------------------------------------
// 系列 API：目录、重排授权与文章关联
// ---------------------------------------------------------------------------

#[tokio::test]
async fn series_management_reorder_and_post_association() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (editor_cookie, editor_csrf) = login_as(&stack.router, &stack.idp, "editor").await;
    let (author_cookie, author_csrf) = login_as(&stack.router, &stack.idp, "author").await;
    let (author2_cookie, author2_csrf) = login_as(&stack.router, &stack.idp, "author2").await;

    // author 无 series.manage：创建被拒；目录可读。
    let (status, body) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/series",
        Some(&author_cookie),
        Some(&author_csrf),
        Some(r#"{"name":"指南","slug":"guide"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, _) = api(
        &stack.router,
        "GET",
        "/api/admin/v1/series",
        Some(&author_cookie),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // editor 建系列。
    let (status, body) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/series",
        Some(&editor_cookie),
        Some(&editor_csrf),
        Some(r#"{"name":"指南","slug":"guide"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let series_id: Uuid = body
        .split("\"id\":\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .parse()
        .unwrap();

    // 两篇作者文章挂入系列（同事务；series 三态对象）。
    let mut posts = Vec::new();
    for (slug, order) in [("ser-1", 1), ("ser-2", 2)] {
        let (status, body) = api(
            &stack.router,
            "POST",
            "/api/admin/v1/posts",
            Some(&author_cookie),
            Some(&author_csrf),
            Some(&format!(
                r#"{{"slug":"{slug}","title":"{slug}","content":"正文"}}"#
            )),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let version: i64 = body
            .split("\"version\":")
            .nth(1)
            .unwrap()
            .split(',')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let post_id: Uuid = body
            .split("\"id\":\"")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let (status, body) = api(
            &stack.router, "PATCH", &format!("/api/admin/v1/posts/{post_id}"),
            Some(&author_cookie), Some(&author_csrf),
            Some(&format!(r#"{{"series":{{"id":"{series_id}","order":{order}}},"expected_version":{version}}}"#)),
        ).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        posts.push(post_id);
    }

    // author2（author 角色，无 any）：系列含他人文章 → 重排 403。
    let (status, body) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/series/guide/reorder",
        Some(&author2_cookie),
        Some(&author2_csrf),
        Some(&format!(
            r#"{{"ordered_post_ids":["{}","{}"]}}"#,
            posts[1], posts[0]
        )),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "Author 不能借重排改他人文章顺序：{body}"
    );

    // editor（update_any）重排倒序：成功且返回新 series 版本。
    let (status, body) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/series/guide/reorder",
        Some(&editor_cookie),
        Some(&editor_csrf),
        Some(&format!(
            r#"{{"ordered_post_ids":["{}","{}"]}}"#,
            posts[1], posts[0]
        )),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // 版本轨迹：创建 1 + 两篇加入各 +1（系列锁协议）+ 重排 +1 = 4。
    assert!(body.contains("\"series_version\":4"), "{body}");

    // 集合不一致（漏一篇）→ 400 可定位错误。
    let (status, body) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/series/guide/reorder",
        Some(&editor_cookie),
        Some(&editor_csrf),
        Some(&format!(r#"{{"ordered_post_ids":["{}"]}}"#, posts[0])),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("成员不一致"), "{body}");

    // 退出系列（series: null）后删除保护解除。
    for post_id in &posts {
        let (status, body) = api(
            &stack.router,
            "PATCH",
            &format!("/api/admin/v1/posts/{post_id}"),
            Some(&author_cookie),
            Some(&author_csrf),
            Some(r#"{"series":null}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(!body.contains(&series_id.to_string()), "已退出系列：{body}");
    }
    let (status, _) = api(
        &stack.router,
        "DELETE",
        "/api/admin/v1/series/guide",
        Some(&editor_cookie),
        Some(&editor_csrf),
        Some(r#"{"expected_version":6}"#),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // 引用保护：另一系列带成员时删除 → 409 series_in_use。
    let (status, body) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/series",
        Some(&editor_cookie),
        Some(&editor_csrf),
        Some(r#"{"name":"占用","slug":"occupied"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let occ_id: Uuid = body
        .split("\"id\":\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let (status, body) = api(
        &stack.router,
        "PATCH",
        &format!("/api/admin/v1/posts/{}", posts[0]),
        Some(&author_cookie),
        Some(&author_csrf),
        Some(&format!(r#"{{"series":{{"id":"{occ_id}","order":1}}}}"#)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = api(
        &stack.router,
        "DELETE",
        "/api/admin/v1/series/occupied",
        Some(&editor_cookie),
        Some(&editor_csrf),
        Some(r#"{"expected_version":2}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.contains("\"code\":\"series_in_use\""), "{body}");
}

#[tokio::test]
async fn series_members_endpoint_lists_other_authors_posts() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (editor_cookie, editor_csrf) = login_as(&stack.router, &stack.idp, "editor").await;
    let (author_cookie, author_csrf) = login_as(&stack.router, &stack.idp, "author").await;
    let (author2_cookie, _author2_csrf) = login_as(&stack.router, &stack.idp, "author2").await;

    let (status, body) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/series",
        Some(&editor_cookie),
        Some(&editor_csrf),
        Some(r#"{"name":"多人","slug":"multi"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let series_id: Uuid = body
        .split("\"id\":\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .parse()
        .unwrap();

    // author 与 author2 各一篇挂入系列（他人文章必须出现在成员目录里）。
    for (user, cookie, csrf, slug) in [
        ("author", &author_cookie, &author_csrf, "mem-1"),
        ("author2", &author2_cookie, &_author2_csrf, "mem-2"),
    ] {
        let _ = user;
        let (status, body) = api(
            &stack.router,
            "POST",
            "/api/admin/v1/posts",
            Some(cookie),
            Some(csrf),
            Some(&format!(
                r#"{{"slug":"{slug}","title":"{slug}","content":"正文"}}"#
            )),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let post_id = response_id(&body);
        let version: i64 = body
            .split("\"version\":")
            .nth(1)
            .unwrap()
            .split(',')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let (status, body) = api(
            &stack.router,
            "PATCH",
            &format!("/api/admin/v1/posts/{post_id}"),
            Some(cookie),
            Some(csrf),
            Some(&format!(
                r#"{{"series":{{"id":"{series_id}","order":{}}},"expected_version":{version}}}"#,
                if slug == "mem-1" { 1 } else { 2 }
            )),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    // editor（series.manage）：成员目录含两位作者的文章。
    let (status, body) = api(
        &stack.router,
        "GET",
        "/api/admin/v1/series/multi/members",
        Some(&editor_cookie),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body.contains("mem-1") && body.contains("mem-2"),
        "含他人文章：{body}"
    );

    // author（无 series.manage）：403——他人草稿不因目录端点泄漏。
    let (status, body) = api(
        &stack.router,
        "GET",
        "/api/admin/v1/series/multi/members",
        Some(&author_cookie),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

#[tokio::test]
async fn series_members_requires_read_permission_for_every_member() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (editor_cookie, editor_csrf) = login_as(&stack.router, &stack.idp, "editor").await;
    let (author_cookie, author_csrf) = login_as(&stack.router, &stack.idp, "author").await;
    let (author2_cookie, author2_csrf) = login_as(&stack.router, &stack.idp, "author2").await;

    // P1 复现场景：给 Author 追加**只有 series.manage** 的自定义角色
    // （无 post.read_any——内置角色恰好两个都有，不能作为授权依据）。
    sqlx::query(
        "INSERT INTO roles (id, name, slug, description, version, created_at, updated_at) \
         SELECT gen_random_uuid(), '仅系列管理', 'series-manage-only', NULL, 1, now(), now() \
         WHERE NOT EXISTS (SELECT 1 FROM roles WHERE slug = 'series-manage-only')",
    )
    .execute(&stack.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO role_permissions (role_id, permission_id) \
         SELECT r.id, p.id FROM roles r JOIN permissions p ON p.key = 'series.manage' \
         WHERE r.slug = 'series-manage-only' \
           AND NOT EXISTS (SELECT 1 FROM role_permissions rp WHERE rp.role_id = r.id AND rp.permission_id = p.id)",
    )
    .execute(&stack.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO user_roles (user_id, role_id) \
         SELECT u.id, r.id FROM users u JOIN roles r ON r.slug = 'series-manage-only' \
         WHERE u.username = 'author' \
           AND NOT EXISTS (SELECT 1 FROM user_roles ur WHERE ur.user_id = u.id AND ur.role_id = r.id)",
    )
    .execute(&stack.pool)
    .await
    .unwrap();

    // editor 建系列；author 与 author2 各挂一篇。
    let (status, body) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/series",
        Some(&editor_cookie),
        Some(&editor_csrf),
        Some(r#"{"name":"混合","slug":"mixed"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let mixed_id: Uuid = body
        .split("\"id\":\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .parse()
        .unwrap();

    for (cookie, csrf, slug, order) in [
        (&author_cookie, &author_csrf, "mix-1", 1),
        (&author2_cookie, &author2_csrf, "mix-2", 2),
    ] {
        let (status, body) = api(
            &stack.router,
            "POST",
            "/api/admin/v1/posts",
            Some(cookie),
            Some(csrf),
            Some(&format!(
                r#"{{"slug":"{slug}","title":"{slug}","content":"正文"}}"#
            )),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let post_id = response_id(&body);
        let version: i64 = body
            .split("\"version\":")
            .nth(1)
            .unwrap()
            .split(',')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let (status, body) = api(
            &stack.router,
            "PATCH",
            &format!("/api/admin/v1/posts/{post_id}"),
            Some(cookie),
            Some(csrf),
            Some(&format!(
                r#"{{"series":{{"id":"{mixed_id}","order":{order}}},"expected_version":{version}}}"#
            )),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    // author（series.manage + post.read own，无 read_any）：
    // 系列含他人文章 → 整次 403，不回含他人草稿标题的（残缺）目录。
    let (status, body) = api(
        &stack.router,
        "GET",
        "/api/admin/v1/series/mixed/members",
        Some(&author_cookie),
        None,
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "任一成员不可读即整次拒绝：{body}"
    );
    assert!(!body.contains("mix-2"), "不得泄漏他人文章条目：{body}");

    // editor（read_any）：完整目录。
    let (status, body) = api(
        &stack.router,
        "GET",
        "/api/admin/v1/series/mixed/members",
        Some(&editor_cookie),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("mix-1") && body.contains("mix-2"), "{body}");

    // 全部成员可读时放行：author 建只含本人文章的系列。
    let (status, body) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/posts",
        Some(&author_cookie),
        Some(&author_csrf),
        Some(r#"{"slug":"solo-1","title":"solo-1","content":"正文"}"#),
    )
    .await;
    let post_id = response_id(&body);
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let version: i64 = body
        .split("\"version\":")
        .nth(1)
        .unwrap()
        .split(',')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let (status, body) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/series",
        Some(&author_cookie),
        Some(&author_csrf),
        Some(r#"{"name":"独著","slug":"solo"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "series.manage 已授予：{body}");
    let solo_id: Uuid = body
        .split("\"id\":\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let (status, body) = api(
        &stack.router,
        "PATCH",
        &format!("/api/admin/v1/posts/{post_id}"),
        Some(&author_cookie),
        Some(&author_csrf),
        Some(&format!(
            r#"{{"series":{{"id":"{solo_id}","order":1}},"expected_version":{version}}}"#
        )),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = api(
        &stack.router,
        "GET",
        "/api/admin/v1/series/solo/members",
        Some(&author_cookie),
        None,
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "全部成员本人可读（post.read own）：{body}"
    );
    assert!(body.contains("solo-1"), "{body}");
}

#[tokio::test]
async fn post_trash_http_scope_restore_and_owner_purge() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (author_cookie, author_csrf) = login_as(&stack.router, &stack.idp, "author").await;
    let (other_cookie, other_csrf) = login_as(&stack.router, &stack.idp, "author2").await;
    let (owner_cookie, owner_csrf) = login_as(&stack.router, &stack.idp, "owner").await;
    let (status, body) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/posts",
        Some(&author_cookie),
        Some(&author_csrf),
        Some(r#"{"slug":"http-trash","title":"标题","content":"正文"}"#),
    )
    .await;
    let post_id = response_id(&body);
    assert_eq!(status, StatusCode::CREATED);
    let (status, body) = api(
        &stack.router,
        "POST",
        &format!("/api/admin/v1/posts/{post_id}/publish"),
        Some(&author_cookie),
        Some(&author_csrf),
        Some(r#"{"expected_version":1}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _) = api(
        &stack.router,
        "POST",
        &format!("/api/admin/v1/posts/{post_id}/trash"),
        Some(&other_cookie),
        Some(&other_csrf),
        Some(r#"{"expected_version":2}"#),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = api(
        &stack.router,
        "POST",
        &format!("/api/admin/v1/posts/{post_id}/trash"),
        Some(&author_cookie),
        Some(&author_csrf),
        Some(r#"{"expected_version":2}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("\"version\":3"));
    let (status, body) = api(
        &stack.router,
        "GET",
        "/api/admin/v1/posts",
        Some(&author_cookie),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.contains("http-trash"));
    let (status, body) = api(
        &stack.router,
        "GET",
        "/api/admin/v1/post-trash?page=1",
        Some(&author_cookie),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("http-trash"));
    let (status, _) = api(
        &stack.router,
        "GET",
        "/api/admin/v1/post-trash?author=author",
        Some(&other_cookie),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = api(
        &stack.router,
        "POST",
        &format!("/api/admin/v1/posts/{post_id}/restore"),
        Some(&author_cookie),
        Some(&author_csrf),
        Some(r#"{"expected_version":2}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, body) = api(
        &stack.router,
        "POST",
        &format!("/api/admin/v1/posts/{post_id}/restore"),
        Some(&author_cookie),
        Some(&author_csrf),
        Some(r#"{"expected_version":3}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("\"status\":\"draft\""));
    let (status, _) = api(
        &stack.router,
        "POST",
        &format!("/api/admin/v1/posts/{post_id}/trash"),
        Some(&author_cookie),
        Some(&author_csrf),
        Some(r#"{"expected_version":4}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = api(
        &stack.router,
        "POST",
        &format!("/api/admin/v1/posts/{post_id}/purge"),
        Some(&author_cookie),
        Some(&author_csrf),
        Some(r#"{"expected_version":5}"#),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = api(
        &stack.router,
        "POST",
        &format!("/api/admin/v1/posts/{post_id}/purge"),
        Some(&owner_cookie),
        Some(&owner_csrf),
        Some(r#"{"expected_version":5}"#),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
}

#[tokio::test]
async fn post_by_id_survives_slug_reuse_and_preserves_write_guards() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (cookie, csrf) = login_as(&stack.router, &stack.idp, "author").await;
    let (other_cookie, other_csrf) = login_as(&stack.router, &stack.idp, "author2").await;
    let (editor_cookie, editor_csrf) = login_as(&stack.router, &stack.idp, "editor").await;
    let (status, body) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/posts",
        Some(&cookie),
        Some(&csrf),
        Some(r#"{"slug":"stable-post","title":"原文章","content":"原正文"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let created: serde_json::Value = serde_json::from_str(&body).unwrap();
    let id = created["id"].as_str().unwrap();
    let uri = format!("/api/admin/v1/posts/{id}");

    // ID 寻址沿用 own/any 授权，知道其他作者的 ID 不增加权限。
    for (method, suffix, payload) in [
        ("GET", "", None),
        (
            "PATCH",
            "",
            Some(r#"{"title":"越权","expected_version":1}"#),
        ),
        ("POST", "/publish", Some(r#"{"expected_version":1}"#)),
        ("POST", "/trash", Some(r#"{"expected_version":1}"#)),
    ] {
        let (status, body) = api(
            &stack.router,
            method,
            &format!("{uri}{suffix}"),
            Some(&other_cookie),
            Some(&other_csrf),
            payload,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {suffix}: {body}");
    }
    let (status, body) = api(
        &stack.router,
        "PATCH",
        &uri,
        Some(&cookie),
        None,
        Some(r#"{"new_slug":"renamed-post","expected_version":1}"#),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "ID 写接口必须检查 CSRF：{body}"
    );

    let (status, body) = api(
        &stack.router,
        "PATCH",
        &uri,
        Some(&cookie),
        Some(&csrf),
        Some(r#"{"new_slug":"renamed-post","expected_version":1}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let renamed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(renamed["id"], id);
    assert_eq!(renamed["slug"], "renamed-post");
    assert_eq!(renamed["version"], 2);

    // 原 slug 被另一个作者占用；已打开的 ID 编辑会话仍绑定原实体。
    let (status, body) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/posts",
        Some(&other_cookie),
        Some(&other_csrf),
        Some(r#"{"slug":"stable-post","title":"新文章","content":"新正文"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let replacement: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_ne!(replacement["id"], id);
    let (status, body) = api(&stack.router, "GET", &uri, Some(&editor_cookie), None, None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "read_any 读取重命名后的同一 ID：{body}"
    );
    let detail: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(detail["id"], id);
    assert_eq!(detail["slug"], "renamed-post");
    assert_eq!(detail["content"], "原正文");

    let (status, body) = api(
        &stack.router,
        "PATCH",
        &uri,
        Some(&editor_cookie),
        Some(&editor_csrf),
        Some(r#"{"title":"编辑修改原文章","expected_version":2}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "update_any 仍有效：{body}");
    for (method, suffix, payload) in [
        ("PATCH", "", r#"{"title":"过期修改","expected_version":2}"#),
        ("POST", "/publish", r#"{"expected_version":2}"#),
    ] {
        let (status, body) = api(
            &stack.router,
            method,
            &format!("{uri}{suffix}"),
            Some(&cookie),
            Some(&csrf),
            Some(payload),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{method} {suffix}: {body}");
        let error: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(error["code"], "version_conflict");
    }

    for (action, version, expected_status) in [
        ("publish", 3, "published"),
        ("unpublish", 4, "draft"),
        ("trash", 5, "draft"),
        ("restore", 6, "draft"),
    ] {
        let (status, body) = api(
            &stack.router,
            "POST",
            &format!("{uri}/{action}"),
            Some(&editor_cookie),
            Some(&editor_csrf),
            Some(&format!(r#"{{"expected_version":{version}}}"#)),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{action}: {body}");
        let result: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(result["id"], id);
        assert_eq!(result["slug"], "renamed-post");
        assert_eq!(result["status"], expected_status);
        assert_eq!(result["version"], version + 1);
    }
    let (status, body) = api(
        &stack.router,
        "GET",
        &format!(
            "/api/admin/v1/posts/{}",
            replacement["id"].as_str().unwrap()
        ),
        Some(&other_cookie),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "新文章通过自身 ID 读取：{body}");
    let untouched: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        untouched, replacement,
        "旧 ID 的操作不能修改复用 slug 的新文章"
    );
}

#[tokio::test]
async fn page_by_id_never_targets_a_replacement_after_deletion() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (cookie, csrf) = login_as(&stack.router, &stack.idp, "editor").await;
    let (author_cookie, author_csrf) = login_as(&stack.router, &stack.idp, "author").await;
    let (status, body) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/pages",
        Some(&cookie),
        Some(&csrf),
        Some(r#"{"slug":"stable-page","title":"原页面","content":"原正文"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let original: serde_json::Value = serde_json::from_str(&body).unwrap();
    let id = original["id"].as_str().unwrap();
    let uri = format!("/api/admin/v1/pages/{id}");
    let delete_version_1 = r#"{"expected_version":1}"#.to_string();

    for (method, suffix, payload) in [
        ("GET", "", None),
        (
            "PATCH",
            "",
            Some(r#"{"title":"越权","expected_version":1}"#),
        ),
        ("POST", "/publish", Some(r#"{"expected_version":1}"#)),
        ("DELETE", "", Some(delete_version_1.as_str())),
    ] {
        let (status, body) = api(
            &stack.router,
            method,
            &format!("{uri}{suffix}"),
            Some(&author_cookie),
            Some(&author_csrf),
            payload,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "页面仍需站点级权限：{body}");
    }
    let (status, body) = api(
        &stack.router,
        "PATCH",
        &uri,
        Some(&cookie),
        Some(&csrf),
        Some(r#"{"new_slug":"renamed-page","expected_version":1}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let renamed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(renamed["id"], id);
    assert_eq!(renamed["slug"], "renamed-page");
    assert_eq!(renamed["version"], 2);
    let (status, body) = api(&stack.router, "GET", &uri, Some(&cookie), None, None).await;
    assert_eq!(status, StatusCode::OK, "重命名后仍通过原 ID 读取：{body}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap(),
        renamed
    );

    for (action, version, expected_status) in
        [("publish", 2, "published"), ("unpublish", 3, "draft")]
    {
        let (status, body) = api(
            &stack.router,
            "POST",
            &format!("{uri}/{action}"),
            Some(&cookie),
            Some(&csrf),
            Some(&format!(r#"{{"expected_version":{version}}}"#)),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{action}: {body}");
        let result: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(result["id"], id);
        assert_eq!(result["status"], expected_status);
        assert_eq!(result["version"], version + 1);
    }
    let delete_version_4 = r#"{"expected_version":4}"#.to_string();
    let (status, body) = api(
        &stack.router,
        "DELETE",
        &uri,
        Some(&cookie),
        None,
        Some(&delete_version_4),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "删除 ID 仍需 CSRF：{body}");
    let (status, body) = api(
        &stack.router,
        "DELETE",
        &uri,
        Some(&cookie),
        Some(&csrf),
        Some(&delete_version_1),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "删除 ID 仍需匹配版本：{body}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap()["code"],
        "version_conflict"
    );
    let (status, body) = api(
        &stack.router,
        "DELETE",
        &uri,
        Some(&cookie),
        Some(&csrf),
        Some(&delete_version_4),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let (status, body) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/pages",
        Some(&cookie),
        Some(&csrf),
        Some(r#"{"slug":"renamed-page","title":"新页面","content":"新正文"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let replacement: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_ne!(replacement["id"], id);

    // 即使请求版本恰好与新页面相同，也不能把已删除 ID 重新解释为 slug。
    for (method, suffix, payload) in [
        ("GET", "", None),
        (
            "PATCH",
            "",
            Some(r#"{"title":"旧编辑会话","expected_version":1}"#),
        ),
        ("POST", "/publish", Some(r#"{"expected_version":1}"#)),
        ("POST", "/unpublish", Some(r#"{"expected_version":1}"#)),
        ("DELETE", "", Some(delete_version_1.as_str())),
    ] {
        let (status, body) = api(
            &stack.router,
            method,
            &format!("{uri}{suffix}"),
            Some(&cookie),
            Some(&csrf),
            payload,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "已删除 ID {method} {suffix}: {body}"
        );
    }
    let (status, body) = api(
        &stack.router,
        "GET",
        &format!(
            "/api/admin/v1/pages/{}",
            replacement["id"].as_str().unwrap()
        ),
        Some(&cookie),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let untouched: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        untouched, replacement,
        "新页面未被旧 ID 读取、改写、发布或删除"
    );
}

#[tokio::test]
async fn management_rejects_slug_addresses_without_changing_content() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    for (kind, username) in [("posts", "author"), ("pages", "editor")] {
        let (cookie, csrf) = login_as(&stack.router, &stack.idp, username).await;
        let (status, body) = api(
            &stack.router,
            "POST",
            &format!("/api/admin/v1/{kind}"),
            Some(&cookie),
            Some(&csrf),
            Some(r#"{"slug":"by-id","title":"合法公开路径","content":"正文"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{kind}: {body}");
        let created: serde_json::Value = serde_json::from_str(&body).unwrap();
        let id = response_id(&body);
        for (method, suffix, payload) in [
            ("GET", "", None),
            (
                "PATCH",
                "",
                Some(r#"{"title":"不得写入","expected_version":1}"#),
            ),
            ("POST", "/publish", Some(r#"{"expected_version":1}"#)),
            ("POST", "/unpublish", Some(r#"{"expected_version":1}"#)),
        ] {
            let (status, body) = api(
                &stack.router,
                method,
                &format!("/api/admin/v1/{kind}/by-id{suffix}"),
                Some(&cookie),
                Some(&csrf),
                payload,
            )
            .await;
            assert_eq!(
                status,
                StatusCode::BAD_REQUEST,
                "slug 管理地址应拒绝：{kind} {method} {body}"
            );
        }
        let (status, body) = api(
            &stack.router,
            "GET",
            &format!("/api/admin/v1/{kind}/{id}"),
            Some(&cookie),
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&body).unwrap(),
            created
        );
    }
}

#[tokio::test]
async fn native_comments_guest_moderation_and_http_boundaries() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (cookie, csrf) = login_as(&stack.router, &stack.idp, "author").await;
    let (status, body) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/posts",
        Some(&cookie),
        Some(&csrf),
        Some(r#"{"slug":"comments-http","title":"Comments","content":"Body"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let post_id = response_id(&body);
    let (status, body) = api(
        &stack.router,
        "POST",
        &format!("/api/admin/v1/posts/{post_id}/publish"),
        Some(&cookie),
        Some(&csrf),
        Some(r#"{"expected_version":1}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let endpoint = "/api/v1/posts/comments-http/comments";
    let payload=serde_json::json!({"nickname":"<script>guest</script>","body":"<img src=x onerror=alert(1)>\nText", "request_id":Uuid::now_v7()}).to_string();
    // Anonymous writes require the configured origin (not arbitrary Host matching).
    for origin in [None, Some("http://evil.test")] {
        let mut req = Request::builder()
            .method("POST")
            .uri(endpoint)
            .header("content-type", "application/json");
        if let Some(origin) = origin {
            req = req.header("origin", origin);
        }
        let response = stack
            .router
            .clone()
            .oneshot(req.body(Body::from(payload.clone())).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    let guest_request = || {
        Request::builder()
            .method("POST")
            .uri(endpoint)
            .header("content-type", "application/json")
            .header("host", "127.0.0.1:18099")
            .header("origin", "http://127.0.0.1:18099")
    };
    let response = stack
        .router
        .clone()
        .oneshot(guest_request().body(Body::from(payload.clone())).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let (_, body) = api(&stack.router, "GET", endpoint, None, None, None).await;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap()["total"],
        0
    );
    let (_, body) = api(
        &stack.router,
        "GET",
        "/api/admin/v1/comments?status=pending",
        Some(&cookie),
        None,
        None,
    )
    .await;
    let data: serde_json::Value = serde_json::from_str(&body).unwrap();
    let cid = data["items"][0]["id"].as_str().unwrap();
    assert_eq!(data["items"][0]["nickname"], "<script>guest</script>");
    let uri = format!("/api/admin/v1/comments/{cid}");
    let moderation = r#"{"version":1,"status":"approved"}"#;
    assert_eq!(
        api(
            &stack.router,
            "POST",
            &uri,
            Some(&cookie),
            None,
            Some(moderation)
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        api(
            &stack.router,
            "POST",
            &uri,
            Some(&cookie),
            Some(&csrf),
            Some(moderation)
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        api(
            &stack.router,
            "POST",
            &uri,
            Some(&cookie),
            Some(&csrf),
            Some(moderation)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let (_, body) = api(&stack.router, "GET", endpoint, None, None, None).await;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap()["total"],
        1
    );
    // A cookie-bearing public submission must also pass session CSRF.
    let response = stack
        .router
        .clone()
        .oneshot(
            guest_request()
                .header("cookie", format!("blog_session={cookie}"))
                .body(Body::from(payload.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let payload=serde_json::json!({"nickname":"spoof","body":"Author reply","parent_id":cid,"request_id":Uuid::now_v7()}).to_string();
    let response = stack
        .router
        .clone()
        .oneshot(
            guest_request()
                .header("cookie", format!("blog_session={cookie}"))
                .header("x-csrf-token", &csrf)
                .body(Body::from(payload))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let (_, body) = api(
        &stack.router,
        "GET",
        "/api/admin/v1/comments?status=pending",
        Some(&cookie),
        None,
        None,
    )
    .await;
    let data: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(data["items"][0]["is_author"], true);
    assert_eq!(data["items"][0]["nickname"], "author");
    // Body limits and unknown fields cannot bypass the public contract.
    let response = stack
        .router
        .clone()
        .oneshot(guest_request().body(Body::from("x".repeat(17000))).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    sqlx::query("UPDATE posts SET visibility='private' WHERE id=$1")
        .bind(post_id)
        .execute(&stack.pool)
        .await
        .unwrap();
    assert_eq!(
        api(&stack.router, "GET", endpoint, None, None, None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

async fn published_comment_endpoint(stack: &Stack, cookie: &str, csrf: &str) -> String {
    let (status, body) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/posts",
        Some(cookie),
        Some(csrf),
        Some(r#"{"slug":"comment-regression","title":"Comments","content":"Body"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let post_id = response_id(&body);
    let (status, body) = api(
        &stack.router,
        "POST",
        &format!("/api/admin/v1/posts/{post_id}/publish"),
        Some(cookie),
        Some(csrf),
        Some(r#"{"expected_version":1}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    "/api/v1/posts/comment-regression/comments".into()
}

fn comment_submit_request(
    endpoint: &str,
    payload: serde_json::Value,
    cookie: Option<&str>,
    csrf: Option<&str>,
) -> Request<Body> {
    let mut request = Request::builder()
        .method("POST")
        .uri(endpoint)
        .header("host", "127.0.0.1:18099")
        .header("origin", "http://127.0.0.1:18099")
        .header("content-type", "application/json");
    if let Some(cookie) = cookie {
        request = request.header("cookie", format!("blog_session={cookie}"));
    }
    if let Some(csrf) = csrf {
        request = request.header("x-csrf-token", csrf);
    }
    request.body(Body::from(payload.to_string())).unwrap()
}

#[tokio::test]
async fn native_comments_clear_stale_cookie_before_explicit_guest_retry() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (cookie, csrf) = login_as(&stack.router, &stack.idp, "author").await;
    let endpoint = published_comment_endpoint(&stack, &cookie, &csrf).await;
    sqlx::query("UPDATE users SET version=version+1 WHERE username='author'")
        .execute(&stack.pool)
        .await
        .unwrap();
    let payload = serde_json::json!({
        "nickname":"Guest", "body":"A preserved draft", "request_id":Uuid::now_v7(),
    });
    // A failed authenticated write must never silently become an anonymous write.
    let response = stack
        .router
        .clone()
        .oneshot(comment_submit_request(
            &endpoint,
            payload.clone(),
            Some(&cookie),
            Some(&csrf),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM comments")
        .fetch_one(&stack.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);

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
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let clear = response
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(clear.starts_with("blog_session=;"));
    assert!(clear.contains("Path=/"));
    assert!(clear.contains("HttpOnly"));
    assert!(clear.contains("SameSite=Lax"));
    assert!(clear.contains("Max-Age=0"));
    // The browser applies Set-Cookie; only an explicit retry now omits it.
    let response = stack
        .router
        .clone()
        .oneshot(comment_submit_request(&endpoint, payload, None, None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let row: (Option<Uuid>, String, String) =
        sqlx::query_as("SELECT user_id,nickname,body FROM comments")
            .fetch_one(&stack.pool)
            .await
            .unwrap();
    assert_eq!(row, (None, "Guest".into(), "A preserved draft".into()));
}

#[tokio::test]
async fn me_keeps_session_cookie_when_identity_storage_fails() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (cookie, _) = login_as(&stack.router, &stack.idp, "author").await;
    // Session validation succeeds in memory, but the identity lookup cannot run.
    stack.pool.close().await;
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
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(response.headers().get("set-cookie").is_none());
}

#[tokio::test]
async fn native_comments_use_server_name_before_guest_nickname_validation() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (cookie, csrf) = login_as(&stack.router, &stack.idp, "author").await;
    let endpoint = published_comment_endpoint(&stack, &cookie, &csrf).await;
    let display_name = format!("{}😀尾", "字".repeat(63));
    sqlx::query("UPDATE users SET display_name=$1 WHERE username='author'")
        .bind(&display_name)
        .execute(&stack.pool)
        .await
        .unwrap();
    let request_id = Uuid::now_v7();
    for nickname in [&display_name, "ignored\n<script>", ""] {
        let response = stack
            .router
            .clone()
            .oneshot(comment_submit_request(
                &endpoint,
                serde_json::json!({"nickname":nickname,"body":"Signed in","request_id":request_id}),
                Some(&cookie),
                Some(&csrf),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
    }
    let (_, body) = api(
        &stack.router,
        "GET",
        "/api/admin/v1/comments?status=pending",
        Some(&cookie),
        None,
        None,
    )
    .await;
    let data: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(data["total"], 1);
    assert_eq!(
        data["items"][0]["nickname"],
        format!("{}😀", "字".repeat(63))
    );
    assert_eq!(data["items"][0]["is_author"], true);
    for body in [" ".to_string(), "字".repeat(2001), "nul\0".to_string()] {
        let response = stack.router.clone().oneshot(comment_submit_request(
            &endpoint,
            serde_json::json!({"nickname":display_name,"body":body,"request_id":Uuid::now_v7()}),
            Some(&cookie), Some(&csrf),
        )).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    // Ignoring authenticated nicknames must not relax the anonymous input rules.
    for nickname in [&display_name, "", "name\nspoof"] {
        let response = stack
            .router
            .clone()
            .oneshot(comment_submit_request(
                &endpoint,
                serde_json::json!({"nickname":nickname,"body":"Guest","request_id":Uuid::now_v7()}),
                None,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
