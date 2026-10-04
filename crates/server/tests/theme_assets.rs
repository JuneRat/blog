//! Release URLs must serve exactly the bytes loaded with their templates.
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use infrastructure::MiniJinjaThemeRenderer;
use interfaces::http::mount_theme_assets;
use tower::ServiceExt;

#[tokio::test]
async fn release_assets_survive_disk_replacement_and_reject_other_versions() {
    let dir = std::env::temp_dir().join(format!("blog-assets-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(dir.join("templates")).unwrap();
    std::fs::create_dir_all(dir.join("assets/nested")).unwrap();
    std::fs::copy("../../themes/default/theme.json", dir.join("theme.json")).unwrap();
    std::fs::copy(
        "../../themes/default/settings.schema.json",
        dir.join("settings.schema.json"),
    )
    .unwrap();
    for entry in ["index", "post", "page", "tag", "category", "series"] {
        std::fs::write(dir.join(format!("templates/{entry}.html")), "old template").unwrap();
    }
    let asset_path = dir.join("assets/nested/样式.css");
    std::fs::write(&asset_path, "old css").unwrap();
    let old = MiniJinjaThemeRenderer::load(&dir).unwrap().assets();
    let old_url = old.url("nested/样式.css");
    let old_router = mount_theme_assets(Router::new(), vec![old]);
    std::fs::write(&asset_path, "new css").unwrap();
    std::fs::write(dir.join("templates/index.html"), "new template").unwrap();
    let new = MiniJinjaThemeRenderer::load(&dir).unwrap().assets();
    let new_url = new.url("nested/样式.css");
    assert_ne!(old_url, new_url);
    let new_router = mount_theme_assets(Router::new(), vec![new]);
    std::fs::remove_dir_all(dir).unwrap();

    for (router, url, expected) in [
        (&old_router, &old_url, "old css"),
        (&new_router, &new_url, "new css"),
    ] {
        let response = router
            .clone()
            .oneshot(Request::builder().uri(url).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "text/css");
        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            "public, max-age=31536000, immutable"
        );
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            expected
        );
        let head = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("HEAD")
                    .uri(url)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(head.status(), StatusCode::OK);
        assert!(
            head.into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .is_empty()
        );
    }
    for url in [
        old_url.clone(),
        new_url.replace("%E6%A0%B7%E5%BC%8F.css", "missing.css"),
        new_url.replace("nested/", "../"),
    ] {
        let response = new_router
            .clone()
            .oneshot(Request::builder().uri(url).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(!response.headers().contains_key(header::CACHE_CONTROL));
    }
}
