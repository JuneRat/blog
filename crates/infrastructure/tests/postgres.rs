//! 基础设施集成测试：真实 PostgreSQL 上验证迁移、约束与并发版本语义。
//!
//! 通过 `BLOG_TEST_ADMIN_URL` 覆盖管理连接（默认 postgres://blog:blog@127.0.0.1:5432/postgres）；
//! 测试库 DSN 从它推导（同名主机上的 blog_test）。库不可达或主机非 loopback 时
//! 直接 panic 失败——这些测试是破坏性的（DROP DATABASE），不允许静默跳过后误报通过。

use std::sync::Arc;

use application::error::UseCaseError;
use application::ports::{
    ContentRenderer, PostRepository, PublishedPostQuery, RbacStore, SaveOutcome,
};
use domain::content::post::{Post, PostSnapshot, PostStatus, Slug, Visibility};
use domain::identity::UserId;
use infrastructure::{
    PostgresPostRepository, PostgresPublishedPostQuery, PostgresRbacStore,
    SanitizingMarkdownRenderer, connect, migrate,
};
use sqlx::{PgPool, Row};
use time::OffsetDateTime;
use tokio::sync::Mutex;

static SERIAL: Mutex<()> = Mutex::const_new(());

fn admin_url() -> String {
    std::env::var("BLOG_TEST_ADMIN_URL")
        .unwrap_or_else(|_| "postgres://blog:blog@127.0.0.1:5432/postgres".into())
}

/// 从管理 DSN 推导同一主机上的测试库 DSN（替换最后一段路径）。
fn test_db_url(admin: &str) -> String {
    let base = admin.trim_end_matches('/');
    let idx = base
        .rfind('/')
        .expect("管理 DSN 缺少路径段，形如 postgres://user:pass@host:port/postgres");
    format!("{}/blog_test", &base[..idx])
}

/// 破坏性测试守卫：只允许 loopback 主机，防止误删远端同名库。
fn assert_loopback(admin: &str) {
    let after_scheme = admin.split("://").nth(1).unwrap_or_default();
    let host_port = after_scheme
        .rsplit_once('@')
        .map(|(_, rest)| rest)
        .unwrap_or(after_scheme);
    let host = host_port.split([':', '/']).next().unwrap_or_default();
    assert!(
        matches!(host, "127.0.0.1" | "::1" | "localhost"),
        "拒绝在非 loopback 主机 {host} 上执行破坏性测试（BLOG_TEST_ADMIN_URL 指向了远端？）"
    );
}

async fn fresh_database() -> PgPool {
    let admin_dsn = admin_url();
    assert_loopback(&admin_dsn);
    let test_dsn = test_db_url(&admin_dsn);

    let admin = connect(&admin_dsn).await.expect("连接管理库失败");

    // raw_sql 走简单协议且不包事务；CREATE/DROP DATABASE 不能在事务块内执行。
    sqlx::raw_sql("DROP DATABASE IF EXISTS blog_test WITH (FORCE)")
        .execute(&admin)
        .await
        .expect("删除旧测试库失败");
    sqlx::raw_sql("CREATE DATABASE blog_test")
        .execute(&admin)
        .await
        .expect("创建测试库失败");
    admin.close().await;

    let pool = connect(&test_dsn).await.expect("连接测试库失败");
    migrate(&pool, "../../migrations/postgres")
        .await
        .expect("迁移失败");
    pool
}

async fn seed_user(pool: &PgPool, username: &str) -> uuid::Uuid {
    let id = uuid::Uuid::now_v7();
    let now = OffsetDateTime::now_utc();
    sqlx::query(
        "INSERT INTO users (id, username, display_name, version, created_at, updated_at) \
         VALUES ($1, $2, $3, 1, $4, $4)",
    )
    .bind(id)
    .bind(username)
    .bind(format!("{username}的展示名"))
    .bind(now)
    .execute(pool)
    .await
    .unwrap();
    id
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
async fn migrations_create_thirteen_core_tables() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;

    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_tables WHERE schemaname='public' AND tablename <> '_sqlx_migrations'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 13, "13 张核心表");

    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT tablename FROM pg_tables WHERE schemaname='public' AND tablename <> '_sqlx_migrations' ORDER BY tablename",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let expected = [
        "categories",
        "oauth_accounts",
        "pages",
        "permissions",
        "post_tags",
        "posts",
        "role_permissions",
        "roles",
        "series",
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
    let repo = PostgresPostRepository::new(pool.clone());

    repo.insert(&draft_snapshot(author, "same-slug"))
        .await
        .unwrap();
    let err = repo
        .insert(&draft_snapshot(author, "same-slug"))
        .await
        .unwrap_err();
    match err {
        UseCaseError::Conflict(target) => assert_eq!(target, "slug"),
        other => panic!("期望 Conflict，得到 {other:?}"),
    }
}

#[tokio::test]
async fn foreign_key_protects_author_reference() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let repo = PostgresPostRepository::new(pool.clone());

    let ghost = uuid::Uuid::now_v7();
    let err = repo
        .insert(&draft_snapshot(ghost, "orphan"))
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
    let repo = PostgresPostRepository::new(pool.clone());

    let series_id = uuid::Uuid::now_v7();
    sqlx::query("INSERT INTO series (id, name, slug, version) VALUES ($1, '系列', 'series-1', 1)")
        .bind(series_id)
        .execute(&pool)
        .await
        .unwrap();

    let mut a = draft_snapshot(author, "series-a");
    a.series_id = Some(series_id);
    a.series_order = Some(2);
    repo.insert(&a).await.unwrap();

    // 同系列同位置：唯一约束拒绝。
    let mut b = draft_snapshot(author, "series-b");
    b.series_id = Some(series_id);
    b.series_order = Some(2);
    let err = repo.insert(&b).await.unwrap_err();
    match err {
        UseCaseError::Conflict(target) => assert_eq!(target, "该系列位置"),
        other => panic!("期望 Conflict，得到 {other:?}"),
    }

    // 不同位置可以插入；留空档合法。
    b.series_order = Some(5);
    repo.insert(&b).await.unwrap();

    // series_order 无 series_id：CHECK 拒绝。
    let mut c = draft_snapshot(author, "series-c");
    c.series_id = None;
    c.series_order = Some(1);
    let err = repo.insert(&c).await.unwrap_err();
    assert!(
        matches!(err, UseCaseError::Repository(_)),
        "series_id 与 series_order 必须同空同非空"
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
    let repo = PostgresPostRepository::new(pool.clone());

    let mut snapshot = draft_snapshot(author, "versioned");
    repo.insert(&snapshot).await.unwrap();
    assert_eq!(snapshot.version, 1);

    // 正确版本：Saved 且携带新版本号，无需回读。
    snapshot.title = "第一次修改".into();
    let outcome = repo
        .save(&snapshot, 1, OffsetDateTime::now_utc())
        .await
        .unwrap();
    assert_eq!(outcome, SaveOutcome::Saved { new_version: 2 });

    // 过期版本：StaleConflict，不覆盖。
    let mut stale_edit = snapshot.clone();
    stale_edit.title = "基于旧版本的并发修改".into();
    stale_edit.version = 1;
    let outcome = repo
        .save(&stale_edit, 1, OffsetDateTime::now_utc())
        .await
        .unwrap();
    assert_eq!(outcome, SaveOutcome::StaleConflict);

    let current = repo.find_by_slug("versioned").await.unwrap().unwrap();
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
        .save(&gone_edit, 2, OffsetDateTime::now_utc())
        .await
        .unwrap();
    assert_eq!(
        outcome,
        SaveOutcome::Gone,
        "软删除后的保存应报 Gone 而非冲突"
    );
}

#[tokio::test]
async fn truly_concurrent_saves_exactly_one_wins() {
    let _g = SERIAL.lock().await;
    let pool = fresh_database().await;
    let author = seed_user(&pool, "author").await;

    let repo_a = PostgresPostRepository::new(pool.clone());
    let pool_b = connect(&test_db_url(&admin_url())).await.unwrap();
    let repo_b = PostgresPostRepository::new(pool_b);

    let snapshot = draft_snapshot(author, "race-real");
    repo_a.insert(&snapshot).await.unwrap();

    let mut edit_a = snapshot.clone();
    edit_a.title = "并发A".into();
    let mut edit_b = snapshot.clone();
    edit_b.title = "并发B".into();
    let now = OffsetDateTime::now_utc();

    // 两条真实连接同时 UPDATE 同一行，都带 expected_version=1。
    let (outcome_a, outcome_b) =
        tokio::join!(repo_a.save(&edit_a, 1, now), repo_b.save(&edit_b, 1, now));
    let outcomes = [outcome_a.unwrap(), outcome_b.unwrap()];
    let saved = outcomes
        .iter()
        .filter(|o| **o == SaveOutcome::Saved { new_version: 2 })
        .count();
    let conflicted = outcomes
        .iter()
        .filter(|o| **o == SaveOutcome::StaleConflict)
        .count();
    assert_eq!(saved, 1, "恰好一个写入成功：{outcomes:?}");
    assert_eq!(conflicted, 1, "另一个必须是版本冲突：{outcomes:?}");

    let final_state = repo_a.find_by_slug("race-real").await.unwrap().unwrap();
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
    let repo = PostgresPostRepository::new(pool.clone());
    let query = PostgresPublishedPostQuery::new(pool.clone());

    // 1. 公开发布
    let mut published = draft_snapshot(author, "public-one");
    {
        let mut post = Post::reconstitute(published.clone());
        post.publish(OffsetDateTime::now_utc()).unwrap();
        published = post.snapshot();
    }
    repo.insert(&published).await.unwrap();

    // 2. 草稿
    repo.insert(&draft_snapshot(author, "draft-one"))
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
    repo.insert(&private).await.unwrap();

    // 4. 软删除的已发布文章
    let mut deleted = draft_snapshot(author, "deleted-one");
    {
        let mut post = Post::reconstitute(deleted.clone());
        post.publish(OffsetDateTime::now_utc()).unwrap();
        deleted = post.snapshot();
    }
    repo.insert(&deleted).await.unwrap();
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
    let repo = PostgresPostRepository::new(pool.clone());

    let snapshot = draft_snapshot(author, "lifecycle");
    repo.insert(&snapshot).await.unwrap();

    let mut post = Post::reconstitute(repo.find_by_slug("lifecycle").await.unwrap().unwrap());
    post.publish(OffsetDateTime::now_utc()).unwrap();
    repo.save(&post.snapshot(), 1, OffsetDateTime::now_utc())
        .await
        .unwrap();

    let mut post = Post::reconstitute(repo.find_by_slug("lifecycle").await.unwrap().unwrap());
    assert_eq!(post.status(), PostStatus::Published);
    let first_published_at = post.snapshot().published_at;

    post.withdraw();
    repo.save(&post.snapshot(), 2, OffsetDateTime::now_utc())
        .await
        .unwrap();

    let post = Post::reconstitute(repo.find_by_slug("lifecycle").await.unwrap().unwrap());
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
    assert_eq!(author_perms, 5, "Author 恰好 5 个 own 动作");

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
        matches!(err, application::error::UseCaseError::Forbidden),
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
        matches!(err, application::error::UseCaseError::Forbidden),
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
        matches!(err, application::error::UseCaseError::Forbidden),
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
    assert!(matches!(err, application::error::UseCaseError::Forbidden));
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
    assert!(matches!(err, application::error::UseCaseError::Forbidden));
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
