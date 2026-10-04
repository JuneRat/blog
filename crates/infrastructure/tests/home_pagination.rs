//! The bounded public query must leave room for the homepage's next-page sentinel.
mod common;

use application::{ports::PublishedPostQuery, site_info::MAX_HOME_PAGE_SIZE};
use infrastructure::{CONTENT_RENDER_VERSION, PostgresPublishedPostQuery};
use sqlx::PgPool;
use uuid::Uuid;

async fn scenario(pool: PgPool) {
    let author = common::seed_user(&pool, "pagination-author").await;
    // Same publication time exercises the ID tie-breaker across every boundary.
    sqlx::query("INSERT INTO posts(id,author_id,slug,title,content,content_html,content_render_version,status,visibility,published_at) SELECT ('00000000-0000-0000-0000-' || lpad(to_hex(n),12,'0'))::uuid,$1,'boundary-'||n,'Boundary '||n,'synthetic','<p>synthetic</p>',$2,'published','public',now()-interval '1 day' FROM generate_series(1,201) n")
        .bind(author).bind(CONTENT_RENDER_VERSION).execute(&pool).await.unwrap();
    for (n, status, visibility, future, deleted) in [
        (1000, "draft", "public", false, false),
        (1001, "published", "private", false, false),
        (1002, "published", "public", true, false),
        (1003, "published", "public", false, true),
    ] {
        sqlx::query("INSERT INTO posts(id,author_id,slug,title,content,content_html,content_render_version,status,visibility,published_at,deleted_at) VALUES($1,$2,$3,'Filtered','synthetic','<p>synthetic</p>',$4,$5,$6,CASE WHEN $7 THEN now()+interval '1 day' ELSE now() END,CASE WHEN $8 THEN now() ELSE NULL END)")
            .bind(Uuid::from_u128(n)).bind(author).bind(format!("filtered-{n}"))
            .bind(CONTENT_RENDER_VERSION).bind(status).bind(visibility).bind(future).bind(deleted)
            .execute(&pool).await.unwrap();
    }
    let query = PostgresPublishedPostQuery::new(common::database(pool.clone()));
    for total in [99_i64, 100, 101, 199, 200, 201] {
        sqlx::query("UPDATE posts SET visibility=CASE WHEN id<=$1 THEN 'public' ELSE 'private' END WHERE id<=$2")
            .bind(Uuid::from_u128(total as u128)).bind(Uuid::from_u128(201))
            .execute(&pool).await.unwrap();
        for page_size in [MAX_HOME_PAGE_SIZE - 1, MAX_HOME_PAGE_SIZE] {
            let pages = (total + page_size - 1) / page_size;
            for page in 1..=pages {
                let offset = (page - 1) * page_size;
                let rows = query.list_public(page_size + 1, offset).await.unwrap();
                let remaining = total - offset;
                assert_eq!(rows.len() as i64, remaining.min(page_size + 1));
                assert_eq!(rows.len() as i64 > page_size, page < pages);
                let expected: Vec<_> = (0..rows.len())
                    .map(|n| format!("boundary-{}", total - offset - n as i64))
                    .collect();
                assert_eq!(
                    rows.iter().map(|row| row.slug.clone()).collect::<Vec<_>>(),
                    expected
                );
            }
            assert!(
                query
                    .list_public(page_size + 1, pages * page_size)
                    .await
                    .unwrap()
                    .is_empty()
            );
        }
    }
    // Arbitrary caller input remains bounded; the extra slot is exactly one row.
    let rows = query.list_public(i64::MAX, -1).await.unwrap();
    assert_eq!(rows.len() as i64, MAX_HOME_PAGE_SIZE + 1);
    assert_eq!(rows[0].slug, "boundary-201");
    let rows = query.list_public(i64::MIN, -1).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].slug, "boundary-201");
}

#[tokio::test]
async fn homepage_sentinel_survives_the_maximum_page_size_and_last_page() {
    let name = format!("blog_home_page_{}", Uuid::now_v7().simple());
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
