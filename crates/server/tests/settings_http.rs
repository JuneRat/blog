//! 站点设置 HTTP 集成测试：会话认证 + CSRF + settings.manage 授权 +
//! 优先级（数据库 > 环境变量/默认值）+ 版本冲突 + 公开页面即时生效。
//!
//! 栈与生产装配同构：public_router（真实主题）+ auth_router + settings_router。

mod common;

use std::sync::{Arc, Mutex};

use application::auth::{AuthDeps, AuthInteractor};
use application::content::PostInteractor;
use application::identity::{Actor, CreateUserCmd, RoleInteractor, UserInteractor};
use application::page::PageInteractor;
use application::ports::{
    Clock, ExternalIdentity, ExternalIdentityClient, OAuthAccountStore, OAuthConfigStore,
    ProviderConfig, ProviderKind, SecureRandom, SettingsStore,
};
use application::public_site::{PublicSiteInteractor, SiteInfo};
use application::settings::SettingsInteractor;
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::middleware;
use http_body_util::BodyExt;
use infrastructure::{
    InMemoryOAuthAttemptStore, InMemorySessionStore, MiniJinjaThemeRenderer,
    PostgresCategoryRepository, PostgresOAuthAccountStore, PostgresOAuthConfigStore,
    PostgresPageRepository, PostgresPostRepository, PostgresPublishedCategoryQuery,
    PostgresPublishedPageQuery, PostgresPublishedPostQuery, PostgresPublishedSeriesQuery,
    PostgresPublishedTagQuery, PostgresRbacStore, PostgresSeriesRepository, PostgresSettingsStore,
    PostgresTagRepository, PostgresUserRepository, SanitizingMarkdownRenderer, SystemClock,
};
use interfaces::http::{PublicSiteState, public_router};
use interfaces::http_admin::settings_router;
use interfaces::http_auth::{AdminState, AuthState, admin_router, auth_router};
use interfaces::http_support::request_context;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 回退装配值：模拟环境变量/默认值（数据库未配置时公开页面使用它）。
const FALLBACK_TITLE: &str = "环境变量站点";
const FALLBACK_DESCRIPTION: &str = "回退描述";

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

fn site_fallback() -> SiteInfo {
    SiteInfo {
        title: FALLBACK_TITLE.into(),
        description: FALLBACK_DESCRIPTION.into(),
    }
}

/// 与生产装配同构的测试栈；返回完整 router、登录依赖与连接池。
struct Stack {
    router: axum::Router,
    idp: Arc<FakeIdpClient>,
    pool: PgPool,
}

async fn build(pool: PgPool) -> Stack {
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let user_repo = Arc::new(PostgresUserRepository::new(pool.clone()));
    let rbac = Arc::new(PostgresRbacStore::new(pool.clone()));
    let roles = Arc::new(RoleInteractor::new(rbac.clone(), user_repo.clone()));
    roles.sync_registry().await.expect("同步权限目录失败");
    let users = Arc::new(UserInteractor::new(user_repo.clone(), rbac, clock.clone()));

    // admin 持有 settings.manage；editor/author 不持有（内容权限集）。
    let mut ids = std::collections::HashMap::new();
    for (username, role) in [
        ("admin", Some("admin")),
        ("editor", Some("editor")),
        ("author", Some("author")),
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
        accounts
            .bind(
                *uid,
                "https://idp.example",
                &format!("sub-{username}"),
                None,
            )
            .await
            .unwrap();
    }

    let idp = Arc::new(FakeIdpClient {
        external_id: Mutex::new("sub-admin".into()),
    });
    let sessions: Arc<dyn application::ports::SessionStore> =
        Arc::new(InMemorySessionStore::with_defaults());
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
        clock.clone(),
        "http://127.0.0.1:18099".into(),
    ));
    let passwords = common::password_interactor(user_repo.clone(), sessions);

    let tag_repo = Arc::new(PostgresTagRepository::new(pool.clone()));
    let category_repo = Arc::new(PostgresCategoryRepository::new(pool.clone()));
    let series_repo = Arc::new(PostgresSeriesRepository::new(pool.clone()));
    let posts = Arc::new(PostInteractor::new(
        Arc::new(PostgresPostRepository::new(pool.clone())),
        tag_repo.clone(),
        category_repo.clone(),
        series_repo.clone(),
        clock.clone(),
    ));
    let pages = Arc::new(PageInteractor::new(
        Arc::new(PostgresPageRepository::new(pool.clone())),
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
    ));

    let settings_store: Arc<dyn SettingsStore> = Arc::new(PostgresSettingsStore::new(pool.clone()));
    let settings = Arc::new(SettingsInteractor::new(
        settings_store.clone(),
        clock,
        site_fallback(),
    ));

    // 公开站点：真实主题 + settings 解析（数据库 site 行 > 装配回退值）。
    let theme = Arc::new(
        MiniJinjaThemeRenderer::load(std::path::Path::new("../../themes/default"))
            .expect("模板加载失败"),
    );
    let public_site = Arc::new(PublicSiteInteractor::new(
        Arc::new(PostgresPublishedPostQuery::new(pool.clone())),
        Arc::new(PostgresPublishedPageQuery::new(pool.clone())),
        Arc::new(PostgresPublishedTagQuery::new(pool.clone())),
        Arc::new(PostgresPublishedCategoryQuery::new(pool.clone())),
        Arc::new(PostgresPublishedSeriesQuery::new(pool.clone())),
        Arc::new(SanitizingMarkdownRenderer::new()),
        theme,
        settings_store,
        site_fallback(),
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
        tags,
        categories,
        series,
        settings,
        roles,
        secure_cookies: false,
    };

    let router = public_router(
        PublicSiteState {
            site: public_site,
            health: None,
        },
        None,
    )
    .merge(auth_router(auth_state))
    .merge(admin_router(admin_state.clone()))
    .merge(settings_router(admin_state))
    .layer(middleware::from_fn(request_context));
    Stack { router, idp, pool }
}

async fn fresh_stack() -> Stack {
    build(common::fresh_database("blog_settings_test").await).await
}

/// 只重建公开路由（新 interactor + 新 settings 存储，同一数据库）：
/// 模拟「重启后」的进程——进程内状态全丢，配置只剩数据库。
async fn revived_public_router(pool: &PgPool) -> axum::Router {
    let theme = Arc::new(
        MiniJinjaThemeRenderer::load(std::path::Path::new("../../themes/default"))
            .expect("模板加载失败"),
    );
    let public_site = Arc::new(PublicSiteInteractor::new(
        Arc::new(PostgresPublishedPostQuery::new(pool.clone())),
        Arc::new(PostgresPublishedPageQuery::new(pool.clone())),
        Arc::new(PostgresPublishedTagQuery::new(pool.clone())),
        Arc::new(PostgresPublishedCategoryQuery::new(pool.clone())),
        Arc::new(PostgresPublishedSeriesQuery::new(pool.clone())),
        Arc::new(SanitizingMarkdownRenderer::new()),
        theme,
        Arc::new(PostgresSettingsStore::new(pool.clone())),
        site_fallback(),
    ));
    public_router(
        PublicSiteState {
            site: public_site,
            health: None,
        },
        None,
    )
}

/// 以指定用户走完整 OAuth 登录，返回 (会话 cookie, CSRF token)。
async fn login_as(stack: &Stack, username: &str) -> (String, String) {
    *stack.idp.external_id.lock().unwrap() = format!("sub-{username}");
    let response = stack
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/auth/login?provider=idp&next=/admin/")
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

    let response = stack
        .router
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
    let body: serde_json::Value = serde_json::from_str(&String::from_utf8_lossy(
        &response.into_body().collect().await.unwrap().to_bytes(),
    ))
    .unwrap();
    (cookie, body["csrf_token"].as_str().unwrap().to_string())
}

async fn get(
    router: &axum::Router,
    uri: &str,
    cookie: Option<&str>,
) -> (StatusCode, serde_json::Value, Option<String>) {
    let mut builder = Request::builder().uri(uri);
    if let Some(cookie) = cookie {
        builder = builder.header("cookie", format!("blog_session={cookie}"));
    }
    let response = router
        .clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let cache = response
        .headers()
        .get(axum::http::header::CACHE_CONTROL)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let body = String::from_utf8_lossy(&response.into_body().collect().await.unwrap().to_bytes())
        .to_string();
    let json = serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
    (status, json, cache)
}

async fn put(
    router: &axum::Router,
    uri: &str,
    cookie: &str,
    csrf: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(uri)
                .header("cookie", format!("blog_session={cookie}"))
                .header("x-csrf-token", csrf)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let text = String::from_utf8_lossy(&response.into_body().collect().await.unwrap().to_bytes())
        .to_string();
    (
        status,
        serde_json::from_str(&text).unwrap_or(serde_json::Value::Null),
    )
}

async fn public_home(router: &axum::Router) -> String {
    let response = router
        .clone()
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    String::from_utf8_lossy(&response.into_body().collect().await.unwrap().to_bytes()).to_string()
}

// ---------------------------------------------------------------------------
// 认证与授权
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unauthenticated_requests_are_rejected() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;

    let (status, body, _) = get(&stack.router, "/api/admin/v1/settings/site", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "unauthenticated");

    let (status, body) = put(
        &stack.router,
        "/api/admin/v1/settings/site",
        "not-a-session",
        "not-a-csrf",
        serde_json::json!({"title": "x", "description": "y"}),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "unauthenticated");
}

#[tokio::test]
async fn settings_manage_is_required_for_read_and_write() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    // editor 有大量内容权限但没有 settings.manage。
    let (cookie, csrf) = login_as(&stack, "editor").await;

    let (status, body, _) = get(&stack.router, "/api/admin/v1/settings/site", Some(&cookie)).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "读取需 settings.manage：{body}"
    );
    assert_eq!(body["code"], "forbidden");

    let (status, body) = put(
        &stack.router,
        "/api/admin/v1/settings/site",
        &cookie,
        &csrf,
        serde_json::json!({"title": "越权标题", "description": "描述"}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "forbidden");

    // 越权 PUT 没有落库：公开页面仍是装配回退值。
    let html = public_home(&stack.router).await;
    assert!(html.contains(FALLBACK_TITLE) && !html.contains("越权标题"));
}

#[tokio::test]
async fn write_requires_csrf_token() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (cookie, _csrf) = login_as(&stack, "admin").await;

    let (status, body) = put(
        &stack.router,
        "/api/admin/v1/settings/site",
        &cookie,
        "wrong-csrf-token",
        serde_json::json!({"title": "标题", "description": "描述"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "invalid_request");
}

// ---------------------------------------------------------------------------
// 读写闭环与优先级
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unconfigured_site_reads_fallback_and_public_uses_it() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (cookie, _csrf) = login_as(&stack, "admin").await;

    let (status, body, cache) =
        get(&stack.router, "/api/admin/v1/settings/site", Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["source"], "fallback");
    assert_eq!(body["version"], 0);
    assert_eq!(body["title"], FALLBACK_TITLE);
    assert_eq!(body["description"], FALLBACK_DESCRIPTION);
    assert_eq!(cache.as_deref(), Some("no-store"), "管理响应不得缓存");

    // 未配置时公开页面渲染装配回退值。
    let html = public_home(&stack.router).await;
    assert!(html.contains(FALLBACK_TITLE));
    assert!(html.contains(FALLBACK_DESCRIPTION));
}

#[tokio::test]
async fn saved_settings_take_effect_on_public_pages_immediately() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (cookie, csrf) = login_as(&stack, "admin").await;

    let (status, body) = put(
        &stack.router,
        "/api/admin/v1/settings/site",
        &cookie,
        &csrf,
        serde_json::json!({
            "title": "  数据库站点标题  ",
            "description": "数据库站点描述",
            "expected_version": 0,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["version"], 1);
    assert_eq!(body["source"], "database");
    assert_eq!(body["title"], "数据库站点标题", "响应为规范化后的值");

    // 无缓存：同一次进程内下一次公开渲染即用数据库值。
    let html = public_home(&stack.router).await;
    assert!(html.contains("数据库站点标题"), "公开页面应使用数据库标题");
    assert!(html.contains("数据库站点描述"));
    assert!(!html.contains(FALLBACK_TITLE), "装配回退值不再生效");

    // 「重启」语义：丢弃全部进程内装配（新 interactor），只保留数据库——
    // 全新公开栈仍读到已保存配置。
    let revived = revived_public_router(&stack.pool).await;
    let html = public_home(&revived).await;
    assert!(html.contains("数据库站点标题"));
}

#[tokio::test]
async fn invalid_values_are_rejected_as_bad_request() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (cookie, csrf) = login_as(&stack, "admin").await;

    let long_title = "长".repeat(201);
    let long_description = "述".repeat(501);
    for (title, description) in [
        ("", "描述"),
        ("   ", "描述"),
        (&long_title, "描述"),
        ("标题", &long_description),
    ] {
        let (status, body) = put(
            &stack.router,
            "/api/admin/v1/settings/site",
            &cookie,
            &csrf,
            serde_json::json!({"title": title, "description": description, "expected_version": 0}),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "({title:?}, ..) 应 400：{body}"
        );
        assert_eq!(body["code"], "invalid_request");
    }

    // 均未落库。
    let (_, body, _) = get(&stack.router, "/api/admin/v1/settings/site", Some(&cookie)).await;
    assert_eq!(body["version"], 0);
}

// ---------------------------------------------------------------------------
// 并发冲突与受保护分组
// ---------------------------------------------------------------------------

#[tokio::test]
async fn stale_expected_version_conflicts_then_recovers() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (cookie, csrf) = login_as(&stack, "admin").await;

    let (status, _) = put(
        &stack.router,
        "/api/admin/v1/settings/site",
        &cookie,
        &csrf,
        serde_json::json!({"title": "第一版", "description": "描述", "expected_version": 0}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // 另一处已把版本推进到 1：旧前提 0 再提交必须 409，且不覆盖。
    let (status, body) = put(
        &stack.router,
        "/api/admin/v1/settings/site",
        &cookie,
        &csrf,
        serde_json::json!({"title": "过期提交", "description": "描述", "expected_version": 0}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "version_conflict");

    let (_, body, _) = get(&stack.router, "/api/admin/v1/settings/site", Some(&cookie)).await;
    assert_eq!(body["title"], "第一版", "冲突提交不得覆盖");

    // 按最新版本重提即成功（前端「仍然覆盖」流程）。
    let version = body["version"].as_i64().unwrap();
    let (status, body) = put(
        &stack.router,
        "/api/admin/v1/settings/site",
        &cookie,
        &csrf,
        serde_json::json!({
            "title": "第二版", "description": "描述", "expected_version": version
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["version"], 2);
}

#[tokio::test]
async fn unknown_or_protected_settings_groups_are_not_routed() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (cookie, csrf) = login_as(&stack, "admin").await;

    // settings API 面只有 site 分组：oauth（受 oauth.manage 保护的分组）、
    // 未知分组与集合路径都不是可寻址资源。
    for uri in [
        "/api/admin/v1/settings/oauth",
        "/api/admin/v1/settings/theme",
        "/api/admin/v1/settings",
    ] {
        let (status, _, _) = get(&stack.router, uri, Some(&cookie)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "GET {uri}");
        let (status, _) = put(
            &stack.router,
            uri,
            &cookie,
            &csrf,
            serde_json::json!({"providers": []}),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "PUT {uri}");
    }
}

#[tokio::test]
async fn oversize_settings_body_is_rejected_before_parsing() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (cookie, csrf) = login_as(&stack, "admin").await;

    let response = stack
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/admin/v1/settings/site")
                .header("cookie", format!("blog_session={cookie}"))
                .header("x-csrf-token", csrf)
                .header("content-type", "application/json")
                .body(Body::from("x".repeat(32 * 1024)))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}
