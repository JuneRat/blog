mod common;
use application::audit::{AuditFilter, AuditQuery, AuditQueryStore};
use infrastructure::audit::PostgresAuditQuery;
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
async fn audit_filters_and_cursor_survive_new_writes_and_deleted_anchor() {
    let pool = common::fresh_database("blog_audit_query_test").await;
    let actor = common::seed_user(&pool, "audit-reader").await;
    let removed_actor = Uuid::now_v7();
    for n in 1..=4u128 {
        sqlx::query("INSERT INTO audit_logs(id,actor_id,ip_address,action,target_type,target_id,metadata,created_at) VALUES($1,$2,'2001:db8::42','post.update','post',$3,$4,'2026-09-27T10:00:00.123456Z')")
            .bind(Uuid::from_u128(n)).bind(match n { 1 => None, 2 => Some(removed_actor), _ => Some(actor) })
            .bind(format!("target-{n}"))
            .bind(json!({"version":n as i64,"changed":["title"],"note":"<img src=x onerror=alert(1)>"}))
            .execute(&pool).await.unwrap();
    }
    let store = PostgresAuditQuery::new(pool.clone());
    let query = |cursor| {
        AuditFilter::try_from(AuditQuery {
            limit: Some(2),
            cursor,
            ..Default::default()
        })
        .unwrap()
    };
    let first = store.list(&query(None)).await.unwrap();
    assert_eq!(
        first.items.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![Uuid::from_u128(4), Uuid::from_u128(3)]
    );
    assert_eq!(
        first.items[0].actor_display.as_deref(),
        Some("audit-reader的展示名")
    );
    assert_eq!(first.items[0].ip_address.as_deref(), Some("2001:db8::42"));
    assert!(
        first.items[0]
            .summary
            .iter()
            .any(|f| f.key == "version" && f.value == "4")
    );
    // Remove the page boundary while inserting a new record. Older entries remain reachable.
    sqlx::query("DELETE FROM audit_logs WHERE id=$1")
        .bind(Uuid::from_u128(3))
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO audit_logs(id,action,target_type,target_id,created_at) VALUES($1,'post.update','post','new','2026-09-27T11:00:00Z')").bind(Uuid::from_u128(5)).execute(&pool).await.unwrap();
    let next = store.list(&query(first.next_cursor)).await.unwrap();
    assert_eq!(
        next.items.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![Uuid::from_u128(2), Uuid::from_u128(1)]
    );
    assert!(next.next_cursor.is_none());
    assert_eq!(next.items[0].actor_id, Some(removed_actor));
    assert_eq!(next.items[0].actor_display, None);
    assert_eq!(next.items[1].actor_id, None);

    for (actor_id, without_actor, target, expected) in [
        (Some(actor), false, "target-4", 4),
        (Some(removed_actor), false, "target-2", 2),
        (None, true, "target-1", 1),
    ] {
        let filter = AuditFilter::try_from(AuditQuery {
            action: Some("post.update".into()),
            actor_id,
            without_actor,
            target_type: Some("post".into()),
            target_id: Some(target.into()),
            from: Some("2026-09-27T10:00:00.123456Z".into()),
            until: Some("2026-09-27T11:00:00Z".into()),
            ..Default::default()
        })
        .unwrap();
        let page = store.list(&filter).await.unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].id, Uuid::from_u128(expected));
    }
    let empty = AuditFilter::try_from(AuditQuery {
        action: Some("' OR true --".into()),
        ..Default::default()
    })
    .unwrap();
    assert!(store.list(&empty).await.unwrap().items.is_empty());
    let end = AuditFilter::try_from(AuditQuery {
        until: Some("2026-09-27T10:00:00.123456Z".into()),
        ..Default::default()
    })
    .unwrap();
    assert!(store.list(&end).await.unwrap().items.is_empty());
}
