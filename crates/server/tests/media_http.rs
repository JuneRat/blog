//! 媒体 HTTP 契约：独立公开 URL、回收站、权限、缓存和使用位置隐私。
//!
//! 这里验证的是**接口层契约**：状态码、业务码、缓存头与匿名可见性；
//! 引用事务与审计由 infrastructure/tests/media.rs 覆盖。

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
use axum::http::{Request, StatusCode, header};
use axum::middleware;
use http_body_util::BodyExt;
use infrastructure::{
    InMemoryOAuthAttemptStore, PostgresCategoryRepository, PostgresOAuthAccountStore,
    PostgresOAuthConfigStore, PostgresPageRepository, PostgresPostRepository, PostgresRbacStore,
    PostgresSeriesRepository, PostgresSessionStore, PostgresTagRepository, PostgresUserRepository,
    SystemClock,
};
use interfaces::http_auth::{AdminState, AuthState, admin_router, auth_router};
use interfaces::http_media::{MediaReadState, media_admin_router, media_read_router};
use interfaces::http_support::request_context;
use serde_json::Value;
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
    pool: PgPool,
}

/// 最小合法 PNG 头：格式嗅探与尺寸解析只需要 IHDR。
fn png_bytes(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
    bytes
}

async fn fresh_stack() -> Stack {
    let pool = common::fresh_database("blog_media_http_test").await;

    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let user_repo = Arc::new(PostgresUserRepository::new(pool.clone()));
    let rbac = Arc::new(PostgresRbacStore::new(pool.clone()));
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

    // author 有 media.read/upload/delete；editor 另有 media.delete_any；stranger 无任何角色。
    for (username, role) in [
        ("author", Some("author")),
        ("author2", Some("author")),
        ("editor", Some("editor")),
        ("stranger", None),
    ] {
        users
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
    }
    // 站点设置（logo）需要 settings.manage：给 editor 追加 admin 角色，
    // 让它同时具备 media.upload 与 settings.manage。
    roles
        .assign_to_username(&Actor::bootstrap_cli(), "editor", "admin")
        .await
        .unwrap();

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
            None.into(),
        )
        .await
        .unwrap();

    let accounts: Arc<dyn OAuthAccountStore> =
        Arc::new(PostgresOAuthAccountStore::new(pool.clone()));
    for username in ["author", "author2", "editor", "stranger"] {
        let user = users.actor_for_username(username).await.unwrap().user_id.0;
        accounts
            .bind(
                user,
                "https://idp.example",
                &format!("sub-{username}"),
                None,
                None.into(),
            )
            .await
            .unwrap();
    }

    let idp = Arc::new(FakeIdpClient {
        external_id: Mutex::new("sub-author".into()),
    });
    let tag_repo: Arc<dyn application::ports::TagRepository> =
        Arc::new(PostgresTagRepository::new(pool.clone()));
    let category_repo: Arc<dyn application::ports::CategoryRepository> =
        Arc::new(PostgresCategoryRepository::new(pool.clone()));
    let series_repo: Arc<dyn application::ports::SeriesRepository> =
        Arc::new(PostgresSeriesRepository::new(pool.clone()));
    let posts = Arc::new(PostInteractor::new(
        Arc::new(PostgresPostRepository::new(
            pool.clone(),
            Arc::new(infrastructure::RenderingRuntime::default()),
        )),
        tag_repo,
        category_repo,
        series_repo,
        clock.clone(),
        common::media_guard(pool.clone()),
    ));
    let pages = Arc::new(application::page::PageInteractor::new(
        Arc::new(PostgresPageRepository::new(
            pool.clone(),
            Arc::new(infrastructure::RenderingRuntime::default()),
        )),
        clock.clone(),
    ));

    let sessions: Arc<dyn application::ports::SessionStore> =
        Arc::new(PostgresSessionStore::with_defaults(pool.clone()));
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
    let media = common::media_interactor(pool.clone(), common::media_dir("http"));

    let auth_state = AuthState {
        auth: auth.clone(),
        passwords: passwords.clone(),
        secure_cookies: false,
    };
    let admin_state = AdminState {
        content_queries: common::content_queries(&pool),
        auth: auth.clone(),
        users,
        passwords,
        posts,
        pages,
        tags: Arc::new(application::tag::TagInteractor::new(
            Arc::new(PostgresTagRepository::new(pool.clone())),
            Arc::new(SystemClock),
        )),
        categories: Arc::new(application::category::CategoryInteractor::new(
            Arc::new(PostgresCategoryRepository::new(pool.clone())),
            Arc::new(SystemClock),
        )),
        series: Arc::new(application::series::SeriesInteractor::new(
            Arc::new(PostgresSeriesRepository::new(pool.clone())),
            Arc::new(SystemClock),
            common::media_guard(pool.clone()),
        )),
        settings: Arc::new(application::settings::SettingsInteractor::new(
            Arc::new(infrastructure::PostgresSettingsStore::new(pool.clone())),
            Arc::new(SystemClock),
            application::public_site::SiteInfo {
                title: "测试站点".into(),
                description: "集成测试".into(),
                logo_url: None,
            },
            common::media_guard(pool.clone()),
        )),
        roles,
        media: media.clone(),
        secure_cookies: false,
    };

    let router = auth_router(auth_state)
        .merge(admin_router(admin_state.clone()))
        .merge(interfaces::http_admin::posts_router(admin_state.clone()))
        .merge(interfaces::http_admin::series_router(admin_state.clone()))
        .merge(interfaces::http_admin::settings_router(admin_state.clone()))
        .merge(media_admin_router(admin_state))
        .merge(media_read_router(MediaReadState { media }))
        .layer(middleware::from_fn(request_context));
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
        .get(header::LOCATION)
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
        .get_all(header::SET_COOKIE)
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
        .get_all(header::SET_COOKIE)
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

    let (_, _, body) = send(
        router,
        "GET",
        "/api/admin/v1/me",
        Some(&cookie),
        None,
        None,
        None,
    )
    .await;
    let csrf = body["csrf_token"].as_str().unwrap().to_string();
    (cookie, csrf)
}

/// 通用请求：可选 cookie / CSRF / JSON 体 / 原始字节体。
async fn send(
    router: &axum::Router,
    method: &str,
    uri: &str,
    cookie: Option<&str>,
    csrf: Option<&str>,
    json: Option<&Value>,
    raw: Option<Vec<u8>>,
) -> (StatusCode, axum::http::HeaderMap, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(cookie) = cookie {
        builder = builder.header("cookie", format!("blog_session={cookie}"));
    }
    if let Some(csrf) = csrf {
        builder = builder.header("x-csrf-token", csrf);
    }
    let request = match (json, raw) {
        (_, Some(bytes)) => builder
            .header("content-type", "image/png")
            .body(Body::from(bytes))
            .unwrap(),
        (Some(value), None) => builder
            .header("content-type", "application/json")
            .body(Body::from(value.to_string()))
            .unwrap(),
        (None, None) => builder.body(Body::empty()).unwrap(),
    };
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).to_string()))
    };
    (status, headers, body)
}

/// 上传一张图片，返回 (id, version)。
async fn upload(stack: &Stack, cookie: &str, csrf: &str, width: u32) -> (String, i64) {
    let (status, _, body) = send(
        &stack.router,
        "POST",
        "/api/admin/v1/media?filename=photo.png",
        Some(cookie),
        Some(csrf),
        None,
        Some(png_bytes(width, width)),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "上传应成功：{body}");
    (
        body["id"].as_str().unwrap().to_string(),
        body["version"].as_i64().unwrap(),
    )
}

#[tokio::test]
async fn upload_and_library_require_permissions_csrf_and_valid_image_content() {
    let _serial = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (cookie, csrf) = login_as(&stack.router, &stack.idp, "author").await;
    let (stranger, stranger_csrf) = login_as(&stack.router, &stack.idp, "stranger").await;
    let endpoint = "/api/admin/v1/media?filename=photo.png";
    assert_eq!(
        send(
            &stack.router,
            "POST",
            endpoint,
            None,
            None,
            None,
            Some(png_bytes(8, 8))
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        send(
            &stack.router,
            "POST",
            endpoint,
            Some(&stranger),
            Some(&stranger_csrf),
            None,
            Some(png_bytes(8, 8))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(
            &stack.router,
            "POST",
            endpoint,
            Some(&cookie),
            None,
            None,
            Some(png_bytes(8, 8))
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    for bytes in [vec![], b"<svg/>".to_vec(), png_bytes(0, 10)] {
        let (status, headers, error) = send(
            &stack.router,
            "POST",
            endpoint,
            Some(&cookie),
            Some(&csrf),
            None,
            Some(bytes),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
        assert_eq!(
            headers["x-request-id"].to_str().unwrap(),
            error["request_id"].as_str().unwrap()
        );
    }
    let (id, version) = upload(&stack, &cookie, &csrf, 16).await;
    assert_eq!(version, 1);
    for uri in [
        "/api/admin/v1/media".into(),
        format!("/api/admin/v1/media/{id}"),
    ] {
        assert_eq!(
            send(
                &stack.router,
                "GET",
                &uri,
                Some(&stranger),
                None,
                None,
                None
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }
    let (status, headers, library) = send(
        &stack.router,
        "GET",
        "/api/admin/v1/media",
        Some(&cookie),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(library["total"], 1);
    assert!(library["items"][0]["deleted_at"].is_null());
    assert!(library["items"][0].get("public_reference_count").is_none());
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    stack.pool.close().await;
}

#[tokio::test]
async fn public_url_works_without_references_and_never_reads_or_refreshes_sessions() {
    let _serial = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (cookie, csrf) = login_as(&stack.router, &stack.idp, "author").await;
    let (id, _) = upload(&stack, &cookie, &csrf, 16).await;
    let uri = format!("/media/{id}");
    sqlx::query("UPDATE sessions SET created_at=now()-interval '20 minutes', last_seen_at=now()-interval '10 minutes'")
        .execute(&stack.pool)
        .await
        .unwrap();
    let before: time::OffsetDateTime =
        sqlx::query_scalar("SELECT last_seen_at FROM sessions LIMIT 1")
            .fetch_one(&stack.pool)
            .await
            .unwrap();
    for session in [None, Some(cookie.as_str()), Some("invalid-session")] {
        let (status, headers, _) =
            send(&stack.router, "GET", &uri, session, None, None, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[header::CONTENT_TYPE], "image/png");
        assert_eq!(
            headers[header::CACHE_CONTROL],
            "public, max-age=31536000, immutable"
        );
        assert!(!headers.contains_key(header::SET_COOKIE));
    }
    let after: time::OffsetDateTime =
        sqlx::query_scalar("SELECT last_seen_at FROM sessions LIMIT 1")
            .fetch_one(&stack.pool)
            .await
            .unwrap();
    assert_eq!(before, after);
    // 没有会话表仍可读取，避免日后无意把图片重新接回身份链路。
    sqlx::query("DROP TABLE sessions")
        .execute(&stack.pool)
        .await
        .unwrap();
    let response = stack
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri(&uri)
                .header("cookie", format!("blog_session={cookie}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let etag = response.headers()[header::ETAG].clone();
    assert_eq!(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .as_ref(),
        png_bytes(16, 16)
    );
    let cached = stack
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri(&uri)
                .header(header::IF_NONE_MATCH, etag.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(cached.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(cached.headers()[header::ETAG], etag);
    let (status, _, body) = send(&stack.router, "HEAD", &uri, None, None, None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, Value::Null);
    for path in [
        "/media/not-a-uuid".into(),
        format!("/media/{}", Uuid::now_v7()),
    ] {
        assert_eq!(
            send(&stack.router, "GET", &path, None, None, None, None)
                .await
                .0,
            StatusCode::NOT_FOUND
        );
    }
    stack.pool.close().await;
}

#[tokio::test]
async fn referenced_media_can_be_trashed_and_restored_without_revoking_its_url() {
    let _serial = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (author, csrf) = login_as(&stack.router, &stack.idp, "author").await;
    let (other, other_csrf) = login_as(&stack.router, &stack.idp, "author2").await;
    let (id, version) = upload(&stack, &author, &csrf, 8).await;
    let avatar = serde_json::json!({"avatar_media_id": id});
    assert_eq!(
        send(
            &stack.router,
            "PUT",
            "/api/admin/v1/me/avatar",
            Some(&author),
            Some(&csrf),
            Some(&avatar),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    let uri = format!("/api/admin/v1/media/{id}");
    let expected = serde_json::json!({"expected_version": version});
    assert_eq!(
        send(
            &stack.router,
            "DELETE",
            &uri,
            Some(&other),
            Some(&other_csrf),
            Some(&expected),
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(
            &stack.router,
            "DELETE",
            &uri,
            Some(&author),
            None,
            Some(&expected),
            None
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        send(
            &stack.router,
            "DELETE",
            &uri,
            Some(&author),
            Some(&csrf),
            Some(&expected),
            None
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        send(
            &stack.router,
            "GET",
            &format!("/media/{id}"),
            None,
            None,
            None,
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    let library = send(
        &stack.router,
        "GET",
        "/api/admin/v1/media",
        Some(&author),
        None,
        None,
        None,
    )
    .await
    .2;
    assert_eq!(library["total"], 0);
    let trash = send(
        &stack.router,
        "GET",
        "/api/admin/v1/media?trash=true",
        Some(&author),
        None,
        None,
        None,
    )
    .await
    .2;
    assert_eq!(trash["total"], 1);
    assert_eq!(trash["items"][0]["reference_count"], 1);
    assert!(trash["items"][0]["deleted_at"].is_string());
    // 历史头像保留，但其他来源不可新选回收站图片。
    assert_eq!(
        send(
            &stack.router,
            "PUT",
            "/api/admin/v1/me/avatar",
            Some(&author),
            Some(&csrf),
            Some(&avatar),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        send(
            &stack.router,
            "PUT",
            "/api/admin/v1/me/avatar",
            Some(&other),
            Some(&other_csrf),
            Some(&avatar),
            None
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let restore = format!("{uri}/restore");
    assert_eq!(
        send(
            &stack.router,
            "POST",
            &restore,
            Some(&author),
            Some(&csrf),
            Some(&expected),
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let next = serde_json::json!({"expected_version": version + 1});
    assert_eq!(
        send(
            &stack.router,
            "POST",
            &restore,
            Some(&other),
            Some(&other_csrf),
            Some(&next),
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (admin, admin_csrf) = login_as(&stack.router, &stack.idp, "editor").await;
    assert_eq!(
        send(
            &stack.router,
            "POST",
            &restore,
            Some(&admin),
            Some(&admin_csrf),
            Some(&next),
            None
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        send(
            &stack.router,
            "PUT",
            "/api/admin/v1/me/avatar",
            Some(&other),
            Some(&other_csrf),
            Some(&avatar),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    let library = send(
        &stack.router,
        "GET",
        "/api/admin/v1/media",
        Some(&author),
        None,
        None,
        None,
    )
    .await
    .2;
    assert_eq!(library["total"], 1);
    assert_eq!(library["items"][0]["version"], version + 2);
    stack.pool.close().await;
}

#[tokio::test]
async fn publicly_readable_media_does_not_expose_private_usage_titles() {
    let _serial = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (author, csrf) = login_as(&stack.router, &stack.idp, "author").await;
    let (id, _) = upload(&stack, &author, &csrf, 8).await;
    let media = Uuid::parse_str(&id).unwrap();
    let other: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE username='author2'")
        .fetch_one(&stack.pool)
        .await
        .unwrap();
    for (slug, status, visibility) in [
        ("private-title", "published", "private"),
        ("draft-title", "draft", "public"),
        ("public-title", "published", "public"),
    ] {
        let post = Uuid::now_v7();
        sqlx::query("INSERT INTO posts(id,author_id,slug,title,content,content_html,content_render_version,status,visibility,published_at) VALUES($1,$2,$3,$3,'body','<p>body</p>',1,$4,$5,now())").bind(post).bind(other).bind(slug).bind(status).bind(visibility).execute(&stack.pool).await.unwrap();
        sqlx::query("INSERT INTO media_refs(media_id,source_type,source_id) VALUES($1,'post',$2)")
            .bind(media)
            .bind(post)
            .execute(&stack.pool)
            .await
            .unwrap();
    }
    let uri = format!("/api/admin/v1/media/{id}");
    let detail = send(&stack.router, "GET", &uri, Some(&author), None, None, None)
        .await
        .2;
    assert_eq!(detail["media"]["reference_count"], 3);
    assert_eq!(detail["hidden_references"], 2);
    assert_eq!(detail["references"][0]["title"], "public-title");
    assert!(!detail.to_string().contains("private-title"));
    assert!(!detail.to_string().contains("draft-title"));
    assert_eq!(
        send(
            &stack.router,
            "GET",
            &format!("/media/{id}"),
            None,
            None,
            None,
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    let (stranger, stranger_csrf) = login_as(&stack.router, &stack.idp, "stranger").await;
    let avatar = serde_json::json!({"avatar_media_id": id});
    assert_eq!(
        send(
            &stack.router,
            "PUT",
            "/api/admin/v1/me/avatar",
            Some(&stranger),
            Some(&stranger_csrf),
            Some(&avatar),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    sqlx::query("UPDATE users SET deleted_at=now(),status='disabled' WHERE username='author'")
        .execute(&stack.pool)
        .await
        .unwrap();
    assert_eq!(
        send(
            &stack.router,
            "GET",
            &format!("/media/{id}"),
            None,
            None,
            None,
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    stack.pool.close().await;
}
