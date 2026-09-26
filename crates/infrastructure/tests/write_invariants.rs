mod common;

use std::sync::Arc;

use application::error::UseCaseError;
use application::ports::{MediaRepository, PageRepository, PostRepository};
use domain::content::{Page, PagePatch, Post, PostPatch, Slug, Visibility};
use domain::identity::UserId;
use domain::media::{ImageFormat, ImageInfo, Media, MediaStatus, MediaTransition};
use infrastructure::{PostgresMediaRepository, PostgresPageRepository, PostgresPostRepository};
use time::OffsetDateTime;
use uuid::Uuid;

#[tokio::test]
async fn body_budget_rejects_create_edit_and_publish_before_database_changes() {
    let pool = common::fresh_database("blog_test_write_budget").await;
    let author = UserId(common::seed_user(&pool, "budget_author").await);
    let runtime = Arc::new(infrastructure::rendering::RenderingRuntime::default());
    let posts = PostgresPostRepository::new(pool.clone(), runtime.clone());
    let pages = PostgresPageRepository::new(pool.clone(), runtime);
    let now = OffsetDateTime::now_utc();
    // Source fits, but entity escaping makes the HTML larger than the budget.
    let expanded = format!("{}& ", "a".repeat(1024)).repeat(765);
    let draft = |slug: &str, content: String| {
        Post::create_draft(
            author,
            Slug::new(slug).unwrap(),
            "Title".into(),
            None,
            content,
            Visibility::Public,
            now,
        )
        .unwrap()
    };
    assert!(
        Post::create_draft(
            author,
            Slug::new("large").unwrap(),
            "Title".into(),
            None,
            "x".repeat(1_100_000),
            Visibility::Public,
            now
        )
        .is_err()
    );
    let mut post = draft("post-budget", "small".into());
    post.publish(now).unwrap();
    let stored_post = posts.insert_post(&post, &[]).await.unwrap();
    let mut page = Page::create_draft(
        Slug::new("about-budget").unwrap(),
        "Title".into(),
        "small".into(),
        Visibility::Public,
        now,
    )
    .unwrap();
    page.publish(now).unwrap();
    let stored_page = pages.insert_page(&page).await.unwrap();

    let mut invalid_post = draft("invalid-budget", expanded.clone());
    assert!(matches!(
        posts.insert_post(&invalid_post, &[]).await,
        Err(UseCaseError::Invalid(_))
    ));
    let mut invalid_page = Page::create_draft(
        Slug::new("invalid-page").unwrap(),
        "Title".into(),
        expanded.clone(),
        Visibility::Public,
        now,
    )
    .unwrap();
    assert!(matches!(
        pages.insert_page(&invalid_page).await,
        Err(UseCaseError::Invalid(_))
    ));

    post.edit(PostPatch {
        content: Some(expanded.clone()),
        ..Default::default()
    })
    .unwrap();
    page.edit(PagePatch {
        content: Some(expanded.clone()),
        ..Default::default()
    })
    .unwrap();
    assert!(matches!(
        posts.commit_post(&post, 1, now, None).await,
        Err(UseCaseError::Invalid(_))
    ));
    assert!(matches!(
        pages.commit_page(&page, 1, now).await,
        Err(UseCaseError::Invalid(_))
    ));
    assert_eq!(
        posts.find_by_id(post.id().0).await.unwrap().unwrap(),
        stored_post.snapshot
    );
    assert_eq!(
        pages.find_by_id(page.id().0).await.unwrap().unwrap(),
        stored_page
    );

    // Simulate a draft written before the budget existed. Publishing it must
    // fail without changing status, version, persisted HTML or references.
    sqlx::query("INSERT INTO posts(id,author_id,slug,title,content) VALUES($1,$2,$3,'Title',$4)")
        .bind(invalid_post.id().0)
        .bind(author.0)
        .bind(invalid_post.slug())
        .bind(&expanded)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO pages(id,slug,title,content) VALUES($1,$2,'Title',$3)")
        .bind(invalid_page.id().0)
        .bind(invalid_page.slug())
        .bind(&expanded)
        .execute(&pool)
        .await
        .unwrap();
    let before_post = posts
        .find_by_id(invalid_post.id().0)
        .await
        .unwrap()
        .unwrap();
    let before_page = pages
        .find_by_id(invalid_page.id().0)
        .await
        .unwrap()
        .unwrap();
    invalid_post.publish(now).unwrap();
    invalid_page.publish(now).unwrap();
    assert!(matches!(
        posts.commit_post(&invalid_post, 1, now, None).await,
        Err(UseCaseError::Invalid(_))
    ));
    assert!(matches!(
        pages.commit_page(&invalid_page, 1, now).await,
        Err(UseCaseError::Invalid(_))
    ));
    assert_eq!(
        posts
            .find_by_id(invalid_post.id().0)
            .await
            .unwrap()
            .unwrap(),
        before_post
    );
    assert_eq!(
        pages
            .find_by_id(invalid_page.id().0)
            .await
            .unwrap()
            .unwrap(),
        before_page
    );
    let refs: i64 = sqlx::query_scalar("SELECT count(*) FROM content_media_refs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(refs, 0);
    pool.close().await;
}

#[tokio::test]
async fn media_transition_matrix_matches_aggregate_and_conditional_writes() {
    let pool = common::fresh_database("blog_test_media_transitions").await;
    let owner = common::seed_user(&pool, "media_rules").await;
    let repo = PostgresMediaRepository::new(pool.clone());
    let now = OffsetDateTime::UNIX_EPOCH;
    for action in [
        MediaTransition::Ready,
        MediaTransition::RequestDeletion,
        MediaTransition::ConfirmDeleted,
        MediaTransition::AbandonStaged,
    ] {
        for status in [
            MediaStatus::Staged,
            MediaStatus::Ready,
            MediaStatus::PendingDeletion,
            MediaStatus::Deleted,
        ] {
            let id = Uuid::now_v7();
            let media = Media::stage(
                id,
                owner,
                format!("objects/{id}.png"),
                "test.png",
                ImageInfo::new(ImageFormat::Png, 1, 1).unwrap(),
                1,
                "a".repeat(64),
                now,
            )
            .unwrap();
            repo.insert_staged(&media.snapshot()).await.unwrap();
            sqlx::query("UPDATE media_assets SET status=$2 WHERE id=$1")
                .bind(id)
                .bind(status.as_str())
                .execute(&pool)
                .await
                .unwrap();
            let mut aggregate =
                Media::reconstitute(repo.find_by_id(id).await.unwrap().unwrap()).unwrap();
            let result = match action {
                MediaTransition::Ready => aggregate.mark_ready(now),
                MediaTransition::RequestDeletion => aggregate.mark_pending_deletion(now),
                MediaTransition::ConfirmDeleted => aggregate.mark_deleted(now),
                MediaTransition::AbandonStaged => aggregate.abandon_staged(now),
            };
            let expected = action.apply(status);
            assert_eq!(result.is_ok(), expected.is_ok());
            match action {
                MediaTransition::Ready => {
                    repo.mark_ready(id, now).await.unwrap();
                }
                MediaTransition::RequestDeletion => {
                    repo.begin_delete(id, 1, now).await.unwrap();
                }
                MediaTransition::ConfirmDeleted => {
                    repo.confirm_deleted(id, now).await.unwrap();
                }
                MediaTransition::AbandonStaged => {
                    repo.claim_abandoned_staged(now + time::Duration::seconds(1), now, 100)
                        .await
                        .unwrap();
                }
            }
            let stored = repo.find_by_id(id).await.unwrap().unwrap();
            assert_eq!(
                stored.status,
                expected.unwrap_or(status),
                "{status:?} {action:?}"
            );
            assert_eq!(stored, aggregate.snapshot(), "{status:?} {action:?}");
            // Keep each matrix case independent from the batch reclaimer.
            sqlx::query("DELETE FROM media_assets WHERE id=$1")
                .bind(id)
                .execute(&pool)
                .await
                .unwrap();
        }
    }
    pool.close().await;
}
