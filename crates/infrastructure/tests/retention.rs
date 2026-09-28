mod common;

use application::{
    error::UseCaseError,
    retention::{RetentionSettings, RetentionStore},
};
use infrastructure::retention::{PostgresRetentionCleanupStore, PostgresRetentionStore};

fn maintenance(pool: &PgPool) -> application::retention::RetentionMaintenance {
    application::retention::RetentionMaintenance::new(std::sync::Arc::new(
        PostgresRetentionCleanupStore::new(pool.clone()),
    ))
}
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

async fn seed(pool: &PgPool) {
    let user = common::seed_user(pool, "retention-owner").await;
    let post = Uuid::now_v7();
    sqlx::query("INSERT INTO posts(id,author_id,slug,content_html,content_render_version) VALUES($1,$2,'retention','',1)")
        .bind(post).bind(user).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO comments(id,post_id,author_name,author_email,content,content_html,content_render_version,ip_address,status,created_at) SELECT gen_random_uuid(),$1,'Name','private@example.com','Body','<p>Body</p>',1,'192.0.2.1','trash',now()-make_interval(days=>n) FROM unnest(ARRAY[181,200,400,1]) n")
        .bind(post).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO audit_logs(id,action,target_type,target_id,created_at) SELECT gen_random_uuid(),'fixture','system','retention',now()-make_interval(days=>n) FROM unnest(ARRAY[181,200,1]) n")
        .execute(pool).await.unwrap();
}

async fn comments_without_ip(pool: &PgPool) -> Value {
    sqlx::query_scalar("SELECT jsonb_agg(to_jsonb(c)-'ip_address' ORDER BY id) FROM comments c")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn policy_defaults_merges_and_conflicts_are_atomic() {
    let pool = common::fresh_database("blog_retention_policy_test").await;
    let store = PostgresRetentionStore::new(pool.clone());
    let actor = Uuid::now_v7();
    let default = store.read().await.unwrap();
    assert_eq!(default, RetentionSettings::default());
    assert_eq!(
        store
            .save(default.clone(), Some(actor).into())
            .await
            .unwrap(),
        default
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM settings")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    sqlx::query(
        "INSERT INTO settings(key,value) VALUES('comments','{\"enabled\":false,\"other\":true}')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let mut value = store.read().await.unwrap();
    value.comment_ip_days = 90;
    value.audit_days = 365;
    let saved = store.save(value.clone(), Some(actor).into()).await.unwrap();
    assert_eq!((saved.comment_version, saved.audit_version), (2, 1));
    assert_eq!(
        sqlx::query_scalar::<_, Value>("SELECT value FROM settings WHERE key='comments'")
            .fetch_one(&pool)
            .await
            .unwrap(),
        json!({"enabled":false,"other":true,"ip_retention_days":90})
    );
    assert!(matches!(
        store.save(value, Some(actor).into()).await,
        Err(UseCaseError::VersionConflict)
    ));
    // A concurrent global comment switch shares the comments group version.
    sqlx::query("UPDATE settings SET version=version+1,value=jsonb_set(value,'{enabled}','true') WHERE key='comments'").execute(&pool).await.unwrap();
    let mut stale = saved.clone();
    stale.audit_days = 30;
    assert!(matches!(
        store.save(stale, Some(actor).into()).await,
        Err(UseCaseError::VersionConflict)
    ));
    assert_eq!(store.read().await.unwrap().audit_days, 365);
    let mut invalid = store.read().await.unwrap();
    invalid.comment_ip_days = 0;
    assert!(matches!(
        store.save(invalid, Some(actor).into()).await,
        Err(UseCaseError::Invalid(_))
    ));
    // The policy change rolls back if its audit cannot be appended.
    sqlx::raw_sql("CREATE FUNCTION deny_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'audit unavailable'; END $$; CREATE TRIGGER deny_audit BEFORE INSERT ON audit_logs FOR EACH ROW EXECUTE FUNCTION deny_audit();").execute(&pool).await.unwrap();
    let before = store.read().await.unwrap();
    let mut change = before.clone();
    change.audit_days = 30;
    assert!(store.save(change, Some(actor).into()).await.is_err());
    assert_eq!(store.read().await.unwrap(), before);
}

#[tokio::test]
async fn cleanup_is_bounded_repeatable_and_preserves_comment_content_and_versions() {
    let pool = common::fresh_database("blog_retention_cleanup_test").await;
    seed(&pool).await;
    let before = comments_without_ip(&pool).await;
    let dry = maintenance(&pool).run(1, 1, true).await.unwrap();
    assert_eq!((dry.comment_ips, dry.audit_logs), (3, 2));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM comments WHERE ip_address IS NOT NULL")
            .fetch_one(&pool)
            .await
            .unwrap(),
        4
    );
    let first = maintenance(&pool).run(1, 1, false).await.unwrap();
    assert_eq!((first.comment_ips, first.audit_logs), (1, 1));
    assert!(first.has_more);
    let rest = maintenance(&pool).run(1, 5, false).await.unwrap();
    assert_eq!((rest.comment_ips, rest.audit_logs), (2, 1));
    assert!(!rest.has_more);
    let again = maintenance(&pool).run(100, 2, false).await.unwrap();
    assert_eq!((again.comment_ips, again.audit_logs), (0, 0));
    assert_eq!(before, comments_without_ip(&pool).await);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM comments WHERE ip_address IS NOT NULL")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM audit_logs WHERE action='maintenance.retention'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        3
    );
    // Invalid persisted policy must fail closed, never silently widen cleanup.
    sqlx::query("INSERT INTO settings(key,value) VALUES('audit','{\"retention_days\":0}')")
        .execute(&pool)
        .await
        .unwrap();
    assert!(maintenance(&pool).run(100, 2, false).await.is_err());
}

#[tokio::test]
async fn cleanup_audit_failure_rolls_back_ip_and_history_deletion() {
    let pool = common::fresh_database("blog_retention_rollback_test").await;
    seed(&pool).await;
    sqlx::raw_sql("CREATE FUNCTION deny_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'audit unavailable'; END $$; CREATE TRIGGER deny_audit BEFORE INSERT ON audit_logs FOR EACH ROW EXECUTE FUNCTION deny_audit();").execute(&pool).await.unwrap();
    assert!(maintenance(&pool).run(100, 2, false).await.is_err());
    let dry = maintenance(&pool).run(100, 2, true).await.unwrap();
    assert_eq!((dry.comment_ips, dry.audit_logs), (3, 2));
}
