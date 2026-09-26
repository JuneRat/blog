//! 基础设施集成测试：真实 PostgreSQL 上验证迁移、约束与并发版本语义。
//!
//! 通过 `BLOG_TEST_ADMIN_URL` 覆盖管理连接（默认 postgres://blog:blog@127.0.0.1:5432/postgres）；
//! 测试库 DSN 从它推导（同名主机上的 blog_test）。库不可达或主机非 loopback 时
//! 直接 panic 失败——这些测试是破坏性的（DROP DATABASE），不允许静默跳过后误报通过。

use std::sync::Arc;

use application::error::{ConflictKind, UseCaseError};
use application::ports::{
    CategoryRepository, ClearPasswordOutcome, OAuthAccountStore, PageCommitOutcome,
    PageDeleteOutcome, PageRepository, PostCommitOutcome, PostRepository, PublishedCategoryQuery,
    PublishedPageQuery, PublishedPostQuery, PublishedSeriesQuery, PublishedTagQuery, RbacStore,
    SaveOutcome, SeriesRepository, SettingsStore, TagRepository, UserRepository,
};
use domain::content::page::{Page, PagePatch};
use domain::content::post::{Post, PostPatch, PostSnapshot, PostStatus, Slug, Visibility};
use domain::identity::{User, UserId};
use infrastructure::{
    PostgresOAuthAccountStore, PostgresPageRepository, PostgresPostRepository,
    PostgresPublishedPageQuery, PostgresPublishedPostQuery, PostgresRbacStore,
    PostgresUserRepository, SanitizingMarkdownRenderer, connect,
};
use sqlx::{PgPool, Row};
use time::OffsetDateTime;
use tokio::sync::Mutex;

mod common;
use common::{admin_url, seed_user, test_db_url};

static SERIAL: Mutex<()> = Mutex::const_new(());

/// 本文件专用测试库；库名在 common 里换取隔离与并行安全。
async fn fresh_database() -> PgPool {
    common::fresh_database("blog_test").await
}

/// 给用户绑定一个外部登录方式（“有效 Owner”判定要求至少一种登录方式）。
async fn seed_binding(pool: &PgPool, user_id: uuid::Uuid) {
    sqlx::query(
        "INSERT INTO oauth_accounts (id, user_id, provider, provider_user_id, created_at, updated_at) \
         VALUES ($1, $2, 'https://idp.example', $3, now(), now())",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(user_id)
    .bind(format!("sub-{user_id}"))
    .execute(pool)
    .await
    .unwrap();
}

/// 给用户启用本地密码（只写占位 PHC；本测试只关心「是否构成有效登录方式」）。
async fn seed_password(pool: &PgPool, user_id: uuid::Uuid) {
    sqlx::query("UPDATE users SET password_hash = $2 WHERE id = $1")
        .bind(user_id)
        .bind("$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA")
        .execute(pool)
        .await
        .unwrap();
}

fn draft_snapshot(author: uuid::Uuid, slug: &str) -> PostSnapshot {
    let post = Post::create_draft(
        UserId(author),
        Slug::new(slug).unwrap(),
        format!("文章 {slug}"),
        None,
        format!("# {slug}\n\n正文"),
        Visibility::Public,
        OffsetDateTime::now_utc(),
    )
    .unwrap();
    post.snapshot()
}

#[tokio::test]
async fn migrations_create_core_tables() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;

    // 13 张核心内容/身份表 + 媒体 2 张 + 持久会话 1 张（docs/database-design.md）。
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_tables WHERE schemaname='public' AND tablename <> '_sqlx_migrations'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 16, "13 张核心表 + 媒体 2 张 + 会话 1 张");

    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT tablename FROM pg_tables WHERE schemaname='public' AND tablename <> '_sqlx_migrations' ORDER BY tablename",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let expected = [
        "categories",
        "content_media_refs",
        "media_assets",
        "oauth_accounts",
        "pages",
        "permissions",
        "post_tags",
        "posts",
        "role_permissions",
        "roles",
        "series",
        "sessions",
        "settings",
        "tags",
        "user_roles",
        "users",
    ];
    for t in expected {
        assert!(tables.iter().any(|x| x == t), "缺少表 {t}");
    }
}

#[tokio::test]
async fn duplicate_slug_is_rejected_as_conflict() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let author = seed_user(&pool, "author").await;
    let repo = PostgresPostRepository::new(
        pool.clone(),
        Arc::new(infrastructure::RenderingRuntime::default()),
    );

    repo.insert_post(
        &Post::reconstitute(draft_snapshot(author, "same-slug")),
        &[],
    )
    .await
    .unwrap();
    let err = repo
        .insert_post(
            &Post::reconstitute(draft_snapshot(author, "same-slug")),
            &[],
        )
        .await
        .unwrap_err();
    match err {
        UseCaseError::Conflict(ConflictKind::Slug) => {}
        other => panic!("期望 Conflict(Slug)，得到 {other:?}"),
    }
}

#[tokio::test]
async fn foreign_key_protects_author_reference() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let repo = PostgresPostRepository::new(
        pool.clone(),
        Arc::new(infrastructure::RenderingRuntime::default()),
    );

    let ghost = uuid::Uuid::now_v7();
    let err = repo
        .insert_post(&Post::reconstitute(draft_snapshot(ghost, "orphan")), &[])
        .await
        .unwrap_err();
    assert!(
        matches!(err, UseCaseError::Repository(_)),
        "未知作者应被 FK 拒绝"
    );
}

#[tokio::test]
async fn series_position_rules_enforced_by_constraints() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let author = seed_user(&pool, "author").await;
    let repo = PostgresPostRepository::new(
        pool.clone(),
        Arc::new(infrastructure::RenderingRuntime::default()),
    );

    let series_id = uuid::Uuid::now_v7();
    sqlx::query("INSERT INTO series (id, name, slug, version) VALUES ($1, '系列', 'series-1', 1)")
        .bind(series_id)
        .execute(&pool)
        .await
        .unwrap();

    let mut a = draft_snapshot(author, "series-a");
    a.series_id = Some(series_id);
    a.series_order = Some(2);
    repo.insert_post(&Post::reconstitute(a.clone()), &[])
        .await
        .unwrap();

    // 同系列同位置：唯一约束拒绝。
    let mut b = draft_snapshot(author, "series-b");
    b.series_id = Some(series_id);
    b.series_order = Some(2);
    let err = repo
        .insert_post(&Post::reconstitute(b.clone()), &[])
        .await
        .unwrap_err();
    match err {
        UseCaseError::Conflict(ConflictKind::SeriesPosition) => {}
        other => panic!("期望 Conflict(SeriesPosition)，得到 {other:?}"),
    }

    // 不同位置可以插入；留空档合法。
    b.series_order = Some(5);
    repo.insert_post(&Post::reconstitute(b.clone()), &[])
        .await
        .unwrap();

    // 非法组合由数据库约束测试直接写 SQL，不通过生产领域写入口构造非法对象。
    let err = sqlx::query(
        "INSERT INTO posts (id, author_id, title, slug, content, series_order) \
         VALUES ($1, $2, '非法系列位置', 'series-c', '正文', 1)",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(author)
    .execute(&pool)
    .await
    .unwrap_err();
    assert!(
        matches!(&err, sqlx::Error::Database(error) if error.code().as_deref() == Some("23514")),
        "series_id 与 series_order 必须同空同非空：{err}"
    );

    // 延后唯一约束：事务内交换位置，提交时必须恢复唯一。
    sqlx::raw_sql(
        "BEGIN; SET CONSTRAINTS posts_series_position_unique DEFERRED; \
         UPDATE posts SET series_order = 5 WHERE slug = 'series-a'; \
         UPDATE posts SET series_order = 2 WHERE slug = 'series-b'; \
         COMMIT;",
    )
    .execute(&pool)
    .await
    .expect("交换系列位置");

    let orders: Vec<(String, i32)> =
        sqlx::query("SELECT slug, series_order FROM posts WHERE series_id = $1 ORDER BY slug")
            .bind(series_id)
            .fetch_all(&pool)
            .await
            .unwrap()
            .into_iter()
            .map(|row: sqlx::postgres::PgRow| {
                (
                    row.get::<String, _>("slug"),
                    row.get::<i32, _>("series_order"),
                )
            })
            .collect();
    assert_eq!(orders, vec![("series-a".into(), 5), ("series-b".into(), 2)]);

    // 延后后仍冲突的提交必须整体失败。
    let err = sqlx::raw_sql(
        "BEGIN; SET CONSTRAINTS posts_series_position_unique DEFERRED; \
         UPDATE posts SET series_order = 5 WHERE slug = 'series-b'; COMMIT;",
    )
    .execute(&pool)
    .await
    .unwrap_err();
    assert!(
        err.to_string().contains("posts_series_position_unique"),
        "冲突提交回滚：{err}"
    );
}

#[tokio::test]
async fn save_returns_three_states_and_new_version() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let author = seed_user(&pool, "author").await;
    let repo = PostgresPostRepository::new(
        pool.clone(),
        Arc::new(infrastructure::RenderingRuntime::default()),
    );

    let mut snapshot = draft_snapshot(author, "versioned");
    repo.insert_post(&Post::reconstitute(snapshot.clone()), &[])
        .await
        .unwrap();
    assert_eq!(snapshot.version, 1);

    // 正确版本：Saved 且携带新版本号，无需回读。
    snapshot.title = "第一次修改".into();
    let outcome = repo
        .commit_post(
            &Post::reconstitute(snapshot.clone()),
            1,
            OffsetDateTime::now_utc(),
            None,
        )
        .await
        .unwrap();
    assert!(matches!(outcome, PostCommitOutcome::Saved(record) if record.snapshot.version == 2));

    // 过期版本：StaleConflict，不覆盖。
    let mut stale_edit = snapshot.clone();
    stale_edit.title = "基于旧版本的并发修改".into();
    stale_edit.version = 1;
    let outcome = repo
        .commit_post(
            &Post::reconstitute(stale_edit.clone()),
            1,
            OffsetDateTime::now_utc(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(outcome, PostCommitOutcome::StaleConflict);

    let current = repo.find_by_id(snapshot.id).await.unwrap().unwrap();
    assert_eq!(current.title, "第一次修改", "并发写入未覆盖最新值");
    assert_eq!(current.version, 2);

    // 记录消失（软删除）：Gone，重试无意义。
    sqlx::raw_sql("UPDATE posts SET deleted_at = now() WHERE slug = 'versioned'")
        .execute(&pool)
        .await
        .unwrap();
    let mut gone_edit = snapshot.clone();
    gone_edit.title = "写给已删除文章".into();
    let outcome = repo
        .commit_post(
            &Post::reconstitute(gone_edit.clone()),
            2,
            OffsetDateTime::now_utc(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        outcome,
        PostCommitOutcome::Gone,
        "软删除后的保存应报 Gone 而非冲突"
    );
}

#[tokio::test]
async fn truly_concurrent_saves_exactly_one_wins() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let author = seed_user(&pool, "author").await;

    let repo_a = PostgresPostRepository::new(
        pool.clone(),
        Arc::new(infrastructure::RenderingRuntime::default()),
    );
    let pool_b = connect(&test_db_url(&admin_url(), "blog_test"))
        .await
        .unwrap();
    let repo_b = PostgresPostRepository::new(
        pool_b,
        Arc::new(infrastructure::RenderingRuntime::default()),
    );

    let snapshot = draft_snapshot(author, "race-real");
    repo_a
        .insert_post(&Post::reconstitute(snapshot.clone()), &[])
        .await
        .unwrap();

    let mut edit_a = snapshot.clone();
    edit_a.title = "并发A".into();
    let mut edit_b = snapshot.clone();
    edit_b.title = "并发B".into();
    let now = OffsetDateTime::now_utc();

    let edit_a = Post::reconstitute(edit_a);
    let edit_b = Post::reconstitute(edit_b);

    // 两条真实连接同时 UPDATE 同一行，都带 expected_version=1。
    let (outcome_a, outcome_b) = tokio::join!(
        repo_a.commit_post(&edit_a, 1, now, None),
        repo_b.commit_post(&edit_b, 1, now, None)
    );
    let outcomes = [outcome_a.unwrap(), outcome_b.unwrap()];
    let saved = outcomes
        .iter()
        .filter(|o| matches!(o, PostCommitOutcome::Saved(record) if record.snapshot.version == 2))
        .count();
    let conflicted = outcomes
        .iter()
        .filter(|o| **o == PostCommitOutcome::StaleConflict)
        .count();
    assert_eq!(saved, 1, "恰好一个写入成功：{outcomes:?}");
    assert_eq!(conflicted, 1, "另一个必须是版本冲突：{outcomes:?}");

    let final_state = repo_a.find_by_id(snapshot.id).await.unwrap().unwrap();
    assert_eq!(final_state.version, 2);
    assert!(
        final_state.title == "并发A" || final_state.title == "并发B",
        "落盘的是胜者内容"
    );
}

#[tokio::test]
async fn public_query_filters_draft_private_and_deleted() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let author = seed_user(&pool, "author").await;
    let repo = PostgresPostRepository::new(
        pool.clone(),
        Arc::new(infrastructure::RenderingRuntime::default()),
    );
    let query = PostgresPublishedPostQuery::new(pool.clone());

    // 1. 公开发布
    let mut published = draft_snapshot(author, "public-one");
    {
        let mut post = Post::reconstitute(published.clone());
        post.publish(OffsetDateTime::now_utc()).unwrap();
        published = post.snapshot();
    }
    repo.insert_post(&Post::reconstitute(published.clone()), &[])
        .await
        .unwrap();

    // 2. 草稿
    repo.insert_post(
        &Post::reconstitute(draft_snapshot(author, "draft-one")),
        &[],
    )
    .await
    .unwrap();

    // 3. 发布但 private
    let mut private = draft_snapshot(author, "private-one");
    private.visibility = Visibility::Private;
    {
        let mut post = Post::reconstitute(private.clone());
        post.publish(OffsetDateTime::now_utc()).unwrap();
        private = post.snapshot();
    }
    repo.insert_post(&Post::reconstitute(private.clone()), &[])
        .await
        .unwrap();

    // 4. 软删除的已发布文章
    let mut deleted = draft_snapshot(author, "deleted-one");
    {
        let mut post = Post::reconstitute(deleted.clone());
        post.publish(OffsetDateTime::now_utc()).unwrap();
        deleted = post.snapshot();
    }
    repo.insert_post(&Post::reconstitute(deleted.clone()), &[])
        .await
        .unwrap();
    sqlx::raw_sql("UPDATE posts SET deleted_at = now() WHERE slug = 'deleted-one'")
        .execute(&pool)
        .await
        .unwrap();

    let list = query.list_public(50, 0).await.unwrap();
    assert_eq!(list.len(), 1, "只有公开已发布未删除的文章可见");
    assert_eq!(list[0].slug, "public-one");
    assert_eq!(list[0].author_display, "author的展示名");

    let found = query.find_public_by_slug("public-one").await.unwrap();
    assert!(found.is_some());
    for slug in ["draft-one", "private-one", "deleted-one"] {
        assert!(
            query.find_public_by_slug(slug).await.unwrap().is_none(),
            "{slug} 不应对匿名可见"
        );
    }
}

#[tokio::test]
async fn status_transitions_persisted_correctly() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let author = seed_user(&pool, "author").await;
    let repo = PostgresPostRepository::new(
        pool.clone(),
        Arc::new(infrastructure::RenderingRuntime::default()),
    );

    let snapshot = draft_snapshot(author, "lifecycle");
    repo.insert_post(&Post::reconstitute(snapshot.clone()), &[])
        .await
        .unwrap();

    let mut post = Post::reconstitute(repo.find_by_id(snapshot.id).await.unwrap().unwrap());
    post.publish(OffsetDateTime::now_utc()).unwrap();
    repo.commit_post(&post, 1, OffsetDateTime::now_utc(), None)
        .await
        .unwrap();

    let mut post = Post::reconstitute(repo.find_by_id(snapshot.id).await.unwrap().unwrap());
    assert_eq!(post.status(), PostStatus::Published);
    let first_published_at = post.snapshot().published_at;

    post.withdraw();
    repo.commit_post(&post, 2, OffsetDateTime::now_utc(), None)
        .await
        .unwrap();

    let post = Post::reconstitute(repo.find_by_id(snapshot.id).await.unwrap().unwrap());
    assert_eq!(post.status(), PostStatus::Draft);
    assert_eq!(
        post.snapshot().published_at,
        first_published_at,
        "撤回保留首次发布时间"
    );
}

#[tokio::test]
async fn markdown_renderer_sanitizes_unsafe_html() {
    let renderer = SanitizingMarkdownRenderer::new();

    let html = renderer
        .render_markdown("# 标题\n\n<script>alert('x')</script>\n\n[链接](https://example.com)");
    assert!(html.contains("<h1>标题</h1>"));
    assert!(!html.contains("<script"), "script 必须被清除");
    assert!(html.contains("href=\"https://example.com\""));

    let html = renderer.render_markdown("![img](javascript:alert(1))");
    assert!(!html.contains("javascript:"), "危险协议必须被清除");
}

// ---------------------------------------------------------------------------
// RBAC：权限目录同步、角色分配与 Owner 保护（真实数据库）
// ---------------------------------------------------------------------------

fn rbac_of(pool: &PgPool) -> PostgresRbacStore {
    PostgresRbacStore::new(pool.clone())
}

#[tokio::test]
async fn rbac_registry_sync_is_idempotent() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let rbac = rbac_of(&pool);

    rbac.sync_permission_registry(application::identity::PERMISSION_REGISTRY)
        .await
        .unwrap();
    rbac.sync_builtin_roles(application::identity::BUILTIN_ROLES)
        .await
        .unwrap();

    let versions_after_first: Vec<(String, i64)> =
        sqlx::query_as("SELECT slug, version FROM roles ORDER BY slug")
            .fetch_all(&pool)
            .await
            .unwrap();

    // 第二次同步（幂等）不得翻倍、漂移，也不得递增 roles.version。
    rbac.sync_permission_registry(application::identity::PERMISSION_REGISTRY)
        .await
        .unwrap();
    rbac.sync_builtin_roles(application::identity::BUILTIN_ROLES)
        .await
        .unwrap();

    let versions_after_second: Vec<(String, i64)> =
        sqlx::query_as("SELECT slug, version FROM roles ORDER BY slug")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        versions_after_first, versions_after_second,
        "重复同步不得递增 roles.version（无变化不动行）"
    );

    let perm_count: i64 = sqlx::query_scalar("SELECT count(*) FROM permissions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        perm_count as usize,
        application::identity::PERMISSION_REGISTRY.len(),
        "权限目录与注册表一致"
    );

    let author_perms: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM role_permissions rp \
         JOIN roles r ON r.id = rp.role_id WHERE r.slug = 'author'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        author_perms, 9,
        "Author 恰好 9 个动作：6 个文章 own 动作（含回收站）+ 3 个媒体动作"
    );

    // Owner 持有全部已注册权限（含 oauth.manage / ownership.manage）。
    let owner_perms: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM role_permissions rp \
         JOIN roles r ON r.id = rp.role_id WHERE r.slug = 'owner'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        owner_perms as usize,
        application::identity::PERMISSION_REGISTRY.len(),
        "Owner 持有全部已注册权限"
    );

    let owner_keys = rbac.permissions_of_role("owner").await.unwrap();
    assert!(owner_keys.has("ownership.manage"));
    assert!(owner_keys.has("oauth.manage"));
    let err = rbac.permissions_of_role("ghost").await.unwrap_err();
    assert!(matches!(err, application::error::UseCaseError::NotFound(_)));

    let roles: Vec<String> = sqlx::query_scalar("SELECT slug FROM roles ORDER BY slug")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(roles, vec!["admin", "author", "editor", "owner"]);
}

#[tokio::test]
async fn rbac_permissions_union_and_version_bump() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let rbac = rbac_of(&pool);
    rbac.sync_permission_registry(application::identity::PERMISSION_REGISTRY)
        .await
        .unwrap();
    rbac.sync_builtin_roles(application::identity::BUILTIN_ROLES)
        .await
        .unwrap();

    let uid = seed_user(&pool, "member").await;
    let (version_before,): (i64,) = sqlx::query_as("SELECT version FROM users WHERE id = $1")
        .bind(uid)
        .fetch_one(&pool)
        .await
        .unwrap();

    // 未知角色拒绝。
    let err = rbac.assign_role(uid, "ghost").await.unwrap_err();
    assert!(matches!(err, application::error::UseCaseError::NotFound(_)));

    rbac.assign_role(uid, "author").await.unwrap();
    rbac.assign_role(uid, "editor").await.unwrap();
    // 重复分配幂等，不重复递增版本。
    rbac.assign_role(uid, "author").await.unwrap();

    let (version_after,): (i64,) = sqlx::query_as("SELECT version FROM users WHERE id = $1")
        .bind(uid)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        version_after,
        version_before + 2,
        "两次新分配各递增一次 users.version"
    );

    // 并集：own（post.create）+ any（post.update_any）同时可见。
    let perms = rbac.permissions_of_user(uid).await.unwrap();
    assert!(perms.has("post.create"), "author 的 own 动作");
    assert!(perms.has("post.update_any"), "editor 的 any 动作");
    assert!(!perms.has("role.manage"), "未授予的管理动作不可见");

    let roles = rbac.roles_of_user(uid).await.unwrap();
    assert_eq!(roles, vec!["author", "editor"]);
}

#[tokio::test]
async fn rbac_last_owner_protection() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let rbac = rbac_of(&pool);
    rbac.sync_permission_registry(application::identity::PERMISSION_REGISTRY)
        .await
        .unwrap();
    rbac.sync_builtin_roles(application::identity::BUILTIN_ROLES)
        .await
        .unwrap();

    let u1 = seed_user(&pool, "first").await;
    let u2 = seed_user(&pool, "second").await;
    seed_binding(&pool, u1).await;
    seed_binding(&pool, u2).await;

    rbac.assign_role(u1, "owner").await.unwrap();

    // 唯一 Owner：移除被拒。
    let err = rbac.remove_role(u1, "owner").await.unwrap_err();
    assert!(
        matches!(err, application::error::UseCaseError::LastOwnerProtected),
        "最后 Owner 不能被移除：{err:?}"
    );

    // 第二个 Owner 后，允许移除第一个。
    rbac.assign_role(u2, "owner").await.unwrap();
    rbac.remove_role(u1, "owner").await.unwrap();

    let owners: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM user_roles ur \
         JOIN roles r ON r.id = ur.role_id \
         JOIN users u ON u.id = ur.user_id \
         WHERE r.slug = 'owner' AND u.deleted_at IS NULL",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(owners, 1);

    // 软删除用户不计入有效 Owner：u1 重新持有 owner，u2 被软删除后
    // u1 成为唯一可登录 Owner，不可再被移除。
    rbac.assign_role(u1, "owner").await.unwrap();
    sqlx::query("UPDATE users SET deleted_at = now() WHERE id = $1")
        .bind(u2)
        .execute(&pool)
        .await
        .unwrap();
    let err = rbac.remove_role(u1, "owner").await.unwrap_err();
    assert!(
        matches!(err, application::error::UseCaseError::LastOwnerProtected),
        "软删除的 Owner 不计入有效数量"
    );
}

#[tokio::test]
async fn owner_without_login_method_does_not_satisfy_last_owner_guard() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let rbac = rbac_of(&pool);
    rbac.sync_permission_registry(application::identity::PERMISSION_REGISTRY)
        .await
        .unwrap();
    rbac.sync_builtin_roles(application::identity::BUILTIN_ROLES)
        .await
        .unwrap();

    let bound = seed_user(&pool, "bound").await;
    let unbound = seed_user(&pool, "unbound").await;
    rbac.assign_role(bound, "owner").await.unwrap();
    rbac.assign_role(unbound, "owner").await.unwrap();
    // 只有 bound 有有效登录方式；unbound 是「登不进去的 Owner」。
    seed_binding(&pool, bound).await;

    // 移除唯一可登录的 Owner 会留下无法登录的 Owner → 拒绝（docs §3）。
    let err = rbac.remove_role(bound, "owner").await.unwrap_err();
    assert!(
        matches!(err, application::error::UseCaseError::LastOwnerProtected),
        "无登录方式的 Owner 不构成有效 Owner：{err:?}"
    );

    // 给 unbound 绑定登录方式后，才允许移除 bound。
    seed_binding(&pool, unbound).await;
    assert!(rbac.remove_role(bound, "owner").await.is_ok());
}

#[tokio::test]
async fn owner_without_login_method_can_be_cleaned_up() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let rbac = rbac_of(&pool);
    rbac.sync_permission_registry(application::identity::PERMISSION_REGISTRY)
        .await
        .unwrap();
    rbac.sync_builtin_roles(application::identity::BUILTIN_ROLES)
        .await
        .unwrap();

    let bound = seed_user(&pool, "bound").await;
    let unbound = seed_user(&pool, "unbound").await;
    rbac.assign_role(bound, "owner").await.unwrap();
    rbac.assign_role(unbound, "owner").await.unwrap();
    seed_binding(&pool, bound).await;

    // 移除登不进去的 Owner 不会减少可用 Owner，允许清理。
    assert!(rbac.remove_role(unbound, "owner").await.is_ok());
    // bound 仍是最后可登录 Owner，受保护。
    let err = rbac.remove_role(bound, "owner").await.unwrap_err();
    assert!(matches!(
        err,
        application::error::UseCaseError::LastOwnerProtected
    ));
}

/// 回归：本地密码是有效登录方式，只用密码（无 oauth）的最后 Owner 必须受保护。
///
/// 早先 `active_owner_count` / `user_has_login_method` 只看 oauth_accounts，
/// 导致密码型 Owner 被当成「登不进去」而移除，站点直接失去 Owner。
#[tokio::test]
async fn password_only_last_owner_is_protected() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let rbac = rbac_of(&pool);
    rbac.sync_permission_registry(application::identity::PERMISSION_REGISTRY)
        .await
        .unwrap();
    rbac.sync_builtin_roles(application::identity::BUILTIN_ROLES)
        .await
        .unwrap();

    let alice = seed_user(&pool, "alice").await;
    rbac.assign_role(alice, "owner").await.unwrap();
    // 关键：只有本地密码，没有任何 oauth 绑定。
    seed_password(&pool, alice).await;

    let err = rbac.remove_role(alice, "owner").await.unwrap_err();
    assert!(
        matches!(err, application::error::UseCaseError::LastOwnerProtected),
        "密码型最后 Owner 不能被移除：{err:?}"
    );

    // 第二个同样只用密码的 Owner 出现后，才允许移除第一个。
    let bob = seed_user(&pool, "bob").await;
    rbac.assign_role(bob, "owner").await.unwrap();
    seed_password(&pool, bob).await;
    assert!(rbac.remove_role(alice, "owner").await.is_ok());
    assert!(
        rbac.remove_role(bob, "owner").await.is_err(),
        "bob 成了最后 Owner"
    );
}

/// 回归：软删除的密码型 Owner 视为「登不进去」，可被清理，且不计入有效 Owner 数。
#[tokio::test]
async fn deleted_password_only_owner_can_be_cleaned_up() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let rbac = rbac_of(&pool);
    rbac.sync_permission_registry(application::identity::PERMISSION_REGISTRY)
        .await
        .unwrap();
    rbac.sync_builtin_roles(application::identity::BUILTIN_ROLES)
        .await
        .unwrap();

    let alice = seed_user(&pool, "alice").await;
    let bob = seed_user(&pool, "bob").await;
    rbac.assign_role(alice, "owner").await.unwrap();
    rbac.assign_role(bob, "owner").await.unwrap();
    seed_password(&pool, alice).await;
    seed_password(&pool, bob).await;

    // bob 被软删除后不再可登录：清理其 owner 角色不影响有效 Owner 数。
    sqlx::query("UPDATE users SET deleted_at = now() WHERE id = $1")
        .bind(bob)
        .execute(&pool)
        .await
        .unwrap();
    assert!(rbac.remove_role(bob, "owner").await.is_ok());

    // alice 现在是唯一可登录 Owner，受保护。
    let err = rbac.remove_role(alice, "owner").await.unwrap_err();
    assert!(matches!(
        err,
        application::error::UseCaseError::LastOwnerProtected
    ));
}

#[tokio::test]
async fn removing_role_the_user_does_not_hold_is_noop() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let rbac = rbac_of(&pool);
    rbac.sync_permission_registry(application::identity::PERMISSION_REGISTRY)
        .await
        .unwrap();
    rbac.sync_builtin_roles(application::identity::BUILTIN_ROLES)
        .await
        .unwrap();

    let owner = seed_user(&pool, "owner").await;
    let plain = seed_user(&pool, "plain").await;
    rbac.assign_role(owner, "owner").await.unwrap();
    seed_binding(&pool, owner).await;

    // plain 本来就不是 owner：移除不应因全局 Owner 数而误报 Forbidden。
    assert!(rbac.remove_role(plain, "owner").await.is_ok());
    assert!(rbac.remove_role(plain, "author").await.is_ok());
    // 真正的最后 Owner 仍受保护。
    let err = rbac.remove_role(owner, "owner").await.unwrap_err();
    assert!(matches!(
        err,
        application::error::UseCaseError::LastOwnerProtected
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_last_owner_removal_keeps_at_least_one_loginable_owner() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let rbac = Arc::new(PostgresRbacStore::new(pool.clone()));
    rbac.sync_permission_registry(application::identity::PERMISSION_REGISTRY)
        .await
        .unwrap();
    rbac.sync_builtin_roles(application::identity::BUILTIN_ROLES)
        .await
        .unwrap();

    let u1 = seed_user(&pool, "owner1").await;
    let u2 = seed_user(&pool, "owner2").await;
    rbac.assign_role(u1, "owner").await.unwrap();
    rbac.assign_role(u2, "owner").await.unwrap();
    seed_binding(&pool, u1).await;
    seed_binding(&pool, u2).await;

    // 两个并发移除：排他锁 + 锁内复核后应恰好一个成功，至少保留一个可登录 Owner。
    let (a, b) = tokio::join!(rbac.remove_role(u1, "owner"), rbac.remove_role(u2, "owner"));
    let succeeded = [a.is_ok(), b.is_ok()].into_iter().filter(|ok| *ok).count();
    assert_eq!(succeeded, 1, "并发移除只能成功一个：{a:?} / {b:?}");

    let remaining: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM user_roles ur \
         JOIN roles r ON r.id = ur.role_id \
         JOIN users u ON u.id = ur.user_id \
         WHERE r.slug = 'owner' AND u.deleted_at IS NULL \
           AND EXISTS (SELECT 1 FROM oauth_accounts oa WHERE oa.user_id = u.id)",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(remaining, 1, "并发后仍保留一个可登录 Owner");
}

#[tokio::test]
async fn page_repository_crud_version_and_public_query() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let pages: Arc<dyn PageRepository> = Arc::new(PostgresPageRepository::new(
        pool.clone(),
        Arc::new(infrastructure::RenderingRuntime::default()),
    ));
    let public: Arc<dyn PublishedPageQuery> = Arc::new(PostgresPublishedPageQuery::new(pool));

    let now = OffsetDateTime::now_utc();
    let mut page = Page::create_draft(
        Slug::new("about").unwrap(),
        "关于".into(),
        "# 关于\n\n正文".into(),
        Visibility::Public,
        now,
    )
    .unwrap();
    assert_eq!(pages.insert_page(&page).await.unwrap(), page.snapshot());

    // pages.slug 唯一：不同 id、相同 slug 的插入映射为 Conflict(Slug)。
    let duplicate = Page::create_draft(
        Slug::new("about").unwrap(),
        "重复页面".into(),
        "x".into(),
        Visibility::Public,
        now,
    )
    .unwrap();
    let err = pages.insert_page(&duplicate).await.unwrap_err();
    assert!(
        matches!(err, UseCaseError::Conflict(ConflictKind::Slug)),
        "{err:?}"
    );

    // 草稿不进入公开查询。
    assert!(
        public.find_public_by_slug("about").await.unwrap().is_none(),
        "草稿页面不可公开读取"
    );

    // 过期版本：StaleConflict（可重试），不是 Gone。
    assert_eq!(
        pages.commit_page(&page, 99, now).await.unwrap(),
        PageCommitOutcome::StaleConflict
    );

    // 发布并写入：命中版本后 +1。
    page.publish(now).unwrap();
    let PageCommitOutcome::Saved(snapshot) = pages.commit_page(&page, 1, now).await.unwrap() else {
        panic!("publish must succeed");
    };
    assert_eq!(snapshot.version, 2);

    let detail = public
        .find_public_by_slug("about")
        .await
        .unwrap()
        .expect("已发布页面应可公开读取");
    assert_eq!(detail.title, "关于");
    assert_eq!(detail.content_html, "<h1>关于</h1>\n<p>正文</p>\n");
    assert!(detail.published_at.is_some());

    // 站点级列表返回全部页面（无作者维度）。
    assert_eq!(pages.list().await.unwrap().len(), 1);

    // 改为 private：立即退出公开集合（页面无软删除，只有状态与可见性）。
    let mut private = Page::reconstitute(snapshot.clone());
    private
        .edit(PagePatch {
            visibility: Some(Visibility::Private),
            ..Default::default()
        })
        .unwrap();
    let PageCommitOutcome::Saved(private) = pages.commit_page(&private, 2, now).await.unwrap()
    else {
        panic!("visibility edit must succeed");
    };
    assert_eq!(private.version, 3);
    assert!(
        public.find_public_by_slug("about").await.unwrap().is_none(),
        "private 页面不可公开读取"
    );
    assert_eq!(
        pages.delete(snapshot.id, 2).await.unwrap(),
        PageDeleteOutcome::StaleVersion
    );
    assert_eq!(
        pages.delete(snapshot.id, 3).await.unwrap(),
        PageDeleteOutcome::Deleted
    );
    assert_eq!(
        pages.delete(snapshot.id, 3).await.unwrap(),
        PageDeleteOutcome::Gone
    );
    assert!(pages.find_by_id(snapshot.id).await.unwrap().is_none());
    pages.insert_page(&duplicate).await.unwrap();
    assert_ne!(
        pages
            .find_by_id(duplicate.snapshot().id)
            .await
            .unwrap()
            .unwrap()
            .id,
        snapshot.id
    );

    // 同一版本的并发删除只能成功一次；另一次必须看到记录已不存在。
    let race = Page::create_draft(
        Slug::new("race-page").unwrap(),
        "并发删除".into(),
        "正文".into(),
        Visibility::Public,
        now,
    )
    .unwrap();
    let race = pages.insert_page(&race).await.unwrap();
    let (left, right) = tokio::join!(pages.delete(race.id, 1), pages.delete(race.id, 1));
    let outcomes = [left.unwrap(), right.unwrap()];
    assert!(outcomes.contains(&PageDeleteOutcome::Deleted));
    assert!(outcomes.contains(&PageDeleteOutcome::Gone));
}

/// 密码凭据的持久化语义：写入递增版本、软删除用户在登录查询层被排除、清除后不再可登录。
#[tokio::test]
async fn password_credentials_are_stored_and_scoped_to_active_users() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let users = PostgresUserRepository::new(pool.clone());

    let user_id = seed_user(&pool, "sun").await;
    // 未设置密码：登录查询返回 None。
    assert!(
        users
            .find_password_credential("sun")
            .await
            .unwrap()
            .is_none()
    );
    assert!(users.password_hash_of(user_id).await.unwrap().is_none());

    let before: i64 = sqlx::query_scalar("SELECT version FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    users
        .set_password_hash(user_id, "$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA")
        .await
        .unwrap();

    let credential = users
        .find_password_credential("sun")
        .await
        .unwrap()
        .expect("设置后应可登录查询到凭据");
    assert_eq!(credential.user_id, user_id);
    assert!(credential.password_hash.starts_with("$argon2id$"));
    let after: i64 = sqlx::query_scalar("SELECT version FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(after, before + 1, "凭据变更必须递增 users.version");

    // 条件写入（登录升级 / 自助改密）：期望值不匹配时拒绝，匹配时才更新。
    let current = credential.password_hash.clone();
    assert!(
        users
            .compare_and_set_password_hash(user_id, Some("$argon2id$stale"), "$argon2id$upgraded")
            .await
            .unwrap()
            .is_none(),
        "expected 不匹配时不得覆盖存储值"
    );
    assert_eq!(
        users.password_hash_of(user_id).await.unwrap().as_deref(),
        Some(current.as_str())
    );
    assert_eq!(
        users
            .compare_and_set_password_hash(user_id, Some(&current), "$argon2id$upgraded")
            .await
            .unwrap(),
        Some(after + 1),
        "expected 匹配时应完成升级"
    );
    assert_eq!(
        users.password_hash_of(user_id).await.unwrap().as_deref(),
        Some("$argon2id$upgraded")
    );

    // `expected = None` 表示「当前必须为空」：已有密码时不得写入（OAuth 用户设初始密码）。
    assert!(
        users
            .compare_and_set_password_hash(user_id, None, "$argon2id$initial")
            .await
            .unwrap()
            .is_none(),
        "当前已有密码时，None 期望不得写入"
    );
    assert_eq!(
        users.password_hash_of(user_id).await.unwrap().as_deref(),
        Some("$argon2id$upgraded")
    );

    // 软删除后：登录查询不再返回（登录失败路径因此无法区分「不存在」与「已停用」）。
    sqlx::query("UPDATE users SET deleted_at = now() WHERE id = $1")
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        users
            .find_password_credential("sun")
            .await
            .unwrap()
            .is_none()
    );
    assert!(users.password_hash_of(user_id).await.unwrap().is_none());

    // 清除密码（恢复账号后）：哈希确实消失，且版本继续递增。
    sqlx::query("UPDATE users SET deleted_at = NULL WHERE id = $1")
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();
    users.clear_password_hash(user_id).await.unwrap();
    assert!(users.password_hash_of(user_id).await.unwrap().is_none());
    assert!(
        users
            .find_password_credential("sun")
            .await
            .unwrap()
            .is_none()
    );

    // 对不存在的用户写入/清除都返回 NotFound，而不是静默成功。
    let missing = uuid::Uuid::now_v7();
    assert!(matches!(
        users.set_password_hash(missing, "x").await.unwrap_err(),
        UseCaseError::NotFound(_)
    ));
    assert!(matches!(
        users.clear_password_hash(missing).await.unwrap_err(),
        UseCaseError::NotFound(_)
    ));
}

/// `clear_password_hash_guarded` 的三条分支：未启用 / 最后一种登录方式 / 可清除。
#[tokio::test]
async fn guarded_password_clear_requires_another_login_method() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let users = PostgresUserRepository::new(pool.clone());

    let user_id = seed_user(&pool, "solo").await;
    // 未启用密码。
    assert_eq!(
        users.clear_password_hash_guarded(user_id).await.unwrap(),
        ClearPasswordOutcome::NoPassword
    );

    // 有密码但没有外部身份：拒绝清除。
    seed_password(&pool, user_id).await;
    assert_eq!(
        users.clear_password_hash_guarded(user_id).await.unwrap(),
        ClearPasswordOutcome::LastLoginMethod
    );
    assert!(
        users.password_hash_of(user_id).await.unwrap().is_some(),
        "拒绝时必须保留密码"
    );

    // 补一个外部身份后才可以清除。
    seed_binding(&pool, user_id).await;
    assert_eq!(
        users.clear_password_hash_guarded(user_id).await.unwrap(),
        ClearPasswordOutcome::Cleared
    );
    assert!(users.password_hash_of(user_id).await.unwrap().is_none());

    // 不存在的用户返回 NotFound，而不是静默成功。
    let missing = uuid::Uuid::now_v7();
    assert!(matches!(
        users
            .clear_password_hash_guarded(missing)
            .await
            .unwrap_err(),
        UseCaseError::NotFound(_)
    ));
}

/// 回归：清除密码与解绑外部身份并发时，不得把最后一种登录方式也去掉。
///
/// 两条路径分开做「检查 + 写入」会各自看到「对方还在」而同时通过，
/// 最终把账号的登录方式清空（write skew）。修法是把二者放进同一把身份排他锁。
#[tokio::test]
async fn clear_password_and_unbind_cannot_both_remove_the_last_login_method() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let users = PostgresUserRepository::new(pool.clone());
    let accounts = PostgresOAuthAccountStore::new(pool.clone());

    let user_id = seed_user(&pool, "dual").await;
    seed_binding(&pool, user_id).await;
    seed_password(&pool, user_id).await;

    // 两个连接、两笔事务，真的并发。
    let external_id = format!("sub-{user_id}");
    let (clear_result, unbind_result) = tokio::join!(
        users.clear_password_hash_guarded(user_id),
        accounts.unbind(user_id, "https://idp.example", &external_id),
    );

    let cleared = matches!(clear_result, Ok(ClearPasswordOutcome::Cleared));
    let unbound = unbind_result.is_ok();
    assert!(
        cleared ^ unbound,
        "恰好一个成功：clear={clear_result:?} unbind={unbind_result:?}"
    );

    let remaining: i64 = sqlx::query_scalar(
        "SELECT (CASE WHEN password_hash IS NOT NULL THEN 1 ELSE 0 END)::bigint \
                + (SELECT count(*) FROM oauth_accounts WHERE user_id = $1) \
         FROM users WHERE id = $1",
    )
    .bind(user_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        remaining >= 1,
        "必须至少保留一种登录方式，实际剩 {remaining}"
    );
}

/// 身份排他锁确实被「清除密码」与「解绑外部身份」共同使用。
///
/// 这是上面那条并发用例的确定性版本：先让一条独立连接持锁，再验证两个操作都
/// **阻塞在锁上**，释放后才各自完成。若任一路径没取同一把锁，它就会立刻返回，
/// 断言随即失败——不依赖调度时序。
#[tokio::test]
async fn password_clear_and_unbind_both_block_on_the_identity_lock() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let users = PostgresUserRepository::new(pool.clone());
    let accounts = PostgresOAuthAccountStore::new(pool.clone());

    let user_id = seed_user(&pool, "locked").await;
    seed_binding(&pool, user_id).await;
    seed_password(&pool, user_id).await;
    let external_id = format!("sub-{user_id}");

    // 独立连接持有身份锁，直到 `release` 发出。
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let holder = {
        let pool = pool.clone();
        tokio::spawn(async move {
            let mut tx = pool.begin().await.unwrap();
            sqlx::query("SELECT pg_advisory_xact_lock($1::int, $2::int)")
                .bind(2048001)
                .bind(1)
                .execute(&mut *tx)
                .await
                .unwrap();
            let _ = release_rx.await;
            tx.commit().await.unwrap();
        })
    };

    // 等到锁真的被持有（轮询 pg_locks，不看时序）。
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let held: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_locks \
             WHERE locktype = 'advisory' AND classid = $1 AND objid = $2 AND objsubid = 2",
        )
        .bind(2048001_i32)
        .bind(1_i32)
        .fetch_one(&pool)
        .await
        .unwrap();
        if held > 0 {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "等待身份锁被持有超时");
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    // 两个操作都必须阻塞在锁上，而不是各自完成。
    let clear_blocked = tokio::time::timeout(
        std::time::Duration::from_millis(300),
        users.clear_password_hash_guarded(user_id),
    );
    let unbind_blocked = tokio::time::timeout(
        std::time::Duration::from_millis(300),
        accounts.unbind(user_id, "https://idp.example", &external_id),
    );
    let (clear_outcome, unbind_outcome) = tokio::join!(clear_blocked, unbind_blocked);
    assert!(
        clear_outcome.is_err(),
        "清除密码没有阻塞在身份锁上：{clear_outcome:?}"
    );
    assert!(
        unbind_outcome.is_err(),
        "解绑外部身份没有阻塞在身份锁上：{unbind_outcome:?}"
    );

    // 释放后两者都能正常推进，且不变式仍然成立。
    let _ = release_tx.send(());
    holder.await.unwrap();
    let cleared = matches!(
        users.clear_password_hash_guarded(user_id).await.unwrap(),
        ClearPasswordOutcome::Cleared | ClearPasswordOutcome::LastLoginMethod
    );
    assert!(cleared, "释放锁后应能给出确定结论");
    let remaining: i64 = sqlx::query_scalar(
        "SELECT (CASE WHEN password_hash IS NOT NULL THEN 1 ELSE 0 END)::bigint \
                + (SELECT count(*) FROM oauth_accounts WHERE user_id = $1) \
         FROM users WHERE id = $1",
    )
    .bind(user_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(remaining >= 1, "至少保留一种登录方式，实际剩 {remaining}");
}

#[tokio::test]
async fn duplicate_username_and_email_map_to_structured_conflicts() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let users = PostgresUserRepository::new(pool.clone());
    let now = OffsetDateTime::now_utc();

    let first = User::new("alice", Some("alice@example.com".into()), None, now)
        .unwrap()
        .snapshot();
    users.insert(&first).await.unwrap();

    // 用户名占用：唯一约束 users_username_key → Conflict(Username)。
    let same_name = User::new("alice", Some("other@example.com".into()), None, now)
        .unwrap()
        .snapshot();
    match users.insert(&same_name).await.unwrap_err() {
        UseCaseError::Conflict(ConflictKind::Username) => {}
        other => panic!("期望 Conflict(Username)，得到 {other:?}"),
    }

    // 邮箱占用（用户名不同）：users_email_key → Conflict(Email)。
    let same_email = User::new("bob", Some("alice@example.com".into()), None, now)
        .unwrap()
        .snapshot();
    match users.insert(&same_email).await.unwrap_err() {
        UseCaseError::Conflict(ConflictKind::Email) => {}
        other => panic!("期望 Conflict(Email)，得到 {other:?}"),
    }
}

#[tokio::test]
async fn admin_listing_reports_login_methods_and_roles_in_bulk() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let rbac = rbac_of(&pool);
    rbac.sync_permission_registry(application::identity::PERMISSION_REGISTRY)
        .await
        .unwrap();
    rbac.sync_builtin_roles(application::identity::BUILTIN_ROLES)
        .await
        .unwrap();

    let bound = seed_user(&pool, "bound").await;
    let password_only = seed_user(&pool, "password-only").await;
    seed_user(&pool, "plain").await;
    // 软删除账号即使仍有绑定也不可登录：与 active_owner_count 的谓词一致。
    let deleted = seed_user(&pool, "deleted").await;
    seed_binding(&pool, bound).await;
    seed_binding(&pool, deleted).await;
    seed_password(&pool, password_only).await;
    sqlx::query("UPDATE users SET deleted_at = now() WHERE id = $1")
        .bind(deleted)
        .execute(&pool)
        .await
        .unwrap();
    rbac.assign_role(bound, "owner").await.unwrap();
    rbac.assign_role(password_only, "editor").await.unwrap();

    let users = PostgresUserRepository::new(pool.clone());
    let rows = users.list_admin(50, 0).await.unwrap();
    assert_eq!(rows.len(), 4);
    let row = |name: &str| rows.iter().find(|r| r.username == name).unwrap();
    assert!(row("bound").can_login(), "oauth 绑定即一种登录方式");
    assert!(row("password-only").can_login(), "本地密码即一种登录方式");
    assert_eq!(row("password-only").external_identities, 0);
    assert!(!row("plain").can_login(), "既无密码也无外部身份");
    assert!(
        row("deleted").deleted && !row("deleted").can_login(),
        "软删除账号不可登录，即使仍有外部身份"
    );
    assert_eq!(row("deleted").external_identities, 1);

    // 批量角色查询与逐个查询结果一致，且不串号。
    let ids: Vec<uuid::Uuid> = rows.iter().map(|r| r.id).collect();
    let roles = rbac.roles_of_users(&ids).await.unwrap();
    assert_eq!(roles.len(), 2);
    assert!(roles.contains(&(bound, "owner".to_string())));
    assert!(roles.contains(&(password_only, "editor".to_string())));
}

#[tokio::test]
async fn admin_listing_paginates_in_stable_username_order() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    for username in ["carol", "alice", "bob"] {
        seed_user(&pool, username).await;
    }

    let users = PostgresUserRepository::new(pool.clone());
    let first = users.list_admin(2, 0).await.unwrap();
    let rest = users.list_admin(2, 2).await.unwrap();
    let names: Vec<&str> = first
        .iter()
        .chain(rest.iter())
        .map(|r| r.username.as_str())
        .collect();
    assert_eq!(names, ["alice", "bob", "carol"]);
}

// ---------------------------------------------------------------------------
// 标签目录与文章关联
// ---------------------------------------------------------------------------

/// 预置一个标签，返回快照。
async fn seed_tag(pool: &sqlx::PgPool, name: &str, slug: &str) -> domain::content::TagSnapshot {
    let tags = infrastructure::PostgresTagRepository::new(pool.clone());
    let tag = domain::content::Tag::new(
        name.into(),
        domain::content::post::Slug::new(slug).unwrap(),
        OffsetDateTime::now_utc(),
    )
    .unwrap();
    let snapshot = tag.snapshot();
    tags.insert(&snapshot).await.unwrap();
    snapshot
}

#[tokio::test]
async fn tag_slug_unique_conflict_maps_to_slug_conflict() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let repo = infrastructure::PostgresTagRepository::new(pool.clone());
    seed_tag(&pool, "Rust", "rust").await;

    let dup = domain::content::Tag::new(
        "另一个".into(),
        domain::content::post::Slug::new("rust").unwrap(),
        OffsetDateTime::now_utc(),
    )
    .unwrap();
    match repo.insert(&dup.snapshot()).await.unwrap_err() {
        UseCaseError::Conflict(ConflictKind::Slug) => {}
        other => panic!("期望 slug 冲突，得到 {other:?}"),
    }
}

#[tokio::test]
async fn tag_rename_is_versioned_and_slug_immutable() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let repo = infrastructure::PostgresTagRepository::new(pool.clone());
    let tag = seed_tag(&pool, "Rust", "rust").await;

    // 版本不匹配：CAS 未命中。
    assert!(
        repo.rename(tag.id, "新名", tag.version + 1)
            .await
            .unwrap()
            .is_none(),
        "过期版本不得写入"
    );
    // 版本匹配：改名成功并递增版本。
    let renamed = repo
        .rename(tag.id, "Rust 语言", tag.version)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(renamed.name, "Rust 语言");
    assert_eq!(renamed.version, tag.version + 1);
    assert_eq!(renamed.slug, "rust", "slug 不随改名变化");
}

#[tokio::test]
async fn tag_delete_refuses_references_including_drafts() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let author = seed_user(&pool, "author").await;
    let repo = infrastructure::PostgresTagRepository::new(pool.clone());
    let posts = PostgresPostRepository::new(
        pool.clone(),
        Arc::new(infrastructure::RenderingRuntime::default()),
    );
    let tag = seed_tag(&pool, "Rust", "rust").await;

    // 草稿文章（不可公开）也占用引用：引用保护不过滤可见性。
    let draft = draft_snapshot(author, "draft-tagged");
    posts
        .insert_post(&Post::reconstitute(draft.clone()), &[tag.id])
        .await
        .unwrap();

    match repo.delete(tag.id, tag.version).await.unwrap() {
        application::ports::TagDeleteOutcome::Referenced { count } => assert_eq!(count, 1),
        other => panic!("期望引用保护，得到 {other:?}"),
    }
    // 数据库 RESTRICT 是兜底：即便绕过业务检查也删不掉。
    let result = sqlx::query("DELETE FROM tags WHERE id = $1")
        .bind(tag.id)
        .execute(&pool)
        .await;
    assert!(result.is_err(), "FK RESTRICT 必须兜底拒绝");

    // 解除引用后删除成功。
    sqlx::query("DELETE FROM post_tags WHERE post_id = $1")
        .bind(draft.id)
        .execute(&pool)
        .await
        .unwrap();
    match repo.delete(tag.id, tag.version).await.unwrap() {
        application::ports::TagDeleteOutcome::Deleted => {}
        other => panic!("期望删除成功，得到 {other:?}"),
    }
}

#[tokio::test]
async fn post_tags_saved_in_same_transaction_as_content() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let author = seed_user(&pool, "author").await;
    let posts = PostgresPostRepository::new(
        pool.clone(),
        Arc::new(infrastructure::RenderingRuntime::default()),
    );
    let rust = seed_tag(&pool, "Rust", "rust").await;
    let essay = seed_tag(&pool, "随笔", "essay").await;

    // 创建即带标签（重复 id 去重）。
    let snapshot = draft_snapshot(author, "tagged-post");
    posts
        .insert_post(
            &Post::reconstitute(snapshot.clone()),
            &[rust.id, rust.id, essay.id],
        )
        .await
        .unwrap();
    let mut got = posts
        .find_record_by_id(snapshot.id)
        .await
        .unwrap()
        .unwrap()
        .tag_ids;
    got.sort();
    let mut want = vec![essay.id, rust.id];
    want.sort();
    assert_eq!(got, want, "重复关联去重为两条关系");

    // 仅替换标签（正文不变）：version 也递增。
    let edit = {
        let mut post = domain::content::Post::reconstitute(snapshot.clone());
        post.edit(domain::content::post::PostPatch::default())
            .unwrap();
        post.snapshot()
    };
    match posts
        .commit_post(
            &Post::reconstitute(edit.clone()),
            snapshot.version,
            OffsetDateTime::now_utc(),
            Some(&[essay.id]),
        )
        .await
        .unwrap()
    {
        PostCommitOutcome::Saved(record) => {
            assert_eq!(
                record.snapshot.version,
                snapshot.version + 1,
                "仅标签变化也 +1"
            );
        }
        other => panic!("期望保存成功，得到 {other:?}"),
    }
    assert_eq!(
        posts
            .find_record_by_id(snapshot.id)
            .await
            .unwrap()
            .unwrap()
            .tag_ids,
        vec![essay.id]
    );

    // 清空标签。
    let edit2 = {
        let mut post = domain::content::Post::reconstitute(edit.clone());
        post.edit(domain::content::post::PostPatch::default())
            .unwrap();
        post.snapshot()
    };
    posts
        .commit_post(
            &Post::reconstitute(edit2.clone()),
            edit.version + 1,
            OffsetDateTime::now_utc(),
            Some(&[]),
        )
        .await
        .unwrap();
    assert!(
        posts
            .find_record_by_id(snapshot.id)
            .await
            .unwrap()
            .unwrap()
            .tag_ids
            .is_empty()
    );
}

#[tokio::test]
async fn post_tag_association_rejects_unknown_tag_via_fk() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let author = seed_user(&pool, "author").await;
    let posts = PostgresPostRepository::new(
        pool.clone(),
        Arc::new(infrastructure::RenderingRuntime::default()),
    );
    let ghost = uuid::Uuid::now_v7();

    let snapshot = draft_snapshot(author, "ghost-tagged");
    match posts
        .insert_post(&Post::reconstitute(snapshot.clone()), &[ghost])
        .await
        .unwrap_err()
    {
        UseCaseError::Invalid(ref m) if m.contains("所选标签不存在") => {}
        other => panic!("期望可定位的标签不存在错误，得到 {other:?}"),
    }
    // 事务回滚：文章本身也不得残留。
    assert!(
        posts.find_by_id(snapshot.id).await.unwrap().is_none(),
        "半套写入不得对外可见"
    );
}

#[tokio::test]
async fn tag_directory_listing_counts_only_public_posts() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let author = seed_user(&pool, "author").await;
    let repo = infrastructure::PostgresTagRepository::new(pool.clone());
    let posts = PostgresPostRepository::new(
        pool.clone(),
        Arc::new(infrastructure::RenderingRuntime::default()),
    );
    let tag = seed_tag(&pool, "Rust", "rust").await;

    // 三篇挂同一标签：公开已发布、草稿、发布但 private。
    let mut public = draft_snapshot(author, "tag-public");
    {
        let mut post = domain::content::Post::reconstitute(public.clone());
        post.publish(OffsetDateTime::now_utc()).unwrap();
        public = post.snapshot();
    }
    posts
        .insert_post(&Post::reconstitute(public.clone()), &[tag.id])
        .await
        .unwrap();
    posts
        .insert_post(
            &Post::reconstitute(draft_snapshot(author, "tag-draft")),
            &[tag.id],
        )
        .await
        .unwrap();
    let mut private = draft_snapshot(author, "tag-private");
    private.visibility = Visibility::Private;
    {
        let mut post = domain::content::Post::reconstitute(private.clone());
        post.publish(OffsetDateTime::now_utc()).unwrap();
        private = post.snapshot();
    }
    posts
        .insert_post(&Post::reconstitute(private.clone()), &[tag.id])
        .await
        .unwrap();

    let list = repo.list().await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].public_post_count, 1, "公开计数不含草稿与私密");
    assert_eq!(repo.public_count(tag.id).await.unwrap(), 1);

    // existing_ids：存在性校验。
    let ghost = uuid::Uuid::now_v7();
    let existing = repo.existing_ids(&[tag.id, ghost]).await.unwrap();
    assert_eq!(existing, vec![tag.id]);
}

#[tokio::test]
async fn public_tag_page_lists_only_public_posts_and_paginates() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let author = seed_user(&pool, "author").await;
    let posts = PostgresPostRepository::new(
        pool.clone(),
        Arc::new(infrastructure::RenderingRuntime::default()),
    );
    let tag_query = infrastructure::PostgresPublishedTagQuery::new(pool.clone());
    let post_query = PostgresPublishedPostQuery::new(pool.clone());
    let tag = seed_tag(&pool, "Rust", "rust").await;

    // 3 篇公开 + 1 草稿（挂同标签）。
    for i in 1..=3 {
        let mut public = draft_snapshot(author, &format!("tag-page-{i}"));
        let mut post = domain::content::Post::reconstitute(public.clone());
        post.publish(OffsetDateTime::now_utc()).unwrap();
        public = post.snapshot();
        posts
            .insert_post(&Post::reconstitute(public.clone()), &[tag.id])
            .await
            .unwrap();
    }
    posts
        .insert_post(
            &Post::reconstitute(draft_snapshot(author, "tag-page-draft")),
            &[tag.id],
        )
        .await
        .unwrap();

    // 标签可见性查询。
    let found = tag_query
        .find_public_by_slug("rust")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.name, "Rust");
    assert!(
        tag_query
            .find_public_by_slug("ghost")
            .await
            .unwrap()
            .is_none()
    );

    // 分页：每页 2 条 → 第 1 页 2 篇、总数 3。
    let (page1, total) = tag_query
        .list_public_posts_by_tag("rust", 2, 0)
        .await
        .unwrap();
    assert_eq!(total, 3);
    assert_eq!(page1.len(), 2);
    let (page2, total2) = tag_query
        .list_public_posts_by_tag("rust", 2, 2)
        .await
        .unwrap();
    assert_eq!(total2, 3, "总数来自同一快照");
    assert_eq!(page2.len(), 1);
    assert!(
        page1
            .iter()
            .chain(page2.iter())
            .all(|p| p.slug != "tag-page-draft"),
        "草稿不出现在公开标签页"
    );

    // 文章详情带标签引用。
    let detail = post_query
        .find_public_by_slug("tag-page-1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(detail.tags.len(), 1);
    assert_eq!(detail.tags[0].slug, "rust");
    assert_eq!(detail.tags[0].name, "Rust");
}

// ---------------------------------------------------------------------------
// 分类树：防环、事务锁与引用保护
// ---------------------------------------------------------------------------

async fn seed_category(
    pool: &sqlx::PgPool,
    name: &str,
    slug: &str,
    parent: Option<uuid::Uuid>,
) -> domain::content::CategorySnapshot {
    let repo = infrastructure::PostgresCategoryRepository::new(pool.clone());
    let category = domain::content::Category::new(
        name.into(),
        domain::content::post::Slug::new(slug).unwrap(),
        parent,
        None,
        OffsetDateTime::now_utc(),
    )
    .unwrap();
    let snapshot = category.snapshot();
    repo.insert(&snapshot).await.unwrap();
    snapshot
}

#[tokio::test]
async fn category_move_rejects_cycles_even_indirect() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let repo = infrastructure::PostgresCategoryRepository::new(pool.clone());
    let a = seed_category(&pool, "A", "a", None).await;
    let b = seed_category(&pool, "B", "b", Some(a.id)).await;

    // 直接自父。
    let err = repo
        .update(a.id, "A", None, Some(a.id), a.version)
        .await
        .unwrap_err();
    assert!(matches!(err, UseCaseError::Invalid(_)), "得到 {err:?}");

    // 一级环：A 的父设为子 B。
    let err = repo
        .update(a.id, "A", None, Some(b.id), a.version)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("环"), "得到 {err:?}");

    // 二级环：A→C→B→A。
    let c = seed_category(&pool, "C", "c", Some(b.id)).await;
    let err = repo
        .update(a.id, "A", None, Some(c.id), a.version)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("环"), "得到 {err:?}");

    // 合法移动（叶子互换父）不受影响；父设为根也合法。
    let moved = repo
        .update(c.id, "C", None, Some(a.id), c.version)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(moved.parent_id, Some(a.id));
    let rooted = repo
        .update(c.id, "C", None, None, moved.version)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rooted.parent_id, None);
}

#[tokio::test]
async fn category_move_rejects_missing_parent_and_checks_version() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let repo = infrastructure::PostgresCategoryRepository::new(pool.clone());
    let a = seed_category(&pool, "A", "a", None).await;

    let ghost = uuid::Uuid::now_v7();
    let err = repo
        .update(a.id, "A", None, Some(ghost), a.version)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("父分类不存在"), "得到 {err:?}");

    assert!(
        repo.update(a.id, "新名", None, None, a.version + 5)
            .await
            .unwrap()
            .is_none(),
        "过期版本不得写入"
    );
}

#[tokio::test]
async fn category_delete_protects_posts_and_children() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let author = seed_user(&pool, "author").await;
    let posts = PostgresPostRepository::new(
        pool.clone(),
        Arc::new(infrastructure::RenderingRuntime::default()),
    );
    let repo = infrastructure::PostgresCategoryRepository::new(pool.clone());
    let parent = seed_category(&pool, "父", "parent", None).await;
    let child = seed_category(&pool, "子", "child", Some(parent.id)).await;

    // 子分类存在：父分类删除被拒。
    match repo.delete(parent.id, parent.version).await.unwrap() {
        application::ports::CategoryDeleteOutcome::Referenced {
            posts: 0,
            children: 1,
        } => {}
        other => panic!("期望子分类保护，得到 {other:?}"),
    }

    // 文章引用（含草稿）同样占用。
    let draft = draft_snapshot(author, "categorized-draft");
    posts
        .insert_post(&Post::reconstitute(draft.clone()), &[])
        .await
        .unwrap();
    sqlx::query("UPDATE posts SET category_id = $1 WHERE slug = 'categorized-draft'")
        .bind(child.id)
        .execute(&pool)
        .await
        .unwrap();
    match repo.delete(child.id, child.version).await.unwrap() {
        application::ports::CategoryDeleteOutcome::Referenced {
            posts: 1,
            children: 0,
        } => {}
        other => panic!("期望文章引用保护，得到 {other:?}"),
    }
    // FK RESTRICT 兜底。
    let result = sqlx::query("DELETE FROM categories WHERE id = $1")
        .bind(child.id)
        .execute(&pool)
        .await;
    assert!(result.is_err());

    // 解除引用后：先删子，再删父，成功。
    sqlx::query("UPDATE posts SET category_id = NULL WHERE slug = 'categorized-draft'")
        .execute(&pool)
        .await
        .unwrap();
    repo.delete(child.id, child.version).await.unwrap();
    repo.delete(parent.id, parent.version).await.unwrap();
}

#[tokio::test]
async fn post_category_saved_in_same_transaction_and_public_page_filters() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let author = seed_user(&pool, "author").await;
    let posts = PostgresPostRepository::new(
        pool.clone(),
        Arc::new(infrastructure::RenderingRuntime::default()),
    );
    let query = PostgresPublishedPostQuery::new(pool.clone());
    let cat_query = infrastructure::PostgresPublishedCategoryQuery::new(pool.clone());
    let cat = seed_category(&pool, "技术", "tech", None).await;

    // 创建即带分类；仅改分类也递增 version。
    let snapshot = draft_snapshot(author, "cat-post");
    posts
        .insert_post(&Post::reconstitute(snapshot.clone()), &[])
        .await
        .unwrap();
    let edit = {
        let mut s = snapshot.clone();
        s.category_id = Some(cat.id);
        s
    };
    match posts
        .commit_post(
            &Post::reconstitute(edit.clone()),
            snapshot.version,
            OffsetDateTime::now_utc(),
            None,
        )
        .await
        .unwrap()
    {
        PostCommitOutcome::Saved(record) => {
            assert_eq!(record.snapshot.version, snapshot.version + 1)
        }
        other => panic!("得到 {other:?}"),
    }

    // 公开分类页：草稿不可见；发布后可见；未知分类 404 语义（空）。
    let (page0, total0) = cat_query
        .list_public_posts_by_category("tech", 20, 0)
        .await
        .unwrap();
    assert_eq!((page0.len(), total0), (0, 0), "草稿不出现在分类页");
    let mut published = edit.clone();
    {
        let mut post = domain::content::Post::reconstitute(published.clone());
        post.publish(OffsetDateTime::now_utc()).unwrap();
        published = post.snapshot();
    }
    posts
        .commit_post(
            &Post::reconstitute(published.clone()),
            snapshot.version + 1,
            OffsetDateTime::now_utc(),
            None,
        )
        .await
        .unwrap();
    let (page1, total1) = cat_query
        .list_public_posts_by_category("tech", 20, 0)
        .await
        .unwrap();
    assert_eq!((page1.len(), total1), (1, 1));
    assert_eq!(page1[0].slug, "cat-post");

    // 详情带分类引用。
    let detail = query
        .find_public_by_slug("cat-post")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(detail.category.as_ref().unwrap().slug, "tech");

    // FK 兜底：并发删除分类后保存文章报可定位错误。
    sqlx::query("DELETE FROM posts WHERE slug = 'cat-post'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM categories WHERE id = $1")
        .bind(cat.id)
        .execute(&pool)
        .await
        .unwrap();
    let ghost_edit = {
        let mut s = draft_snapshot(author, "ghost-cat");
        s.category_id = Some(cat.id);
        s
    };
    match posts
        .insert_post(&Post::reconstitute(ghost_edit.clone()), &[])
        .await
        .unwrap_err()
    {
        UseCaseError::Invalid(ref m) if m.contains("分类") => {}
        other => panic!("期望分类不存在错误，得到 {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// 系列：重排锁协议、引用保护与公开页
// ---------------------------------------------------------------------------

async fn seed_series(
    pool: &sqlx::PgPool,
    name: &str,
    slug: &str,
) -> domain::content::SeriesSnapshot {
    let repo = infrastructure::PostgresSeriesRepository::new(pool.clone());
    let series = domain::content::Series::new(
        name.into(),
        domain::content::post::Slug::new(slug).unwrap(),
        None,
        OffsetDateTime::now_utc(),
    )
    .unwrap();
    let snapshot = series.snapshot();
    repo.insert(&snapshot).await.unwrap();
    snapshot
}

/// 建一篇挂入系列的文章（直接写库，绕过用例）。
async fn post_in_series(
    pool: &sqlx::PgPool,
    author: uuid::Uuid,
    series: uuid::Uuid,
    slug: &str,
    order: i32,
) -> PostSnapshot {
    let repo = PostgresPostRepository::new(
        pool.clone(),
        Arc::new(infrastructure::RenderingRuntime::default()),
    );
    let mut snapshot = draft_snapshot(author, slug);
    snapshot.series_id = Some(series);
    snapshot.series_order = Some(order);
    repo.insert_post(&Post::reconstitute(snapshot.clone()), &[])
        .await
        .unwrap();
    snapshot
}

#[tokio::test]
async fn series_reorder_rewrites_orders_and_bumps_versions() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let author = seed_user(&pool, "author").await;
    let s = seed_series(&pool, "指南", "guide").await;
    let a = post_in_series(&pool, author, s.id, "part-1", 1).await;
    let b = post_in_series(&pool, author, s.id, "part-2", 2).await;

    // 基线：两篇 post_in_series 的 insert 已递增系列版本两次。
    // 完整排列倒序。
    let outcome = repo_of(&pool)
        .reorder(s.id, s.version + 2, &[b.id, a.id])
        .await
        .unwrap();
    assert_eq!(
        outcome,
        application::ports::ReorderOutcome::Reordered {
            new_version: s.version + 3
        }
    );

    // 顺序重写为 1..n；posts.version 与 series.version 递增。
    let rows: Vec<(String, i32, i64)> = sqlx::query_as(
        "SELECT slug, series_order, version FROM posts WHERE series_id = $1 ORDER BY series_order",
    )
    .bind(s.id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![
            ("part-2".into(), 1, b.version + 1),
            ("part-1".into(), 2, a.version + 1),
        ]
    );
    let (sv,): (i64,) = sqlx::query_as("SELECT version FROM series WHERE id = $1")
        .bind(s.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(sv, s.version + 3);
}

fn repo_of(pool: &sqlx::PgPool) -> infrastructure::PostgresSeriesRepository {
    infrastructure::PostgresSeriesRepository::new(pool.clone())
}

#[tokio::test]
async fn series_reorder_rejects_stale_version_and_mismatched_membership() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let author = seed_user(&pool, "author").await;
    let s = seed_series(&pool, "指南", "guide").await;
    let a = post_in_series(&pool, author, s.id, "part-1", 1).await;
    let b = post_in_series(&pool, author, s.id, "part-2", 2).await;

    // 旧 series.version：拒绝（防旧目录重排）。
    assert_eq!(
        repo_of(&pool)
            .reorder(s.id, s.version + 99, &[a.id, b.id])
            .await
            .unwrap(),
        application::ports::ReorderOutcome::StaleSeriesVersion
    );
    // 不完整的集合：拒绝，不落任何写入。
    assert_eq!(
        repo_of(&pool)
            .reorder(s.id, s.version + 2, &[a.id])
            .await
            .unwrap(),
        application::ports::ReorderOutcome::MembershipMismatch
    );
    let (pa,): (i64,) = sqlx::query_as("SELECT version FROM posts WHERE id = $1")
        .bind(a.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(pa, a.version, "失败的重排不递增文章版本");
}

#[tokio::test]
async fn concurrent_reorders_exactly_one_wins_on_series_version() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let author = seed_user(&pool, "author").await;
    let s = seed_series(&pool, "并发", "race").await;
    let a = post_in_series(&pool, author, s.id, "r-1", 1).await;
    let b = post_in_series(&pool, author, s.id, "r-2", 2).await;

    // 两个连接都用同一 expected 版本竞争：恰好一个成功，另一个 StaleSeriesVersion。
    let pool2 = pool.clone();
    let base = s.version + 2; // 两篇 insert 已递增。
    let (first, second) = tokio::join!(
        async {
            repo_of(&pool)
                .reorder(s.id, base, &[a.id, b.id])
                .await
                .unwrap()
        },
        async {
            repo_of(&pool2)
                .reorder(s.id, base, &[b.id, a.id])
                .await
                .unwrap()
        },
    );
    let outcomes = [first, second];
    let ok = outcomes
        .iter()
        .filter(|o| {
            **o == application::ports::ReorderOutcome::Reordered {
                new_version: s.version + 3,
            }
        })
        .count();
    let stale = outcomes
        .iter()
        .filter(|o| **o == application::ports::ReorderOutcome::StaleSeriesVersion)
        .count();
    assert_eq!((ok, stale), (1, 1), "恰好一个重排成功：{outcomes:?}");
}

#[tokio::test]
async fn series_delete_protects_referenced_posts() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let author = seed_user(&pool, "author").await;
    let s = seed_series(&pool, "指南", "guide").await;
    post_in_series(&pool, author, s.id, "part-1", 1).await;

    match repo_of(&pool).delete(s.id, s.version + 1).await.unwrap() {
        application::ports::SeriesDeleteOutcome::Referenced { count } => assert_eq!(count, 1),
        other => panic!("期望引用保护，得到 {other:?}"),
    }
    let result = sqlx::query("DELETE FROM series WHERE id = $1")
        .bind(s.id)
        .execute(&pool)
        .await;
    assert!(result.is_err(), "FK RESTRICT 兜底");

    // 解除关联后可删。
    sqlx::query("UPDATE posts SET series_id = NULL, series_order = NULL WHERE series_id = $1")
        .bind(s.id)
        .execute(&pool)
        .await
        .unwrap();
    match repo_of(&pool).delete(s.id, s.version + 1).await.unwrap() {
        application::ports::SeriesDeleteOutcome::Deleted => {}
        other => panic!("期望删除成功，得到 {other:?}"),
    }
}

#[tokio::test]
async fn post_series_position_conflict_maps_to_series_position() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let author = seed_user(&pool, "author").await;
    let s = seed_series(&pool, "指南", "guide").await;
    let repo = PostgresPostRepository::new(
        pool.clone(),
        Arc::new(infrastructure::RenderingRuntime::default()),
    );
    post_in_series(&pool, author, s.id, "occ-1", 1).await;

    // 换到已占用的位置：unique 冲突翻译为 SeriesPosition。
    let snapshot = draft_snapshot(author, "occ-2");
    repo.insert_post(&Post::reconstitute(snapshot.clone()), &[])
        .await
        .unwrap();
    let edit = {
        let mut snap = snapshot.clone();
        snap.series_id = Some(s.id);
        snap.series_order = Some(1);
        snap
    };
    match repo
        .commit_post(
            &Post::reconstitute(edit.clone()),
            snapshot.version,
            OffsetDateTime::now_utc(),
            None,
        )
        .await
        .unwrap_err()
    {
        UseCaseError::Conflict(ConflictKind::SeriesPosition) => {}
        other => panic!("期望系列位置冲突，得到 {other:?}"),
    }

    // 公开系列页：按序号升序、只列公开成员（草稿占位造成空档但不外泄）。
    let query = infrastructure::PostgresPublishedSeriesQuery::new(pool.clone());
    let (posts, total) = query
        .list_public_posts_by_series("guide", 20, 0)
        .await
        .unwrap();
    assert_eq!((posts.len(), total), (0, 0), "草稿不出现在公开系列页");
}

#[tokio::test]
async fn post_series_edit_participates_in_series_version_protocol() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let author = seed_user(&pool, "author").await;
    let s = seed_series(&pool, "协议", "protocol").await;
    let repo = PostgresPostRepository::new(
        pool.clone(),
        Arc::new(infrastructure::RenderingRuntime::default()),
    );
    let series_repo = repo_of(&pool);
    let a = post_in_series(&pool, author, s.id, "pp-1", 1).await;
    let b = post_in_series(&pool, author, s.id, "pp-2", 2).await;

    // P1 回归：文章编辑改序号必须递增 series.version——
    // 否则手持旧系列版本的重排仍会成功，覆盖刚保存的位置。
    let moved = {
        let mut snap = a.clone();
        snap.series_order = Some(5);
        snap
    };
    match repo
        .commit_post(
            &Post::reconstitute(moved.clone()),
            a.version,
            OffsetDateTime::now_utc(),
            None,
        )
        .await
        .unwrap()
    {
        PostCommitOutcome::Saved(record) => assert_eq!(record.snapshot.version, a.version + 1),
        other => panic!("得到 {other:?}"),
    }
    let (sv,): (i64,) = sqlx::query_as("SELECT version FROM series WHERE id = $1")
        .bind(s.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    // 基线：post_in_series×2 的 insert 已递增过两次（创建即入系列）。
    assert_eq!(sv, s.version + 3, "insert×2 + 本次序号变化各递增一次");

    // 用编辑前的系列版本重排：现在必须被拒（旧目录失效）。
    assert_eq!(
        series_repo
            .reorder(s.id, s.version, &[a.id, b.id])
            .await
            .unwrap(),
        application::ports::ReorderOutcome::StaleSeriesVersion,
        "旧系列版本不得再重排"
    );

    // 退出系列同样递增（旧目录成员集已变）。
    let left = {
        let mut snap = moved.clone();
        snap.series_id = None;
        snap.series_order = None;
        snap
    };
    repo.commit_post(
        &Post::reconstitute(left.clone()),
        moved.version + 1,
        OffsetDateTime::now_utc(),
        None,
    )
    .await
    .unwrap();
    let (sv2,): (i64,) = sqlx::query_as("SELECT version FROM series WHERE id = $1")
        .bind(s.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(sv2, s.version + 4, "退出系列再递增一次");

    // 与文章归属无关的纯正文编辑不递增系列版本。
    let content_only = {
        let mut snap = left.clone();
        snap.content = "只改正文".into();
        snap
    };
    repo.commit_post(
        &Post::reconstitute(content_only.clone()),
        left.version + 1,
        OffsetDateTime::now_utc(),
        None,
    )
    .await
    .unwrap();
    let (sv3,): (i64,) = sqlx::query_as("SELECT version FROM series WHERE id = $1")
        .bind(s.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(sv3, s.version + 4, "纯正文编辑不动系列版本");

    // 创建即入系列：insert 也递增。
    let s2 = seed_series(&pool, "新系列", "fresh").await;
    post_in_series(&pool, author, s2.id, "pp-3", 1).await;
    let (sv4,): (i64,) = sqlx::query_as("SELECT version FROM series WHERE id = $1")
        .bind(s2.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(sv4, s2.version + 1, "创建即入系列递增 series.version");
}

// ---------------------------------------------------------------------------
// settings 的 site 分组：UPSERT + 版本 CAS、分组隔离与重启保留
// ---------------------------------------------------------------------------

fn site_value(title: &str, description: &str) -> application::ports::SiteSettingsValue {
    application::ports::SiteSettingsValue {
        title: Some(title.into()),
        description: Some(description.into()),
        logo_media_id: None,
    }
}

#[tokio::test]
async fn settings_site_upsert_and_version_cas() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let store = infrastructure::PostgresSettingsStore::new(pool.clone());

    // 未配置：find 返回 None。
    assert!(store.find_site().await.unwrap().is_none());

    // expected=0 且行不存在：插入，版本从 1 起。
    assert_eq!(
        store
            .save_site(
                &site_value("数据库标题", "数据库描述"),
                0,
                OffsetDateTime::now_utc()
            )
            .await
            .unwrap(),
        SaveOutcome::Saved { new_version: 1 }
    );
    let record = store.find_site().await.unwrap().unwrap();
    assert_eq!(record.version, 1);
    assert_eq!(
        (
            record.value.title.as_deref(),
            record.value.description.as_deref()
        ),
        (Some("数据库标题"), Some("数据库描述"))
    );

    // 行已存在再用 0 当前提：冲突，不覆盖。
    assert_eq!(
        store
            .save_site(&site_value("抢写", "抢写"), 0, OffsetDateTime::now_utc())
            .await
            .unwrap(),
        SaveOutcome::StaleConflict
    );

    // 版本匹配：替换并递增。
    assert_eq!(
        store
            .save_site(
                &site_value("新标题", "新描述"),
                1,
                OffsetDateTime::now_utc()
            )
            .await
            .unwrap(),
        SaveOutcome::Saved { new_version: 2 }
    );
    let record = store.find_site().await.unwrap().unwrap();
    assert_eq!(record.value.title.as_deref(), Some("新标题"));

    // 存储形态：schema_version 随写入落库（按分组自描述，读取侧忽略）。
    let (schema,): (i64,) =
        sqlx::query_as("SELECT (value->>'schema_version')::bigint FROM settings WHERE key='site'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(schema, 1);
    let keys: Vec<String> = sqlx::query_scalar(
        "SELECT jsonb_object_keys(value) FROM settings WHERE key='site' ORDER BY 1",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    // site 分组的写入形态固定：schema_version + 标题/描述 + logo 占位（null = 无 logo）。
    assert_eq!(
        keys,
        vec!["description", "logo_media_id", "schema_version", "title"]
    );

    // 旧版本前提再次写入：冲突，版本停在 2。
    assert_eq!(
        store
            .save_site(&site_value("过期", "过期"), 1, OffsetDateTime::now_utc())
            .await
            .unwrap(),
        SaveOutcome::StaleConflict
    );
    let (version,): (i64,) = sqlx::query_as("SELECT version FROM settings WHERE key='site'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(version, 2);
}

#[tokio::test]
async fn settings_partial_row_reads_missing_fields_as_none() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    // 手工/历史写入的不完整行：缺字段按 None 读出，由应用层逐字段回退。
    sqlx::query(
        "INSERT INTO settings (key, value) VALUES ('site', \
         '{\"schema_version\":1,\"title\":\"手工标题\"}'::jsonb)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let store = infrastructure::PostgresSettingsStore::new(pool);
    let record = store.find_site().await.unwrap().unwrap();
    assert_eq!(record.value.title.as_deref(), Some("手工标题"));
    assert_eq!(record.value.description, None);
}

#[tokio::test]
async fn settings_row_survives_new_pool_and_keeps_oauth_group_isolated() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let dsn = admin_url();
    let test_dsn = test_db_url(&dsn, "blog_test");

    let first = infrastructure::PostgresSettingsStore::new(pool);
    first
        .save_site(
            &site_value("持久标题", "持久描述"),
            0,
            OffsetDateTime::now_utc(),
        )
        .await
        .unwrap();

    // 「重启」：丢弃原连接池，用全新连接读同一数据库，配置仍在。
    let pool2 = connect(&test_dsn).await.unwrap();
    let second = infrastructure::PostgresSettingsStore::new(pool2.clone());
    let record = second.find_site().await.unwrap().unwrap();
    assert_eq!(record.value.title.as_deref(), Some("持久标题"));
    assert_eq!(record.version, 1);

    // oauth 分组与 site 分组物理隔离：互不触碰对方的行。
    use application::ports::OAuthConfigStore;
    let oauth = infrastructure::PostgresOAuthConfigStore::new(pool2.clone());
    oauth
        .save(&[application::ports::ProviderConfig {
            id: "idp".into(),
            name: None,
            kind: application::ports::ProviderKind::Oidc,
            issuer: Some("https://idp.example".into()),
            client_id: "client".into(),
            secret_ref: "IDP_SECRET".into(),
            scopes: vec![],
        }])
        .await
        .unwrap();
    second
        .save_site(
            &site_value("再改一次", "描述"),
            1,
            OffsetDateTime::now_utc(),
        )
        .await
        .unwrap();
    assert_eq!(
        oauth.list().await.unwrap().len(),
        1,
        "site 写入不影响 oauth 分组"
    );
    let keys: Vec<String> = sqlx::query_scalar("SELECT key FROM settings ORDER BY key")
        .fetch_all(&pool2)
        .await
        .unwrap();
    assert_eq!(keys, vec!["oauth", "site"]);
}

#[tokio::test]
async fn settings_concurrent_saves_on_two_connections_exactly_one_wins() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let pool2 = connect(&test_db_url(&admin_url(), "blog_test"))
        .await
        .unwrap();

    let a = infrastructure::PostgresSettingsStore::new(pool);
    let b = infrastructure::PostgresSettingsStore::new(pool2);
    a.save_site(&site_value("初版", "描述"), 0, OffsetDateTime::now_utc())
        .await
        .unwrap();

    let (ra, rb) = {
        let left = site_value("连接甲", "描述");
        let right = site_value("连接乙", "描述");
        let now = OffsetDateTime::now_utc();
        tokio::join!(a.save_site(&left, 1, now), b.save_site(&right, 1, now),)
    };
    let outcomes = [ra.unwrap(), rb.unwrap()];
    assert_eq!(
        outcomes
            .iter()
            .filter(|o| matches!(o, SaveOutcome::Saved { .. }))
            .count(),
        1,
        "两连接并发保存恰好一方成功：{outcomes:?}"
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|o| matches!(o, SaveOutcome::StaleConflict))
            .count(),
        1
    );
    let record = a.find_site().await.unwrap().unwrap();
    assert_eq!(record.version, 2, "只递增一次");
}

#[tokio::test]
async fn trash_restore_purge_and_series_reorder_obey_versions() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let author = seed_user(&pool, "trash_author").await;
    let posts = PostgresPostRepository::new(
        pool.clone(),
        Arc::new(infrastructure::RenderingRuntime::default()),
    );
    let series = infrastructure::PostgresSeriesRepository::new(pool.clone());
    let series_id = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO series (id, name, slug, version) VALUES ($1, '回收测试', 'trash-series', 1)",
    )
    .bind(series_id)
    .execute(&pool)
    .await
    .unwrap();
    let mut a = draft_snapshot(author, "trash-a");
    a.series_id = Some(series_id);
    a.series_order = Some(1);
    a.status = PostStatus::Published;
    a.published_at = Some(OffsetDateTime::now_utc());
    let mut b = draft_snapshot(author, "trash-b");
    b.series_id = Some(series_id);
    b.series_order = Some(2);
    posts
        .insert_post(&Post::reconstitute(a.clone()), &[])
        .await
        .unwrap();
    posts
        .insert_post(&Post::reconstitute(b.clone()), &[])
        .await
        .unwrap();
    let now = OffsetDateTime::now_utc();
    let mut a_post = Post::reconstitute(a.clone());
    assert!(a_post.trash(now));
    assert!(matches!(
        posts.commit_lifecycle(&a_post, a.version, now).await.unwrap(),
        PostCommitOutcome::Saved(record) if record.snapshot.version == 2
    ));
    assert!(
        posts
            .list_by_author(author)
            .await
            .unwrap()
            .iter()
            .all(|p| p.id != a.id)
    );
    assert_eq!(
        posts.list_trash_by_author(author, 20, 0).await.unwrap().1,
        1
    );
    assert!(
        PostgresPublishedPostQuery::new(pool.clone())
            .find_public_by_slug("trash-a")
            .await
            .unwrap()
            .is_none()
    );
    let public = PostgresPublishedPostQuery::new(pool.clone());
    assert!(
        public
            .list_public(20, 0)
            .await
            .unwrap()
            .iter()
            .all(|p| p.slug != "trash-a")
    );
    assert!(
        public
            .list_public_for_sitemap(20)
            .await
            .unwrap()
            .iter()
            .all(|p| p.slug != "trash-a")
    );
    assert!(a_post.restore());
    assert_eq!(
        posts.commit_lifecycle(&a_post, 1, now).await.unwrap(),
        PostCommitOutcome::StaleConflict
    );
    assert!(matches!(
        posts.commit_lifecycle(&a_post, 2, now).await.unwrap(),
        PostCommitOutcome::Saved(record) if record.snapshot.version == 3
    ));
    let restored = posts.find_by_id(a.id).await.unwrap().unwrap();
    assert_eq!(restored.status, PostStatus::Draft);
    assert!(restored.published_at.is_some());
    assert!(a_post.trash(now));
    assert!(matches!(
        posts.commit_lifecycle(&a_post, 3, now).await.unwrap(),
        PostCommitOutcome::Saved(record) if record.snapshot.version == 4
    ));
    let (series_version,): (i64,) = sqlx::query_as("SELECT version FROM series WHERE id = $1")
        .bind(series_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let order = [b.id, a.id];
    let (purge, reorder) = tokio::join!(
        posts.purge(a.id, 4),
        series.reorder(series_id, series_version, &order)
    );
    let purge = purge.unwrap();
    let reorder = reorder.unwrap();
    match (purge, reorder) {
        (SaveOutcome::Saved { .. }, application::ports::ReorderOutcome::StaleSeriesVersion) => {
            assert!(posts.find_by_id(a.id).await.unwrap().is_none());
            assert_eq!(series.members_of(series_id).await.unwrap().len(), 1);
        }
        (SaveOutcome::StaleConflict, application::ports::ReorderOutcome::Reordered { .. }) => {
            assert!(posts.find_by_id(a.id).await.unwrap().is_some());
            assert_eq!(series.members_of(series_id).await.unwrap().len(), 2);
        }
        other => panic!("并发结果不符合系列版本协议：{other:?}"),
    }
}

#[tokio::test]
async fn purge_releases_slug_and_cascades_tags_only_after_trash() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let author = seed_user(&pool, "purge_author").await;
    let posts = PostgresPostRepository::new(
        pool.clone(),
        Arc::new(infrastructure::RenderingRuntime::default()),
    );
    let tag_id = uuid::Uuid::now_v7();
    sqlx::query("INSERT INTO tags (id, name, slug, version) VALUES ($1, '标签', 'purge-tag', 1)")
        .bind(tag_id)
        .execute(&pool)
        .await
        .unwrap();
    let post = draft_snapshot(author, "purge-slug");
    posts
        .insert_post(&Post::reconstitute(post.clone()), &[tag_id])
        .await
        .unwrap();
    assert_eq!(posts.purge(post.id, 1).await.unwrap(), SaveOutcome::Gone);
    assert_eq!(
        posts
            .find_record_by_id(post.id)
            .await
            .unwrap()
            .unwrap()
            .tag_ids,
        vec![tag_id]
    );
    let now = OffsetDateTime::now_utc();
    let mut trashed = Post::reconstitute(post.clone());
    assert!(trashed.trash(now));
    assert!(matches!(
        posts.commit_lifecycle(&trashed, 1, now).await.unwrap(),
        PostCommitOutcome::Saved(record) if record.snapshot.version == 2
    ));
    assert_eq!(
        posts.purge(post.id, 1).await.unwrap(),
        SaveOutcome::StaleConflict
    );
    assert!(matches!(
        posts.purge(post.id, 2).await.unwrap(),
        SaveOutcome::Saved { .. }
    ));
    assert!(posts.find_record_by_id(post.id).await.unwrap().is_none());
    posts
        .insert_post(
            &Post::reconstitute(draft_snapshot(author, "purge-slug")),
            &[],
        )
        .await
        .unwrap();
    let mut archived = draft_snapshot(author, "archived-trash");
    archived.status = PostStatus::Archived;
    posts
        .insert_post(&Post::reconstitute(archived.clone()), &[])
        .await
        .unwrap();
    let now = OffsetDateTime::now_utc();
    let mut archived_post = Post::reconstitute(archived.clone());
    assert!(archived_post.trash(now));
    posts
        .commit_lifecycle(&archived_post, 1, now)
        .await
        .unwrap();
    assert!(archived_post.restore());
    posts
        .commit_lifecycle(&archived_post, 2, now)
        .await
        .unwrap();
    assert_eq!(
        posts.find_by_id(archived.id).await.unwrap().unwrap().status,
        PostStatus::Archived
    );
}

// ---------------------------------------------------------------------------
// 领域提交端口：完整提交结果与后台读取的一致快照
// ---------------------------------------------------------------------------

#[tokio::test]
async fn content_commit_returns_complete_record_and_rolls_back_failed_references() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let author = seed_user(&pool, "record-author").await;
    let tags = [
        seed_tag(&pool, "标签 A", "record-a").await.id,
        seed_tag(&pool, "标签 B", "record-b").await.id,
    ];
    let repo = PostgresPostRepository::new(
        pool.clone(),
        Arc::new(infrastructure::RenderingRuntime::default()),
    );
    let post = Post::reconstitute(draft_snapshot(author, "commit-record"));
    let inserted = repo
        .insert_post(&post, &[tags[1], tags[0], tags[1]])
        .await
        .unwrap();
    let mut sorted = tags.to_vec();
    sorted.sort();
    assert_eq!(inserted.tag_ids, sorted);
    assert_eq!(
        repo.find_record_by_id(inserted.snapshot.id).await.unwrap(),
        Some(inserted.clone())
    );

    let mut edited = Post::reconstitute(inserted.snapshot.clone());
    edited
        .edit(PostPatch {
            title: Some("新标题".into()),
            ..Default::default()
        })
        .unwrap();
    let now = OffsetDateTime::now_utc().replace_nanosecond(0).unwrap();
    let PostCommitOutcome::Saved(committed) = repo
        .commit_post(&edited, inserted.snapshot.version, now, Some(&[tags[1]]))
        .await
        .unwrap()
    else {
        panic!("commit must succeed");
    };
    assert_eq!(committed.snapshot.title, "新标题");
    assert_eq!(committed.snapshot.version, inserted.snapshot.version + 1);
    assert_eq!(committed.snapshot.updated_at, now);
    assert_eq!(committed.tag_ids, vec![tags[1]]);
    assert_eq!(
        repo.find_record_by_id(committed.snapshot.id).await.unwrap(),
        Some((*committed).clone())
    );

    // 引用在正文/标签写入后校验；失败必须把版本、正文和标签一起回滚。
    let mut invalid = Post::reconstitute(committed.snapshot.clone());
    invalid
        .edit(PostPatch {
            title: Some("不能落库".into()),
            cover_media_id: Some(Some(uuid::Uuid::now_v7())),
            ..Default::default()
        })
        .unwrap();
    assert!(
        repo.commit_post(&invalid, committed.snapshot.version, now, Some(&[tags[0]]))
            .await
            .is_err()
    );
    assert_eq!(
        repo.find_record_by_id(committed.snapshot.id).await.unwrap(),
        Some((*committed).clone())
    );
    assert!(matches!(
        repo.commit_post(&edited, inserted.snapshot.version, now, None)
            .await
            .unwrap(),
        PostCommitOutcome::StaleConflict
    ));
}

#[tokio::test]
async fn content_commit_reads_never_mix_body_and_tags_during_concurrent_edits() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let author = seed_user(&pool, "snapshot-author").await;
    let a = seed_tag(&pool, "A", "snapshot-a").await.id;
    let b = seed_tag(&pool, "B", "snapshot-b").await.id;
    let repo = Arc::new(PostgresPostRepository::new(
        pool,
        Arc::new(infrastructure::RenderingRuntime::default()),
    ));
    let mut post = Post::reconstitute(draft_snapshot(author, "coherent-record"));
    post.edit(PostPatch {
        title: Some("A".into()),
        ..Default::default()
    })
    .unwrap();
    let initial = repo.insert_post(&post, &[a]).await.unwrap();
    let id = initial.snapshot.id;
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let writer_repo = repo.clone();
    let writer_barrier = barrier.clone();
    let writer = tokio::spawn(async move {
        let mut current = initial;
        writer_barrier.wait().await;
        for index in 0..40 {
            let (title, tag) = if index % 2 == 0 { ("B", b) } else { ("A", a) };
            let mut post = Post::reconstitute(current.snapshot.clone());
            post.edit(PostPatch {
                title: Some(title.into()),
                ..Default::default()
            })
            .unwrap();
            let PostCommitOutcome::Saved(record) = writer_repo
                .commit_post(
                    &post,
                    current.snapshot.version,
                    OffsetDateTime::now_utc(),
                    Some(&[tag]),
                )
                .await
                .unwrap()
            else {
                panic!("single writer must succeed");
            };
            assert_eq!(record.snapshot.title, title);
            assert_eq!(record.tag_ids, vec![tag]);
            current = *record;
            tokio::task::yield_now().await;
        }
    });
    barrier.wait().await;
    for _ in 0..160 {
        let record = repo.find_record_by_id(id).await.unwrap().unwrap();
        let expected_tag = match record.snapshot.title.as_str() {
            "A" => a,
            "B" => b,
            unexpected => panic!("unexpected title {unexpected}"),
        };
        assert_eq!(record.tag_ids, vec![expected_tag]);
        tokio::task::yield_now().await;
    }
    writer.await.unwrap();
}

#[tokio::test]
async fn content_commit_lifecycle_preserves_relations_and_checks_original_state() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let author = seed_user(&pool, "lifecycle-record-author").await;
    let tag = seed_tag(&pool, "保留标签", "lifecycle-record-tag").await.id;
    let repo =
        PostgresPostRepository::new(pool, Arc::new(infrastructure::RenderingRuntime::default()));
    let mut post = Post::reconstitute(draft_snapshot(author, "lifecycle-record"));
    let now = OffsetDateTime::now_utc();
    post.publish(now).unwrap();
    let inserted = repo.insert_post(&post, &[tag]).await.unwrap();
    let mut trashed = Post::reconstitute(inserted.snapshot.clone());
    assert!(trashed.trash(now));
    assert!(matches!(
        repo.commit_lifecycle(&trashed, inserted.snapshot.version + 1, now)
            .await
            .unwrap(),
        PostCommitOutcome::StaleConflict
    ));
    let PostCommitOutcome::Saved(deleted) = repo
        .commit_lifecycle(&trashed, inserted.snapshot.version, now)
        .await
        .unwrap()
    else {
        panic!("trash must succeed");
    };
    assert_eq!(deleted.tag_ids, vec![tag]);
    assert_eq!(deleted.snapshot.status, PostStatus::Published);
    assert!(deleted.snapshot.deleted_at.is_some());
    // 即使携带最新版本，重复提交旧的移入操作也不能作用于已在回收站的记录。
    assert!(matches!(
        repo.commit_lifecycle(&trashed, deleted.snapshot.version, now)
            .await
            .unwrap(),
        PostCommitOutcome::Gone
    ));
    let mut restored = Post::reconstitute(deleted.snapshot.clone());
    assert!(restored.restore());
    let PostCommitOutcome::Saved(restored) = repo
        .commit_lifecycle(&restored, deleted.snapshot.version, now)
        .await
        .unwrap()
    else {
        panic!("restore must succeed");
    };
    assert_eq!(restored.snapshot.status, PostStatus::Draft);
    assert!(restored.snapshot.deleted_at.is_none());
    assert_eq!(
        restored.snapshot.published_at,
        inserted.snapshot.published_at
    );
    assert_eq!(restored.tag_ids, vec![tag]);
    assert_eq!(
        repo.find_record_by_id(restored.snapshot.id).await.unwrap(),
        Some(*restored)
    );
}
