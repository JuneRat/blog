//! 媒体 HTTP 集成测试：上传权限与内容校验、公开访问边界、引用保护删除。
//!
//! 这里验证的是**接口层契约**：状态码、业务码、缓存头与匿名可见性；
//! 引用表与状态机的细节由 infrastructure/tests/media.rs 覆盖。

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
    InMemoryOAuthAttemptStore, InMemorySessionStore, PostgresCategoryRepository,
    PostgresOAuthAccountStore, PostgresOAuthConfigStore, PostgresPageRepository,
    PostgresPostRepository, PostgresRbacStore, PostgresSeriesRepository, PostgresTagRepository,
    PostgresUserRepository, SystemClock,
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
    #[allow(dead_code)]
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
        user_repo.clone(),
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
    for username in ["author", "author2", "editor", "stranger"] {
        let user = users.actor_for_username(username).await.unwrap().user_id.0;
        accounts
            .bind(
                user,
                "https://idp.example",
                &format!("sub-{username}"),
                None,
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
        .merge(media_read_router(MediaReadState {
            media,
            auth: auth.clone(),
        }))
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

/// 创建并（可选）发布一篇引用指定图片的文章。
async fn publish_post_referencing(
    stack: &Stack,
    cookie: &str,
    csrf: &str,
    slug: &str,
    media_id: &str,
) -> Uuid {
    let content = format!("正文\n\n![替代文字](/media/{media_id})\n");
    let (status, _, body) = send(
        &stack.router,
        "POST",
        "/api/admin/v1/posts",
        Some(cookie),
        Some(csrf),
        Some(&serde_json::json!({"slug": slug, "title": "带图文章", "content": content})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "创建文章失败：{body}");
    let post_id: Uuid = body["id"].as_str().unwrap().parse().unwrap();
    let (status, _, body) = send(
        &stack.router,
        "POST",
        &format!("/api/admin/v1/posts/{post_id}/publish"),
        Some(cookie),
        Some(csrf),
        Some(&serde_json::json!({})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "发布失败：{body}");
    post_id
}

/// 创建一篇**只以封面**引用图片的文章并发布，返回文章 ID 与发布后的 version。
///
/// 封面引用与正文图片同源：保存时把 `{封面} ∪ 正文图片` 固化进引用表。
async fn publish_post_with_cover(
    stack: &Stack,
    cookie: &str,
    csrf: &str,
    slug: &str,
    cover_id: &str,
) -> (Uuid, i64) {
    let (status, _, body) = send(
        &stack.router,
        "POST",
        "/api/admin/v1/posts",
        Some(cookie),
        Some(csrf),
        Some(&serde_json::json!({
            "slug": slug,
            "title": "带封面的文章",
            "content": "正文本身没有图片，封面是唯一引用。",
            "cover_media_id": cover_id,
        })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "创建带封面文章失败：{body}");
    let post_id: Uuid = body["id"].as_str().unwrap().parse().unwrap();
    let (status, _, body) = send(
        &stack.router,
        "POST",
        &format!("/api/admin/v1/posts/{post_id}/publish"),
        Some(cookie),
        Some(csrf),
        Some(&serde_json::json!({})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "发布失败：{body}");
    (post_id, body["version"].as_i64().unwrap())
}

// ---------------------------------------------------------------------------
// 权限与内容校验
// ---------------------------------------------------------------------------
#[tokio::test]
async fn upload_requires_permission_and_validates_file_content() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;

    let (stranger, stranger_csrf) = login_as(&stack.router, &stack.idp, "stranger").await;
    let (status, _, body) = send(
        &stack.router,
        "POST",
        "/api/admin/v1/media?filename=x.png",
        Some(&stranger),
        Some(&stranger_csrf),
        None,
        Some(png_bytes(4, 4)),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "无 media.upload 应 403：{body}"
    );
    assert_eq!(body["code"], "forbidden");

    let (author, csrf) = login_as(&stack.router, &stack.idp, "author").await;
    // 扩展名声称是图片但内容不是：必须按内容拒绝。
    for bytes in [
        b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>".to_vec(),
        b"not an image".to_vec(),
    ] {
        let (status, _, body) = send(
            &stack.router,
            "POST",
            "/api/admin/v1/media?filename=evil.png",
            Some(&author),
            Some(&csrf),
            None,
            Some(bytes),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "实际：{body}");
        assert_eq!(body["code"], "invalid_request");
    }

    // 缺少 CSRF 的上传必须被拒（写请求统一要求）。
    let (status, _, _) = send(
        &stack.router,
        "POST",
        "/api/admin/v1/media?filename=x.png",
        Some(&author),
        None,
        None,
        Some(png_bytes(4, 4)),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn media_library_and_detail_require_read_permission() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (author, csrf) = login_as(&stack.router, &stack.idp, "author").await;
    let (id, _) = upload(&stack, &author, &csrf, 40).await;

    let (stranger, stranger_csrf) = login_as(&stack.router, &stack.idp, "stranger").await;
    let (status, _, body) = send(
        &stack.router,
        "GET",
        "/api/admin/v1/media",
        Some(&stranger),
        Some(&stranger_csrf),
        None,
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "无 media.read 应 403：{body}"
    );

    let (status, headers, body) = send(
        &stack.router,
        "GET",
        "/api/admin/v1/media?page=1",
        Some(&author),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "实际：{body}");
    assert_eq!(headers.get(header::CACHE_CONTROL).unwrap(), "no-store");
    assert_eq!(body["total"], 1);
    let item = &body["items"][0];
    assert_eq!(item["id"], id.as_str());
    assert_eq!(item["original_name"], "photo.png");
    assert_eq!(item["width"], 40);
    assert_eq!(item["owner_display"], "author");
    assert_eq!(item["reference_count"], 0);
    assert_eq!(item["public_reference_count"], 0);
    assert_eq!(item["url"], format!("/media/{id}"));

    // 使用位置详情：未引用时为空列表。
    let (status, _, body) = send(
        &stack.router,
        "GET",
        &format!("/api/admin/v1/media/{id}"),
        Some(&author),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["references"].as_array().unwrap().len(), 0);
}

// ---------------------------------------------------------------------------
// 公开访问边界
// ---------------------------------------------------------------------------

#[tokio::test]
async fn anonymous_read_follows_publish_and_withdraw() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (author, csrf) = login_as(&stack.router, &stack.idp, "author").await;
    let (id, _) = upload(&stack, &author, &csrf, 24).await;

    // 新上传默认不公开：匿名 404，路径与「不存在」无法区分。
    let (status, _, _) = send(
        &stack.router,
        "GET",
        &format!("/media/{id}"),
        None,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // 上传者持有 media.read：后台预览可读，且不得进入任何缓存。
    let (status, headers, _) = send(
        &stack.router,
        "GET",
        &format!("/media/{id}"),
        Some(&author),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers.get(header::CACHE_CONTROL).unwrap(), "no-store");

    // 草稿引用仍然不公开。
    let post_id = publish_post_referencing(&stack, &author, &csrf, "with-image", &id).await;
    let (status, headers, body) = send(
        &stack.router,
        "GET",
        &format!("/media/{id}"),
        None,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "公开发布后匿名必须可读");
    assert_eq!(headers.get(header::CONTENT_TYPE).unwrap(), "image/png");
    assert_eq!(
        headers.get(header::CACHE_CONTROL).unwrap(),
        "no-cache",
        "公开图片只能重校验，不能长缓存"
    );
    let etag = headers
        .get(header::ETAG)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(body.is_string(), "图片响应体应是字节流");

    // 条件请求命中 304。
    let (status, _, _) = send(
        &stack.router,
        "GET",
        &format!("/media/{id}"),
        None,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let response = stack
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/media/{id}"))
                .header(header::IF_NONE_MATCH, &etag)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);

    // 撤回后停止匿名访问，后台预览仍可用。
    let (status, _, body) = send(
        &stack.router,
        "POST",
        &format!("/api/admin/v1/posts/{post_id}/unpublish"),
        Some(&author),
        Some(&csrf),
        Some(&serde_json::json!({})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "撤回失败：{body}");
    let (status, _, _) = send(
        &stack.router,
        "GET",
        &format!("/media/{id}"),
        None,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "撤回后必须立即停止匿名读取");
    let (status, _, _) = send(
        &stack.router,
        "GET",
        &format!("/media/{id}"),
        Some(&author),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "后台预览不受撤回影响");

    // 畸形 id 也是 404，不泄漏解析差异。
    let (status, _, _) = send(
        &stack.router,
        "GET",
        "/media/not-a-uuid",
        None,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// 引用保护与删除
// ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_is_refused_while_referenced_then_succeeds_after_detaching() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (author, csrf) = login_as(&stack.router, &stack.idp, "author").await;
    let (id, version) = upload(&stack, &author, &csrf, 16).await;
    let post_id = publish_post_referencing(&stack, &author, &csrf, "referencing", &id).await;

    let (status, _, body) = send(
        &stack.router,
        "DELETE",
        &format!("/api/admin/v1/media/{id}"),
        Some(&author),
        Some(&csrf),
        Some(&serde_json::json!({"expected_version": version})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "被引用时必须拒绝：{body}");
    assert_eq!(body["code"], "media_in_use");

    // 使用位置必须能被界面读到，说明「为什么不能删」。
    let (status, _, body) = send(
        &stack.router,
        "GET",
        &format!("/api/admin/v1/media/{id}"),
        Some(&author),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["media"]["reference_count"], 1);
    assert_eq!(body["media"]["public_reference_count"], 1);
    let reference = &body["references"][0];
    assert_eq!(reference["kind"], "post");
    assert_eq!(reference["slug"], "referencing");
    assert_eq!(reference["public"], true);
    assert_eq!(reference["status"], "published");

    // 解除引用（保存不含图片的正文）后即可删除。
    let post = send(
        &stack.router,
        "GET",
        &format!("/api/admin/v1/posts/{post_id}"),
        Some(&author),
        None,
        None,
        None,
    )
    .await
    .2;
    let (status, _, body) = send(
        &stack.router,
        "PATCH",
        &format!("/api/admin/v1/posts/{post_id}"),
        Some(&author),
        Some(&csrf),
        Some(&serde_json::json!({
            "content": "已移除图片",
            "expected_version": post["version"],
        })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "保存失败：{body}");

    let (status, _, _) = send(
        &stack.router,
        "DELETE",
        &format!("/api/admin/v1/media/{id}"),
        Some(&author),
        Some(&csrf),
        Some(&serde_json::json!({"expected_version": version})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // 删除后：匿名与后台预览都不再可读，库里也不再出现。
    let (status, _, _) = send(
        &stack.router,
        "GET",
        &format!("/media/{id}"),
        Some(&author),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, _, body) = send(
        &stack.router,
        "GET",
        "/api/admin/v1/media",
        Some(&author),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(body["total"], 0, "已删除资产不再出现在媒体库");
}

#[tokio::test]
async fn deleting_another_authors_media_requires_the_any_permission() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (editor, editor_csrf) = login_as(&stack.router, &stack.idp, "editor").await;
    let (id, version) = upload(&stack, &editor, &editor_csrf, 12).await;

    let (author, csrf) = login_as(&stack.router, &stack.idp, "author").await;
    let (status, _, body) = send(
        &stack.router,
        "DELETE",
        &format!("/api/admin/v1/media/{id}"),
        Some(&author),
        Some(&csrf),
        Some(&serde_json::json!({"expected_version": version})),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "Author 没有 media.delete_any，不得删除他人上传：{body}"
    );

    // Editor 持有 media.delete_any。
    let (status, _, _) = send(
        &stack.router,
        "DELETE",
        &format!("/api/admin/v1/media/{id}"),
        Some(&editor),
        Some(&editor_csrf),
        Some(&serde_json::json!({"expected_version": version})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn saving_content_with_an_unknown_media_reference_is_rejected() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (author, csrf) = login_as(&stack.router, &stack.idp, "author").await;
    let ghost = Uuid::now_v7();

    let (status, _, body) = send(
        &stack.router,
        "POST",
        "/api/admin/v1/posts",
        Some(&author),
        Some(&csrf),
        Some(&serde_json::json!({
            "slug": "ghost-image",
            "title": "引用不存在的图片",
            "content": format!("![x](/media/{ghost})"),
        })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "实际：{body}");
    assert_eq!(body["code"], "invalid_request");

    // 整篇草稿都没有落库。
    let (status, _, listing) = send(
        &stack.router,
        "GET",
        "/api/admin/v1/posts",
        Some(&author),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !listing.to_string().contains("ghost-image"),
        "失败创建不得落库：{listing}"
    );
}

/// `media.read` 只授予「浏览媒体库」，不得顺带泄露他人草稿/私密内容的标题与 slug。
///
/// 关键区分：**引用计数是全局的**（它决定能否删除），**使用位置必须按调用者
/// 的内容权限过滤**；被过滤掉的条数以 `hidden_references` 如实返回。
#[tokio::test]
async fn usage_locations_are_filtered_by_the_callers_content_permissions() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;

    // author 上传；author2 在自己的草稿里引用它。
    let (author, author_csrf) = login_as(&stack.router, &stack.idp, "author").await;
    let (id, version) = upload(&stack, &author, &author_csrf, 20).await;

    let (author2, author2_csrf) = login_as(&stack.router, &stack.idp, "author2").await;
    let (status, _, body) = send(
        &stack.router,
        "POST",
        "/api/admin/v1/posts",
        Some(&author2),
        Some(&author2_csrf),
        Some(&serde_json::json!({
            "slug": "author2-draft",
            "title": "别人的草稿标题",
            "content": format!("![x](/media/{id})"),
        })),
        None,
    )
    .await;
    let post_id: Uuid = body["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(status, StatusCode::CREATED, "创建草稿失败：{body}");

    // author（只有 post.read own）：看不到 author2 的草稿，但计数是全局的。
    let (status, _, body) = send(
        &stack.router,
        "GET",
        &format!("/api/admin/v1/media/{id}"),
        Some(&author),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "实际：{body}");
    assert_eq!(body["media"]["reference_count"], 1, "引用计数必须全局");
    assert_eq!(
        body["references"].as_array().unwrap().len(),
        0,
        "不得泄露他人草稿的使用位置：{body}"
    );
    assert_eq!(body["hidden_references"], 1);
    let raw = body.to_string();
    assert!(
        !raw.contains("author2-draft") && !raw.contains("别人的草稿标题"),
        "响应里不得出现他人草稿的 slug 或标题：{raw}"
    );

    // 删除保护仍按全部引用判定：作者看不到那处引用也删不掉。
    let (status, _, body) = send(
        &stack.router,
        "DELETE",
        &format!("/api/admin/v1/media/{id}"),
        Some(&author),
        Some(&author_csrf),
        Some(&serde_json::json!({"expected_version": version})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "实际：{body}");
    assert_eq!(body["code"], "media_in_use");

    // 发布之后，这处引用变成**公开可读**的内容：它的标题与 slug 本来就能匿名访问，
    // 因此对任何持有 media.read 的人都可见，不再算作隐藏项。
    let (status, _, body) = send(
        &stack.router,
        "POST",
        &format!("/api/admin/v1/posts/{post_id}/publish"),
        Some(&author2),
        Some(&author2_csrf),
        Some(&serde_json::json!({})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "发布失败：{body}");

    let (status, _, body) = send(
        &stack.router,
        "GET",
        &format!("/api/admin/v1/media/{id}"),
        Some(&author),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["references"].as_array().unwrap().len(),
        1,
        "公开可见的内容不算隐藏：{body}"
    );
    assert_eq!(body["references"][0]["public"], true);
    assert_eq!(body["hidden_references"], 0);

    // editor 持有 post.read_any：可以看到该位置，且没有隐藏项。
    let (editor, _) = login_as(&stack.router, &stack.idp, "editor").await;
    let (status, _, body) = send(
        &stack.router,
        "GET",
        &format!("/api/admin/v1/media/{id}"),
        Some(&editor),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["references"].as_array().unwrap().len(), 1);
    assert_eq!(body["references"][0]["slug"], "author2-draft");
    assert_eq!(
        body["references"][0]["public"], true,
        "此刻该文章已公开发布"
    );
    assert_eq!(body["hidden_references"], 0);
}

// ---------------------------------------------------------------------------
// 封面：随内容保存提交、匿名边界、删除保护、替换与移除
// ---------------------------------------------------------------------------

/// 封面引用走与正文图片**同一张引用表**：创建/保存即提交，并被删除保护与
/// 公开来源判定共同使用；替换或移除封面会在同一事务里释放旧引用。
#[tokio::test]
async fn post_cover_is_saved_with_content_and_protects_the_image() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (author, csrf) = login_as(&stack.router, &stack.idp, "author").await;
    let (cover, cover_version) = upload(&stack, &author, &csrf, 32).await;
    let (cover2, cover2_version) = upload(&stack, &author, &csrf, 30).await;

    let (post_id, version) =
        publish_post_with_cover(&stack, &author, &csrf, "cover-post", &cover).await;

    // 后台读取封面：id 与站内地址都在，界面无需自己拼前缀。
    let (status, _, body) = send(
        &stack.router,
        "GET",
        &format!("/api/admin/v1/posts/{post_id}"),
        Some(&author),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["cover_media_id"], cover.as_str());
    assert_eq!(body["cover_url"], format!("/media/{cover}"));

    // 公开 + 被封面引用：匿名可读，且删除被引用保护拒绝。
    let (status, headers, _) = send(
        &stack.router,
        "GET",
        &format!("/media/{cover}"),
        None,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers.get(header::CACHE_CONTROL).unwrap(), "no-cache");
    let (status, _, body) = send(
        &stack.router,
        "DELETE",
        &format!("/api/admin/v1/media/{cover}"),
        Some(&author),
        Some(&csrf),
        Some(&serde_json::json!({"expected_version": cover_version})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "封面引用必须阻止删除：{body}");
    assert_eq!(body["code"], "media_in_use");

    // 使用位置把这条封面引用展示出来。
    let (status, _, body) = send(
        &stack.router,
        "GET",
        &format!("/api/admin/v1/media/{cover}"),
        Some(&author),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["media"]["reference_count"], 1);
    assert_eq!(body["media"]["public_reference_count"], 1);
    assert_eq!(body["references"][0]["kind"], "post");
    assert_eq!(body["references"][0]["slug"], "cover-post");
    assert_eq!(body["references"][0]["public"], true);

    // 替换封面：新图受保护，旧图在同一事务被释放。
    let (status, _, body) = send(
        &stack.router,
        "PATCH",
        &format!("/api/admin/v1/posts/{post_id}"),
        Some(&author),
        Some(&csrf),
        Some(&serde_json::json!({"cover_media_id": cover2, "expected_version": version})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "替换封面失败：{body}");
    assert_eq!(body["cover_media_id"], cover2.as_str());
    let (status, _, _) = send(
        &stack.router,
        "DELETE",
        &format!("/api/admin/v1/media/{cover}"),
        Some(&author),
        Some(&csrf),
        Some(&serde_json::json!({"expected_version": cover_version})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "旧封面已无引用，应可删除");
    let (status, _, _) = send(
        &stack.router,
        "DELETE",
        &format!("/api/admin/v1/media/{cover2}"),
        Some(&author),
        Some(&csrf),
        Some(&serde_json::json!({"expected_version": cover2_version})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "新封面仍被引用");

    // 移除封面：引用随保存一起消失，图片立即可删。
    let current = send(
        &stack.router,
        "GET",
        &format!("/api/admin/v1/posts/{post_id}"),
        Some(&author),
        None,
        None,
        None,
    )
    .await
    .2;
    let (status, _, body) = send(
        &stack.router,
        "PATCH",
        &format!("/api/admin/v1/posts/{post_id}"),
        Some(&author),
        Some(&csrf),
        Some(&serde_json::json!({
            "cover_media_id": null,
            "expected_version": current["version"],
        })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "移除封面失败：{body}");
    assert!(body["cover_media_id"].is_null());
    assert!(body["cover_url"].is_null());
    let (status, _, _) = send(
        &stack.router,
        "DELETE",
        &format!("/api/admin/v1/media/{cover2}"),
        Some(&author),
        Some(&csrf),
        Some(&serde_json::json!({"expected_version": cover2_version})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "移除封面后应可删除");
}

/// 封面的匿名访问跟随文章公开状态：草稿/撤回/private/回收站都立即失效，
/// 后台预览不受影响。
#[tokio::test]
async fn cover_anonymous_access_follows_post_visibility() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (author, csrf) = login_as(&stack.router, &stack.idp, "author").await;
    let (cover, _) = upload(&stack, &author, &csrf, 22).await;

    // 先建草稿（带封面）：不公开。
    let (status, _, body) = send(
        &stack.router,
        "POST",
        "/api/admin/v1/posts",
        Some(&author),
        Some(&csrf),
        Some(&serde_json::json!({
            "slug": "cover-visibility",
            "title": "封面可见性",
            "content": "正文",
            "cover_media_id": cover,
        })),
        None,
    )
    .await;
    let post_id: Uuid = body["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, _, _) = send(
        &stack.router,
        "GET",
        &format!("/media/{cover}"),
        None,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "草稿封面不公开");

    // 发布：匿名可读。
    let (status, _, body) = send(
        &stack.router,
        "POST",
        &format!("/api/admin/v1/posts/{post_id}/publish"),
        Some(&author),
        Some(&csrf),
        Some(&serde_json::json!({})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let mut version = body["version"].as_i64().unwrap();
    let (status, _, _) = send(
        &stack.router,
        "GET",
        &format!("/media/{cover}"),
        None,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // 撤回：匿名立即失效，后台预览仍在。
    let (status, _, body) = send(
        &stack.router,
        "POST",
        &format!("/api/admin/v1/posts/{post_id}/unpublish"),
        Some(&author),
        Some(&csrf),
        Some(&serde_json::json!({"expected_version": version})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    version = body["version"].as_i64().unwrap();
    let (status, _, _) = send(
        &stack.router,
        "GET",
        &format!("/media/{cover}"),
        None,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "撤回后立即停止匿名读取");
    let (status, _, _) = send(
        &stack.router,
        "GET",
        &format!("/media/{cover}"),
        Some(&author),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "后台预览不受影响");

    // 重新发布后改为 private：匿名失效。
    let (status, _, body) = send(
        &stack.router,
        "POST",
        &format!("/api/admin/v1/posts/{post_id}/publish"),
        Some(&author),
        Some(&csrf),
        Some(&serde_json::json!({"expected_version": version})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    version = body["version"].as_i64().unwrap();
    let (status, _, body) = send(
        &stack.router,
        "PATCH",
        &format!("/api/admin/v1/posts/{post_id}"),
        Some(&author),
        Some(&csrf),
        Some(&serde_json::json!({"visibility": "private", "expected_version": version})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    version = body["version"].as_i64().unwrap();
    let (status, _, _) = send(
        &stack.router,
        "GET",
        &format!("/media/{cover}"),
        None,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "private 后立即停止匿名读取");

    // 改回 public 并入回收站：匿名再次失效，且引用仍占用（不得被删除）。
    let (status, _, body) = send(
        &stack.router,
        "PATCH",
        &format!("/api/admin/v1/posts/{post_id}"),
        Some(&author),
        Some(&csrf),
        Some(&serde_json::json!({"visibility": "public", "expected_version": version})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    version = body["version"].as_i64().unwrap();
    let (status, _, body) = send(
        &stack.router,
        "POST",
        &format!("/api/admin/v1/posts/{post_id}/trash"),
        Some(&author),
        Some(&csrf),
        Some(&serde_json::json!({"expected_version": version})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _, _) = send(
        &stack.router,
        "GET",
        &format!("/media/{cover}"),
        None,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "回收站文章封面立即失效");
}

/// 封面写入同样受 expected_version 约束：旧版本请求一律 409，且不改动服务器内容。
#[tokio::test]
async fn stale_cover_save_is_a_version_conflict() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (author, csrf) = login_as(&stack.router, &stack.idp, "author").await;
    let (a, _) = upload(&stack, &author, &csrf, 26).await;
    let (b, _) = upload(&stack, &author, &csrf, 25).await;
    let (post_id, version) =
        publish_post_with_cover(&stack, &author, &csrf, "cover-conflict", &a).await;

    let (status, _, body) = send(
        &stack.router,
        "PATCH",
        &format!("/api/admin/v1/posts/{post_id}"),
        Some(&author),
        Some(&csrf),
        Some(&serde_json::json!({"cover_media_id": b, "expected_version": version})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "首次替换应成功：{body}");

    // 旧 version 再提交一次（即使只是移除封面）也必须冲突，而不是静默成功。
    let (status, _, body) = send(
        &stack.router,
        "PATCH",
        &format!("/api/admin/v1/posts/{post_id}"),
        Some(&author),
        Some(&csrf),
        Some(&serde_json::json!({"cover_media_id": null, "expected_version": version})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "旧版本必须 409：{body}");
    assert_eq!(body["code"], "version_conflict");

    let post = send(
        &stack.router,
        "GET",
        &format!("/api/admin/v1/posts/{post_id}"),
        Some(&author),
        None,
        None,
        None,
    )
    .await
    .2;
    assert_eq!(
        post["cover_media_id"],
        b.as_str(),
        "失败请求不得改动服务器内容"
    );
}

/// 真并发：替换封面（把封面指向候选图）与删除该候选图只有一个能赢。
///
/// 锁序保证两种结果都安全：要么封面写入先落地（图片随即被引用，删除被拒），
/// 要么删除先落地（图片不再 `ready`，整次封面保存回滚，封面保持旧图）。
/// 不存在「引用已写入、文件已回收」的破图中间态。
#[tokio::test]
async fn concurrent_cover_replacement_and_image_deletion_are_serialized() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (author, csrf) = login_as(&stack.router, &stack.idp, "author").await;
    let (old_cover, _) = upload(&stack, &author, &csrf, 26).await;
    let (candidate, candidate_version) = upload(&stack, &author, &csrf, 25).await;
    let (post_id, version) =
        publish_post_with_cover(&stack, &author, &csrf, "cover-race", &old_cover).await;

    // 两个 JSON 体必须活到 join 之后：future 借用了它们。
    let patch_json = serde_json::json!({"cover_media_id": candidate, "expected_version": version});
    let delete_json = serde_json::json!({"expected_version": candidate_version});
    let patch_uri = format!("/api/admin/v1/posts/{post_id}");
    let delete_uri = format!("/api/admin/v1/media/{candidate}");
    let patch = send(
        &stack.router,
        "PATCH",
        &patch_uri,
        Some(&author),
        Some(&csrf),
        Some(&patch_json),
        None,
    );
    let delete = send(
        &stack.router,
        "DELETE",
        &delete_uri,
        Some(&author),
        Some(&csrf),
        Some(&delete_json),
        None,
    );
    let ((patch_status, _, patch_body), (delete_status, _, delete_body)) =
        tokio::join!(patch, delete);

    let post = send(
        &stack.router,
        "GET",
        &format!("/api/admin/v1/posts/{post_id}"),
        Some(&author),
        None,
        None,
        None,
    )
    .await
    .2;

    match (patch_status, delete_status) {
        (StatusCode::OK, StatusCode::CONFLICT) => {
            assert_eq!(delete_body["code"], "media_in_use");
            assert_eq!(
                post["cover_media_id"],
                candidate.as_str(),
                "封面写入先落地：新图成为引用，删除被引用保护拒绝"
            );
        }
        (StatusCode::BAD_REQUEST, StatusCode::NO_CONTENT) => {
            assert_eq!(patch_body["code"], "invalid_request");
            assert_eq!(
                post["cover_media_id"],
                old_cover.as_str(),
                "删除先落地：整次封面保存回滚，封面保持旧图"
            );
        }
        other => panic!("只应出现上述两种次序，实际 {other:?}"),
    }
}

/// 系列封面与文章封面同一套规则：系列目录页公开可达，因此系列封面即公开来源；
/// 删除系列必须在同一事务清理引用，之后图片才可删除。
#[tokio::test]
async fn series_cover_is_public_and_protects_the_image() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    // series.manage 属于 Editor（Author 没有）。
    let (editor, csrf) = login_as(&stack.router, &stack.idp, "editor").await;
    let (cover, cover_version) = upload(&stack, &editor, &csrf, 28).await;

    let (status, _, body) = send(
        &stack.router,
        "POST",
        "/api/admin/v1/series",
        Some(&editor),
        Some(&csrf),
        Some(&serde_json::json!({"name": "封面系列", "slug": "cover-series"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(body["cover_media_id"].is_null());
    let version = body["version"].as_i64().unwrap();

    let (status, _, body) = send(
        &stack.router,
        "PATCH",
        "/api/admin/v1/series/cover-series",
        Some(&editor),
        Some(&csrf),
        Some(&serde_json::json!({
            "name": "封面系列",
            "cover_media_id": cover,
            "expected_version": version,
        })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "设置系列封面失败：{body}");
    assert_eq!(body["cover_media_id"], cover.as_str());
    assert_eq!(body["cover_url"], format!("/media/{cover}"));

    // 系列存在即公开可达：封面构成公开来源。
    let (status, _, _) = send(
        &stack.router,
        "GET",
        &format!("/media/{cover}"),
        None,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "系列封面是公开来源");

    let (status, _, body) = send(
        &stack.router,
        "DELETE",
        &format!("/api/admin/v1/media/{cover}"),
        Some(&editor),
        Some(&csrf),
        Some(&serde_json::json!({"expected_version": cover_version})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "系列封面必须阻止删除：{body}");
    let (status, _, body) = send(
        &stack.router,
        "GET",
        &format!("/api/admin/v1/media/{cover}"),
        Some(&editor),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["references"][0]["kind"], "series");
    assert_eq!(body["references"][0]["slug"], "cover-series");
    assert_eq!(body["references"][0]["public"], true);

    // 旧版本再次提交必须 409。
    let (status, _, body) = send(
        &stack.router,
        "PATCH",
        "/api/admin/v1/series/cover-series",
        Some(&editor),
        Some(&csrf),
        Some(&serde_json::json!({
            "name": "封面系列",
            "cover_media_id": null,
            "expected_version": version,
        })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "旧版本必须 409：{body}");
    assert_eq!(body["code"], "version_conflict");

    // 删除系列（无成员）清理引用后，图片才能删除。
    let (status, _, body) = send(
        &stack.router,
        "DELETE",
        "/api/admin/v1/series/cover-series",
        Some(&editor),
        Some(&csrf),
        Some(&serde_json::json!({"expected_version": version + 1})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "删除系列失败：{body}");
    let (status, _, _) = send(
        &stack.router,
        "GET",
        &format!("/media/{cover}"),
        None,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "系列删除后封面不再是公开来源"
    );
    let (status, _, _) = send(
        &stack.router,
        "DELETE",
        &format!("/api/admin/v1/media/{cover}"),
        Some(&editor),
        Some(&csrf),
        Some(&serde_json::json!({"expected_version": cover_version})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "引用随系列删除被清理");
}

/// 正文与封面引用同一张图时只算一处引用（复合主键去重），
/// 移除封面后正文引用仍然占用。
#[tokio::test]
async fn cover_and_body_referencing_the_same_image_is_one_reference() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (author, csrf) = login_as(&stack.router, &stack.idp, "author").await;
    let (id, version) = upload(&stack, &author, &csrf, 24).await;

    let (status, _, body) = send(
        &stack.router,
        "POST",
        "/api/admin/v1/posts",
        Some(&author),
        Some(&csrf),
        Some(&serde_json::json!({
            "slug": "shared-image",
            "title": "同一张图既是封面也是正文图片",
            "content": format!("![正文里的同一张图](/media/{id})"),
            "cover_media_id": id,
        })),
        None,
    )
    .await;
    let post_id: Uuid = body["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (status, _, body) = send(
        &stack.router,
        "GET",
        &format!("/api/admin/v1/media/{id}"),
        Some(&author),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["media"]["reference_count"], 1,
        "正文与封面同一张图必须只计一处引用：{body}"
    );

    // 移除封面：正文仍引用，图片继续受保护。
    let post = send(
        &stack.router,
        "GET",
        &format!("/api/admin/v1/posts/{post_id}"),
        Some(&author),
        None,
        None,
        None,
    )
    .await
    .2;
    let (status, _, body) = send(
        &stack.router,
        "PATCH",
        &format!("/api/admin/v1/posts/{post_id}"),
        Some(&author),
        Some(&csrf),
        Some(&serde_json::json!({
            "cover_media_id": null,
            "expected_version": post["version"],
        })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "移除封面失败：{body}");
    let (status, _, _) = send(
        &stack.router,
        "DELETE",
        &format!("/api/admin/v1/media/{id}"),
        Some(&author),
        Some(&csrf),
        Some(&serde_json::json!({"expected_version": version})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "正文引用仍在，删除应被拒绝");
}

// ---------------------------------------------------------------------------
// 媒体库第三段：用户头像与站点 logo
// ---------------------------------------------------------------------------

/// 自助头像：本人即可设置（无需额外权限），匿名可读（账号未软删除），
/// 并阻止删除被引用的图片；换头像**不**递增 users.version，当前会话不被踢下线。
#[tokio::test]
async fn self_avatar_is_public_and_does_not_invalidate_the_session() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (author, csrf) = login_as(&stack.router, &stack.idp, "author").await;
    let (avatar, avatar_version) = upload(&stack, &author, &csrf, 20).await;

    // 缺 CSRF 的写请求必须被拒。
    let (status, _, _) = send(
        &stack.router,
        "PUT",
        "/api/admin/v1/me/avatar",
        Some(&author),
        None,
        Some(&serde_json::json!({"avatar_media_id": avatar})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _, body) = send(
        &stack.router,
        "PUT",
        "/api/admin/v1/me/avatar",
        Some(&author),
        Some(&csrf),
        Some(&serde_json::json!({"avatar_media_id": avatar})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["avatar_media_id"], avatar.as_str());
    assert_eq!(body["avatar_url"], format!("/media/{avatar}"));

    // 同一会话仍然有效：换头像不递增 users.version（那是会话绑定版本）。
    let (status, _, me) = send(
        &stack.router,
        "GET",
        "/api/admin/v1/me",
        Some(&author),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "换头像不得让会话失效：{me}");
    assert_eq!(me["avatar_media_id"], avatar.as_str());
    assert_eq!(me["username"], "author");

    // 账号未软删除 → 头像匿名可读。
    let (status, headers, _) = send(
        &stack.router,
        "GET",
        &format!("/media/{avatar}"),
        None,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "未软删除账号的头像应匿名可读");
    assert_eq!(headers.get(header::CACHE_CONTROL).unwrap(), "no-cache");

    // 删除保护与使用位置。
    let (status, _, body) = send(
        &stack.router,
        "DELETE",
        &format!("/api/admin/v1/media/{avatar}"),
        Some(&author),
        Some(&csrf),
        Some(&serde_json::json!({"expected_version": avatar_version})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "头像引用必须阻止删除：{body}");
    let (status, _, body) = send(
        &stack.router,
        "GET",
        &format!("/api/admin/v1/media/{avatar}"),
        Some(&author),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["references"][0]["kind"], "user");
    assert_eq!(body["references"][0]["slug"], "author");
    assert_eq!(body["references"][0]["public"], true);

    // 清除头像后引用释放，图片可删。
    let (status, _, body) = send(
        &stack.router,
        "PUT",
        "/api/admin/v1/me/avatar",
        Some(&author),
        Some(&csrf),
        Some(&serde_json::json!({"avatar_media_id": null})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["avatar_media_id"].is_null());
    let (status, _, _) = send(
        &stack.router,
        "GET",
        &format!("/media/{avatar}"),
        None,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "清除头像后不再是公开来源");
    let (status, _, _) = send(
        &stack.router,
        "DELETE",
        &format!("/api/admin/v1/media/{avatar}"),
        Some(&author),
        Some(&csrf),
        Some(&serde_json::json!({"expected_version": avatar_version})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "清除头像后应可删除");
}

/// 归属校验：无 `media.read` 的用户不能把他人**私有**图片附着为头像
/// （403 `media_not_attachable`）；同一图片随公开文章发布成为公开来源后，
/// 附着不再产生新的暴露面，放行。
#[tokio::test]
async fn avatar_cannot_expose_another_users_private_image() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (author, author_csrf) = login_as(&stack.router, &stack.idp, "author").await;
    let (image, _version) = upload(&stack, &author, &author_csrf, 24).await;
    let (stranger, csrf) = login_as(&stack.router, &stack.idp, "stranger").await;

    // 私有图片：附着被拒，且不泄漏更多细节。
    let (status, _, body) = send(
        &stack.router,
        "PUT",
        "/api/admin/v1/me/avatar",
        Some(&stranger),
        Some(&csrf),
        Some(&serde_json::json!({"avatar_media_id": image})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["code"], "media_not_attachable");

    // 图片仍是私有的：匿名不可读。
    let (status, _, _) = send(
        &stack.router,
        "GET",
        &format!("/media/{image}"),
        None,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "私有图片不得经头像变成公开");

    // 同一图片被公开文章引用后成为公开来源：附着放行（本就匿名可读）。
    publish_post_referencing(
        &stack,
        &author,
        &author_csrf,
        "avatar-attach-public",
        &image,
    )
    .await;
    let (status, _, body) = send(
        &stack.router,
        "PUT",
        "/api/admin/v1/me/avatar",
        Some(&stranger),
        Some(&csrf),
        Some(&serde_json::json!({"avatar_media_id": image})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["avatar_media_id"], image.as_str());

    let (status, _, _) = send(
        &stack.router,
        "GET",
        &format!("/media/{image}"),
        None,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "公开来源图片匿名可读");
}

/// 站点 logo：与 settings.site 整组保存同事务写入引用；站点配置公开，
/// 因此 logo 只要有引用即匿名可读；删除受保护，版本 CAS 照常。
#[tokio::test]
async fn site_logo_is_saved_with_settings_and_protects_the_image() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    // settings.manage 来自追加的 admin 角色；media.upload 来自 editor 角色。
    let (editor, csrf) = login_as(&stack.router, &stack.idp, "editor").await;
    let (logo, logo_version) = upload(&stack, &editor, &csrf, 30).await;

    // 初始未配置：无 logo，版本 0。
    let (status, _, body) = send(
        &stack.router,
        "GET",
        "/api/admin/v1/settings/site",
        Some(&editor),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["logo_media_id"].is_null());
    assert_eq!(body["version"], 0);

    // 保存 logo（整组 PUT）。
    let (status, _, body) = send(
        &stack.router,
        "PUT",
        "/api/admin/v1/settings/site",
        Some(&editor),
        Some(&csrf),
        Some(&serde_json::json!({
            "title": "测试站点",
            "description": "站点描述",
            "logo_media_id": logo,
            "expected_version": 0,
        })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["logo_media_id"], logo.as_str());
    assert_eq!(body["logo_url"], format!("/media/{logo}"));
    assert_eq!(body["version"], 1);
    assert_eq!(body["source"], "database");

    // 站点配置公开 → logo 是公开来源。
    let (status, headers, _) = send(
        &stack.router,
        "GET",
        &format!("/media/{logo}"),
        None,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "站点 logo 应匿名可读");
    assert_eq!(headers.get(header::CACHE_CONTROL).unwrap(), "no-cache");

    // 删除保护与使用位置。
    let (status, _, body) = send(
        &stack.router,
        "DELETE",
        &format!("/api/admin/v1/media/{logo}"),
        Some(&editor),
        Some(&csrf),
        Some(&serde_json::json!({"expected_version": logo_version})),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "站点 logo 必须阻止删除：{body}"
    );
    let (status, _, body) = send(
        &stack.router,
        "GET",
        &format!("/api/admin/v1/media/{logo}"),
        Some(&editor),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["references"][0]["kind"], "site");
    assert_eq!(body["references"][0]["public"], true);

    // 旧版本再次提交必须 409。
    let (status, _, body) = send(
        &stack.router,
        "PUT",
        "/api/admin/v1/settings/site",
        Some(&editor),
        Some(&csrf),
        Some(&serde_json::json!({
            "title": "测试站点",
            "description": "站点描述",
            "logo_media_id": logo,
            "expected_version": 0,
        })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "旧版本必须 409：{body}");
    assert_eq!(body["code"], "version_conflict");

    // 移除 logo：引用释放，图片可删。
    let (status, _, body) = send(
        &stack.router,
        "PUT",
        "/api/admin/v1/settings/site",
        Some(&editor),
        Some(&csrf),
        Some(&serde_json::json!({
            "title": "测试站点",
            "description": "站点描述",
            "logo_media_id": null,
            "expected_version": 1,
        })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["logo_media_id"].is_null());
    let (status, _, _) = send(
        &stack.router,
        "GET",
        &format!("/media/{logo}"),
        None,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "移除 logo 后不再是公开来源");
    let (status, _, _) = send(
        &stack.router,
        "DELETE",
        &format!("/api/admin/v1/media/{logo}"),
        Some(&editor),
        Some(&csrf),
        Some(&serde_json::json!({"expected_version": logo_version})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "移除 logo 后应可删除");
}
