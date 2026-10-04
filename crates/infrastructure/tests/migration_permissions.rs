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

#[tokio::test]
async fn search_migration_needs_no_superuser_or_runtime_role_create_privilege() {
    use application::content_queries::ContentListRequest;
    use application::ports::AdminPostQuery;

    let database = "blog_search_migration_permissions_test";
    let pool = common::fresh_database(database).await;
    let owner = format!("search_owner_{}", uuid::Uuid::now_v7().simple());
    let app = format!("search_app_{}", uuid::Uuid::now_v7().simple());
    let author = common::seed_user(&pool, "search-author").await;
    sqlx::query(
        "INSERT INTO posts(id,author_id,slug,content,content_html,content_render_version) \
        VALUES(gen_random_uuid(),$1,'permission-target','中文子串 ExampleNeedle','',1)",
    )
    .bind(author)
    .execute(&pool)
    .await
    .unwrap();
    // Replay just the new migration with a non-superuser schema owner. Runtime
    // readers retain SELECT + schema USAGE, without database/schema CREATE.
    sqlx::raw_sql(&format!(
        "DROP INDEX posts_admin_active_idx,posts_admin_trash_idx,posts_admin_author_active_idx, \
         posts_admin_author_trash_idx,pages_admin_active_idx,pages_admin_trash_idx; \
         DROP EXTENSION pg_trgm CASCADE; \
         CREATE ROLE {owner}; CREATE ROLE {app}; \
         GRANT CREATE ON DATABASE {database} TO {owner}; \
         GRANT USAGE,CREATE ON SCHEMA public TO {owner}; \
         ALTER TABLE posts OWNER TO {owner}; ALTER TABLE pages OWNER TO {owner}; \
         GRANT USAGE ON SCHEMA public TO {app}; GRANT SELECT ON posts,pages,users TO {app};"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let selected_owner = owner.clone();
    let owner_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .after_connect(move |connection, _| {
            let role = selected_owner.clone();
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
    sqlx::raw_sql(include_str!(
        "../../../migrations/postgres/0005_content_admin_search_indexes.sql"
    ))
    .execute(&owner_pool)
    .await
    .unwrap();
    let trusted_owner: bool = sqlx::query_scalar(
        "SELECT NOT r.rolsuper AND NOT r.rolcreaterole AND NOT r.rolcreatedb \
         FROM pg_extension e JOIN pg_roles r ON r.oid=e.extowner WHERE e.extname='pg_trgm'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(trusted_owner);
    let runtime_create: bool = sqlx::query_scalar(
        "SELECT has_database_privilege($1,current_database(),'CREATE') OR has_schema_privilege($1,'public','CREATE')"
    ).bind(&app).fetch_one(&pool).await.unwrap();
    assert!(!runtime_create);
    let selected_app = app.clone();
    let app_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .after_connect(move |connection, _| {
            let role = selected_app.clone();
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
    let query = infrastructure::PostgresAdminContentQuery::new(common::database(app_pool.clone()));
    for q in ["中文子串", "exampleneedle"] {
        let request = ContentListRequest {
            q: Some(q.into()),
            ..Default::default()
        };
        let (items, total) = query
            .list(None, &request.try_into().unwrap())
            .await
            .unwrap();
        assert_eq!(total, 1);
        assert_eq!(items[0].slug, "permission-target");
    }
    sqlx::raw_sql("DROP EXTENSION pg_trgm CASCADE; CREATE SCHEMA other_trgm; CREATE EXTENSION pg_trgm WITH SCHEMA other_trgm")
        .execute(&pool).await.unwrap();
    let error = sqlx::raw_sql(include_str!(
        "../../../migrations/postgres/0005_content_admin_search_indexes.sql"
    ))
    .execute(&owner_pool)
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("pg_trgm must be installed in public")
    );
    sqlx::raw_sql("DROP EXTENSION pg_trgm CASCADE; DROP SCHEMA other_trgm")
        .execute(&pool)
        .await
        .unwrap();
    owner_pool.close().await;
    app_pool.close().await;
    sqlx::raw_sql(&format!(
        "ALTER TABLE posts OWNER TO CURRENT_USER; ALTER TABLE pages OWNER TO CURRENT_USER; \
         DROP OWNED BY {app}; DROP ROLE {app}; DROP OWNED BY {owner} CASCADE; DROP ROLE {owner};"
    ))
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;
}
