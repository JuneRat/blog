mod common;

use application::{
    UseCaseError,
    audit::AuditContext,
    html_rebuild::{HtmlKind, HtmlRebuildInteractor, HtmlRebuildStore, RebuildOptions},
    ports::{ContentRenderer, RenderedContent},
};
use async_trait::async_trait;
use infrastructure::{PostgresHtmlRebuildStore, RenderingRuntime};
use sqlx::PgPool;
use std::{
    future::Future,
    sync::{Arc, Mutex},
};
use uuid::Uuid;

// New regressions own random databases and clean up even if their assertions panic.
async fn isolated<F, R>(scenario: F)
where
    F: FnOnce(PgPool) -> R + Send + 'static,
    R: Future<Output = ()> + Send + 'static,
{
    let name = format!("blog_html_admin_{}", Uuid::now_v7().simple());
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

async fn seed_sources(pool: &PgPool, author: Uuid) {
    sqlx::query("INSERT INTO posts(id,author_id,slug,content,content_html,content_render_version) VALUES($1,$2,'audit-post','**Post source**','stale',$3)")
        .bind(Uuid::from_u128(1)).bind(author).bind(infrastructure::CONTENT_RENDER_VERSION + 1)
        .execute(pool).await.unwrap();
    sqlx::query("INSERT INTO pages(id,slug,content,content_html,content_render_version) VALUES($1,'audit-page','**Page source**','stale',$2)")
        .bind(Uuid::from_u128(2)).bind(infrastructure::CONTENT_RENDER_VERSION + 1)
        .execute(pool).await.unwrap();
    sqlx::query("INSERT INTO comments(id,post_id,author_name,content,content_html,content_render_version) VALUES($1,$2,'Reader','**Comment source**','stale',$3)")
        .bind(Uuid::from_u128(3)).bind(Uuid::from_u128(1)).bind(infrastructure::COMMENT_RENDER_VERSION + 1)
        .execute(pool).await.unwrap();
}

#[tokio::test]
async fn administrator_rebuild_audits_all_sources_and_cli_default_stays_system() {
    isolated(|pool| async move {
        let author = common::seed_user(&pool, "audit-author").await;
        let administrator = common::seed_user(&pool, "rebuild-administrator").await;
        seed_sources(&pool, author).await;
        let runtime = Arc::new(RenderingRuntime::default());
        let store = PostgresHtmlRebuildStore::new(
            common::database(pool.clone()),
            runtime.clone(),
            runtime.clone(),
        )
        .with_audit(AuditContext {
            actor_id: Some(administrator),
            ip_address: Some("2001:db8::17".parse().unwrap()),
            ..Default::default()
        });
        for kind in [HtmlKind::Post, HtmlKind::Page, HtmlKind::Comment] {
            assert_eq!(
                store.rebuild_batch(kind, None, 100).await.unwrap().rebuilt,
                1
            );
        }
        let audits: Vec<(String, Option<Uuid>, Option<String>, serde_json::Value)> =
            sqlx::query_as(
                "SELECT action,actor_id,host(ip_address),metadata FROM audit_logs ORDER BY action",
            )
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(audits.len(), 3);
        for (action, actor_id, address, metadata) in audits {
            assert_eq!(
                actor_id,
                Some(administrator),
                "来源必须是触发者而不是内容作者"
            );
            assert_eq!(address.as_deref(), Some("2001:db8::17"));
            assert_eq!(metadata.as_object().unwrap().len(), 2);
            assert_eq!(metadata["version"], 1);
            assert_eq!(
                metadata["render_version"],
                if action.starts_with("comment") {
                    infrastructure::COMMENT_RENDER_VERSION
                } else {
                    infrastructure::CONTENT_RENDER_VERSION
                }
            );
        }
        let prior: (i64, time::OffsetDateTime) =
            sqlx::query_as("SELECT version,updated_at FROM posts WHERE id=$1")
                .bind(Uuid::from_u128(1))
                .fetch_one(&pool)
                .await
                .unwrap();
        sqlx::query("UPDATE posts SET content_render_version=$2 WHERE id=$1")
            .bind(Uuid::from_u128(1))
            .bind(infrastructure::CONTENT_RENDER_VERSION + 1)
            .execute(&pool)
            .await
            .unwrap();
        let cli_store =
            PostgresHtmlRebuildStore::new(common::database(pool.clone()), runtime.clone(), runtime);
        assert_eq!(
            cli_store
                .rebuild_batch(HtmlKind::Post, None, 100)
                .await
                .unwrap()
                .rebuilt,
            1
        );
        let system_audit: (Option<Uuid>, Option<String>) = sqlx::query_as(
            "SELECT actor_id,host(ip_address) FROM audit_logs WHERE actor_id IS NULL",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(system_audit, (None, None));
        let after: (i64, time::OffsetDateTime) =
            sqlx::query_as("SELECT version,updated_at FROM posts WHERE id=$1")
                .bind(Uuid::from_u128(1))
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(after, prior, "审计来源不改变业务版本或更新时间");
    })
    .await;
}

#[tokio::test]
async fn failed_actor_audit_rolls_back_its_record_and_refs_but_preserves_prior_commits() {
    isolated(|pool| async move {
        let administrator = common::seed_user(&pool, "partial-administrator").await;
        let media = Uuid::now_v7();
        sqlx::query("INSERT INTO media(id,path,filename,mime_type,size,width,height,checksum_sha256) VALUES($1,$2,'audit.png','image/png',1,1,1,repeat('b',64))")
            .bind(media).bind(format!("objects/{media}.png")).execute(&pool).await.unwrap();
        for n in [1, 2] {
            sqlx::query("INSERT INTO posts(id,author_id,slug,content,content_html,content_render_version) VALUES($1,$2,$3,$4,'stale',$5)")
                .bind(Uuid::from_u128(n)).bind(administrator).bind(format!("partial-{n}"))
                .bind(format!("![audit](/media/{media})")).bind(infrastructure::CONTENT_RENDER_VERSION + 1)
                .execute(&pool).await.unwrap();
        }
        sqlx::raw_sql("CREATE FUNCTION fail_second_rebuild_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.target_id='00000000-0000-0000-0000-000000000002' THEN RAISE EXCEPTION 'audit unavailable'; END IF; RETURN NEW; END $$; CREATE TRIGGER fail_second_rebuild_audit BEFORE INSERT ON audit_logs FOR EACH ROW EXECUTE FUNCTION fail_second_rebuild_audit()")
            .execute(&pool).await.unwrap();
        let runtime = Arc::new(RenderingRuntime::default());
        let store = PostgresHtmlRebuildStore::new(common::database(pool.clone()), runtime.clone(), runtime)
            .with_audit(AuditContext {actor_id: Some(administrator), ip_address: Some("192.0.2.17".parse().unwrap()), ..Default::default()});
        let error = store.rebuild_batch(HtmlKind::Post, None, 100).await.unwrap_err();
        assert_eq!(error.id, Some(Uuid::from_u128(2)));
        assert_eq!((error.progress.rebuilt, error.progress.skipped), (1, 0));
        assert_eq!(error.progress.cursor, Some(Uuid::from_u128(1)));
        let rows: Vec<(i32, String)> = sqlx::query_as("SELECT content_render_version,content_html FROM posts ORDER BY id")
            .fetch_all(&pool).await.unwrap();
        assert_eq!(rows[0].0, infrastructure::CONTENT_RENDER_VERSION);
        assert!(rows[0].1.contains("<img"));
        assert_eq!(rows[1], (infrastructure::CONTENT_RENDER_VERSION + 1, "stale".into()));
        let refs: Vec<Uuid> = sqlx::query_scalar("SELECT source_id FROM media_refs WHERE media_id=$1")
            .bind(media).fetch_all(&pool).await.unwrap();
        assert_eq!(refs, [Uuid::from_u128(1)]);
        let audits: Vec<(String, Option<Uuid>, Option<String>)> = sqlx::query_as("SELECT target_id,actor_id,host(ip_address) FROM audit_logs")
            .fetch_all(&pool).await.unwrap();
        assert_eq!(audits, [(Uuid::from_u128(1).to_string(), Some(administrator), Some("192.0.2.17".into()))]);
        sqlx::raw_sql("DROP TRIGGER fail_second_rebuild_audit ON audit_logs; DROP FUNCTION fail_second_rebuild_audit()")
            .execute(&pool).await.unwrap();
        assert_eq!(store.rebuild_batch(HtmlKind::Post, None, 100).await.unwrap().rebuilt, 1);
        assert_eq!(store.pending().await.unwrap().posts, 0);
    }).await;
}

struct UnexpectedRenderer;

#[async_trait]
impl ContentRenderer for UnexpectedRenderer {
    async fn render_content(&self, _: &str) -> Result<RenderedContent, UseCaseError> {
        panic!("恢复隔离不能触发正文渲染")
    }
}

#[async_trait]
impl application::ports::CommentRenderer for UnexpectedRenderer {
    async fn render_comment(&self, _: &str) -> Result<String, UseCaseError> {
        panic!("恢复隔离不能触发评论渲染")
    }
}

#[tokio::test]
async fn recovery_isolation_blocks_all_write_batches_but_keeps_pending_readable() {
    isolated(|pool| async move {
        let author = common::seed_user(&pool, "isolated-author").await;
        seed_sources(&pool, author).await;
        let name: String = sqlx::query_scalar("SELECT current_database()")
            .fetch_one(&pool).await.unwrap();
        sqlx::raw_sql(&format!("COMMENT ON DATABASE {name} IS 'blog:recovery-isolated:html-admin-test'"))
            .execute(&pool).await.unwrap();
        let store = PostgresHtmlRebuildStore::new(
            common::database(pool.clone()), Arc::new(UnexpectedRenderer), Arc::new(UnexpectedRenderer),
        );
        let before: serde_json::Value = sqlx::query_scalar("SELECT jsonb_build_array((SELECT to_jsonb(p) FROM posts p),(SELECT to_jsonb(p) FROM pages p),(SELECT to_jsonb(c) FROM comments c))")
            .fetch_one(&pool).await.unwrap();
        assert_eq!(store.pending().await.unwrap(), application::html_rebuild::RebuildCounts {posts: 1, pages: 1, comments: 1});
        for kind in [HtmlKind::Post, HtmlKind::Page, HtmlKind::Comment] {
            let error = store.rebuild_batch(kind, None, 100).await.unwrap_err();
            assert!(error.source.to_string().contains("恢复隔离期间禁止 HTML 重建"));
            assert_eq!(error.id, None);
            assert_eq!((error.progress.rebuilt, error.progress.skipped, error.progress.cursor), (0, 0, None));
        }
        let after: serde_json::Value = sqlx::query_scalar("SELECT jsonb_build_array((SELECT to_jsonb(p) FROM posts p),(SELECT to_jsonb(p) FROM pages p),(SELECT to_jsonb(c) FROM comments c))")
            .fetch_one(&pool).await.unwrap();
        assert_eq!(after, before);
        let audits: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_logs").fetch_one(&pool).await.unwrap();
        assert_eq!(audits, 0);
        sqlx::raw_sql(&format!("COMMENT ON DATABASE {name} IS NULL"))
            .execute(&pool).await.unwrap();
        let runtime = Arc::new(RenderingRuntime::default());
        let store = PostgresHtmlRebuildStore::new(common::database(pool.clone()), runtime.clone(), runtime);
        for kind in [HtmlKind::Post, HtmlKind::Page, HtmlKind::Comment] {
            assert_eq!(store.rebuild_batch(kind, None, 100).await.unwrap().rebuilt, 1);
        }
        assert!(store.pending().await.unwrap().is_empty());
    }).await;
}

struct ChangingRenderer {
    pool: PgPool,
    calls: Mutex<Vec<String>>,
}

#[async_trait]
impl ContentRenderer for ChangingRenderer {
    async fn render_content(&self, source: &str) -> Result<RenderedContent, UseCaseError> {
        self.calls.lock().unwrap().push(source.to_string());
        if source == "first" {
            // 模拟渲染期间另一个写入者改变版本，但仍留下旧渲染规则。
            sqlx::query("UPDATE posts SET version=version+1 WHERE id=$1")
                .bind(Uuid::from_u128(1))
                .execute(&self.pool)
                .await
                .unwrap();
        } else if source == "change-candidates" {
            // ID 已选入同批，但正文读取前可能已被其他写入者重建或删除。
            sqlx::query("UPDATE posts SET content_html='<p>current</p>',content_render_version=$2 WHERE id=$1")
                .bind(Uuid::from_u128(2)).bind(infrastructure::CONTENT_RENDER_VERSION).execute(&self.pool).await.unwrap();
            sqlx::query("DELETE FROM posts WHERE id=$1")
                .bind(Uuid::from_u128(3))
                .execute(&self.pool)
                .await
                .unwrap();
        }
        RenderingRuntime::default().render_content(source).await
    }
}

#[tokio::test]
async fn batch_skips_candidates_changed_before_their_source_is_loaded() {
    let pool = common::fresh_database("blog_html_candidates_test").await;
    let author = common::seed_user(&pool, "candidate-author").await;
    for (n, source) in [
        (1, "change-candidates"),
        (2, "current"),
        (3, "removed"),
        (4, "last"),
    ] {
        sqlx::query("INSERT INTO posts(id,author_id,slug,content,content_html,content_render_version) VALUES($1,$2,$3,$3,'stale',1)")
            .bind(Uuid::from_u128(n)).bind(author).bind(source).execute(&pool).await.unwrap();
    }
    let renderer = Arc::new(ChangingRenderer {
        pool: pool.clone(),
        calls: Mutex::new(vec![]),
    });
    let store = PostgresHtmlRebuildStore::new(
        common::database(pool.clone()),
        renderer.clone(),
        Arc::new(RenderingRuntime::default()),
    );
    let batch = store.rebuild_batch(HtmlKind::Post, None, 4).await.unwrap();
    assert_eq!((batch.rebuilt, batch.skipped), (2, 2));
    assert_eq!(batch.cursor, Some(Uuid::from_u128(4)));
    assert_eq!(
        *renderer.calls.lock().unwrap(),
        ["change-candidates", "last"]
    );
    assert_eq!(store.pending().await.unwrap().posts, 0);
    pool.close().await;
}

#[tokio::test]
async fn cursor_passes_conflicting_rows_and_next_run_revisits_remaining_old_versions() {
    let pool = common::fresh_database("blog_html_cursor_test").await;
    let author = common::seed_user(&pool, "cursor-author").await;
    for (n, source) in [(1, "first"), (2, "second")] {
        sqlx::query("INSERT INTO posts(id,author_id,slug,content,content_html,content_render_version) VALUES($1,$2,$3,$3,'stale',1)")
            .bind(Uuid::from_u128(n)).bind(author).bind(source).execute(&pool).await.unwrap();
    }
    let renderer = Arc::new(ChangingRenderer {
        pool: pool.clone(),
        calls: Mutex::new(vec![]),
    });
    let run = HtmlRebuildInteractor::new(Arc::new(PostgresHtmlRebuildStore::new(
        common::database(pool.clone()),
        renderer.clone(),
        Arc::new(RenderingRuntime::default()),
    )));
    let result = run
        .run(RebuildOptions {
            batch_size: 1,
            max_batches: 10,
            dry_run: false,
        })
        .await
        .unwrap();
    assert_eq!(
        (result.rebuilt.posts, result.skipped.posts, result.batches),
        (1, 1, 3)
    );
    assert_eq!(*renderer.calls.lock().unwrap(), ["first", "second"]);
    assert_eq!(result.pending.unwrap().posts, 1);
    assert!(result.has_more);
    let first: (String, i32, i64) =
        sqlx::query_as("SELECT content_html,content_render_version,version FROM posts WHERE id=$1")
            .bind(Uuid::from_u128(1))
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(first, ("stale".into(), 1, 2));
    let runtime = Arc::new(RenderingRuntime::default());
    let result = HtmlRebuildInteractor::new(Arc::new(PostgresHtmlRebuildStore::new(
        common::database(pool.clone()),
        runtime.clone(),
        runtime,
    )))
    .run(RebuildOptions::default())
    .await
    .unwrap();
    assert_eq!(result.rebuilt.posts, 1);
    assert_eq!(result.skipped.posts, 0);
    assert!(!result.has_more);
    pool.close().await;
}
