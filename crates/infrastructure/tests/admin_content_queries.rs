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

#[tokio::test]
async fn indexed_search_preserves_literal_substrings_and_field_boundaries() {
    let pool = common::fresh_database("blog_content_search_literals_test").await;
    let author = common::seed_user(&pool, "literal-author").await;
    let query = PostgresAdminContentQuery::new(common::database(pool.clone()));
    for table in ["posts", "pages"] {
        for (i, (title, slug, content)) in [
            (
                "TitleNeedle",
                "title-target",
                "Token%_\\literal 中文段落甲乙 MiddleNeedle",
            ),
            ("Decoy", "slug-needle", "TokenXXliteral 中文段落丙丁"),
            ("BoundaryLeft", "BoundaryRight", "No match"),
            ("Same field", "same-field", "BoundaryLeft\nBoundaryRight"),
        ]
        .into_iter()
        .enumerate()
        {
            let author_column = if table == "posts" { "author_id," } else { "" };
            let author_value = if table == "posts" { "$5," } else { "" };
            let sql = format!(
                "INSERT INTO {table} ({author_column}id,title,slug,content,content_html,content_render_version,deleted_at) \
                 VALUES({author_value}gen_random_uuid(),$1,$2,$3,'',1,CASE WHEN $4 THEN now() END)"
            );
            let mut seed = sqlx::query(&sql)
                .bind(title)
                .bind(slug)
                .bind(content)
                .bind(i % 2 == 1);
            if table == "posts" {
                seed = seed.bind(author);
            }
            seed.execute(&pool).await.unwrap();
        }
        if table == "posts" {
            sqlx::query("UPDATE posts SET excerpt='ExcerptNeedle' WHERE slug='title-target'")
                .execute(&pool)
                .await
                .unwrap();
        }
        for trash in [false, true] {
            for q in [
                "titleNEEDLE",
                "SLUG-NEEDLE",
                "middleneedle",
                "ExcerptNeedle",
                "中文段落",
                "甲",
                "甲乙",
                "%",
                "_",
                "\\",
                "Token%_\\literal",
                "BoundaryLeft\nBoundaryRight",
                "nonexistent-value",
            ] {
                // Compare against the original search contract, including short
                // Chinese queries and characters that are wildcards in LIKE.
                let excerpt = if table == "posts" {
                    " OR strpos(lower(coalesce(excerpt,'')),lower($1))>0"
                } else {
                    ""
                };
                let order = if trash { "deleted_at" } else { "updated_at" };
                let expected: Vec<uuid::Uuid> = sqlx::query_scalar(&format!(
                    "SELECT id FROM {table} WHERE (deleted_at IS NOT NULL)=$2 AND \
                     (strpos(lower(title),lower($1))>0 OR strpos(lower(slug),lower($1))>0 \
                      OR strpos(lower(content),lower($1))>0{excerpt}) ORDER BY {order} DESC,id DESC"
                ))
                .bind(q)
                .bind(trash)
                .fetch_all(&pool)
                .await
                .unwrap();
                let request = ContentListRequest {
                    trash,
                    q: Some(q.into()),
                    ..Default::default()
                };
                let (ids, total): (Vec<_>, i64) = if table == "posts" {
                    let (items, total) =
                        AdminPostQuery::list(&query, Some(author), &request.try_into().unwrap())
                            .await
                            .unwrap();
                    (items.into_iter().map(|item| item.id).collect(), total)
                } else {
                    let (items, total) = AdminPageQuery::list(&query, &request.try_into().unwrap())
                        .await
                        .unwrap();
                    (items.into_iter().map(|item| item.id).collect(), total)
                };
                assert_eq!(ids, expected, "{table}, trash={trash}, q={q:?}");
                assert_eq!(total, expected.len() as i64);
            }
        }
    }
    pool.close().await;
}
