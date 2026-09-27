//! 新基线的内容关系、预约与回收站回归；仅使用独立测试库。
mod common;

use application::ports::{
    PageCommitOutcome, PageRepository, PostCommitOutcome, PostRepository, PublishedPageQuery,
    PublishedPostQuery, PublishedSeriesQuery, SeriesRepository, TagRepository,
};
use domain::content::{
    Page, Post, PostDraftMetadata, PostPatch, PostStatus, Series, SeriesPlacement, Slug, Tag,
    Visibility,
};
use domain::identity::UserId;
use infrastructure::{
    PostgresPageRepository, PostgresPostRepository, PostgresPublishedPageQuery,
    PostgresPublishedPostQuery, PostgresPublishedSeriesQuery, PostgresSeriesRepository,
    PostgresTagRepository, RenderingRuntime,
};
use std::sync::Arc;
use time::{Duration, OffsetDateTime};

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn multiple_series_allow_tied_weights_and_directory_deletion_keeps_posts() {
    let _guard = SERIAL.lock().await;
    let pool = common::fresh_database("blog_content_lifecycle_test").await;
    let author = common::seed_user(&pool, "author").await;
    let posts = PostgresPostRepository::new(pool.clone(), Arc::new(RenderingRuntime::default()));
    let series = PostgresSeriesRepository::new(pool.clone());
    let tags = PostgresTagRepository::new(pool.clone());
    let now = OffsetDateTime::now_utc();
    let a = Series::new("甲".into(), Slug::new("a").unwrap(), None, now).unwrap();
    let b = Series::new("乙".into(), Slug::new("b").unwrap(), None, now).unwrap();
    series.insert(&a, Some(author).into()).await.unwrap();
    series.insert(&b, Some(author).into()).await.unwrap();
    let tag = Tag::new("标签".into(), Slug::new("tag").unwrap(), now).unwrap();
    tags.insert(&tag, Some(author).into()).await.unwrap();
    let mut ids = Vec::new();
    for slug in ["one", "two"] {
        let mut post = Post::create_draft_with_metadata(
            UserId(author),
            Slug::new(slug).unwrap(),
            slug.into(),
            None,
            "正文".into(),
            Visibility::Public,
            PostDraftMetadata {
                series: vec![
                    SeriesPlacement::new(a.id(), 0).unwrap(),
                    SeriesPlacement::new(b.id(), 7).unwrap(),
                ],
                ..Default::default()
            },
            now,
        )
        .unwrap();
        post.publish(now).unwrap();
        let record = posts
            .insert_post(&post, &[tag.id()], Some(author).into())
            .await
            .unwrap();
        assert_eq!(record.snapshot.series.len(), 2);
        ids.push(record.snapshot.id);
    }
    let public = PostgresPublishedPostQuery::new(pool.clone());
    assert_eq!(
        public
            .find_public_by_slug("one")
            .await
            .unwrap()
            .unwrap()
            .series
            .len(),
        2
    );
    let (ordered, total) = PostgresPublishedSeriesQuery::new(pool.clone())
        .list_public_posts_by_series("a", 20, 0)
        .await
        .unwrap();
    assert_eq!(total, 2);
    assert_eq!(
        ordered.iter().map(|p| p.slug.as_str()).collect::<Vec<_>>(),
        vec!["one", "two"]
    );
    let previous = posts.find_record_by_id(ids[0]).await.unwrap().unwrap();
    let mut stale = Post::reconstitute(previous.snapshot.clone()).unwrap();
    stale
        .edit(PostPatch {
            title: Some("过期保存".into()),
            ..Default::default()
        })
        .unwrap();
    let version = series.find_by_slug("a").await.unwrap().unwrap().version;
    series
        .reorder(a.id(), version, &[ids[1], ids[0]], Some(author).into())
        .await
        .unwrap();
    assert_eq!(
        posts
            .commit_post(
                &stale,
                previous.snapshot.version,
                now,
                None,
                Some(author).into()
            )
            .await
            .unwrap(),
        PostCommitOutcome::StaleConflict
    );
    // B 的权重不受 A 重排影响。
    assert!(
        series
            .members_of(b.id())
            .await
            .unwrap()
            .iter()
            .all(|m| m.position == 7)
    );
    let before = posts.find_record_by_id(ids[0]).await.unwrap().unwrap();
    tags.delete(tag.id(), 1, Some(author).into()).await.unwrap();
    let after = posts.find_record_by_id(ids[0]).await.unwrap().unwrap();
    assert_eq!(after.snapshot.version, before.snapshot.version + 1);
    assert!(after.tag_ids.is_empty());
    let version = series.find_by_slug("a").await.unwrap().unwrap().version;
    series
        .delete(a.id(), version, Some(author).into())
        .await
        .unwrap();
    let record = posts.find_record_by_id(ids[0]).await.unwrap().unwrap();
    assert_eq!(
        record.snapshot.series,
        vec![SeriesPlacement::new(b.id(), 7).unwrap()]
    );
    assert_eq!(record.snapshot.version, after.snapshot.version + 1);
    assert_eq!(posts.list_by_author(author).await.unwrap().len(), 2);
    let missing_actor: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_logs WHERE actor_id IS DISTINCT FROM $1")
            .bind(author)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(missing_actor, 0, "业务审计记录实际操作人");
}

#[tokio::test]
async fn due_publishing_is_atomic_idempotent_and_does_not_revive_cancelled_or_trashed_content() {
    let _guard = SERIAL.lock().await;
    let pool = common::fresh_database("blog_content_lifecycle_test").await;
    let author = common::seed_user(&pool, "author").await;
    let posts = PostgresPostRepository::new(pool.clone(), Arc::new(RenderingRuntime::default()));
    let pages = PostgresPageRepository::new(pool.clone(), Arc::new(RenderingRuntime::default()));
    let public_posts = PostgresPublishedPostQuery::new(pool.clone());
    let public_pages = PostgresPublishedPageQuery::new(pool.clone());
    let now = OffsetDateTime::now_utc() - Duration::hours(2);
    let at = now + Duration::hours(1);
    let mut scheduled_ids = Vec::new();
    for slug in ["due", "cancelled", "trashed"] {
        let mut post = Post::create_draft(
            UserId(author),
            Slug::new(slug).unwrap(),
            slug.into(),
            None,
            "正文".into(),
            Visibility::Public,
            now,
        )
        .unwrap();
        posts
            .insert_post(&post, &[], Some(author).into())
            .await
            .unwrap();
        post.schedule(at, now).unwrap();
        let PostCommitOutcome::Saved(record) = posts
            .commit_post(&post, 1, now, None, Some(author).into())
            .await
            .unwrap()
        else {
            panic!()
        };
        let mut post = Post::reconstitute(record.snapshot.clone()).unwrap();
        if slug == "cancelled" {
            post.withdraw();
            posts
                .commit_post(&post, 2, now, None, Some(author).into())
                .await
                .unwrap();
        }
        if slug == "trashed" {
            post.trash(now);
            posts
                .commit_lifecycle(&post, 2, now, Some(author).into())
                .await
                .unwrap();
        }
        scheduled_ids.push(record.snapshot.id);
        assert!(
            public_posts
                .find_public_by_slug(slug)
                .await
                .unwrap()
                .is_none()
        );
    }
    let mut page = Page::create_draft(
        Slug::new("about").unwrap(),
        "关于".into(),
        "正文".into(),
        Visibility::Public,
        now,
    )
    .unwrap();
    pages.insert_page(&page, Some(author).into()).await.unwrap();
    page.schedule(at, now).unwrap();
    pages
        .commit_page(&page, 1, now, Some(author).into())
        .await
        .unwrap();
    assert!(
        public_pages
            .find_public_by_slug("about")
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        infrastructure::publish_due_content(&pool, at - Duration::seconds(1), 100)
            .await
            .unwrap(),
        0
    );
    let (left, right) = tokio::join!(
        infrastructure::publish_due_content(&pool, at, 100),
        infrastructure::publish_due_content(&pool, at, 100)
    );
    assert_eq!(left.unwrap() + right.unwrap(), 2);
    assert_eq!(
        infrastructure::publish_due_content(&pool, at, 100)
            .await
            .unwrap(),
        0
    );
    let due = posts.find_by_id(scheduled_ids[0]).await.unwrap().unwrap();
    assert_eq!(due.status, PostStatus::Published);
    assert_eq!(due.version, 3);
    assert_eq!(due.published_at, Some(at));
    assert!(
        public_posts
            .find_public_by_slug("due")
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        public_posts
            .find_public_by_slug("cancelled")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        public_posts
            .find_public_by_slug("trashed")
            .await
            .unwrap()
            .is_none()
    );
    let mut page =
        Page::reconstitute(pages.find_by_id(page.id().0).await.unwrap().unwrap()).unwrap();
    page.archive().unwrap();
    let PageCommitOutcome::Saved(snapshot) = pages
        .commit_page(&page, 3, at, Some(author).into())
        .await
        .unwrap()
    else {
        panic!()
    };
    page = Page::reconstitute(snapshot).unwrap();
    page.trash(at);
    pages
        .commit_lifecycle(&page, 4, at, Some(author).into())
        .await
        .unwrap();
    assert!(pages.list().await.unwrap().is_empty());
    assert_eq!(pages.list_trash(20, 0).await.unwrap().1, 1);
    page.restore();
    let PageCommitOutcome::Saved(restored) = pages
        .commit_lifecycle(&page, 5, at, Some(author).into())
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(restored.status, domain::content::PageStatus::Draft);
    assert_eq!(restored.published_at, Some(at));
    assert!(
        public_pages
            .find_public_by_slug("about")
            .await
            .unwrap()
            .is_none()
    );
    let audit_count:i64=sqlx::query_scalar("SELECT count(*) FROM audit_logs WHERE action IN ('post.publish_due','page.publish_due') AND actor_id IS NULL").fetch_one(&pool).await.unwrap();
    assert_eq!(audit_count, 2);
}

#[tokio::test]
async fn purge_deletes_entire_comment_tree_and_audit_failure_rolls_back_content() {
    let _guard = SERIAL.lock().await;
    let pool = common::fresh_database("blog_content_lifecycle_test").await;
    let author = common::seed_user(&pool, "author").await;
    let repo = PostgresPostRepository::new(pool.clone(), Arc::new(RenderingRuntime::default()));
    let now = OffsetDateTime::now_utc();
    let post = Post::create_draft(
        UserId(author),
        Slug::new("with-comments").unwrap(),
        "标题".into(),
        None,
        "正文".into(),
        Visibility::Public,
        now,
    )
    .unwrap();
    let id = post.id().0;
    repo.insert_post(&post, &[], Some(author).into())
        .await
        .unwrap();
    let root = uuid::Uuid::now_v7();
    let reply = uuid::Uuid::now_v7();
    let leaf = uuid::Uuid::now_v7();
    for (id, parent, root_id) in [
        (root, None, None),
        (reply, Some(root), Some(root)),
        (leaf, Some(reply), Some(root)),
    ] {
        sqlx::query("INSERT INTO comments (id,post_id,parent_id,root_id,author_name,content,content_html,content_render_version) VALUES ($1,$2,$3,$4,'访客','评论','<p>评论</p>',1)")
            .bind(id).bind(post.id().0).bind(parent).bind(root_id).execute(&pool).await.unwrap();
    }
    sqlx::raw_sql("CREATE FUNCTION reject_content_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='post.update' THEN RAISE EXCEPTION 'audit unavailable'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_content_audit BEFORE INSERT ON audit_logs FOR EACH ROW EXECUTE FUNCTION reject_content_audit();").execute(&pool).await.unwrap();
    let mut changed = post.clone();
    changed
        .edit(PostPatch {
            title: Some("不应保存".into()),
            ..Default::default()
        })
        .unwrap();
    assert!(
        repo.commit_post(&changed, 1, now, None, Some(author).into())
            .await
            .is_err()
    );
    let unchanged = repo.find_record_by_id(id).await.unwrap().unwrap();
    assert_eq!(unchanged.snapshot, post.snapshot());
    assert!(matches!(
        repo.purge(id, 1, Some(author).into()).await.unwrap(),
        application::ports::SaveOutcome::Gone
    ));
    let mut trash = post;
    trash.trash(now);
    repo.commit_lifecycle(&trash, 1, now, Some(author).into())
        .await
        .unwrap();
    assert!(matches!(
        repo.purge(id, 2, Some(author).into()).await.unwrap(),
        application::ports::SaveOutcome::Saved { .. }
    ));
    assert!(repo.find_by_id(id).await.unwrap().is_none());
    let remaining: i64 = sqlx::query_scalar("SELECT count(*) FROM comments WHERE post_id=$1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(remaining, 0);
}
