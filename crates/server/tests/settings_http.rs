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
    ProviderConfig, ProviderKind, SecureRandom, SettingsStore, ThemeSettingsStore,
};
use application::public_site::{PublicSiteInteractor, SiteInfo};
use application::settings::SettingsInteractor;
use application::themes::ThemeRegistry;
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
    PostgresTagRepository, PostgresUserRepository, RenderingRuntime, SystemClock,
};
use interfaces::http::{PublicSiteState, mount_theme_assets, public_router};
use interfaces::http_admin::{pages_router, settings_router};
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
        logo_url: None,
    }
}

/// 测试用可信站点地址：SEO/canonical/feed 链接都基于它拼接。
fn test_base_url() -> application::seo::PublicBaseUrl {
    application::seo::PublicBaseUrl::parse("https://blog.test").unwrap()
}

/// 与生产装配同构的测试栈；返回完整 router、登录依赖与连接池。
struct Stack {
    router: axum::Router,
    idp: Arc<FakeIdpClient>,
    pool: PgPool,
}

async fn build(pool: PgPool) -> Stack {
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let rendering = Arc::new(RenderingRuntime::default());
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
            None,
        )
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
        Arc::new(PostgresPostRepository::new(pool.clone(), rendering.clone())),
        tag_repo.clone(),
        category_repo.clone(),
        series_repo.clone(),
        clock.clone(),
        common::media_guard(pool.clone()),
    ));
    let pages = Arc::new(PageInteractor::new(
        Arc::new(PostgresPageRepository::new(pool.clone(), rendering.clone())),
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

    let settings_store: Arc<dyn SettingsStore> = Arc::new(PostgresSettingsStore::new(pool.clone()));
    let theme_store: Arc<dyn ThemeSettingsStore> =
        Arc::new(PostgresSettingsStore::new(pool.clone()));
    let theme_data = Arc::new(application::theme_data::ThemeData::new(
        Arc::new(PostgresPublishedPostQuery::new(pool.clone())),
        Arc::new(PostgresPublishedTagQuery::new(pool.clone())),
        Arc::new(PostgresPublishedCategoryQuery::new(pool.clone())),
    ));
    let default_theme = MiniJinjaThemeRenderer::load(std::path::Path::new("../../themes/default"))
        .expect("默认主题加载失败")
        .with_data(theme_data.clone());
    let paper_theme = MiniJinjaThemeRenderer::load(std::path::Path::new("../../themes/paper"))
        .expect("Paper 主题加载失败")
        .with_data(theme_data);
    let theme_assets = vec![default_theme.assets(), paper_theme.assets()];
    let mut registry = ThemeRegistry::new("default".into());
    registry
        .add(
            "default".into(),
            "Default".into(),
            rendering.theme_renderer(default_theme),
        )
        .unwrap();
    registry
        .add(
            "paper".into(),
            "Paper".into(),
            rendering.theme_renderer(paper_theme),
        )
        .unwrap();
    let registry = Arc::new(registry);
    let settings = Arc::new(
        SettingsInteractor::new(
            settings_store.clone(),
            clock,
            site_fallback(),
            common::media_guard(pool.clone()),
        )
        .with_themes(theme_store.clone(), registry.clone()),
    );

    // 公开站点：真实主题 + settings 解析（数据库 site 行 > 装配回退值）。
    let theme = registry.renderer("default").unwrap();
    let public_site = Arc::new(
        PublicSiteInteractor::new(
            Arc::new(PostgresPublishedPostQuery::new(pool.clone())),
            Arc::new(PostgresPublishedPageQuery::new(pool.clone())),
            Arc::new(PostgresPublishedTagQuery::new(pool.clone())),
            Arc::new(PostgresPublishedCategoryQuery::new(pool.clone())),
            Arc::new(PostgresPublishedSeriesQuery::new(pool.clone())),
            theme,
            settings_store,
            site_fallback(),
            test_base_url(),
        )
        .with_themes(theme_store, registry),
    );

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
        media: common::media_interactor(pool.clone(), common::media_dir("settings")),
        secure_cookies: false,
    };

    let router = mount_theme_assets(
        public_router(
            PublicSiteState {
                site: public_site,
                health: None,
            },
            None,
        ),
        theme_assets,
    )
    .merge(auth_router(auth_state))
    .merge(admin_router(admin_state.clone()))
    .merge(pages_router(admin_state.clone()))
    .merge(settings_router(admin_state.clone()))
    .merge(interfaces::http_retention::retention_router(
        interfaces::http_retention::RetentionState {
            retention: Arc::new(application::retention::RetentionInteractor::new(Arc::new(
                infrastructure::retention::PostgresRetentionStore::new(pool.clone()),
            ))),
            admin: admin_state,
        },
    ))
    .layer(middleware::from_fn(request_context));
    Stack { router, idp, pool }
}

async fn fresh_stack() -> Stack {
    build(common::fresh_database("blog_settings_test").await).await
}

#[tokio::test]
async fn retention_requires_permission_csrf_and_current_group_versions() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let path = "/api/admin/v1/settings/retention";
    assert_eq!(
        get(&stack.router, path, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (editor, editor_csrf) = login_as(&stack, "editor").await;
    assert_eq!(
        get(&stack.router, path, Some(&editor)).await.0,
        StatusCode::FORBIDDEN
    );
    let (admin, csrf) = login_as(&stack, "admin").await;
    let (status, mut body, cache) = get(&stack.router, path, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(cache.unwrap().contains("no-store"));
    assert_eq!(body["comment_ip_days"], 180);
    assert_eq!(body["audit_version"], 0);
    body["comment_ip_days"] = serde_json::json!(60);
    assert_eq!(
        put(&stack.router, path, &editor, &editor_csrf, body.clone())
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        put(&stack.router, path, &admin, "invalid-csrf", body.clone())
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let (status, saved) = put(&stack.router, path, &admin, &csrf, body.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(saved["comment_version"], 1);
    assert_eq!(saved["audit_version"], 0);
    assert_eq!(
        put(&stack.router, path, &admin, &csrf, body).await.0,
        StatusCode::CONFLICT
    );
    let mut invalid = saved;
    invalid["audit_days"] = serde_json::json!(-1);
    assert_eq!(
        put(&stack.router, path, &admin, &csrf, invalid).await.0,
        StatusCode::BAD_REQUEST
    );
}

/// 只重建公开路由（新 interactor + 新 settings 存储，同一数据库）：
/// 模拟「重启后」的进程——进程内状态全丢，配置只剩数据库。
async fn revived_public_router(pool: &PgPool) -> axum::Router {
    let theme = RenderingRuntime::default().theme_renderer(
        MiniJinjaThemeRenderer::load(std::path::Path::new("../../themes/default"))
            .expect("模板加载失败"),
    );
    let public_site = Arc::new(PublicSiteInteractor::new(
        Arc::new(PostgresPublishedPostQuery::new(pool.clone())),
        Arc::new(PostgresPublishedPageQuery::new(pool.clone())),
        Arc::new(PostgresPublishedTagQuery::new(pool.clone())),
        Arc::new(PostgresPublishedCategoryQuery::new(pool.clone())),
        Arc::new(PostgresPublishedSeriesQuery::new(pool.clone())),
        theme,
        Arc::new(PostgresSettingsStore::new(pool.clone())),
        site_fallback(),
        test_base_url(),
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

async fn page_write(
    router: &axum::Router,
    method: &str,
    uri: &str,
    cookie: &str,
    csrf: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
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
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

async fn public_text(router: &axum::Router, uri: &str) -> (StatusCode, String) {
    let response = router
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).to_string())
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

    let actor: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE username='admin'")
        .fetch_one(&stack.pool)
        .await
        .unwrap();
    let audits: Vec<(Uuid, serde_json::Value)> = sqlx::query_as("SELECT actor_id,metadata FROM audit_logs WHERE action='settings.site' AND target_id='site'")
        .fetch_all(&stack.pool).await.unwrap();
    assert_eq!(audits, vec![(actor, serde_json::json!({"version":1}))]);
    // Repeated same-value saves and stale submissions must not invent audit events.
    for (version, expected) in [(1, StatusCode::OK), (0, StatusCode::CONFLICT)] {
        let (status, _) = put(&stack.router, "/api/admin/v1/settings/site", &cookie, &csrf,
            serde_json::json!({"title":"数据库站点标题","description":"数据库站点描述","expected_version":version})).await;
        assert_eq!(status, expected);
    }
    // Exercise the actual HTTP path: failed audit means no settings change.
    sqlx::raw_sql("CREATE FUNCTION fail_site_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'audit unavailable'; END $$; CREATE TRIGGER fail_site_audit BEFORE INSERT ON audit_logs FOR EACH ROW WHEN (NEW.action='settings.site') EXECUTE FUNCTION fail_site_audit()")
        .execute(&stack.pool).await.unwrap();
    let (status, _) = put(
        &stack.router,
        "/api/admin/v1/settings/site",
        &cookie,
        &csrf,
        serde_json::json!({"title":"Rejected","description":"Rejected","expected_version":1}),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    let version: i64 = sqlx::query_scalar("SELECT version FROM settings WHERE key='site'")
        .fetch_one(&stack.pool)
        .await
        .unwrap();
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_logs WHERE action='settings.site'")
            .fetch_one(&stack.pool)
            .await
            .unwrap();
    assert_eq!((version, count), (1, 1));

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
async fn page_trash_hides_public_entries_and_only_purge_releases_slug() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (editor_cookie, editor_csrf) = login_as(&stack, "editor").await;
    let (author_cookie, author_csrf) = login_as(&stack, "author").await;
    let (status, created) = page_write(
        &stack.router,
        "POST",
        "/api/admin/v1/pages",
        &editor_cookie,
        &editor_csrf,
        serde_json::json!({"slug":"about","title":"关于","content":"# 关于","visibility":"public"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().unwrap();
    let path = &format!("/api/admin/v1/pages/{id}");
    let (status, published) = page_write(
        &stack.router,
        "POST",
        &format!("{path}/publish"),
        &editor_cookie,
        &editor_csrf,
        serde_json::json!({"expected_version":1}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{published}");
    assert_eq!(published["version"], 2);
    assert_eq!(public_text(&stack.router, "/about").await.0, StatusCode::OK);
    assert!(
        public_text(&stack.router, "/sitemap.xml")
            .await
            .1
            .contains("https://blog.test/about")
    );

    let body = serde_json::json!({"expected_version":2});
    let (status, _) = page_write(
        &stack.router,
        "POST",
        &format!("{path}/trash"),
        &author_cookie,
        &author_csrf,
        body.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = page_write(
        &stack.router,
        "POST",
        &format!("{path}/trash"),
        &editor_cookie,
        "bad-csrf",
        body.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    for stale in [serde_json::json!({"expected_version":1})] {
        let (status, error) = page_write(
            &stack.router,
            "POST",
            &format!("{path}/trash"),
            &editor_cookie,
            &editor_csrf,
            stale,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{error}");
        assert_eq!(error["code"], "version_conflict");
    }
    assert_eq!(public_text(&stack.router, "/about").await.0, StatusCode::OK);

    let (status, _) = page_write(
        &stack.router,
        "POST",
        &format!("{path}/trash"),
        &editor_cookie,
        &editor_csrf,
        body.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        public_text(&stack.router, "/about").await.0,
        StatusCode::NOT_FOUND
    );
    assert!(
        !public_text(&stack.router, "/sitemap.xml")
            .await
            .1
            .contains("https://blog.test/about")
    );
    let (status, _, _) = get(&stack.router, path, Some(&editor_cookie)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = page_write(
        &stack.router,
        "POST",
        &format!("{path}/trash"),
        &editor_cookie,
        &editor_csrf,
        body.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // A trashed page retains its address; only Owner may physically purge it.
    let (status, _) = page_write(
        &stack.router,
        "POST",
        "/api/admin/v1/pages",
        &editor_cookie,
        &editor_csrf,
        serde_json::json!({"slug":"about","title":"Reserved","content":"Body"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = page_write(
        &stack.router,
        "POST",
        &format!("{path}/purge"),
        &editor_cookie,
        &editor_csrf,
        serde_json::json!({"expected_version":3}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    sqlx::query("INSERT INTO user_roles(user_id,role_id) SELECT u.id,r.id FROM users u CROSS JOIN roles r WHERE u.username='admin' AND r.code='owner'")
        .execute(&stack.pool).await.unwrap();
    let (owner_cookie, owner_csrf) = login_as(&stack, "admin").await;
    for (version, expected) in [(2, StatusCode::CONFLICT), (3, StatusCode::NO_CONTENT)] {
        let (status, _) = page_write(
            &stack.router,
            "POST",
            &format!("{path}/purge"),
            &owner_cookie,
            &owner_csrf,
            serde_json::json!({"expected_version":version}),
        )
        .await;
        assert_eq!(status, expected);
    }

    let (status, replacement) = page_write(
        &stack.router,
        "POST",
        "/api/admin/v1/pages",
        &editor_cookie,
        &editor_csrf,
        serde_json::json!({"slug":"about","title":"新页面","content":"new"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{replacement}");
    assert_ne!(replacement["id"], id);
    let (status, stale) = page_write(
        &stack.router,
        "POST",
        &format!("{path}/trash"),
        &editor_cookie,
        &editor_csrf,
        serde_json::json!({"expected_version":1}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{stale}");
    assert_eq!(
        get(
            &stack.router,
            &format!(
                "/api/admin/v1/pages/{}",
                replacement["id"].as_str().unwrap()
            ),
            Some(&editor_cookie)
        )
        .await
        .0,
        StatusCode::OK
    );
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

    // oauth（受 oauth.manage 保护的分组）、未知分组与集合路径不可寻址。
    for uri in ["/api/admin/v1/settings/oauth", "/api/admin/v1/settings"] {
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
async fn theme_switch_is_authorized_versioned_and_updates_html_and_assets() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let uri = "/api/admin/v1/settings/theme";
    let (status, _, _) = get(&stack.router, uri, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (editor_cookie, editor_csrf) = login_as(&stack, "editor").await;
    let (status, _, _) = get(&stack.router, uri, Some(&editor_cookie)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = put(
        &stack.router,
        uri,
        &editor_cookie,
        &editor_csrf,
        serde_json::json!({"slug":"paper","expected_version":0}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (cookie, csrf) = login_as(&stack, "admin").await;
    let (status, initial, cache) = get(&stack.router, uri, Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cache.as_deref(), Some("no-store"));
    assert_eq!(initial["slug"], "default");
    assert_eq!(initial["source"], "fallback");
    assert_eq!(initial["version"], 0);
    assert_eq!(initial["available"].as_array().unwrap().len(), 2);
    assert!(
        public_home(&stack.router)
            .await
            .contains("/assets/default/")
    );

    let (status, body) = put(
        &stack.router,
        uri,
        &cookie,
        "wrong-csrf",
        serde_json::json!({"slug":"paper","expected_version":0}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, body) = put(
        &stack.router,
        uri,
        &cookie,
        &csrf,
        serde_json::json!({"slug":"../paper","expected_version":0}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, saved) = put(
        &stack.router,
        uri,
        &cookie,
        &csrf,
        serde_json::json!({"slug":"paper","expected_version":0}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["version"], 1);
    assert_eq!(saved["source"], "database");
    let html = public_home(&stack.router).await;
    assert!(html.contains("/assets/paper/"));
    assert!(!html.contains("/assets/default/"));
    let asset_url = html
        .split("href=\"")
        .filter_map(|part| part.split('"').next())
        .find(|url| url.starts_with("/assets/paper/"))
        .unwrap();
    let (status, _, _) = get(&stack.router, asset_url, None).await;
    assert_eq!(status, StatusCode::OK);

    let (status, stale) = put(
        &stack.router,
        uri,
        &cookie,
        &csrf,
        serde_json::json!({"slug":"default","expected_version":0}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{stale}");
    let (status, same) = put(
        &stack.router,
        uri,
        &cookie,
        &csrf,
        serde_json::json!({"slug":"paper","expected_version":1}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(same["version"], 1);
    let (status, restored) = put(
        &stack.router,
        uri,
        &cookie,
        &csrf,
        serde_json::json!({"slug":"default","expected_version":1}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{restored}");
    assert_eq!(restored["version"], 2);
    assert!(
        public_home(&stack.router)
            .await
            .contains("/assets/default/")
    );
}

#[tokio::test]
async fn missing_saved_theme_falls_back_and_can_be_replaced() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    sqlx::query("INSERT INTO settings (key,value,version) VALUES ('theme', '{\"schema_version\":1,\"slug\":\"removed\"}'::jsonb, 1)")
        .execute(&stack.pool).await.unwrap();
    let (cookie, csrf) = login_as(&stack, "admin").await;
    let (_, view, _) = get(&stack.router, "/api/admin/v1/settings/theme", Some(&cookie)).await;
    assert_eq!(view["slug"], "removed");
    assert_eq!(view["effective_slug"], "default");
    assert!(
        public_home(&stack.router)
            .await
            .contains("/assets/default/")
    );
    let (status, saved) = put(
        &stack.router,
        "/api/admin/v1/settings/theme",
        &cookie,
        &csrf,
        serde_json::json!({"slug":"paper","expected_version":1}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["effective_slug"], "paper");
    assert!(public_home(&stack.router).await.contains("/assets/paper/"));
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
