mod common;
use application::{
    comments::*,
    error::UseCaseError,
    identity::{Actor, ActorChannel},
    ports::CommentRenderer,
};
use domain::identity::{PermissionSet, UserId};
use infrastructure::{
    COMMENT_RENDER_VERSION, RenderingRuntime,
    comments::{PostgresCommentRepository, rebuild_comment_html},
};
use sqlx::{PgPool, Row};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use uuid::Uuid;

fn actor(id: Uuid, all: bool) -> Actor {
    Actor::new(
        UserId(id),
        ActorChannel::Session,
        PermissionSet::from_keys(if all {
            vec!["post.update_any", "settings.manage"]
        } else {
            vec!["post.update"]
        }),
    )
}
fn cmd(body: &str, parent_id: Option<Uuid>) -> SubmitComment {
    SubmitComment {
        nickname: "Guest".into(),
        email: Some("guest@example.com".into()),
        body: body.into(),
        parent_id,
    }
}
fn service(pool: &PgPool) -> CommentInteractor {
    let renderer = Arc::new(RenderingRuntime::default());
    CommentInteractor::new(
        Arc::new(PostgresCommentRepository::new(
            pool.clone(),
            renderer.clone(),
        )),
        renderer,
    )
}
async fn fixture(db: &str) -> (PgPool, CommentInteractor, Actor, Uuid) {
    let pool = common::fresh_database(db).await;
    let user = common::seed_user(&pool, "author").await;
    let post = seed_post(&pool, user, "discussion").await;
    let service = service(&pool);
    (pool, service, actor(user, true), post)
}
async fn seed_post(pool: &PgPool, owner: Uuid, slug: &str) -> Uuid {
    let post = Uuid::now_v7();
    sqlx::query("INSERT INTO posts(id,author_id,slug,title,content,content_html,content_render_version,status,published_at) VALUES($1,$2,$3,'Discussion','Body','<p>Body</p>',1,'published',now())")
        .bind(post).bind(owner).bind(slug).execute(pool).await.unwrap();
    post
}
async fn change(
    service: &CommentInteractor,
    admin: &Actor,
    id: Uuid,
    version: i64,
    status: CommentStatus,
) {
    service
        .moderate(
            admin,
            id,
            version,
            ModerationAction::SetStatus(status),
            None,
        )
        .await
        .unwrap();
}
async fn new_comment(service: &CommentInteractor, admin: &Actor, parent: Option<Uuid>) -> Comment {
    service
        .submit(
            "discussion",
            None,
            Some("198.51.100.2".parse().unwrap()),
            cmd("**Hello**\n<script>alert(1)</script>", parent),
        )
        .await
        .unwrap();
    service
        .list(admin, Some("pending"), None, 1)
        .await
        .unwrap()
        .items
        .remove(0)
}

#[tokio::test]
async fn persisted_html_privacy_identity_and_independent_submissions() {
    let (pool, service, admin, post) = fixture("blog_test_comments_html").await;
    let root = new_comment(&service, &admin, None).await;
    assert_eq!(root.author_email.as_deref(), Some("guest@example.com"));
    assert_eq!(root.ip_address.as_deref(), Some("198.51.100.2"));
    assert_eq!(
        root.content_html,
        service.preview(&root.body).await.unwrap()
    );
    assert!(root.content_html.contains("<strong>Hello</strong>"));
    assert!(!root.content_html.contains("<script>"));
    assert_eq!(
        service
            .public_list("discussion", None, 1)
            .await
            .unwrap()
            .total,
        0
    );
    change(&service, &admin, root.id, 1, CommentStatus::Approved).await;
    let public =
        serde_json::to_value(service.public_list("discussion", None, 1).await.unwrap()).unwrap();
    for key in [
        "body",
        "author_email",
        "ip_address",
        "user_id",
        "status",
        "version",
    ] {
        assert!(public["items"][0].get(key).is_none(), "{key}");
    }
    assert!(!public.to_string().contains("guest@example.com"));
    let source = cmd("Duplicate is an independent request", None);
    let (a, b) = tokio::join!(
        service.submit("discussion", None, None, source.clone()),
        service.submit("discussion", None, None, source)
    );
    a.unwrap();
    b.unwrap();
    assert_eq!(
        service
            .list(&admin, Some("pending"), None, 1)
            .await
            .unwrap()
            .total,
        2
    );
    sqlx::query("UPDATE users SET display_name=$2 WHERE id=$1")
        .bind(admin.user_id.0)
        .bind("  Alice\nAdmin\u{7}  ")
        .execute(&pool)
        .await
        .unwrap();
    let mut account = cmd("Account comment", None);
    account.nickname = "\nspoof".into();
    account.email = Some("invalid".into());
    service
        .submit("discussion", Some(&admin), None, account)
        .await
        .unwrap();
    let account = service
        .list(&admin, Some("pending"), None, 1)
        .await
        .unwrap()
        .items
        .remove(0);
    assert_eq!(account.nickname, "AliceAdmin");
    assert!(account.is_author);
    assert!(account.author_email.is_none());
    let mut bad = cmd("Guest", None);
    bad.email = Some("invalid".into());
    assert!(matches!(
        service.submit("discussion", None, None, bad).await,
        Err(UseCaseError::Invalid(_))
    ));
    let version: i64 = sqlx::query_scalar("SELECT version FROM posts WHERE id=$1")
        .bind(post)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(version, 1);
    let audits: Vec<serde_json::Value> =
        sqlx::query_scalar("SELECT metadata FROM audit_logs WHERE action LIKE 'comment.%'")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(audits.len(), 5);
    assert!(audits.iter().all(|v| !v.to_string().contains("example.com")
        && v.get("body").is_none()
        && v.get("content").is_none()));
    pool.close().await;
}

#[tokio::test]
async fn multilevel_replies_survive_deleted_and_hidden_ancestors() {
    let (pool, service, admin, post) = fixture("blog_test_comments_tree").await;
    let root = new_comment(&service, &admin, None).await;
    assert!(matches!(
        service
            .submit("discussion", None, None, cmd("Reply", Some(root.id)))
            .await,
        Err(UseCaseError::NotFound(_))
    ));
    change(&service, &admin, root.id, 1, CommentStatus::Approved).await;
    let reply = new_comment(&service, &admin, Some(root.id)).await;
    change(&service, &admin, reply.id, 1, CommentStatus::Approved).await;
    let nested = new_comment(&service, &admin, Some(reply.id)).await;
    change(&service, &admin, nested.id, 1, CommentStatus::Approved).await;
    assert_eq!(nested.root_id, Some(root.id));
    assert_eq!(nested.parent_id, Some(reply.id));
    assert_eq!(
        service
            .public_list("discussion", Some(root.id), 1)
            .await
            .unwrap()
            .total,
        2
    );
    assert!(matches!(
        service.public_list("discussion", Some(reply.id), 1).await,
        Err(UseCaseError::NotFound(_))
    ));
    seed_post(&pool, admin.user_id.0, "other").await;
    assert!(matches!(
        service
            .submit("other", None, None, cmd("Cross post", Some(nested.id)))
            .await,
        Err(UseCaseError::NotFound(_))
    ));
    change(&service, &admin, root.id, 2, CommentStatus::Trash).await;
    change(&service, &admin, reply.id, 2, CommentStatus::Spam).await;
    let roots = service.public_list("discussion", None, 1).await.unwrap();
    assert_eq!(roots.total, 1);
    assert!(roots.items[0].deleted);
    assert!(roots.items[0].placeholder);
    assert!(roots.items[0].content_html.is_empty());
    assert!(roots.items[0].nickname.is_empty());
    assert!(!roots.items[0].is_author);
    let replies = service
        .public_list("discussion", Some(root.id), 1)
        .await
        .unwrap();
    assert_eq!(replies.total, 2);
    assert!(replies.items[0].placeholder);
    assert!(!replies.items[0].deleted);
    assert!(replies.items[1].parent_nickname.is_none());
    assert!(!replies.items[1].placeholder);
    // A public descendant remains replyable even when its root was deleted.
    let fourth = new_comment(&service, &admin, Some(nested.id)).await;
    assert_eq!(fourth.root_id, Some(root.id));
    assert!(matches!(
        service
            .moderate(
                &admin,
                root.id,
                3,
                ModerationAction::SetStatus(CommentStatus::Approved),
                None
            )
            .await,
        Err(UseCaseError::Invalid(_))
    ));
    change(&service, &admin, root.id, 3, CommentStatus::Pending).await;
    assert!(
        service
            .public_list("discussion", None, 1)
            .await
            .unwrap()
            .items[0]
            .placeholder
    );
    change(&service, &admin, root.id, 4, CommentStatus::Approved).await;
    assert!(
        !service
            .public_list("discussion", None, 1)
            .await
            .unwrap()
            .items[0]
            .placeholder
    );
    assert!(
        sqlx::query("DELETE FROM comments WHERE id=$1")
            .bind(root.id)
            .execute(&pool)
            .await
            .is_err()
    );
    // Explicit tree deletion is safe for post purge; individual parents are protected.
    sqlx::query("DELETE FROM comments WHERE post_id=$1")
        .bind(post)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM posts WHERE id=$1")
        .bind(post)
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
}

#[tokio::test]
async fn moderation_scope_cas_noop_and_post_visibility() {
    let (pool, service, admin, post) = fixture("blog_test_comments_scope").await;
    let root = new_comment(&service, &admin, None).await;
    let outsider = actor(common::seed_user(&pool, "outsider").await, false);
    assert_eq!(
        service.list(&outsider, None, None, 1).await.unwrap().total,
        0
    );
    assert!(matches!(
        service
            .moderate(
                &outsider,
                root.id,
                1,
                ModerationAction::SetStatus(CommentStatus::Approved),
                None
            )
            .await,
        Err(UseCaseError::Forbidden)
    ));
    change(&service, &admin, root.id, 1, CommentStatus::Pending).await;
    let (a, b) = tokio::join!(
        service.moderate(
            &admin,
            root.id,
            1,
            ModerationAction::SetStatus(CommentStatus::Approved),
            None
        ),
        service.moderate(
            &admin,
            root.id,
            1,
            ModerationAction::SetStatus(CommentStatus::Trash),
            None
        )
    );
    assert!(a.is_ok() ^ b.is_ok());
    assert!(
        matches!(a, Err(UseCaseError::VersionConflict))
            || matches!(b, Err(UseCaseError::VersionConflict))
    );
    for change in [
        "status='draft'",
        "status='published',visibility='private'",
        "visibility='public',deleted_at=now()",
        "deleted_at=NULL,published_at=now()+interval '1 hour'",
    ] {
        sqlx::query(&format!("UPDATE posts SET {change} WHERE id=$1"))
            .bind(post)
            .execute(&pool)
            .await
            .unwrap();
        assert!(matches!(
            service.public_list("discussion", None, 1).await,
            Err(UseCaseError::NotFound(_))
        ));
        assert!(matches!(
            service
                .submit("discussion", None, None, cmd("Hidden", None))
                .await,
            Err(UseCaseError::NotFound(_))
        ));
    }
    pool.close().await;
}

#[tokio::test]
async fn policy_defaults_post_versions_and_global_merge() {
    let (pool, service, admin, post) = fixture("blog_test_comments_policy").await;
    let root = new_comment(&service, &admin, None).await;
    change(&service, &admin, root.id, 1, CommentStatus::Approved).await;
    let global = service.policy(&admin, None, None, None).await.unwrap();
    assert!(global.enabled);
    assert_eq!(global.version, 0);
    let noop = service
        .policy(&admin, None, Some(global.clone()), None)
        .await
        .unwrap();
    assert_eq!(noop.version, 0);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM settings WHERE key='comments'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    let p = service
        .policy(&admin, Some(post), None, None)
        .await
        .unwrap();
    assert_eq!(p.version, 1);
    let closed = service
        .policy(
            &admin,
            Some(post),
            Some(CommentPolicy {
                enabled: false,
                ..p.clone()
            }),
            None,
        )
        .await
        .unwrap();
    assert_eq!(closed.version, 2);
    assert!(matches!(
        service.policy(&admin, Some(post), Some(p), None).await,
        Err(UseCaseError::VersionConflict)
    ));
    assert_eq!(
        service
            .policy(&admin, Some(post), Some(closed.clone()), None)
            .await
            .unwrap()
            .version,
        2
    );
    let visible = service.public_list("discussion", None, 1).await.unwrap();
    assert!(!visible.enabled);
    assert_eq!(visible.total, 1);
    assert!(matches!(
        service
            .submit("discussion", None, None, cmd("Closed", None))
            .await,
        Err(UseCaseError::Invalid(_))
    ));
    // The article editor's previous expected_version cannot overwrite a policy change.
    assert_eq!(
        sqlx::query("UPDATE posts SET title='Lost' WHERE id=$1 AND version=1")
            .bind(post)
            .execute(&pool)
            .await
            .unwrap()
            .rows_affected(),
        0
    );
    service
        .policy(
            &admin,
            Some(post),
            Some(CommentPolicy {
                enabled: true,
                ..closed
            }),
            None,
        )
        .await
        .unwrap();
    sqlx::query("INSERT INTO settings(key,value) VALUES('comments','{\"ip_retention_days\":30}')")
        .execute(&pool)
        .await
        .unwrap();
    let p = service.policy(&admin, None, None, None).await.unwrap();
    let (a, b) = tokio::join!(
        service.policy(
            &admin,
            None,
            Some(CommentPolicy {
                enabled: false,
                ..p.clone()
            }),
            None
        ),
        service.policy(
            &admin,
            None,
            Some(CommentPolicy {
                enabled: false,
                ..p
            }),
            None
        )
    );
    assert!(a.is_ok() ^ b.is_ok());
    let value: serde_json::Value =
        sqlx::query_scalar("SELECT value FROM settings WHERE key='comments'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(value["ip_retention_days"], 30);
    assert!(
        !service
            .public_list("discussion", None, 1)
            .await
            .unwrap()
            .enabled
    );
    let ordinary = actor(admin.user_id.0, false);
    assert!(matches!(
        service.policy(&ordinary, None, None, None).await,
        Err(UseCaseError::Forbidden)
    ));
    pool.close().await;
}

#[tokio::test]
async fn public_roots_and_flat_descendants_have_independent_pagination() {
    let (pool, service, admin, _) = fixture("blog_test_comments_pages").await;
    let root = new_comment(&service, &admin, None).await;
    change(&service, &admin, root.id, 1, CommentStatus::Approved).await;
    for _ in 0..21 {
        new_comment(&service, &admin, None).await;
        new_comment(&service, &admin, Some(root.id)).await;
    }
    sqlx::query("UPDATE comments SET status='approved'")
        .execute(&pool)
        .await
        .unwrap();
    for (parent, total, last) in [(None, 22, 2), (Some(root.id), 21, 1)] {
        let first = service.public_list("discussion", parent, 1).await.unwrap();
        assert_eq!(first.total, total);
        assert_eq!(first.items.len(), 20);
        assert_eq!(
            service
                .public_list("discussion", parent, 2)
                .await
                .unwrap()
                .items
                .len(),
            last
        );
        let beyond = service.public_list("discussion", parent, 3).await.unwrap();
        assert_eq!(beyond.total, total);
        assert!(beyond.items.is_empty());
    }
    assert!(matches!(
        service.public_list("discussion", None, 0).await,
        Err(UseCaseError::Invalid(_))
    ));
    pool.close().await;
}

#[tokio::test]
async fn audit_failure_rolls_back_create_moderation_and_both_policies() {
    let (pool, service, admin, post) = fixture("blog_test_comments_audit").await;
    let root = new_comment(&service, &admin, None).await;
    sqlx::query("ALTER TABLE audit_logs ADD CONSTRAINT fail_audit CHECK(action='never') NOT VALID")
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        service
            .submit("discussion", None, None, cmd("Rollback", None))
            .await
            .is_err()
    );
    assert!(
        service
            .moderate(
                &admin,
                root.id,
                1,
                ModerationAction::SetStatus(CommentStatus::Approved),
                None
            )
            .await
            .is_err()
    );
    for (target, version) in [(None, 0), (Some(post), 1)] {
        assert!(
            service
                .policy(
                    &admin,
                    target,
                    Some(CommentPolicy {
                        enabled: false,
                        version
                    }),
                    None
                )
                .await
                .is_err()
        );
    }
    let comments = service.list(&admin, None, None, 1).await.unwrap();
    assert_eq!(comments.total, 1);
    assert_eq!(comments.items[0].status, "pending");
    assert_eq!(comments.items[0].version, 1);
    assert_eq!(
        service
            .policy(&admin, None, None, None)
            .await
            .unwrap()
            .version,
        0
    );
    let p = service
        .policy(&admin, Some(post), None, None)
        .await
        .unwrap();
    assert!(p.enabled);
    assert_eq!(p.version, 1);
    pool.close().await;
}

struct RacingRenderer {
    pool: PgPool,
    id: Uuid,
    changed: AtomicBool,
}
#[async_trait::async_trait]
impl CommentRenderer for RacingRenderer {
    async fn render_comment(&self, source: &str) -> Result<String, UseCaseError> {
        if !self.changed.swap(true, Ordering::SeqCst) {
            sqlx::query("UPDATE comments SET content='New source',content_html='<p>New source</p>',content_render_version=$2,version=version+1 WHERE id=$1")
                .bind(self.id).bind(COMMENT_RENDER_VERSION).execute(&self.pool).await.unwrap();
        }
        RenderingRuntime::default().render_comment(source).await
    }
}
#[tokio::test]
async fn rebuild_updates_only_derived_fields_and_does_not_clobber_a_newer_source() {
    let (pool, service, admin, _) = fixture("blog_test_comments_rebuild").await;
    let root = new_comment(&service, &admin, None).await;
    let before: time::OffsetDateTime =
        sqlx::query_scalar("SELECT updated_at FROM comments WHERE id=$1")
            .bind(root.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    sqlx::query("UPDATE comments SET content_html='stale',content_render_version=2 WHERE id=$1")
        .bind(root.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        rebuild_comment_html(&pool, &RenderingRuntime::default())
            .await
            .unwrap(),
        1
    );
    let row = sqlx::query(
        "SELECT content_html,version,updated_at,content_render_version FROM comments WHERE id=$1",
    )
    .bind(root.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.get::<String, _>("content_html"), root.content_html);
    assert_eq!(row.get::<i64, _>("version"), 1);
    assert_eq!(row.get::<time::OffsetDateTime, _>("updated_at"), before);
    assert_eq!(
        row.get::<i32, _>("content_render_version"),
        COMMENT_RENDER_VERSION
    );
    sqlx::query("UPDATE comments SET content_render_version=2 WHERE id=$1")
        .bind(root.id)
        .execute(&pool)
        .await
        .unwrap();
    let rebuilt = rebuild_comment_html(
        &pool,
        &RacingRenderer {
            pool: pool.clone(),
            id: root.id,
            changed: AtomicBool::new(false),
        },
    )
    .await
    .unwrap();
    assert_eq!(rebuilt, 0, "并发编辑使重建失效时不计入成功数");
    let latest = service
        .list(&admin, None, None, 1)
        .await
        .unwrap()
        .items
        .remove(0);
    assert_eq!(latest.body, "New source");
    assert_eq!(latest.content_html, "<p>New source</p>");
    assert_eq!(latest.version, 2);
    pool.close().await;
}
