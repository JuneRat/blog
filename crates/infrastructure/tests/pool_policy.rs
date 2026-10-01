mod common;

use common::connect_with_config;
use infrastructure::DatabasePoolConfig;
use std::time::{Duration, Instant};

#[tokio::test]
async fn pool_capacity_and_server_limits_apply_to_all_connections() {
    let name = "blog_pool_policy_test";
    let admin = common::fresh_database(name).await;
    let url = common::test_db_url(&common::admin_url(), name);
    let config = DatabasePoolConfig {
        max_connections: 2,
        min_connections: 2,
        acquire_timeout_ms: 150,
        statement_timeout_ms: 200,
        lock_timeout_ms: 75,
        idle_in_transaction_timeout_ms: 300,
        connect_retries: 0,
        ..Default::default()
    };
    let pool = connect_with_config(&url, &config).await.unwrap();
    let mut first = pool.acquire().await.unwrap();
    let mut second = pool.acquire().await.unwrap();
    for connection in [&mut first, &mut second] {
        let settings: (String, String, String) = sqlx::query_as("SELECT current_setting('statement_timeout'), current_setting('lock_timeout'), current_setting('idle_in_transaction_session_timeout')")
            .fetch_one(&mut **connection).await.unwrap();
        assert_eq!(settings, ("200ms".into(), "75ms".into(), "300ms".into()));
    }
    let start = Instant::now();
    assert!(matches!(
        pool.acquire().await,
        Err(sqlx::Error::PoolTimedOut)
    ));
    assert!(start.elapsed() >= Duration::from_millis(100));
    assert_eq!(pool.size(), 2);
    // Statement timeout cancels the server query and the pool remains usable.
    let error = sqlx::query("SELECT pg_sleep(10)")
        .execute(&mut *first)
        .await
        .unwrap_err();
    assert_eq!(
        error.as_database_error().unwrap().code().as_deref(),
        Some("57014")
    );
    assert_eq!(
        sqlx::query_scalar::<_, i32>("SELECT 1")
            .fetch_one(&mut *first)
            .await
            .unwrap(),
        1
    );
    sqlx::query("CREATE TABLE pool_lock (id int)")
        .execute(&admin)
        .await
        .unwrap();
    let mut owner = admin.begin().await.unwrap();
    sqlx::query("LOCK TABLE pool_lock IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *owner)
        .await
        .unwrap();
    let error = sqlx::query("SELECT * FROM pool_lock")
        .fetch_all(&mut *second)
        .await
        .unwrap_err();
    assert_eq!(
        error.as_database_error().unwrap().code().as_deref(),
        Some("55P03")
    );
    owner.rollback().await.unwrap();
    sqlx::query("BEGIN").execute(&mut *second).await.unwrap();
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(sqlx::query("SELECT 1").execute(&mut *second).await.is_err());
    drop(first);
    drop(second);
    assert_eq!(
        sqlx::query_scalar::<_, i32>("SELECT 1")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    pool.close().await;
    // Unset application limits must preserve DSN/role policy.
    let inherited = connect_with_config(
        &format!("{url}?options[statement_timeout]=750"),
        &DatabasePoolConfig::default(),
    )
    .await
    .unwrap();
    let setting: String = sqlx::query_scalar("SHOW statement_timeout")
        .fetch_one(&inherited)
        .await
        .unwrap();
    assert_eq!(setting, "750ms");
    inherited.close().await;
    admin.close().await;
}

#[tokio::test]
#[ignore = "requires a TLS PostgreSQL fixture and BLOG_TEST_TLS_URL; CI runs it explicitly"]
async fn database_tls_verifies_the_server_when_requested() {
    let url = std::env::var("BLOG_TEST_TLS_URL")
        .expect("TLS test requires BLOG_TEST_TLS_URL with sslmode=verify-full and a private CA");
    let mut untrusted = url::Url::parse(&url).expect("valid TLS fixture URL");
    let options: Vec<(String, String)> = untrusted.query_pairs().into_owned().collect();
    assert!(
        options
            .iter()
            .any(|(key, value)| key == "sslmode" && value == "verify-full"),
        "TLS fixture must verify the server identity"
    );
    assert!(
        options.iter().any(|(key, _)| key == "sslrootcert"),
        "TLS fixture must use a private CA"
    );
    untrusted
        .query_pairs_mut()
        .clear()
        .extend_pairs(options.iter().filter(|(key, _)| key != "sslrootcert"));
    let policy = DatabasePoolConfig {
        connect_retries: 0,
        ..Default::default()
    };
    let pool = connect_with_config(&url, &policy).await.unwrap();
    let encrypted: bool =
        sqlx::query_scalar("SELECT ssl FROM pg_stat_ssl WHERE pid=pg_backend_pid()")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(encrypted);
    pool.close().await;
    // Dropping the private CA must fail certificate validation, never fall back.
    let error = connect_with_config(untrusted.as_str(), &policy)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("invalid peer certificate"),
        "expected certificate validation failure: {error}"
    );
}
