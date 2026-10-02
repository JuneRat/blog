use super::*;
use serde_json::{Value, json};

async fn new_post(stack: &Stack, cookie: &str, csrf: &str, slug: &str, content: &str) -> Value {
    let (status, body) = api(
        &stack.router,
        "POST",
        "/api/admin/v1/posts",
        Some(cookie),
        Some(csrf),
        Some(&json!({"slug":slug,"title":slug,"content":content}).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    serde_json::from_str(&body).unwrap()
}
fn item(post: &Value, version: i64) -> Value {
    json!({"id":post["id"],"expected_version":version})
}
async fn batch(
    stack: &Stack,
    cookie: &str,
    csrf: &str,
    resource: &str,
    payload: Value,
) -> (StatusCode, Value) {
    let (status, body) = api(
        &stack.router,
        "POST",
        &format!("/api/admin/v1/{resource}/batch"),
        Some(cookie),
        Some(csrf),
        Some(&payload.to_string()),
    )
    .await;
    (
        status,
        serde_json::from_str(&body).unwrap_or_else(|e| panic!("{e}: {body}")),
    )
}
async fn posts_state(stack: &Stack) -> Value {
    sqlx::query_scalar("SELECT COALESCE(jsonb_agg(to_jsonb(p) ORDER BY p.id),'[]') FROM posts p")
        .fetch_one(&stack.pool)
        .await
        .unwrap()
}
async fn comments_state(stack: &Stack) -> Value {
    sqlx::query_scalar("SELECT COALESCE(jsonb_agg(to_jsonb(c) ORDER BY c.id),'[]') FROM comments c")
        .fetch_one(&stack.pool)
        .await
        .unwrap()
}
async fn audit_count(stack: &Stack, action: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM audit_logs WHERE action=$1")
        .bind(action)
        .fetch_one(&stack.pool)
        .await
        .unwrap()
}
async fn comment(stack: &Stack, post: &Value, parent: Option<Uuid>) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO comments(id,post_id,parent_id,root_id,author_name,content,content_html,content_render_version) VALUES($1,$2,$3,$3,'Guest','Body','<p>Body</p>',1)")
        .bind(id).bind(post["id"].as_str().unwrap().parse::<Uuid>().unwrap()).bind(parent).execute(&stack.pool).await.unwrap();
    id
}

#[tokio::test]
async fn post_batch_rolls_back_mixed_ownership_missing_targets_and_stale_versions() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (author, csrf) = login_as(&stack.router, &stack.idp, "author").await;
    let a = new_post(&stack, &author, &csrf, "batch-a", "Body").await;
    let (other, other_csrf) = login_as(&stack.router, &stack.idp, "author2").await;
    let b = new_post(&stack, &other, &other_csrf, "batch-b", "Body").await;
    let before = posts_state(&stack).await;
    let (status, result) = batch(
        &stack,
        &author,
        &csrf,
        "posts",
        json!({"action":"trash","items":[item(&a,1),item(&b,1)]}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{result}");
    assert_eq!(posts_state(&stack).await, before);
    let (owner, owner_csrf) = login_as(&stack.router, &stack.idp, "owner").await;
    for (target, expected) in [
        (item(&b, 2), StatusCode::CONFLICT),
        (
            json!({"id":Uuid::now_v7(),"expected_version":1}),
            StatusCode::NOT_FOUND,
        ),
    ] {
        let (status, result) = batch(
            &stack,
            &owner,
            &owner_csrf,
            "posts",
            json!({"action":"trash","items":[item(&a,1),target]}),
        )
        .await;
        assert_eq!(status, expected, "{result}");
        assert_eq!(posts_state(&stack).await, before);
    }
    assert_eq!(audit_count(&stack, "post.batch").await, 0);
    let (status, result) = batch(
        &stack,
        &owner,
        &owner_csrf,
        "posts",
        json!({"action":"trash","items":[item(&b,1),item(&a,1)]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["affected"], 2);
    assert_eq!(result["items"][0]["id"], b["id"]);
    assert_eq!(result["items"][1]["version"], 2);
    assert_eq!(audit_count(&stack, "post.batch").await, 1);
    let (status, result) = batch(
        &stack,
        &owner,
        &owner_csrf,
        "posts",
        json!({"action":"restore","items":[item(&a,2),item(&b,2)]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    for post in posts_state(&stack).await.as_array().unwrap() {
        assert_eq!(post["status"], "draft");
        assert_eq!(post["version"], 3);
        assert!(post["deleted_at"].is_null());
    }
}

#[tokio::test]
async fn post_batch_status_category_noops_and_domain_validation() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (cookie, csrf) = login_as(&stack.router, &stack.idp, "owner").await;
    let a = new_post(&stack, &cookie, &csrf, "publish-a", "Body").await;
    let b = new_post(&stack, &cookie, &csrf, "publish-b", "").await;
    let before = posts_state(&stack).await;
    let (status, _) = batch(&stack, &cookie, &csrf, "posts", json!({"action":"change_status","params":{"status":"published"},"items":[item(&a,1),item(&b,1)]})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(posts_state(&stack).await, before);
    let (status, body) = api(
        &stack.router,
        "PATCH",
        &format!("/api/admin/v1/posts/{}", b["id"].as_str().unwrap()),
        Some(&cookie),
        Some(&csrf),
        Some(r#"{"content":"Body","expected_version":1}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let source = posts_state(&stack).await;
    let (status, result) = batch(&stack, &cookie, &csrf, "posts", json!({"action":"change_status","params":{"status":"published"},"items":[item(&a,1),item(&b,2)]})).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    let published = posts_state(&stack).await;
    for (old, new) in source
        .as_array()
        .unwrap()
        .iter()
        .zip(published.as_array().unwrap())
    {
        assert_eq!(new["status"], "published");
        assert_eq!(new["content"], old["content"]);
        assert_eq!(new["content_html"], old["content_html"]);
    }
    let count = audit_count(&stack, "post.batch").await;
    let (status, noop) = batch(&stack, &cookie, &csrf, "posts", json!({"action":"change_status","params":{"status":"published"},"items":[item(&a,2),item(&b,3)]})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(noop["affected"], 0);
    assert_eq!(noop["items"][0]["changed"], false);
    assert_eq!(posts_state(&stack).await, published);
    assert_eq!(audit_count(&stack, "post.batch").await, count);
    assert_eq!(batch(&stack, &cookie, &csrf, "posts", json!({"action":"change_status","params":{"status":"published"},"items":[item(&a,1),item(&b,3)]})).await.0, StatusCode::CONFLICT);
    let category = Uuid::now_v7();
    assert_eq!(batch(&stack, &cookie, &csrf, "posts", json!({"action":"change_category","params":{"category_id":category},"items":[item(&a,2),item(&b,3)]})).await.0, StatusCode::NOT_FOUND);
    sqlx::query("INSERT INTO categories(id,name,slug) VALUES($1,'Batch','batch-category')")
        .bind(category)
        .execute(&stack.pool)
        .await
        .unwrap();
    assert_eq!(batch(&stack, &cookie, &csrf, "posts", json!({"action":"change_category","params":{"category_id":category},"items":[item(&a,2),item(&b,3)]})).await.0, StatusCode::OK);
    assert_eq!(batch(&stack, &cookie, &csrf, "posts", json!({"action":"change_category","params":{"category_id":null},"items":[item(&a,3),item(&b,4)]})).await.0, StatusCode::OK);
    for post in posts_state(&stack).await.as_array().unwrap() {
        assert!(post["category_id"].is_null());
    }
    assert_eq!(batch(&stack, &cookie, &csrf, "posts", json!({"action":"change_status","params":{"status":"draft"},"items":[item(&a,4),item(&b,5)]})).await.0, StatusCode::OK);
    let at = application::public_site::api_datetime(
        time::OffsetDateTime::now_utc() + time::Duration::days(1),
    );
    assert_eq!(batch(&stack, &cookie, &csrf, "posts", json!({"action":"change_status","params":{"status":"scheduled","published_at":at},"items":[item(&a,5),item(&b,6)]})).await.0, StatusCode::OK);
    for post in posts_state(&stack).await.as_array().unwrap() {
        assert_eq!(post["status"], "scheduled");
    }
    assert_eq!(batch(&stack, &cookie, &csrf, "posts", json!({"action":"change_status","params":{"status":"archived"},"items":[item(&a,6),item(&b,7)]})).await.0, StatusCode::OK);
    let archived = posts_state(&stack).await;
    assert_eq!(batch(&stack, &cookie, &csrf, "posts", json!({"action":"change_status","params":{"status":"published"},"items":[item(&a,7),item(&b,8)]})).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(posts_state(&stack).await, archived);
}

#[tokio::test]
async fn post_batch_purge_cleans_relations_and_audit_failure_restores_everything() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (cookie, csrf) = login_as(&stack.router, &stack.idp, "owner").await;
    let a = new_post(&stack, &cookie, &csrf, "purge-a", "Body").await;
    let b = new_post(&stack, &cookie, &csrf, "purge-b", "Body").await;
    let root = comment(&stack, &a, None).await;
    comment(&stack, &a, Some(root)).await;
    let media = Uuid::now_v7();
    let series = Uuid::now_v7();
    let tag = Uuid::now_v7();
    sqlx::query("INSERT INTO media(id,path,filename,mime_type,size,width,height,checksum_sha256) VALUES($1,$2,'batch.png','image/png',1,1,1,$3)").bind(media).bind(format!("{media}.png")).bind("0".repeat(64)).execute(&stack.pool).await.unwrap();
    sqlx::query("INSERT INTO series(id,name,slug) VALUES($1,'Batch','batch-series')")
        .bind(series)
        .execute(&stack.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO tags(id,name,slug) VALUES($1,'Batch','batch-tag')")
        .bind(tag)
        .execute(&stack.pool)
        .await
        .unwrap();
    for post in [&a, &b] {
        let id: Uuid = post["id"].as_str().unwrap().parse().unwrap();
        sqlx::query("INSERT INTO post_series(post_id,series_id,position) VALUES($1,$2,0)")
            .bind(id)
            .bind(series)
            .execute(&stack.pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO post_tags(post_id,tag_id) VALUES($1,$2)")
            .bind(id)
            .bind(tag)
            .execute(&stack.pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO media_refs(media_id,source_type,source_id) VALUES($1,'post',$2)")
            .bind(media)
            .bind(id)
            .execute(&stack.pool)
            .await
            .unwrap();
    }
    assert_eq!(
        batch(
            &stack,
            &cookie,
            &csrf,
            "posts",
            json!({"action":"purge","items":[item(&a,1),item(&b,1)]})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        batch(
            &stack,
            &cookie,
            &csrf,
            "posts",
            json!({"action":"trash","items":[item(&a,1),item(&b,1)]})
        )
        .await
        .0,
        StatusCode::OK
    );
    let before = posts_state(&stack).await;
    let comments = comments_state(&stack).await;
    sqlx::query("ALTER TABLE audit_logs ADD CONSTRAINT fail_batch_audit CHECK(action <> 'post.batch') NOT VALID").execute(&stack.pool).await.unwrap();
    let payload = json!({"action":"purge","items":[item(&a,2),item(&b,2)]});
    assert_eq!(
        batch(&stack, &cookie, &csrf, "posts", payload.clone())
            .await
            .0,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(posts_state(&stack).await, before);
    assert_eq!(comments_state(&stack).await, comments);
    let counts: (i64,i64,i64,i64) = sqlx::query_as("SELECT (SELECT count(*) FROM post_series),(SELECT count(*) FROM post_tags),(SELECT count(*) FROM media_refs),(SELECT version FROM series WHERE slug='batch-series')").fetch_one(&stack.pool).await.unwrap();
    assert_eq!(counts, (2, 2, 2, 1));
    sqlx::query("ALTER TABLE audit_logs DROP CONSTRAINT fail_batch_audit")
        .execute(&stack.pool)
        .await
        .unwrap();
    let (status, result) = batch(&stack, &cookie, &csrf, "posts", payload).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["affected"], 2);
    assert!(result["items"][0]["version"].is_null());
    assert_eq!(posts_state(&stack).await, json!([]));
    assert_eq!(comments_state(&stack).await, json!([]));
    let counts: (i64,i64,i64,i64,i64) = sqlx::query_as("SELECT (SELECT count(*) FROM post_series),(SELECT count(*) FROM post_tags),(SELECT count(*) FROM media_refs),(SELECT version FROM series WHERE slug='batch-series'),(SELECT count(*) FROM media)").fetch_one(&stack.pool).await.unwrap();
    assert_eq!(counts, (0, 0, 0, 2, 1));
    assert_eq!(audit_count(&stack, "post.batch").await, 2);
}

#[tokio::test]
async fn comment_batch_authorization_transitions_noops_and_reply_relations() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (author, csrf) = login_as(&stack.router, &stack.idp, "author").await;
    let a = new_post(&stack, &author, &csrf, "comment-a", "Body").await;
    let (other, other_csrf) = login_as(&stack.router, &stack.idp, "author2").await;
    let b = new_post(&stack, &other, &other_csrf, "comment-b", "Body").await;
    let root = comment(&stack, &a, None).await;
    let child = comment(&stack, &a, Some(root)).await;
    let foreign = comment(&stack, &b, None).await;
    let target = |id, version| json!({"id":id,"expected_version":version});
    let before = comments_state(&stack).await;
    assert_eq!(
        batch(
            &stack,
            &author,
            &csrf,
            "comments",
            json!({"action":"approve","items":[target(root,1),target(foreign,1)]})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(comments_state(&stack).await, before);
    assert_eq!(
        batch(
            &stack,
            &author,
            &csrf,
            "comments",
            json!({"action":"approve","items":[target(root,1),target(child,2)]})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(comments_state(&stack).await, before);
    assert_eq!(
        batch(
            &stack,
            &author,
            &csrf,
            "comments",
            json!({"action":"approve","items":[target(root,1),target(Uuid::now_v7(),1)]})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        batch(
            &stack,
            &author,
            &csrf,
            "comments",
            json!({"action":"approve","items":[target(child,1),target(root,1)]})
        )
        .await
        .0,
        StatusCode::OK
    );
    let approved = comments_state(&stack).await;
    assert_eq!(
        approved.as_array().unwrap()[1]["parent_id"],
        root.to_string()
    );
    assert_eq!(approved.as_array().unwrap()[1]["root_id"], root.to_string());
    let (_, noop) = batch(
        &stack,
        &author,
        &csrf,
        "comments",
        json!({"action":"approve","items":[target(root,2),target(child,2)]}),
    )
    .await;
    assert_eq!(noop["affected"], 0);
    assert_eq!(audit_count(&stack, "comment.batch").await, 1);
    assert_eq!(
        batch(
            &stack,
            &author,
            &csrf,
            "comments",
            json!({"action":"approve","items":[target(root,1),target(child,2)]})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        batch(
            &stack,
            &author,
            &csrf,
            "comments",
            json!({"action":"trash","items":[target(root,2)]})
        )
        .await
        .0,
        StatusCode::OK
    );
    let trashed = comments_state(&stack).await;
    assert_eq!(
        batch(
            &stack,
            &author,
            &csrf,
            "comments",
            json!({"action":"approve","items":[target(child,2),target(root,3)]})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(comments_state(&stack).await, trashed);
    assert_eq!(
        batch(
            &stack,
            &author,
            &csrf,
            "comments",
            json!({"action":"restore","items":[target(root,3)]})
        )
        .await
        .0,
        StatusCode::OK
    );
    let restored = comments_state(&stack).await;
    assert_eq!(restored[0]["status"], "pending");
    assert_eq!(restored[0]["moderation_reason"], "restored");
    assert_eq!(restored[1]["status"], "approved");
}

#[tokio::test]
async fn comment_batch_audit_failure_rolls_back_all_moderation() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (cookie, csrf) = login_as(&stack.router, &stack.idp, "author").await;
    let post = new_post(&stack, &cookie, &csrf, "audit-comment", "Body").await;
    let a = comment(&stack, &post, None).await;
    let b = comment(&stack, &post, None).await;
    let before = comments_state(&stack).await;
    sqlx::query("ALTER TABLE audit_logs ADD CONSTRAINT fail_batch_audit CHECK(action <> 'comment.batch') NOT VALID").execute(&stack.pool).await.unwrap();
    let payload = json!({"action":"approve","items":[{"id":a,"expected_version":1},{"id":b,"expected_version":1}]});
    assert_eq!(
        batch(&stack, &cookie, &csrf, "comments", payload).await.0,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(comments_state(&stack).await, before);
    assert_eq!(audit_count(&stack, "comment.batch").await, 0);
}

#[tokio::test]
async fn batch_requests_enforce_auth_csrf_strict_shapes_and_bounds() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (cookie, csrf) = login_as(&stack.router, &stack.idp, "author").await;
    for (resource, action) in [("posts", "trash"), ("comments", "approve")] {
        let path = format!("/api/admin/v1/{resource}/batch");
        let id = Uuid::now_v7();
        let target = json!({"id":id,"expected_version":1});
        let payload = json!({"action":action,"items":[target.clone()]}).to_string();
        assert_eq!(
            api(&stack.router, "POST", &path, None, None, Some(&payload))
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            api(
                &stack.router,
                "POST",
                &path,
                Some(&cookie),
                None,
                Some(&payload)
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        for payload in [
            json!({"action":action,"items":[]}),
            json!({"action":action,"items":[{"id":id}]}),
            json!({"action":action,"items":[{"id":id,"expected_version":0}]}),
            json!({"action":action,"items":[target.clone(),target.clone()]}),
            json!({"action":action,"items":(0..101).map(|_|json!({"id":Uuid::now_v7(),"expected_version":1})).collect::<Vec<_>>()}),
            json!({"action":"delete","items":[target.clone()]}),
            json!({"action":action,"ids":[id]}),
            json!({"action":action,"items":[target],"unexpected":true}),
        ] {
            let (status, result) = batch(&stack, &cookie, &csrf, resource, payload).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{result}");
            assert_eq!(result["code"], "invalid_request");
        }
        let padded = format!("{}{}", payload, " ".repeat(17 * 1024));
        assert_eq!(
            api(
                &stack.router,
                "POST",
                &path,
                Some(&cookie),
                Some(&csrf),
                Some(&padded)
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
}

#[tokio::test]
async fn overlapping_batches_in_opposite_order_commit_once_without_deadlock() {
    let _g = SERIAL.lock().await;
    let stack = fresh_stack().await;
    let (cookie, csrf) = login_as(&stack.router, &stack.idp, "owner").await;
    let a = new_post(&stack, &cookie, &csrf, "concurrent-a", "Body").await;
    let b = new_post(&stack, &cookie, &csrf, "concurrent-b", "Body").await;
    let c = comment(&stack, &a, None).await;
    let d = comment(&stack, &b, None).await;
    for (resource, action, items) in [
        ("posts", "trash", vec![item(&a, 1), item(&b, 1)]),
        (
            "comments",
            "approve",
            vec![
                json!({"id":c,"expected_version":1}),
                json!({"id":d,"expected_version":1}),
            ],
        ),
    ] {
        let (left, right) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            tokio::join!(
                batch(
                    &stack,
                    &cookie,
                    &csrf,
                    resource,
                    json!({"action":action,"items":items})
                ),
                batch(
                    &stack,
                    &cookie,
                    &csrf,
                    resource,
                    json!({"action":action,"items":items.iter().rev().collect::<Vec<_>>()})
                )
            )
        })
        .await
        .expect("opposite request order must not deadlock");
        assert!(
            matches!(
                (left.0, right.0),
                (StatusCode::OK, StatusCode::CONFLICT) | (StatusCode::CONFLICT, StatusCode::OK)
            ),
            "{left:?}, {right:?}"
        );
    }
    assert_eq!(audit_count(&stack, "post.batch").await, 1);
    assert_eq!(audit_count(&stack, "comment.batch").await, 1);
}
