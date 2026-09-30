//! URL identity and upgrade backfill must protect the same files the HTTP route serves.
mod common;

use application::{
    html_rebuild::{HtmlKind, HtmlRebuildStore},
    media_cleanup::{MediaPurgeStore, PurgePlan, VerifiedPlan},
    ports::{PageRepository, PostCommitOutcome, PostRepository},
};
use domain::{
    content::{Page, Post, PostPatch, Slug, Visibility},
    identity::UserId,
};
use infrastructure::{
    CONTENT_RENDER_VERSION, PostgresHtmlRebuildStore, PostgresPageRepository,
    PostgresPostRepository, RenderingRuntime, SanitizingMarkdownRenderer,
    media_cleanup::PostgresMediaPurgeStore,
};
use sqlx::PgPool;
use std::{future::Future, sync::Arc};
use time::OffsetDateTime;
use uuid::Uuid;

// Random databases avoid colliding with fixed-name suites and are removed even
// when an assertion in the spawned scenario panics.
async fn isolated<F, R>(scenario: F)
where
    F: FnOnce(PgPool) -> R + Send + 'static,
    R: Future<Output = ()> + Send + 'static,
{
    let name = format!("blog_media_urls_{}", Uuid::now_v7().simple());
    let pool = common::fresh_database(&name).await;
    let result = tokio::spawn(scenario(pool.clone())).await;
    pool.close().await;
    let admin = common::connect(&common::admin_url()).await.unwrap();
    sqlx::raw_sql(&format!("DROP DATABASE {name} WITH (FORCE)"))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    if let Err(error) = result {
        std::panic::resume_unwind(error.into_panic());
    }
}

async fn media(pool: &PgPool, trashed: bool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO media(id,path,filename,mime_type,size,width,height,checksum_sha256,deleted_at) VALUES($1,$2,'test.png','image/png',11,1,1,repeat('a',64),CASE WHEN $3 THEN now() ELSE NULL END)")
        .bind(id).bind(format!("objects/{id}.png")).bind(trashed).execute(pool).await.unwrap();
    id
}

fn draft(author: Uuid, slug: &str, source: &str) -> Post {
    Post::create_draft(
        UserId(author),
        Slug::new(slug).unwrap(),
        "URL identity".into(),
        None,
        source.into(),
        Visibility::Public,
        OffsetDateTime::now_utc(),
    )
    .unwrap()
}

async fn refs(pool: &PgPool, media: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM media_refs WHERE media_id=$1")
        .bind(media)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn plan(store: &PostgresMediaPurgeStore, id: Uuid) -> VerifiedPlan {
    VerifiedPlan {
        plan: PurgePlan {
            format: 1,
            operation_id: Uuid::now_v7(),
            database: store.identity().await.unwrap(),
            media_root: "/private/tmp/synthetic-media-reference-test".into(),
            created_at: None,
            items: store.candidates(&[id]).await.unwrap(),
        },
        sha256: "b".repeat(64),
    }
}

#[tokio::test]
async fn saves_track_query_fragment_and_encoded_local_urls_but_ignore_external_images() {
    isolated(|pool| async move {
        let author = common::seed_user(&pool, "url-author").await;
        let id = media(&pool, false).await;
        let external = Uuid::now_v7(); // No local row: accidental attribution would reject the save.
        let encoded: String = id.to_string().bytes().map(|b| format!("%{b:02X}")).collect();
        let source = format!(
            "![query](/media/{id}?v=1&size=large)\n\n![fragment](/media/{id}#preview)\n\n![encoded](/media/{encoded}?v=2#preview)\n\n![external](https://example.com/media/{external}?v=1)\n\n![external](//example.com/media/{external})\n\n[link](/media/{external})"
        );
        let runtime = Arc::new(RenderingRuntime::default());
        let posts = PostgresPostRepository::new(common::database(pool.clone()), runtime.clone());
        let pages = PostgresPageRepository::new(common::database(pool.clone()), runtime);
        let mut post = draft(author, "url-post", &source);
        posts.insert_post(&post, &[], None.into()).await.unwrap();
        let page = Page::create_draft(
            Slug::new("url-page").unwrap(),
            "URL page".into(),
            source,
            Visibility::Public,
            OffsetDateTime::now_utc(),
        )
        .unwrap();
        pages.insert_page(&page, None.into()).await.unwrap();
        assert_eq!(refs(&pool, id).await, 2);
        assert_eq!(refs(&pool, external).await, 0);
        post.edit(PostPatch {
            content: Some(format!("![updated](/media/{encoded}?v=3#new)")),
            ..Default::default()
        })
        .unwrap();
        assert!(matches!(
            posts.commit_post(&post, 1, OffsetDateTime::now_utc(), None, None.into()).await.unwrap(),
            PostCommitOutcome::Saved(_)
        ));
        assert_eq!(refs(&pool, id).await, 2);
        let versions: Vec<i32> = sqlx::query_scalar("SELECT content_render_version FROM posts UNION ALL SELECT content_render_version FROM pages")
            .fetch_all(&pool).await.unwrap();
        assert_eq!(versions, vec![CONTENT_RENDER_VERSION; 2]);
        sqlx::query("UPDATE media SET deleted_at=now() WHERE id=$1")
            .bind(id).execute(&pool).await.unwrap();
        assert!(PostgresMediaPurgeStore::new(common::database(pool.clone()), None)
            .candidates(&[id]).await.unwrap_err().to_string().contains("referenced"));
    })
    .await;
}

#[tokio::test]
async fn old_html_backfills_trashed_media_atomically_and_blocks_purge_until_complete() {
    isolated(|pool| async move {
        assert_eq!(CONTENT_RENDER_VERSION, 2);
        let author = common::seed_user(&pool, "legacy-author").await;
        let id = media(&pool, true).await;
        let purge = PostgresMediaPurgeStore::new(common::database(pool.clone()), None);
        let old_plan = plan(&purge, id).await;
        let source = format!("![historical](/media/{id}?v=1#preview)");
        let html = SanitizingMarkdownRenderer::new().render_markdown(&source);
        let post = Uuid::now_v7();
        let page = Uuid::now_v7();
        // Version 1 deliberately models an existing installation with unchanged
        // HTML bytes but missing image bookkeeping; no new SQL migration is needed.
        sqlx::query("INSERT INTO posts(id,author_id,slug,content,content_html,content_render_version) VALUES($1,$2,'legacy-url-post',$3,$4,1)")
            .bind(post).bind(author).bind(&source).bind(&html).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO pages(id,slug,content,content_html,content_render_version) VALUES($1,'legacy-url-page',$2,$3,1)")
            .bind(page).bind(&source).bind(&html).execute(&pool).await.unwrap();
        let before: Vec<(Uuid, i64, OffsetDateTime, String)> = sqlx::query_as("SELECT id,version,updated_at,content_html FROM posts UNION ALL SELECT id,version,updated_at,content_html FROM pages ORDER BY id")
            .fetch_all(&pool).await.unwrap();
        infrastructure::migrate_schema(&common::database(pool.clone()), "../../migrations/postgres")
            .await.unwrap();
        assert_eq!(refs(&pool, id).await, 0, "ordinary migration does not rebuild");
        for error in [purge.candidates(&[id]).await.unwrap_err(), purge.commit(&old_plan).await.unwrap_err()] {
            assert!(error.to_string().contains("rebuild-html"));
        }
        assert!(sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM media WHERE id=$1)")
            .bind(id).fetch_one(&pool).await.unwrap());
        let runtime = Arc::new(RenderingRuntime::default());
        let rebuild = PostgresHtmlRebuildStore::new(common::database(pool.clone()), runtime.clone(), runtime.clone());
        sqlx::raw_sql("CREATE FUNCTION fail_url_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'audit unavailable'; END $$; CREATE TRIGGER fail_url_audit BEFORE INSERT ON audit_logs FOR EACH ROW EXECUTE FUNCTION fail_url_audit()")
            .execute(&pool).await.unwrap();
        assert!(rebuild.rebuild_batch(HtmlKind::Post, None, 100).await.is_err());
        assert_eq!(refs(&pool, id).await, 0);
        assert_eq!(sqlx::query_scalar::<_, i32>("SELECT content_render_version FROM posts WHERE id=$1")
            .bind(post).fetch_one(&pool).await.unwrap(), 1);
        sqlx::raw_sql("DROP TRIGGER fail_url_audit ON audit_logs; DROP FUNCTION fail_url_audit()")
            .execute(&pool).await.unwrap();
        for kind in [HtmlKind::Post, HtmlKind::Page] {
            assert_eq!(rebuild.rebuild_batch(kind, None, 100).await.unwrap().rebuilt, 1);
        }
        assert_eq!(refs(&pool, id).await, 2);
        assert_eq!(rebuild.pending().await.unwrap().posts, 0);
        assert_eq!(rebuild.pending().await.unwrap().pages, 0);
        let after: Vec<(Uuid, i64, OffsetDateTime, String)> = sqlx::query_as("SELECT id,version,updated_at,content_html FROM posts UNION ALL SELECT id,version,updated_at,content_html FROM pages ORDER BY id")
            .fetch_all(&pool).await.unwrap();
        assert_eq!(before, after, "backfill keeps HTML, edit version, and timestamps");
        for error in [purge.candidates(&[id]).await.unwrap_err(), purge.commit(&old_plan).await.unwrap_err()] {
            assert!(error.to_string().contains("referenced"));
        }
        // The historical exception does not permit adding a trashed file to a new source.
        assert!(PostgresPostRepository::new(common::database(pool.clone()), runtime)
            .insert_post(&draft(author, "new-url-post", &source), &[], None.into()).await.is_err());
        assert_eq!(refs(&pool, id).await, 2);
    })
    .await;
}

#[tokio::test]
async fn an_existing_purge_receipt_can_finish_its_file_only_retry_after_upgrade() {
    isolated(|pool| async move {
        let author = common::seed_user(&pool, "receipt-author").await;
        let id = media(&pool, true).await;
        let purge = PostgresMediaPurgeStore::new(common::database(pool.clone()), None);
        let committed = plan(&purge, id).await;
        purge.commit(&committed).await.unwrap();
        sqlx::query("INSERT INTO posts(id,author_id,slug,content,content_html,content_render_version) VALUES($1,$2,'old-after-receipt','old','<p>old</p>',1)")
            .bind(Uuid::now_v7()).bind(author).execute(&pool).await.unwrap();
        purge.commit(&committed).await.unwrap();
        assert!(purge.has_receipt(&committed, &committed.plan.items[0]).await.unwrap());
        assert!(purge.may_remove_file(&committed, &committed.plan.items[0]).await.unwrap());
    })
    .await;
}
