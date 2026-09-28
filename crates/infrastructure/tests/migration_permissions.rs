mod common;

use sqlx::migrate::Migrate;

#[tokio::test]
async fn read_only_verification_waits_for_schema_owner_and_releases_lock_on_mismatch() {
    let database = "blog_migration_permissions_test";
    let pool = common::fresh_database(database).await;
    let role = format!("verify_{}", uuid::Uuid::now_v7().simple());
    sqlx::raw_sql(&format!("CREATE ROLE {role}; GRANT USAGE ON SCHEMA public TO {role}; GRANT SELECT ON users,_sqlx_migrations TO {role};"))
        .execute(&pool).await.unwrap();
    let selected_role = role.clone();
    let restricted = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .after_connect(move |connection, _| {
            let role = selected_role.clone();
            Box::pin(async move {
                sqlx::query(&format!("SET ROLE {role}"))
                    .execute(connection)
                    .await?;
                Ok(())
            })
        })
        .connect(&common::test_db_url(&common::admin_url(), database))
        .await
        .unwrap();
    let mut owner = pool.acquire().await.unwrap();
    owner.lock().await.unwrap();
    let worker_pool = restricted.clone();
    let mut job = tokio::spawn(async move {
        infrastructure::verify_schema(
            &common::database(worker_pool.clone()),
            "../../migrations/postgres",
        )
        .await
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(150), &mut job)
            .await
            .is_err()
    );
    let checksum: Vec<u8> =
        sqlx::query_scalar("SELECT checksum FROM _sqlx_migrations WHERE version=1")
            .fetch_one(&mut *owner)
            .await
            .unwrap();
    sqlx::query("UPDATE _sqlx_migrations SET checksum=decode('00','hex') WHERE version=1")
        .execute(&mut *owner)
        .await
        .unwrap();
    owner.unlock().await.unwrap();
    assert!(
        job.await
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("校验和不匹配")
    );
    // An error must close the locked session, otherwise the next startup hangs.
    sqlx::query("UPDATE _sqlx_migrations SET checksum=$1 WHERE version=1")
        .bind(checksum)
        .execute(&pool)
        .await
        .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        infrastructure::migrate_schema(
            &common::database(restricted.clone()),
            "../../migrations/postgres",
        ),
    )
    .await
    .unwrap()
    .unwrap();
    restricted.close().await;
    sqlx::raw_sql(&format!("DROP OWNED BY {role}; DROP ROLE {role};"))
        .execute(&pool)
        .await
        .unwrap();
}
