use application::plugins::PluginAssets;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
use interfaces::http_plugins::mount_plugin_assets;
use std::{collections::BTreeMap, sync::Arc};
use tower::ServiceExt;

#[tokio::test]
async fn plugin_files_use_versioned_urls_correct_mime_and_cannot_escape_snapshot() {
    let assets = PluginAssets {
        id: "fixture".into(),
        version: "v1".into(),
        files: Arc::new(BTreeMap::from([
            (
                "browser/main.js".into(),
                Arc::from(b"/* script */".as_slice()),
            ),
            ("style.css".into(), Arc::from(b".fixture {}".as_slice())),
        ])),
    };
    let router = mount_plugin_assets(Router::new(), vec![assets.clone()]);
    for (path, mime, body) in [
        ("browser/main.js", "text/javascript", "/* script */"),
        ("style.css", "text/css", ".fixture {}"),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(assets.url(path).unwrap())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_TYPE], mime);
        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            "public, max-age=31536000, immutable"
        );
        assert_eq!(
            response.headers()[header::X_CONTENT_TYPE_OPTIONS],
            "nosniff"
        );
        assert_eq!(response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            body
        );
    }
    for path in [
        "/assets/plugins/fixture/v2/style.css",
        "/assets/plugins/fixture/v1/../style.css",
        "/assets/plugins/fixture/v1/%2e%2e/style.css",
        "/assets/plugins/fixture/v1/missing.js",
    ] {
        let response = router
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(!response.headers().contains_key(header::CACHE_CONTROL));
    }
}
