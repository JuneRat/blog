//! HTTP cancellation must leave PostgreSQL work bounded by server-side limits.
mod common;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
    middleware,
    routing::get,
};
use infrastructure::DatabasePoolConfig;
use interfaces::http_support::request_context;
use std::time::Duration;
use tower::ServiceExt;

async fn cancelled_request(
    pool: sqlx::PgPool,
    observer: &sqlx::PgPool,
    statement: &'static str,
    wait_event: &str,
    in_transaction: bool,
) {
    let backend: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&pool)
        .await
        .unwrap();
    let app = Router::new()
        .route(
            "/work",
            get(move || {
                let pool = pool.clone();
                async move {
                    let result = if in_transaction {
                        let mut transaction = pool.begin().await.unwrap();
                        sqlx::query("UPDATE deadline_lock SET id=2")
                            .execute(&mut *transaction)
                            .await
                            .unwrap();
                        let result = sqlx::query(statement).execute(&mut *transaction).await;
                        transaction.commit().await.unwrap();
                        result
                    } else {
                        sqlx::query(statement).execute(&pool).await
                    };
                    match result {
                        Ok(_) => StatusCode::OK,
                        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
                    }
                }
            }),
        )
        .layer(middleware::from_fn(request_context));
    let pending =
        tokio::spawn(app.oneshot(Request::builder().uri("/work").body(Body::empty()).unwrap()));
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            assert!(
                !pending.is_finished(),
                "request finished before entering {wait_event}"
            );
            let started: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_stat_activity \
                 WHERE pid=$1 AND state='active' AND query=$2 AND wait_event=$3)",
            )
            .bind(backend)
            .bind(statement)
            .bind(wait_event)
            .fetch_one(observer)
            .await
            .unwrap();
            if started {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("request never reached the expected PostgreSQL wait");

    // A disconnected client drops the request future while SQL is running.
    // PostgreSQL's real deadlines must still bound the abandoned query.
    pending.abort();
    let cancelled = tokio::time::timeout(Duration::from_secs(5), pending)
        .await
        .expect("request cancellation did not finish")
        .expect_err("aborted request must not complete normally");
    assert!(cancelled.is_cancelled(), "request task must be cancelled");
}

async fn cancellation_fixture(
    name: &str,
    admin_url: &str,
) -> (infrastructure::Database, sqlx::PgPool) {
    let admin = common::fresh_database_with_url(name, admin_url).await;
    sqlx::query("CREATE TABLE deadline_lock(id integer)")
        .execute(&admin)
        .await
        .unwrap();
    let database = infrastructure::connect_with_config(
        &common::test_db_url(admin_url, name),
        &DatabasePoolConfig {
            max_connections: 1,
            min_connections: 1,
            statement_timeout_ms: 1_000,
            lock_timeout_ms: 500,
            connect_retries: 0,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    (database, admin)
}

async fn assert_cancelled_queries_recover(database: infrastructure::Database, admin: sqlx::PgPool) {
    let pool = infrastructure::test_support::pool(database.clone());
    cancelled_request(
        pool.clone(),
        &admin,
        "SELECT pg_sleep(10)",
        "PgSleep",
        false,
    )
    .await;
    let (one,): (i32,) = tokio::time::timeout(
        Duration::from_secs(3),
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
    cancelled_request(
        pool.clone(),
        &admin,
        "SELECT * FROM deadline_lock",
        "relation",
        false,
    )
    .await;
    let (one,): (i32,) = tokio::time::timeout(
        Duration::from_secs(3),
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

fn tls_fixture_url() -> String {
    let url = std::env::var("BLOG_TEST_TLS_URL")
        .expect("TLS cancellation test requires BLOG_TEST_TLS_URL");
    let parsed = url::Url::parse(&url).expect("valid TLS fixture URL");
    assert!(
        parsed
            .query_pairs()
            .any(|(key, value)| key == "sslmode" && value == "verify-full"),
        "TLS cancellation fixture must verify the server identity"
    );
    url
}

async fn assert_encrypted(database: &infrastructure::Database) {
    let pool = infrastructure::test_support::pool(database.clone());
    let encrypted: bool =
        sqlx::query_scalar("SELECT ssl FROM pg_stat_ssl WHERE pid=pg_backend_pid()")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        encrypted,
        "cancellation regression must exercise a TLS connection"
    );
}

#[tokio::test]
async fn cancelled_sql_and_lock_wait_release_the_only_pool_connection() {
    let (database, admin) =
        cancellation_fixture("blog_request_deadlines_test", &common::admin_url()).await;
    assert_cancelled_queries_recover(database, admin).await;
}

#[tokio::test]
#[ignore = "requires a TLS PostgreSQL fixture and BLOG_TEST_TLS_URL; CI runs it explicitly"]
async fn cancelled_tls_sql_and_lock_wait_release_the_only_pool_connection() {
    let (database, admin) =
        cancellation_fixture("blog_tls_request_deadlines_test", &tls_fixture_url()).await;
    assert_encrypted(&database).await;
    assert_cancelled_queries_recover(database, admin).await;
}

#[tokio::test]
#[ignore = "requires a TLS PostgreSQL fixture and BLOG_TEST_TLS_URL; CI runs it explicitly"]
async fn cancelled_tls_transaction_rolls_back_before_connection_reuse() {
    let (database, admin) =
        cancellation_fixture("blog_tls_cancelled_transaction_test", &tls_fixture_url()).await;
    assert_encrypted(&database).await;
    sqlx::query("INSERT INTO deadline_lock VALUES(1)")
        .execute(&admin)
        .await
        .unwrap();
    let pool = infrastructure::test_support::pool(database.clone());
    cancelled_request(pool.clone(), &admin, "SELECT pg_sleep(10)", "PgSleep", true).await;
    let id: i32 = tokio::time::timeout(
        Duration::from_secs(3),
        sqlx::query_scalar("SELECT id FROM deadline_lock").fetch_one(&pool),
    )
    .await
    .expect("cancelled transaction retained the pool connection")
    .expect("cancelled transaction left the connection in an aborted transaction");
    assert_eq!(id, 1, "cancelled transaction must not commit its update");
    database.close().await;
    admin.close().await;
}
