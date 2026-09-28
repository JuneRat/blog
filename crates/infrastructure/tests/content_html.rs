//! 持久化渲染结果：源文、HTML、媒体关系原子提交；显式重建与并发编辑不相互覆盖。
use std::sync::Arc;

use application::error::UseCaseError;
use application::ports::{
    ContentRenderer, PageCommitOutcome, PageRepository, PostCommitOutcome, PostRepository,
    PublishedPageQuery, PublishedPostQuery, RenderedContent,
};
use async_trait::async_trait;
use domain::content::{Page, PagePatch};
use domain::content::{Post, PostPatch, Slug, Visibility};
use domain::identity::UserId;
use infrastructure::persistence::{CONTENT_RENDER_VERSION, rebuild_content_html};
use infrastructure::{
    PostgresPageRepository, PostgresPostRepository, PostgresPublishedPageQuery,
    PostgresPublishedPostQuery, RenderingRuntime, SanitizingMarkdownRenderer,
};
use sqlx::PgPool;
use time::OffsetDateTime;
use tokio::sync::{Mutex, Notify};
use uuid::Uuid;

mod common;
static SERIAL: Mutex<()> = Mutex::const_new(());

async fn database() -> PgPool {
    common::fresh_database("blog_content_html_test").await
}

fn draft(author: Uuid, source: &str) -> Post {
    Post::create_draft(
        UserId(author),
        Slug::new("rendered-post").unwrap(),
        "标题".into(),
        None,
        source.into(),
        Visibility::Public,
        OffsetDateTime::now_utc(),
    )
    .unwrap()
}

async fn stored(
    pool: &PgPool,
    table: &str,
    id: Uuid,
) -> (String, String, i32, i64, OffsetDateTime) {
    assert!(matches!(table, "posts" | "pages"));
    sqlx::query_as(&format!("SELECT content, content_html, content_render_version, version, updated_at FROM {table} WHERE id=$1"))
        .bind(id).fetch_one(pool).await.unwrap()
}

#[tokio::test]
async fn post_and_page_commit_sanitized_html_and_public_reads_use_it() {
    let _guard = SERIAL.lock().await;
    let pool = database().await;
    let author = common::seed_user(&pool, "writer").await;
    let runtime = Arc::new(RenderingRuntime::default());
    let posts = PostgresPostRepository::new(pool.clone(), runtime.clone());
    let pages = PostgresPageRepository::new(pool.clone(), runtime);
    let source = "# 标题\n\n<script>alert(1)</script>\n\n**正文** [bad](javascript:alert(1))";
    let expected = SanitizingMarkdownRenderer::new().render_markdown(source);
    assert!(!expected.contains("<script"));
    assert!(!expected.contains("javascript:"));
    let mut post = draft(author, source);
    post.publish(OffsetDateTime::now_utc()).unwrap();
    let record = posts.insert_post(&post, &[], None.into()).await.unwrap();
    let post_id = record.snapshot.id;
    let mut page = Page::create_draft(
        Slug::new("rendered-page").unwrap(),
        "页面".into(),
        source.into(),
        Visibility::Public,
        OffsetDateTime::now_utc(),
    )
    .unwrap();
    page.publish(OffsetDateTime::now_utc()).unwrap();
    let page_record = pages.insert_page(&page, None.into()).await.unwrap();
    let page_id = page_record.id;
    for (table, id) in [("posts", post_id), ("pages", page_id)] {
        let (raw, html, render_version, version, _) = stored(&pool, table, id).await;
        assert_eq!(raw, source);
        assert_eq!(html, expected);
        assert_eq!(render_version, CONTENT_RENDER_VERSION);
        assert_eq!(version, 1);
    }
    assert_eq!(
        PostgresPublishedPostQuery::new(pool.clone())
            .find_public_by_slug("rendered-post")
            .await
            .unwrap()
            .unwrap()
            .content_html,
        expected
    );
    assert_eq!(
        PostgresPublishedPageQuery::new(pool.clone())
            .find_public_by_slug("rendered-page")
            .await
            .unwrap()
            .unwrap()
            .content_html,
        expected
    );

    post.edit(PostPatch {
        content: Some("更新 **文章**".into()),
        ..Default::default()
    })
    .unwrap();
    assert!(matches!(
        posts
            .commit_post(&post, 1, OffsetDateTime::now_utc(), None, None.into())
            .await
            .unwrap(),
        PostCommitOutcome::Saved(_)
    ));
    page.edit(PagePatch {
        content: Some("更新 **页面**".into()),
        ..Default::default()
    })
    .unwrap();
    assert!(matches!(
        pages
            .commit_page(&page, 1, OffsetDateTime::now_utc(), None.into())
            .await
            .unwrap(),
        PageCommitOutcome::Saved(_)
    ));
    for (table, id) in [("posts", post_id), ("pages", page_id)] {
        let (raw, html, _, version, _) = stored(&pool, table, id).await;
        assert_eq!(
            html,
            SanitizingMarkdownRenderer::new().render_markdown(&raw)
        );
        assert_eq!(version, 2);
    }
    // 读取只信已保存的派生物；不会在公开查询中再次转换 Markdown。
    sqlx::query("UPDATE posts SET content = '未提交的错误源文' WHERE id=$1")
        .bind(post_id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        PostgresPublishedPostQuery::new(pool.clone())
            .find_public_by_slug("rendered-post")
            .await
            .unwrap()
            .unwrap()
            .content_html,
        "<p>更新 <strong>文章</strong></p>\n"
    );
}

#[tokio::test]
async fn media_failure_and_stale_version_leave_source_html_and_refs_unchanged() {
    let _guard = SERIAL.lock().await;
    let pool = database().await;
    let author = common::seed_user(&pool, "writer").await;
    let media_id = Uuid::now_v7();
    sqlx::query("INSERT INTO media(id,uploaded_by,path,filename,mime_type,size,width,height,checksum_sha256) VALUES($1,$2,$3,'image.png','image/png',1,1,1,$4)")
        .bind(media_id).bind(author).bind(media_id.to_string()).bind("a".repeat(64))
        .execute(&pool).await.unwrap();
    let repo = PostgresPostRepository::new(pool.clone(), Arc::new(RenderingRuntime::default()));
    let source = format!(
        "![图](/media/{media_id})\n<!-- <img src='/media/{}'> -->",
        Uuid::now_v7()
    );
    let record = repo
        .insert_post(&draft(author, &source), &[], None.into())
        .await
        .unwrap();
    let id = record.snapshot.id;
    let before = stored(&pool, "posts", id).await;
    let mut post = Post::reconstitute(record.snapshot).unwrap();
    post.edit(PostPatch {
        content: Some(format!("![不存在](/media/{})", Uuid::now_v7())),
        ..Default::default()
    })
    .unwrap();
    assert!(
        repo.commit_post(&post, 1, OffsetDateTime::now_utc(), None, None.into())
            .await
            .is_err()
    );
    assert_eq!(stored(&pool, "posts", id).await, before);
    post.edit(PostPatch {
        content: Some("过期保存".into()),
        ..Default::default()
    })
    .unwrap();
    assert!(matches!(
        repo.commit_post(&post, 0, OffsetDateTime::now_utc(), None, None.into())
            .await
            .unwrap(),
        PostCommitOutcome::StaleConflict
    ));
    assert_eq!(stored(&pool, "posts", id).await, before);
    let references: Vec<Uuid> = sqlx::query_scalar(
        "SELECT media_id FROM media_refs WHERE source_type='post' AND source_id=$1 ORDER BY media_id",
    )
    .bind(id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(references, vec![media_id]);
}

#[tokio::test]
async fn schema_migration_leaves_html_for_explicit_rebuild_without_editing_business_versions() {
    let _guard = SERIAL.lock().await;
    let pool = database().await;
    let author = common::seed_user(&pool, "writer").await;
    let posts = PostgresPostRepository::new(pool.clone(), Arc::new(RenderingRuntime::default()));
    let record = posts
        .insert_post(&draft(author, "# 既有文章"), &[], None.into())
        .await
        .unwrap();
    let pages = PostgresPageRepository::new(pool.clone(), Arc::new(RenderingRuntime::default()));
    let page = Page::create_draft(
        Slug::new("old-page").unwrap(),
        "既有页面".into(),
        "**旧正文**".into(),
        Visibility::Public,
        OffsetDateTime::now_utc(),
    )
    .unwrap();
    let page_record = pages.insert_page(&page, None.into()).await.unwrap();
    let before_post = stored(&pool, "posts", record.snapshot.id).await;
    let before_page = stored(&pool, "pages", page_record.id).await;
    // 结构就绪不会隐式重建派生物；显式重建才刷新 HTML，且不改业务版本。
    sqlx::raw_sql("UPDATE posts SET content_html='',content_render_version=2; UPDATE pages SET content_html='',content_render_version=2;")
        .execute(&pool).await.unwrap();
    infrastructure::migrate_schema(&pool, "../../migrations/postgres")
        .await
        .unwrap();
    for (table, id) in [("posts", record.snapshot.id), ("pages", page_record.id)] {
        let (_, html, render_version, _, _) = stored(&pool, table, id).await;
        assert_eq!(html, "");
        assert_eq!(render_version, 2);
    }
    assert_eq!(
        rebuild_content_html(&pool, &RenderingRuntime::default())
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        stored(&pool, "posts", record.snapshot.id).await,
        before_post
    );
    assert_eq!(stored(&pool, "pages", page_record.id).await, before_page);
    assert_eq!(
        rebuild_content_html(&pool, &RenderingRuntime::default())
            .await
            .unwrap(),
        0
    );
}

struct PausedRenderer {
    entered: Arc<Notify>,
    resume: Arc<Notify>,
}

#[async_trait]
impl ContentRenderer for PausedRenderer {
    async fn render_content(&self, source: &str) -> Result<RenderedContent, UseCaseError> {
        self.entered.notify_one();
        self.resume.notified().await;
        RenderingRuntime::default().render_content(source).await
    }
}

#[tokio::test]
async fn rebuild_cannot_overwrite_a_concurrent_editor_commit() {
    let _guard = SERIAL.lock().await;
    let pool = database().await;
    let author = common::seed_user(&pool, "writer").await;
    let repo = PostgresPostRepository::new(pool.clone(), Arc::new(RenderingRuntime::default()));
    let record = repo
        .insert_post(&draft(author, "旧正文"), &[], None.into())
        .await
        .unwrap();
    let id = record.snapshot.id;
    sqlx::query("UPDATE posts SET content_render_version=2 WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let entered = Arc::new(Notify::new());
    let resume = Arc::new(Notify::new());
    let renderer = PausedRenderer {
        entered: entered.clone(),
        resume: resume.clone(),
    };
    let rebuilding_pool = pool.clone();
    let rebuild =
        tokio::spawn(async move { rebuild_content_html(&rebuilding_pool, &renderer).await });
    entered.notified().await;
    let mut post = Post::reconstitute(record.snapshot).unwrap();
    post.edit(PostPatch {
        content: Some("新 **正文**".into()),
        ..Default::default()
    })
    .unwrap();
    repo.commit_post(&post, 1, OffsetDateTime::now_utc(), None, None.into())
        .await
        .unwrap();
    let committed = stored(&pool, "posts", id).await;
    resume.notify_one();
    assert_eq!(rebuild.await.unwrap().unwrap(), 0);
    assert_eq!(stored(&pool, "posts", id).await, committed);
    assert_eq!(committed.1, "<p>新 <strong>正文</strong></p>\n");
    assert_eq!(committed.3, 2);
}
