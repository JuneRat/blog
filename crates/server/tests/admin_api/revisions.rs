use super::*;
use serde_json::{Value, json};

async fn call(
    stack: &Stack,
    session: &(String, String),
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let (status, body) = api(
        &stack.router,
        method,
        path,
        Some(&session.0),
        Some(&session.1),
        Some(&body.to_string()),
    )
    .await;
    (status, serde_json::from_str(&body).unwrap_or(Value::Null))
}
async fn ok(
    stack: &Stack,
    session: &(String, String),
    method: &str,
    path: &str,
    body: Value,
) -> Value {
    let (status, body) = call(stack, session, method, path, body).await;
    assert!(status.is_success(), "{method} {path}: {status} {body}");
    body
}

#[tokio::test]
async fn published_post_and_page_save_drafts_restore_history_and_publish_explicitly() {
    let _guard = SERIAL.lock().await;
    let stack = fresh_stack_with_themes(true).await;
    let owner = login_as(&stack.router, &stack.idp, "owner").await;
    let other = login_as(&stack.router, &stack.idp, "author2").await;
    for resource in ["posts", "pages"] {
        let slug = format!("revision-{resource}");
        let base = format!("/api/admin/v1/{resource}");
        let created = ok(
            &stack,
            &owner,
            "POST",
            &base,
            json!({"slug":slug,"title":"Original title","content":"Original body"}),
        )
        .await;
        let id = created["id"].as_str().unwrap();
        let path = format!("{base}/{id}");
        ok(
            &stack,
            &owner,
            "POST",
            &format!("{path}/publish"),
            json!({"expected_version":1}),
        )
        .await;
        let live_before: (String, String, String, time::OffsetDateTime) = sqlx::query_as(&format!(
            "SELECT title,content,visibility,updated_at FROM {resource} WHERE id=$1"
        ))
        .bind(id.parse::<Uuid>().unwrap())
        .fetch_one(&stack.pool)
        .await
        .unwrap();
        let draft = ok(&stack, &owner, "PATCH", &path, json!({"title":"Secret draft","content":"Unpublished body","visibility":"private","expected_version":2})).await;
        assert_eq!(draft["version"], 3);
        assert_eq!(draft["has_pending_changes"], true);
        let live_after: (String, String, String, time::OffsetDateTime) = sqlx::query_as(&format!(
            "SELECT title,content,visibility,updated_at FROM {resource} WHERE id=$1"
        ))
        .bind(id.parse::<Uuid>().unwrap())
        .fetch_one(&stack.pool)
        .await
        .unwrap();
        assert_eq!(live_before, live_after);
        let public = if resource == "posts" {
            format!("/posts/{slug}")
        } else {
            format!("/{slug}")
        };
        let (status, html) = api(&stack.router, "GET", &public, None, None, None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("Original body"));
        assert!(!html.contains("Unpublished body"));
        let loaded = ok(&stack, &owner, "GET", &path, json!(null)).await;
        assert_eq!(loaded["content"], "Unpublished body");
        assert_eq!(loaded["has_pending_changes"], true);
        assert_eq!(
            call(
                &stack,
                &owner,
                "PATCH",
                &path,
                json!({"title":"Stale","expected_version":2})
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            call(
                &stack,
                &other,
                "GET",
                &format!("{path}/revisions"),
                json!(null)
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
        let history = ok(
            &stack,
            &owner,
            "GET",
            &format!("{path}/revisions"),
            json!(null),
        )
        .await;
        assert_eq!(history.as_array().unwrap().len(), 3);
        assert_eq!(history[0]["version"], 3);
        let revision = history[2]["id"].as_str().unwrap();
        let restore = format!("{path}/revisions/{revision}/restore");
        assert_eq!(
            call(
                &stack,
                &owner,
                "POST",
                &restore,
                json!({"expected_version":2})
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
        let restored = ok(
            &stack,
            &owner,
            "POST",
            &restore,
            json!({"expected_version":3}),
        )
        .await;
        assert_eq!(restored["content"], "Original body");
        assert_eq!(restored["has_pending_changes"], true);
        let published = ok(
            &stack,
            &owner,
            "POST",
            &format!("{path}/publish"),
            json!({"expected_version":4}),
        )
        .await;
        assert_eq!(published["version"], 5);
        assert_eq!(published["has_pending_changes"], false);
        ok(
            &stack,
            &owner,
            "PATCH",
            &path,
            json!({"content":"Next body","expected_version":5}),
        )
        .await;
        if resource == "posts" {
            assert_eq!(
                call(
                    &stack,
                    &owner,
                    "POST",
                    &format!("{base}/batch"),
                    json!({"action":"change_status","params":{"status":"published"},"items":[{"id":id,"expected_version":6}]})
                )
                .await
                .0,
                StatusCode::BAD_REQUEST
            );
        }
        // Withdrawal must keep the server editing copy; re-publishing explicitly applies it.
        let withdrawn = ok(
            &stack,
            &owner,
            "POST",
            &format!("{path}/unpublish"),
            json!({"expected_version":6}),
        )
        .await;
        assert_eq!(withdrawn["content"], "Next body");
        assert_eq!(withdrawn["has_pending_changes"], true);
        let unchanged = ok(
            &stack,
            &owner,
            "POST",
            &format!("{path}/unpublish"),
            json!({"expected_version":7}),
        )
        .await;
        assert_eq!(unchanged["version"], 7);
        assert_eq!(unchanged["has_pending_changes"], true);
        let draft = ok(&stack, &owner, "GET", &path, json!(null)).await;
        assert_eq!(draft["content"], "Next body");
        assert_eq!(draft["status"], "draft");
        ok(
            &stack,
            &owner,
            "POST",
            &format!("{path}/publish"),
            json!({"expected_version":7}),
        )
        .await;
        let (_, html) = api(&stack.router, "GET", &public, None, None, None).await;
        assert!(html.contains("Next body"));
        let foreign = format!("{base}/{}/revisions/{revision}", Uuid::now_v7());
        assert_eq!(
            call(&stack, &owner, "GET", &foreign, json!(null)).await.0,
            StatusCode::NOT_FOUND
        );
        ok(
            &stack,
            &owner,
            "POST",
            &format!("{path}/trash"),
            json!({"expected_version":8}),
        )
        .await;
        assert_eq!(
            call(
                &stack,
                &owner,
                "GET",
                &format!("{path}/revisions"),
                json!(null)
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
        ok(
            &stack,
            &owner,
            "POST",
            &format!("{path}/purge"),
            json!({"expected_version":9}),
        )
        .await;
        let count: i64 = sqlx::query_scalar(&format!(
            "SELECT count(*) FROM content_revisions WHERE {}_id=$1",
            if resource == "posts" { "post" } else { "page" }
        ))
        .bind(id.parse::<Uuid>().unwrap())
        .fetch_one(&stack.pool)
        .await
        .unwrap();
        assert_eq!(count, 0);
    }
}

#[tokio::test]
async fn revision_media_is_private_protected_pruned_and_draft_failure_is_atomic() {
    let _guard = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let owner = login_as(&stack.router, &stack.idp, "owner").await;
    let image = Uuid::now_v7();
    sqlx::query("INSERT INTO media(id,path,filename,mime_type,size,width,height,checksum_sha256) VALUES($1,$2,'revision.png','image/png',1,1,1,repeat('a',64))")
        .bind(image).bind(format!("objects/{image}.png")).execute(&stack.pool).await.unwrap();
    let created = ok(&stack, &owner, "POST", "/api/admin/v1/posts", json!({"slug":"media-revisions","title":"Media","content":format!("![old](/media/{image})")})).await;
    let id = created["id"].as_str().unwrap();
    let path = format!("/api/admin/v1/posts/{id}");
    ok(
        &stack,
        &owner,
        "POST",
        &format!("{path}/publish"),
        json!({"expected_version":1}),
    )
    .await;
    sqlx::raw_sql("CREATE FUNCTION fail_draft() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='post.draft' THEN RAISE EXCEPTION 'injected'; END IF; RETURN NEW; END $$; CREATE TRIGGER fail_draft BEFORE INSERT ON audit_logs FOR EACH ROW EXECUTE FUNCTION fail_draft()").execute(&stack.pool).await.unwrap();
    assert_eq!(
        call(
            &stack,
            &owner,
            "PATCH",
            &path,
            json!({"content":"Must roll back","expected_version":2})
        )
        .await
        .0,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    let current = ok(&stack, &owner, "GET", &path, json!(null)).await;
    assert_eq!(current["version"], 2);
    assert_eq!(current["has_pending_changes"], false);
    assert_eq!(
        ok(
            &stack,
            &owner,
            "GET",
            &format!("{path}/revisions"),
            json!(null)
        )
        .await
        .as_array()
        .unwrap()
        .len(),
        2
    );
    sqlx::query("DROP TRIGGER fail_draft ON audit_logs")
        .execute(&stack.pool)
        .await
        .unwrap();
    ok(
        &stack,
        &owner,
        "PATCH",
        &path,
        json!({"content":"No image","expected_version":2}),
    )
    .await;
    ok(
        &stack,
        &owner,
        "POST",
        &format!("{path}/publish"),
        json!({"expected_version":3}),
    )
    .await;
    let usage = ok(
        &stack,
        &owner,
        "GET",
        &format!("/api/admin/v1/media/{image}"),
        json!(null),
    )
    .await;
    assert!(!usage["references"].as_array().unwrap().is_empty());
    assert!(
        usage["references"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["kind"] == "post_revision" && r["public"] == false)
    );
    let other = login_as(&stack.router, &stack.idp, "author2").await;
    let hidden = ok(
        &stack,
        &other,
        "GET",
        &format!("/api/admin/v1/media/{image}"),
        json!(null),
    )
    .await;
    assert!(hidden["references"].as_array().unwrap().is_empty());
    assert_eq!(
        hidden["hidden_references"],
        usage["references"].as_array().unwrap().len()
    );
    assert!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM media_refs WHERE media_id=$1")
            .bind(image)
            .fetch_one(&stack.pool)
            .await
            .unwrap()
            > 0
    );
    for version in 4..57 {
        ok(
            &stack,
            &owner,
            "PATCH",
            &path,
            json!({"content":format!("Draft {version}"),"expected_version":version}),
        )
        .await;
    }
    let history = ok(
        &stack,
        &owner,
        "GET",
        &format!("{path}/revisions"),
        json!(null),
    )
    .await;
    assert_eq!(history.as_array().unwrap().len(), 50);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM media_refs WHERE media_id=$1")
            .bind(image)
            .fetch_one(&stack.pool)
            .await
            .unwrap(),
        0
    );
    let editor = ok(&stack, &owner, "GET", &path, json!(null)).await;
    assert_eq!(editor["content"], "Draft 56");
    assert_eq!(editor["has_pending_changes"], true);
    // Purging an active editing copy must also resolve both sides of its foreign keys.
    ok(
        &stack,
        &owner,
        "POST",
        &format!("{path}/trash"),
        json!({"expected_version":57}),
    )
    .await;
    ok(
        &stack,
        &owner,
        "POST",
        &format!("{path}/purge"),
        json!({"expected_version":58}),
    )
    .await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM content_revisions")
            .fetch_one(&stack.pool)
            .await
            .unwrap(),
        0
    );
}
