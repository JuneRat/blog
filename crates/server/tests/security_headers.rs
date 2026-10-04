//! 与主路由相同的外层中间件：成功、拒绝、404/405、后台深链和可信 HTTPS 配置。
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
    middleware,
    routing::get,
};
use tower::ServiceExt;

fn router(https: bool) -> Router {
    Router::new()
        .route("/", get(|| async { "public" }))
        .route("/admin/{*path}", get(|| async { "admin" }))
        .route(
            "/api/admin/v1/me",
            get(|| async { StatusCode::UNAUTHORIZED }),
        )
        .layer(middleware::from_fn_with_state(
            https,
            interfaces::http_support::security_headers,
        ))
}

#[tokio::test]
async fn security_headers_cover_errors_and_admin_deep_links() {
    for (method, path, status) in [
        ("GET", "/", StatusCode::OK),
        ("GET", "/admin/posts/42", StatusCode::OK),
        ("GET", "/api/admin/v1/me", StatusCode::UNAUTHORIZED),
        ("GET", "/media/missing", StatusCode::NOT_FOUND),
        ("POST", "/", StatusCode::METHOD_NOT_ALLOWED),
    ] {
        let response = router(true)
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), status);
        let headers = response.headers();
        assert_eq!(headers["x-content-type-options"], "nosniff");
        assert_eq!(headers["x-frame-options"], "DENY");
        assert_eq!(headers["referrer-policy"], "same-origin");
        assert_eq!(headers["strict-transport-security"], "max-age=31536000");
        let csp = headers["content-security-policy"].to_str().unwrap();
        assert!(csp.contains("frame-ancestors 'none'"));
        assert_eq!(
            csp.contains("script-src 'self'"),
            path.starts_with("/admin/")
        );
        if path.starts_with("/admin/") {
            assert!(csp.contains("style-src 'self' 'unsafe-inline'"));
        }
    }
}

#[tokio::test]
async fn forwarded_proto_cannot_enable_hsts_on_http_development() {
    let response = router(false)
        .oneshot(
            Request::builder()
                .uri("/")
                .header("x-forwarded-proto", "https")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(!response.headers().contains_key("strict-transport-security"));
}
