//! HTTP cancellation must leave PostgreSQL work bounded by server-side limits.
mod common;

use axum::{
    Extension, Router,
    body::Body,
    http::{Request, StatusCode},
    middleware,
    routing::get,
};
use infrastructure::DatabasePoolConfig;
use interfaces::{http_limits::RequestTimeouts, http_support::request_context};
use std::time::Duration;
use tower::ServiceExt;

async fn cancelled_request(pool: sqlx::PgPool, statement: &'static str) {
    let app = Router::new()
        .route(
            "/work",
            get(move || {
                let pool = pool.clone();
                async move {
                    match sqlx::query(statement).execute(&pool).await {
                        Ok(_) => StatusCode::OK,
                        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
                    }
                }
            }),
        )
        .layer(middleware::from_fn(request_context))
        .layer(Extension(RequestTimeouts {
            request: Duration::from_millis(20),
            upload: Duration::from_millis(20),
        }));
    let response = app
        .oneshot(Request::builder().uri("/work").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
}

#[tokio::test]
async fn cancelled_sql_and_lock_wait_release_the_only_pool_connection() {
    let name = "blog_request_deadlines_test";
    let admin = common::fresh_database(name).await;
    sqlx::query("CREATE TABLE deadline_lock(id integer)")
        .execute(&admin)
        .await
        .unwrap();
    let database = infrastructure::connect_with_config(
        &common::test_db_url(&common::admin_url(), name),
        &DatabasePoolConfig {
            max_connections: 1,
            min_connections: 1,
            statement_timeout_ms: 200,
            lock_timeout_ms: 80,
            connect_retries: 0,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let pool = infrastructure::test_support::pool(database.clone());
    cancelled_request(pool.clone(), "SELECT pg_sleep(10)").await;
    let (one,): (i32,) = tokio::time::timeout(
        Duration::from_secs(1),
        sqlx::query_as("SELECT 1").fetch_one(&pool),
    )
    .await
    .expect("cancelled SQL retained the pool connection")
    .unwrap();
    assert_eq!(one, 1);

    let mut blocker = admin.begin().await.unwrap();
    sqlx::query("LOCK TABLE deadline_lock IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await
        .unwrap();
    cancelled_request(pool.clone(), "SELECT * FROM deadline_lock").await;
    let (one,): (i32,) = tokio::time::timeout(
        Duration::from_secs(1),
        sqlx::query_as("SELECT 1").fetch_one(&pool),
    )
    .await
    .expect("cancelled lock wait retained the pool connection")
    .unwrap();
    assert_eq!(one, 1);
    blocker.rollback().await.unwrap();
    database.close().await;
    admin.close().await;
}
