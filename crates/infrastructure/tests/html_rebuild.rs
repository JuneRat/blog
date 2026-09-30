mod common;

use application::{
    UseCaseError,
    html_rebuild::{HtmlKind, HtmlRebuildInteractor, HtmlRebuildStore, RebuildOptions},
    ports::{ContentRenderer, RenderedContent},
};
use async_trait::async_trait;
use infrastructure::{PostgresHtmlRebuildStore, RenderingRuntime};
use sqlx::PgPool;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

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
