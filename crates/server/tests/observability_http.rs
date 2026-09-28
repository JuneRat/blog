use axum::{
    Extension, Router,
    body::Body,
    http::{Method, Request, StatusCode},
    middleware,
    routing::get,
};
use interfaces::{
    http_support::request_context,
    observability::{BuildInfo, Telemetry},
};
use std::time::Duration;
use tower::ServiceExt;

fn telemetry() -> Telemetry {
    Telemetry::new(&BuildInfo {
        version: "test",
        revision: "abcdef1",
    })
}

#[tokio::test]
async fn metrics_use_templates_bounded_methods_and_count_errors_without_secrets() {
    let metrics = telemetry();
    let app = Router::new()
        .route(
            "/items/{id}",
            get(|| async { StatusCode::INTERNAL_SERVER_ERROR }),
        )
        .fallback(|| async { StatusCode::NOT_FOUND })
        .layer(middleware::from_fn(request_context))
        .layer(Extension(metrics.clone()));
    for uri in [
        "/items/private-one?secret=query-token",
        "/items/private-two",
        "/unknown/private-three",
    ] {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert!(response.headers().contains_key("x-request-id"));
    }
    for method in ["ARBITRARY_ONE", "ARBITRARY_TWO"] {
        app.clone()
            .oneshot(
                Request::builder()
                    .method(Method::from_bytes(method.as_bytes()).unwrap())
                    .uri("/items/id")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
    }
    let output = metrics.encode().unwrap();
    assert!(
        output
            .lines()
            .any(|line| line.starts_with("blog_http_requests_total{")
                && line.contains("route=\"/items/{id}\"")
                && line.contains("status=\"500\"")
                && line.ends_with(" 2")),
        "{output}"
    );
    assert!(output.contains("route=\"unmatched\""));
    assert!(output.contains("method=\"OTHER\""));
    assert!(output.contains("blog_http_request_duration_seconds_bucket"));
    assert!(output.contains("blog_http_requests_in_flight 0"));
    for secret in [
        "private-one",
        "private-two",
        "private-three",
        "query-token",
        "ARBITRARY_ONE",
        "request_id",
    ] {
        assert!(
            !output.contains(secret),
            "metrics must not include {secret}"
        );
    }
}

#[tokio::test]
async fn cancelling_a_handler_releases_inflight_and_records_cancellation() {
    let metrics = telemetry();
    let app = Router::new()
        .route(
            "/pending",
            get(|| async { std::future::pending::<StatusCode>().await }),
        )
        .layer(middleware::from_fn(request_context))
        .layer(Extension(metrics.clone()));
    let response = tokio::time::timeout(
        Duration::from_millis(20),
        app.oneshot(
            Request::builder()
                .uri("/pending")
                .body(Body::empty())
                .unwrap(),
        ),
    )
    .await;
    assert!(response.is_err());
    let output = metrics.encode().unwrap();
    assert!(output.contains("blog_http_requests_in_flight 0"));
    assert!(output.lines().any(
        |line| line.starts_with("blog_http_requests_cancelled_total{") && line.ends_with(" 1")
    ));
    assert!(!output.contains("blog_http_requests_total{"));
}

#[tokio::test]
async fn render_metrics_share_runtime_counts_and_exclude_draft_text() {
    use application::ports::ContentRenderer;
    let metrics = telemetry();
    let runtime = infrastructure::RenderingRuntime::default()
        .with_observer(std::sync::Arc::new(metrics.clone()));
    runtime.render_content("secret draft body").await.unwrap();
    // Cache hits do not represent a worker execution.
    runtime.render_content("secret draft body").await.unwrap();
    metrics.publication_run(Duration::from_millis(5), true);
    metrics.publication_run(Duration::from_millis(5), false);
    let output = metrics.encode().unwrap();
    assert!(output.contains("blog_render_waiting{kind=\"content\"} 0"));
    assert!(output.contains("blog_render_active{kind=\"content\"} 0"));
    assert!(output.contains("blog_render_completed_total{kind=\"content\",result=\"success\"} 1"));
    assert!(output.contains("blog_render_queue_total{kind=\"content\",result=\"admitted\"} 1"));
    assert!(output.contains("blog_scheduled_publication_runs_total{result=\"success\"} 1"));
    assert!(output.contains("blog_scheduled_publication_runs_total{result=\"error\"} 1"));
    assert!(!output.contains("secret draft body"));
    assert!(!output.contains("blog_scheduled_publication_last_success_timestamp_seconds 0"));
}
