mod common;

use application::ports::HealthCheck;
use infrastructure::{DatabasePoolConfig, PgHealthCheck, connect_with_config};

#[tokio::test]
async fn site_settings_read_observer_distinguishes_missing_failure_and_recovery() {
    use application::ports::{SettingsReadObserver, SettingsStore, SiteSettingsReadOutcome};
    use std::sync::{Arc, Mutex};
    #[derive(Default)]
    struct Observer(Mutex<Vec<(SiteSettingsReadOutcome, bool)>>);
    impl SettingsReadObserver for Observer {
        fn observe_site_read(&self, outcome: SiteSettingsReadOutcome, recovered: bool) {
            self.0.lock().unwrap().push((outcome, recovered));
        }
    }
    let pool = common::fresh_database("blog_settings_read_health_test").await;
    let observer = Arc::new(Observer::default());
    let store = infrastructure::PostgresSettingsStore::new(common::database(pool.clone()))
        .with_read_observer(observer.clone());
    assert!(store.find_site().await.unwrap().is_none());
    sqlx::query("INSERT INTO settings(key,value) VALUES('site','{}')")
        .execute(&pool)
        .await
        .unwrap();
    assert!(store.find_site().await.unwrap().is_some());
    sqlx::query("UPDATE settings SET value='{\"title\":42}' WHERE key='site'")
        .execute(&pool)
        .await
        .unwrap();
    assert!(store.find_site().await.is_err());
    assert!(store.find_site().await.is_err());
    sqlx::query("UPDATE settings SET value='{}' WHERE key='site'")
        .execute(&pool)
        .await
        .unwrap();
    assert!(store.find_site().await.unwrap().is_some());
    assert!(store.find_site().await.unwrap().is_some());
    assert_eq!(
        *observer.0.lock().unwrap(),
        vec![
            (SiteSettingsReadOutcome::Missing, false),
            (SiteSettingsReadOutcome::Configured, false),
            (SiteSettingsReadOutcome::Failed, false),
            (SiteSettingsReadOutcome::Failed, false),
            (SiteSettingsReadOutcome::Configured, true),
            (SiteSettingsReadOutcome::Configured, false),
        ]
    );
    pool.close().await;
}

#[tokio::test]
async fn recovery_marker_and_pool_lifecycle_are_preserved_by_database_handle() {
    let name = "blog_database_handle_test";
    let admin = common::fresh_database(name).await;
    let database = connect_with_config(
        &common::test_db_url(&common::admin_url(), name),
        &DatabasePoolConfig {
            max_connections: 1,
            min_connections: 1,
            connect_retries: 0,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let clone = database.clone();
    let health = PgHealthCheck::new(clone.clone());
    assert!(!database.is_recovery_isolated().await.unwrap());
    for (comment, isolated) in [
        ("unrelated operator note", false),
        ("blog:recovery-isolated", false),
        ("blog:recovery-isolated:backup-123", true),
    ] {
        sqlx::raw_sql(&format!("COMMENT ON DATABASE {name} IS '{comment}'"))
            .execute(&admin)
            .await
            .unwrap();
        assert_eq!(database.is_recovery_isolated().await.unwrap(), isolated);
    }
    sqlx::raw_sql(&format!("COMMENT ON DATABASE {name} IS NULL"))
        .execute(&admin)
        .await
        .unwrap();
    assert!(!database.is_recovery_isolated().await.unwrap());

    // Scraping a fully occupied pool must not try to acquire a connection.
    let raw = infrastructure::test_support::pool(clone.clone());
    let held = raw.acquire().await.unwrap();
    let snapshot = database.pool_snapshot();
    assert_eq!(snapshot.connections, 1);
    assert_eq!(snapshot.idle_connections, 0);
    assert_eq!(snapshot.max_connections, 1);
    drop(held);
    assert!(health.check().await);

    database.close().await;
    assert!(clone.is_recovery_isolated().await.is_err());
    assert!(!health.check().await);
    assert_eq!(clone.pool_snapshot().connections, 0);
    admin.close().await;
}
