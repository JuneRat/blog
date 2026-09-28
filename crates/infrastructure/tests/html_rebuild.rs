mod common;

use application::{
    UseCaseError,
    html_rebuild::{HtmlRebuildInteractor, RebuildOptions},
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
        }
        RenderingRuntime::default().render_content(source).await
    }
}

#[tokio::test]
async fn cursor_passes_conflicting_rows_and_next_run_revisits_remaining_old_versions() {
    let pool = common::fresh_database("blog_html_cursor_test").await;
    let author = common::seed_user(&pool, "cursor-author").await;
    for (n, source) in [(1, "first"), (2, "second")] {
        sqlx::query("INSERT INTO posts(id,author_id,slug,content,content_html,content_render_version) VALUES($1,$2,$3,$3,'stale',2)")
            .bind(Uuid::from_u128(n)).bind(author).bind(source).execute(&pool).await.unwrap();
    }
    let renderer = Arc::new(ChangingRenderer {
        pool: pool.clone(),
        calls: Mutex::new(vec![]),
    });
    let run = HtmlRebuildInteractor::new(Arc::new(PostgresHtmlRebuildStore::new(
        pool.clone(),
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
    assert_eq!(first, ("stale".into(), 2, 2));
    let runtime = Arc::new(RenderingRuntime::default());
    let result = HtmlRebuildInteractor::new(Arc::new(PostgresHtmlRebuildStore::new(
        pool.clone(),
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
