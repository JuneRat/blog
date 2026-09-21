//! 后台 SPA 静态路由：/admin 子树 fallback、缓存头、不遮挡其他路由、dist 缺失时不注册。
//! 不依赖数据库：用临时目录构造 dist。

use std::path::PathBuf;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::routing::get;
use http_body_util::BodyExt;
use interfaces::http::mount_admin_spa;
use tower::ServiceExt;

const INDEX_MARKER: &str = "<div id=\"root\">INDEXMARKER</div>";
const ASSET_MARKER: &str = "console.log(\"ASSETMARKER\");";

/// 构造临时 dist 目录（index.html + assets/app-abc123.js）。
fn fixture_dist() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("blog-admin-spa-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(dir.join("assets")).unwrap();
    std::fs::write(
        dir.join("index.html"),
        format!("<!doctype html>{INDEX_MARKER}"),
    )
    .unwrap();
    std::fs::write(dir.join("assets/app-abc123.js"), ASSET_MARKER).unwrap();
    dir
}

async fn fetch(router: &Router, uri: &str) -> (StatusCode, Option<String>, String) {
    let response = router
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let cache = response
        .headers()
        .get(header::CACHE_CONTROL)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let body = String::from_utf8_lossy(&response.into_body().collect().await.unwrap().to_bytes())
        .to_string();
    (status, cache, body)
}

fn app_with(dist: Option<PathBuf>) -> Router {
    let base = Router::new()
        .route("/posts/{slug}", get(|| async { "POSTROUTE" }))
        .route("/healthz", get(|| async { "ok" }));
    mount_admin_spa(base, dist)
}

#[tokio::test]
async fn admin_spa_serves_index_deep_links_and_immutable_assets() {
    let dist = fixture_dist();
    let app = app_with(Some(dist.clone()));

    // /admin 与 /admin/ 都返回 index.html，且 no-cache。
    for uri in ["/admin", "/admin/"] {
        let (status, cache, body) = fetch(&app, uri).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        assert!(body.contains(INDEX_MARKER), "{uri}: {body}");
        assert_eq!(cache.as_deref(), Some("no-cache"), "{uri}");
    }

    // SPA 深链回退到 index.html（后台内部路由）。
    let (status, cache, body) = fetch(&app, "/admin/posts/some-slug").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains(INDEX_MARKER));
    assert_eq!(cache.as_deref(), Some("no-cache"));

    // 带指纹的静态资源长缓存 immutable。
    let (status, cache, body) = fetch(&app, "/admin/assets/app-abc123.js").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains(ASSET_MARKER));
    assert_eq!(
        cache.as_deref(),
        Some("public, max-age=31536000, immutable")
    );

    // 缺失资源是 404：绝不能带 immutable，否则一次拼写错误会被缓存一年。
    let (status, cache, _) = fetch(&app, "/admin/assets/missing.js").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_ne!(
        cache.as_deref(),
        Some("public, max-age=31536000, immutable"),
        "404 资源不得 immutable"
    );
    assert_eq!(cache.as_deref(), Some("no-cache"));

    // 不遮挡其他已注册路由。
    let (status, _, body) = fetch(&app, "/posts/hello").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "POSTROUTE");
    let (status, _, body) = fetch(&app, "/healthz").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "ok");

    std::fs::remove_dir_all(&dist).ok();
}

#[tokio::test]
async fn admin_route_is_absent_when_dist_missing() {
    // None 与不存在的目录都不注册 /admin。
    let missing = std::env::temp_dir().join("blog-admin-spa-does-not-exist");
    let app = app_with(Some(missing));
    let (status, _, _) = fetch(&app, "/admin").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = fetch(&app, "/admin/posts/x").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let app = app_with(None);
    let (status, _, _) = fetch(&app, "/admin").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
