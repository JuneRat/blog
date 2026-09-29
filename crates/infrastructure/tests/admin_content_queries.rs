mod common;
use application::content_queries::ContentListRequest;
use application::ports::{AdminPageQuery, AdminPostQuery};
use infrastructure::PostgresAdminContentQuery;

#[tokio::test]
async fn search_returns_matching_content_across_pages_and_scopes() {
    use std::collections::HashSet;
    let pool = common::fresh_database("blog_content_search_test").await;
    let author = common::seed_user(&pool, "author").await;
    let disabled = common::seed_user(&pool, "disabled-author").await;
    sqlx::query("UPDATE users SET status='disabled' WHERE id=$1")
        .bind(disabled)
        .execute(&pool)
        .await
        .unwrap();
    let query = PostgresAdminContentQuery::new(common::database(pool.clone()));
    let fixture_size = application::content_queries::CONTENT_PER_PAGE + 1;
    for table in ["posts", "pages"] {
        let (column, value) = if table == "posts" {
            ("author_id,", "$2,")
        } else {
            ("", "")
        };
        let sql = format!("INSERT INTO {table} ({column} id,slug,title,content,content_html,content_render_version)
            SELECT {value} gen_random_uuid(),'match-' || i,'Title ' || i,'Body 跨页 Needle','',1 FROM generate_series(1,$1::int) i RETURNING id");
        let mut seed = sqlx::query_scalar::<_, uuid::Uuid>(&sql).bind(fixture_size as i32);
        if table == "posts" {
            seed = seed.bind(author);
        }
        let expected: HashSet<_> = seed.fetch_all(&pool).await.unwrap().into_iter().collect();
        let unrelated = format!("INSERT INTO {table} ({column} id,slug,title,content,content_html,content_render_version)
            SELECT {value} gen_random_uuid(),'unrelated-' || i,'Other','No match','',1 FROM generate_series(1,$1::int) i");
        let mut seed = sqlx::query(&unrelated).bind(fixture_size as i32);
        if table == "posts" {
            seed = seed.bind(author);
        }
        seed.execute(&pool).await.unwrap();
        let mut ordered = Vec::new();
        let mut found = HashSet::new();
        for page in [1, 2] {
            let request = ContentListRequest {
                page,
                q: Some("  跨页 needle ".into()),
                ..Default::default()
            };
            let ids: Vec<_> = if table == "posts" {
                AdminPostQuery::list(&query, None, &request.try_into().unwrap())
                    .await
                    .unwrap()
                    .0
                    .into_iter()
                    .map(|row| row.id)
                    .collect()
            } else {
                AdminPageQuery::list(&query, &request.try_into().unwrap())
                    .await
                    .unwrap()
                    .0
                    .into_iter()
                    .map(|row| row.id)
                    .collect()
            };
            for id in ids {
                ordered.push(id);
                assert!(found.insert(id), "跨页内容不应重复");
            }
        }
        assert_eq!(found, expected);
        assert!(
            ordered.windows(2).all(|pair| pair[0] > pair[1]),
            "相同更新时间按 ID 稳定排序"
        );
    }
    let category = uuid::Uuid::now_v7();
    sqlx::query("INSERT INTO categories(id,slug,name) VALUES($1,'category','Category')")
        .bind(category)
        .execute(&pool)
        .await
        .unwrap();
    let target: uuid::Uuid = sqlx::query_scalar("INSERT INTO posts(id,author_id,slug,content,content_html,content_render_version,category_id)
        VALUES(gen_random_uuid(),$1,'historical','Needle','',1,$2) RETURNING id")
        .bind(disabled).bind(category).fetch_one(&pool).await.unwrap();
    let request = ContentListRequest {
        q: Some("needle".into()),
        category_id: Some(category),
        ..Default::default()
    };
    let rows = AdminPostQuery::list(&query, Some(disabled), &request.clone().try_into().unwrap())
        .await
        .unwrap()
        .0;
    assert_eq!(
        rows.iter().map(|row| row.id).collect::<Vec<_>>(),
        vec![target]
    );
    assert!(
        rows.iter()
            .all(|row| row.author_username == "disabled-author")
    );
    assert!(
        AdminPostQuery::list(&query, Some(author), &request.clone().try_into().unwrap())
            .await
            .unwrap()
            .0
            .is_empty()
    );
    sqlx::query("UPDATE posts SET deleted_at=now() WHERE id=$1")
        .bind(target)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        AdminPostQuery::list(&query, Some(disabled), &request.clone().try_into().unwrap())
            .await
            .unwrap()
            .0
            .is_empty()
    );
    let trash = ContentListRequest {
        trash: true,
        ..request
    };
    let restored = AdminPostQuery::list(&query, Some(disabled), &trash.try_into().unwrap())
        .await
        .unwrap()
        .0;
    assert_eq!(
        restored.iter().map(|row| row.id).collect::<Vec<_>>(),
        vec![target]
    );
    pool.close().await;
}
