mod common;
use application::content_queries::ContentListRequest;
use application::ports::{AdminPageQuery, AdminPostQuery};
use infrastructure::PostgresAdminContentQuery;

fn request(
    page: i64,
    trash: bool,
    status: Option<&str>,
    visibility: Option<&str>,
) -> ContentListRequest {
    ContentListRequest {
        page,
        trash,
        status: status.map(str::to_owned),
        visibility: visibility.map(str::to_owned),
    }
}

#[tokio::test]
async fn lists_bound_rows_filter_totals_and_break_timestamp_ties_by_id() {
    let pool = common::fresh_database("blog_admin_content_queries_test").await;
    let author = common::seed_user(&pool, "author").await;
    let other = common::seed_user(&pool, "other").await;
    for table in ["posts", "pages"] {
        let (column, value) = if table == "posts" {
            ("author_id,", "$1,")
        } else {
            ("", "")
        };
        let sql = format!("INSERT INTO {table} ({column} id, slug, title, content, content_html, content_render_version, status, visibility, published_at, updated_at, deleted_at)
            SELECT {value} gen_random_uuid(), 'item-' || i, '标题 ' || i, repeat('body', 10000), '<p>body</p>', 1,
            CASE WHEN i % 2 = 0 THEN 'published' ELSE 'draft' END,
            CASE WHEN i % 2 = 0 THEN 'private' ELSE 'public' END,
            '2026-01-01'::timestamptz, '2026-01-01'::timestamptz,
            CASE WHEN i > 45 THEN '2026-02-01'::timestamptz END
            FROM generate_series(1, 48) i");
        let mut query = sqlx::query(&sql);
        if table == "posts" {
            query = query.bind(author);
        }
        query.execute(&pool).await.unwrap();
    }
    sqlx::query("INSERT INTO posts (id, author_id, slug, content_html, content_render_version) VALUES (gen_random_uuid(), $1, 'other-author', '', 1)")
        .bind(other).execute(&pool).await.unwrap();
    let query = PostgresAdminContentQuery::new(common::database(pool.clone()));
    for is_post in [true, false] {
        let mut all_ids = Vec::new();
        for (page, count) in [(1, 20), (2, 20), (3, 5), (4, 0)] {
            let request = request(page, false, None, None);
            let (ids, total): (Vec<_>, _) = if is_post {
                let filter = request.try_into().unwrap();
                let (rows, total) = AdminPostQuery::list(&query, author, &filter).await.unwrap();
                assert!(rows.iter().all(|r| r.author_id == author));
                (rows.into_iter().map(|r| r.id).collect(), total)
            } else {
                let filter = request.try_into().unwrap();
                let (rows, total) = AdminPageQuery::list(&query, &filter).await.unwrap();
                (rows.into_iter().map(|r| r.id).collect(), total)
            };
            assert_eq!(total, 45);
            assert_eq!(ids.len(), count);
            all_ids.extend(ids);
        }
        assert!(
            all_ids.windows(2).all(|ids| ids[0] > ids[1]),
            "同时间戳跨页不能重复或乱序"
        );
        for (request, expected) in [
            (request(1, false, Some("published"), Some("private")), 22),
            (request(1, false, Some("published"), Some("public")), 0),
            (request(1, true, None, None), 3),
            (request(1, true, Some("published"), Some("private")), 2),
        ] {
            let (count, total) = if is_post {
                let filter = request.try_into().unwrap();
                let (rows, total) = AdminPostQuery::list(&query, author, &filter).await.unwrap();
                assert!(
                    rows.iter()
                        .all(|r| filter.status().is_none_or(|s| r.status == s)
                            && filter.visibility().is_none_or(|s| r.visibility == s))
                );
                (rows.len(), total)
            } else {
                let filter = request.try_into().unwrap();
                let (rows, total) = AdminPageQuery::list(&query, &filter).await.unwrap();
                assert!(
                    rows.iter()
                        .all(|r| filter.status().is_none_or(|s| r.status == s)
                            && filter.visibility().is_none_or(|s| r.visibility == s))
                );
                (rows.len(), total)
            };
            assert_eq!(total, expected);
            assert_eq!(count as i64, expected.min(20));
        }
    }
    assert_eq!(
        AdminPostQuery::list(
            &query,
            other,
            &request(1, false, None, None).try_into().unwrap()
        )
        .await
        .unwrap()
        .1,
        1
    );
    assert_eq!(
        AdminPostQuery::list(
            &query,
            uuid::Uuid::now_v7(),
            &request(1, false, None, None).try_into().unwrap()
        )
        .await
        .unwrap()
        .1,
        0
    );
}

#[tokio::test]
async fn summaries_reject_unknown_persisted_status_and_visibility() {
    let pool = common::fresh_database("blog_invalid_content_summary_test").await;
    let author = common::seed_user(&pool, "author").await;
    sqlx::query("INSERT INTO posts(id,author_id,slug,content_html,content_render_version) VALUES(gen_random_uuid(),$1,'post','',1)")
        .bind(author)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO pages(id,slug,content_html,content_render_version) VALUES(gen_random_uuid(),'page','',1)")
        .execute(&pool)
        .await
        .unwrap();
    let query = PostgresAdminContentQuery::new(common::database(pool.clone()));
    for table in ["posts", "pages"] {
        for (column, reset) in [("status", "draft"), ("visibility", "public")] {
            // Simulate data from an incompatible schema; never emit unknown states as valid DTOs.
            sqlx::raw_sql(&format!("ALTER TABLE {table} DROP CONSTRAINT {table}_{column}_check; UPDATE {table} SET {column}='unknown'"))
                .execute(&pool).await.unwrap();
            let error = if table == "posts" {
                AdminPostQuery::list(
                    &query,
                    author,
                    &ContentListRequest::default().try_into().unwrap(),
                )
                .await
                .unwrap_err()
            } else {
                AdminPageQuery::list(&query, &ContentListRequest::default().try_into().unwrap())
                    .await
                    .unwrap_err()
            };
            assert!(matches!(error, application::UseCaseError::Repository(_)));
            sqlx::raw_sql(&format!("UPDATE {table} SET {column}='{reset}'"))
                .execute(&pool)
                .await
                .unwrap();
        }
    }
    pool.close().await;
}
