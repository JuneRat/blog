mod common;
use application::{
    comments::*,
    error::UseCaseError,
    identity::{Actor, ActorChannel},
};
use domain::identity::{PermissionSet, UserId};
use infrastructure::comments::PostgresCommentRepository;
use std::sync::Arc;
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
fn cmd(body: &str, parent: Option<Uuid>) -> SubmitComment {
    SubmitComment {
        nickname: "<img src=x onerror=alert(1)>".into(),
        body: body.into(),
        parent_id: parent,
        request_id: Uuid::now_v7(),
    }
}
#[tokio::test]
async fn comments_visibility_authorization_concurrency_and_lifecycle() {
    let pool = common::fresh_database("blog_test_comments").await;
    let owner = common::seed_user(&pool, "comments_owner").await;
    let outsider = common::seed_user(&pool, "comments_other").await;
    let author = actor(owner, false);
    let other = actor(outsider, false);
    let admin = actor(outsider, true);
    let post = Uuid::now_v7();
    sqlx::query("INSERT INTO posts(id,author_id,slug,title,status,published_at) VALUES($1,$2,'discussion','Discussion','published',now())").bind(post).bind(owner).execute(&pool).await.unwrap();
    let service = Arc::new(CommentInteractor::new(Arc::new(
        PostgresCommentRepository::new(pool.clone()),
    )));
    let request = cmd("第一条\n<script>alert(1)</script>", None);
    // Concurrent retries yield one pending record and no post version changes.
    let (a, b) = tokio::join!(
        service.submit("discussion", None, "client1", request.clone()),
        service.submit("discussion", None, "client1", request.clone())
    );
    a.unwrap();
    b.unwrap();
    assert_eq!(
        service
            .public_list("discussion", None, 1)
            .await
            .unwrap()
            .total,
        0
    );
    let pending = service
        .list(&author, Some("pending"), None, 1)
        .await
        .unwrap();
    assert_eq!(pending.total, 1);
    let root = pending.items[0].clone();
    assert!(!root.is_author);
    assert_eq!(service.list(&other, None, None, 1).await.unwrap().total, 0);
    assert!(matches!(
        service.moderate(&other, root.id, 1, Some("approved")).await,
        Err(UseCaseError::Forbidden)
    ));
    assert!(matches!(
        service.policy(&other, Some(post), None).await,
        Err(UseCaseError::Forbidden)
    ));
    assert!(
        service
            .submit(
                "discussion",
                None,
                "client2",
                cmd("pending parent", Some(root.id))
            )
            .await
            .is_err()
    );
    service
        .moderate(&author, root.id, 1, Some("approved"))
        .await
        .unwrap();
    assert!(matches!(
        service.moderate(&author, root.id, 1, Some("spam")).await,
        Err(UseCaseError::VersionConflict)
    ));
    assert_eq!(
        service
            .public_list("discussion", None, 1)
            .await
            .unwrap()
            .total,
        1
    );
    // Server-confirmed identity, independent of the supplied nickname.
    service
        .submit(
            "discussion",
            Some(&author),
            "client3",
            cmd("作者回复", Some(root.id)),
        )
        .await
        .unwrap();
    let reply = service
        .list(&author, Some("pending"), None, 1)
        .await
        .unwrap()
        .items
        .remove(0);
    assert!(reply.is_author);
    assert_eq!(reply.nickname, "comments_owner的展示名");
    service
        .moderate(&admin, reply.id, 1, Some("approved"))
        .await
        .unwrap();
    assert_eq!(
        service
            .public_list("discussion", Some(root.id), 1)
            .await
            .unwrap()
            .total,
        1
    );
    assert!(
        service
            .submit(
                "discussion",
                None,
                "client4",
                cmd("third level", Some(reply.id))
            )
            .await
            .is_err()
    );
    // Same-post invariant, including the database FK/trigger boundary.
    let second = Uuid::now_v7();
    sqlx::query("INSERT INTO posts(id,author_id,slug,status,published_at) VALUES($1,$2,'other','published',now())").bind(second).bind(owner).execute(&pool).await.unwrap();
    assert!(
        service
            .submit("other", None, "client4", cmd("cross post", Some(root.id)))
            .await
            .is_err()
    );
    service
        .moderate(&author, root.id, 2, Some("rejected"))
        .await
        .unwrap();
    assert_eq!(
        service
            .public_list("discussion", None, 1)
            .await
            .unwrap()
            .total,
        0
    );
    assert!(matches!(
        service.public_list("discussion", Some(root.id), 1).await,
        Err(UseCaseError::NotFound(_))
    ));
    service
        .moderate(&author, root.id, 3, Some("approved"))
        .await
        .unwrap();
    // Own policy has its own CAS, keeping historical comments readable.
    let policy = service.policy(&author, Some(post), None).await.unwrap();
    service
        .policy(
            &author,
            Some(post),
            Some(CommentPolicy {
                enabled: false,
                ..policy.clone()
            }),
        )
        .await
        .unwrap();
    assert!(matches!(
        service.policy(&author, Some(post), Some(policy)).await,
        Err(UseCaseError::VersionConflict)
    ));
    let public = service.public_list("discussion", None, 1).await.unwrap();
    assert!(!public.enabled);
    assert_eq!(public.total, 1);
    assert!(
        service
            .submit("discussion", None, "client4", cmd("closed", None))
            .await
            .is_err()
    );
    let policy = service.policy(&author, Some(post), None).await.unwrap();
    service
        .policy(
            &author,
            Some(post),
            Some(CommentPolicy {
                enabled: true,
                ..policy
            }),
        )
        .await
        .unwrap();
    let global = service.policy(&admin, None, None).await.unwrap();
    service
        .policy(
            &admin,
            None,
            Some(CommentPolicy {
                enabled: false,
                ..global
            }),
        )
        .await
        .unwrap();
    assert!(
        !service
            .public_list("discussion", None, 1)
            .await
            .unwrap()
            .enabled
    );
    assert!(
        service
            .submit("discussion", None, "client4", cmd("global closed", None))
            .await
            .is_err()
    );
    let global = service.policy(&admin, None, None).await.unwrap();
    service
        .policy(
            &admin,
            None,
            Some(CommentPolicy {
                enabled: true,
                ..global
            }),
        )
        .await
        .unwrap();
    // One client cannot evade rate/duplicate checks by generating fresh request IDs.
    service
        .submit("discussion", None, "limited", cmd("one", None))
        .await
        .unwrap();
    assert!(
        service
            .submit("discussion", None, "limited", cmd("one", None))
            .await
            .is_err()
    );
    let (a, b, c) = tokio::join!(
        service.submit("discussion", None, "limited", cmd("two", None)),
        service.submit("discussion", None, "limited", cmd("three", None)),
        service.submit("discussion", None, "limited", cmd("four", None))
    );
    assert_eq!(
        [a.is_ok(), b.is_ok(), c.is_ok()]
            .iter()
            .filter(|x| **x)
            .count(),
        2
    );
    // All three forms of withdrawal hide both roots and replies, and reject retries.
    for change in [
        "status='draft'",
        "status='published',visibility='private'",
        "visibility='public',deleted_at=now()",
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
            service.public_list("discussion", Some(root.id), 1).await,
            Err(UseCaseError::NotFound(_))
        ));
        assert!(matches!(
            service
                .submit("discussion", None, "client1", request.clone())
                .await,
            Err(UseCaseError::NotFound(_))
        ));
    }
    let version: i64 = sqlx::query_scalar("SELECT version FROM posts WHERE id=$1")
        .bind(post)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(version, 1);
    sqlx::query("UPDATE posts SET deleted_at=NULL WHERE id=$1")
        .bind(post)
        .execute(&pool)
        .await
        .unwrap();
    // Both root and reply pages are bounded, and count only approved rows.
    for i in 0..22 {
        service
            .submit(
                "discussion",
                None,
                &format!("page{i}"),
                cmd(&format!("page{i}"), None),
            )
            .await
            .unwrap();
    }
    sqlx::query("UPDATE comments SET status='approved' WHERE post_id=$1")
        .bind(post)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        service
            .public_list("discussion", None, 1)
            .await
            .unwrap()
            .items
            .len(),
        20
    );
    assert!(
        !service
            .public_list("discussion", None, 2)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    for i in 0..22 {
        service
            .submit(
                "discussion",
                None,
                &format!("replypage{i}"),
                cmd(&format!("replypage{i}"), Some(root.id)),
            )
            .await
            .unwrap();
    }
    sqlx::query("UPDATE comments SET status='approved' WHERE post_id=$1")
        .bind(post)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        service
            .public_list("discussion", Some(root.id), 1)
            .await
            .unwrap()
            .items
            .len(),
        20
    );
    assert_eq!(
        service
            .public_list("discussion", Some(root.id), 2)
            .await
            .unwrap()
            .items
            .len(),
        3
    );
    let version: i64 = sqlx::query_scalar("SELECT version FROM comments WHERE id=$1")
        .bind(root.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    service
        .moderate(&author, root.id, version, None)
        .await
        .unwrap();
    let replies: i64 = sqlx::query_scalar("SELECT count(*) FROM comments WHERE parent_id=$1")
        .bind(root.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(replies, 0);
    sqlx::query("DELETE FROM posts WHERE id=$1")
        .bind(post)
        .execute(&pool)
        .await
        .unwrap();
    let remaining: i64 = sqlx::query_scalar("SELECT count(*) FROM comments WHERE post_id=$1")
        .bind(post)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(remaining, 0);
}
