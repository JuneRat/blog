//! 完整装配 + HTTP 集成测试：真实 PostgreSQL + 真实主题模板。
//! 验证公开 SSR 的可见性边界：发布可读、撤回/草稿/private/软删除不可访问。

mod common;

use std::sync::Arc;

use application::content::{CreatePostCmd, PostInteractor};
use application::identity::{Actor, CreateUserCmd, RoleInteractor, UserInteractor};
use application::page::{CreatePageCmd, PageInteractor, PageVisibility};
use application::ports::{
    CategoryRepository, PublishedCategoryQuery, PublishedTagQuery, TagRepository,
};
use application::ports::{
    PageRepository, PostRepository, PublishedPageQuery, PublishedPostQuery, UserRepository,
};
use application::public_site::{PublicSiteInteractor, SiteInfo};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use infrastructure::{
    MiniJinjaThemeRenderer, PostgresCategoryRepository, PostgresPageRepository,
    PostgresPostRepository, PostgresPublishedCategoryQuery, PostgresPublishedPageQuery,
    PostgresPublishedPostQuery, PostgresPublishedTagQuery, PostgresRbacStore,
    PostgresTagRepository, PostgresUserRepository, RenderingRuntime, SystemClock,
};
use interfaces::http::public_router_minimal;
use sqlx::PgPool;
use tower::ServiceExt;

/// 各测试重建同一个数据库，必须串行执行。
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Stack {
    router: axum::Router,
    posts: Arc<PostInteractor>,
    pages: Arc<PageInteractor>,
    /// 标签目录（测试直接预置标签关系）。
    tags: Arc<dyn TagRepository>,
    /// 分类目录（测试直接预置分类）。
    categories: Arc<dyn CategoryRepository>,
    /// 系列目录（测试直接预置系列）。
    series: Arc<dyn application::ports::SeriesRepository>,
    #[allow(dead_code)]
    users: Arc<UserInteractor>,
    pool: PgPool,
    author: Actor,
    /// editor 持有站点级 page.* 权限（author 没有）。
    editor: Actor,
}

async fn actor_for(users: &Arc<UserInteractor>, username: &str) -> Actor {
    users.actor_for_username(username).await.unwrap()
}

async fn stack() -> Stack {
    stack_with_theme("../../themes/default").await
}

async fn stack_with_theme(theme_dir: &str) -> Stack {
    let pool = common::fresh_database("blog_server_test").await;

    let clock = Arc::new(SystemClock);
    let rendering = Arc::new(RenderingRuntime::default());
    let user_repo: Arc<dyn UserRepository> = Arc::new(PostgresUserRepository::new(pool.clone()));
    let post_repo: Arc<dyn PostRepository> =
        Arc::new(PostgresPostRepository::new(pool.clone(), rendering.clone()));
    let page_repo: Arc<dyn PageRepository> =
        Arc::new(PostgresPageRepository::new(pool.clone(), rendering.clone()));
    let rbac = Arc::new(PostgresRbacStore::new(pool.clone()));
    let roles = Arc::new(RoleInteractor::new(rbac.clone(), user_repo.clone()));
    roles.sync_registry().await.expect("同步权限目录失败");
    let public_query: Arc<dyn PublishedPostQuery> =
        Arc::new(PostgresPublishedPostQuery::new(pool.clone()));
    let public_page_query: Arc<dyn PublishedPageQuery> =
        Arc::new(PostgresPublishedPageQuery::new(pool.clone()));

    let users = Arc::new(UserInteractor::new(
        user_repo,
        rbac,
        clock.clone(),
        common::media_guard(pool.clone()),
    ));
    let tag_repo: Arc<dyn TagRepository> = Arc::new(PostgresTagRepository::new(pool.clone()));
    let public_tag_query: Arc<dyn PublishedTagQuery> =
        Arc::new(PostgresPublishedTagQuery::new(pool.clone()));
    let category_repo: Arc<dyn CategoryRepository> =
        Arc::new(PostgresCategoryRepository::new(pool.clone()));
    let public_category_query: Arc<dyn PublishedCategoryQuery> =
        Arc::new(PostgresPublishedCategoryQuery::new(pool.clone()));
    let theme = rendering.theme_renderer(
        MiniJinjaThemeRenderer::load(std::path::Path::new(theme_dir))
            .expect("模板加载失败")
            .with_data(Arc::new(application::theme_data::ThemeData::new(
                public_query.clone(),
                public_tag_query.clone(),
                public_category_query.clone(),
            ))),
    );
    let series_repo: Arc<dyn application::ports::SeriesRepository> =
        Arc::new(infrastructure::PostgresSeriesRepository::new(pool.clone()));
    let public_series_query: Arc<dyn application::ports::PublishedSeriesQuery> = Arc::new(
        infrastructure::PostgresPublishedSeriesQuery::new(pool.clone()),
    );
    let posts = Arc::new(PostInteractor::new(
        post_repo,
        tag_repo.clone(),
        category_repo.clone(),
        series_repo.clone(),
        clock.clone(),
        common::media_guard(pool.clone()),
    ));
    let pages = Arc::new(PageInteractor::new(page_repo, clock));
    let fallback = SiteInfo {
        title: "测试站点".into(),
        description: "集成测试".into(),
        logo_url: None,
    };
    let public_site = Arc::new(PublicSiteInteractor::new(
        public_query,
        public_page_query,
        public_tag_query,
        public_category_query,
        public_series_query,
        theme,
        // 公开渲染的站点信息经 settings 解析：site 行未配置时回退装配值。
        Arc::new(infrastructure::PostgresSettingsStore::new(pool.clone())),
        fallback,
        application::seo::PublicBaseUrl::parse("https://blog.test").unwrap(),
    ));

    for username in ["author", "editor"] {
        let display_name = if username == "author" {
            "作者甲".to_string()
        } else {
            format!("{username} 的展示名")
        };
        users
            .create_user(
                &Actor::bootstrap_cli(),
                CreateUserCmd {
                    username: username.into(),
                    email: None,
                    display_name: Some(display_name),
                },
            )
            .await
            .unwrap();
    }
    // 测试作者需要 author 角色才能创建/发布文章（RBAC 已接入用例）。
    roles
        .assign_to_username(&Actor::bootstrap_cli(), "author", "author")
        .await
        .unwrap();
    roles
        .assign_to_username(&Actor::bootstrap_cli(), "editor", "editor")
        .await
        .unwrap();
    let author = actor_for(&users, "author").await;
    let editor = actor_for(&users, "editor").await;

    let router = public_router_minimal(public_site);
    Stack {
        router,
        posts,
        pages,
        tags: tag_repo,
        categories: category_repo,
        series: series_repo,
        users,
        pool,
        author,
        editor,
    }
}

async fn get(router: &axum::Router, uri: &str) -> (StatusCode, String) {
    let response = router
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let body = String::from_utf8_lossy(&body).to_string();
    (status, body)
}

fn cmd(slug: &str, title: &str) -> CreatePostCmd {
    CreatePostCmd {
        slug: Some(slug.into()),
        title: title.into(),
        excerpt: Some(format!("{title} 的摘要")),
        content: format!("# {title}\n\n正文，包含 **加粗** 与 `code`。"),
        visibility: domain_visibility_public(),
        tag_ids: Vec::new(),
        category_id: None,
        series: vec![],
        cover_media_id: None,
    }
}

fn domain_visibility_public() -> application::content::PostVisibility {
    application::content::PostVisibility::Public
}

fn domain_visibility_private() -> application::content::PostVisibility {
    application::content::PostVisibility::Private
}

#[tokio::test]
async fn published_post_is_readable_and_withdrawn_becomes_404() {
    let _g = SERIAL.lock().await;
    let s = stack().await;

    let created = s
        .posts
        .create(&s.author, cmd("acceptance-post", "验收文章"))
        .await
        .unwrap();
    assert_eq!(created.status, "draft");

    // 草稿：未发布不可访问。
    let (status, _) = get(&s.router, "/posts/acceptance-post").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "草稿不可匿名读取");

    // 发布后可访问，内容来自 Markdown 渲染。
    s.posts.publish(&s.author, created.id, None).await.unwrap();
    let (status, body) = get(&s.router, "/posts/acceptance-post").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("验收文章"));
    assert!(
        body.contains("<strong>加粗</strong>"),
        "Markdown 已渲染：{body}"
    );
    assert!(body.contains("作者甲"), "公开署名来自用户展示名");

    // 撤回后立即不可访问（无页面缓存）。
    s.posts.withdraw(&s.author, created.id, None).await.unwrap();
    let (status, _) = get(&s.router, "/posts/acceptance-post").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "撤回后不可访问");
}

#[tokio::test]
async fn index_lists_only_public_posts() {
    let _g = SERIAL.lock().await;
    let s = stack().await;

    let _created = s
        .posts
        .create(&s.author, cmd("visible", "公开文章"))
        .await
        .unwrap();
    s.posts.publish(&s.author, _created.id, None).await.unwrap();

    let _created = s
        .posts
        .create(&s.author, cmd("hidden-draft", "草稿文章"))
        .await
        .unwrap();

    let mut private = cmd("hidden-private", "私有文章");
    private.visibility = domain_visibility_private();
    let _created = s.posts.create(&s.author, private).await.unwrap();
    s.posts.publish(&s.author, _created.id, None).await.unwrap();

    let _created = s
        .posts
        .create(&s.author, cmd("hidden-deleted", "回收站文章"))
        .await
        .unwrap();
    s.posts.publish(&s.author, _created.id, None).await.unwrap();
    sqlx::raw_sql("UPDATE posts SET deleted_at = now() WHERE slug = 'hidden-deleted'")
        .execute(&s.pool)
        .await
        .unwrap();

    let (status, body) = get(&s.router, "/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("公开文章"));
    assert!(!body.contains("草稿文章"), "草稿不进入公开列表");
    assert!(!body.contains("私有文章"), "private 不进入公开列表");
    assert!(!body.contains("回收站文章"), "软删除不进入公开列表");

    for slug in ["hidden-draft", "hidden-private", "hidden-deleted"] {
        let (status, _) = get(&s.router, &format!("/posts/{slug}")).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{slug} 详情不可访问");
    }
}

#[tokio::test]
async fn healthz_responds_ok() {
    let _g = SERIAL.lock().await;
    let s = stack().await;
    let (status, body) = get(&s.router, "/healthz").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "ok");
}

#[tokio::test]
async fn title_and_excerpt_html_is_escaped_in_templates() {
    let _g = SERIAL.lock().await;
    let s = stack().await;

    // 标题/摘要注入脚本载荷：正文清洗只覆盖 Markdown 输出，
    // 标题与摘要依赖模板自动转义，这里把该保证钉进测试。
    let mut cmd = cmd("xss-title", "<script>alert('title')</script>");
    cmd.excerpt = Some("<img src=x onerror=alert('excerpt')>".into());
    let _created = s.posts.create(&s.author, cmd).await.unwrap();
    s.posts.publish(&s.author, _created.id, None).await.unwrap();

    let (status, body) = get(&s.router, "/posts/xss-title").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !body.contains("<script>alert('title')</script>"),
        "标题中的脚本必须被转义"
    );
    assert!(
        !body.contains("<img src=x onerror="),
        "摘要中的事件处理器必须被转义"
    );
    assert!(body.contains("&lt;script&gt;"), "转义后的标题应可见");

    let (_, index) = get(&s.router, "/").await;
    assert!(
        !index.contains("<script>alert('title')</script>"),
        "列表页同样转义"
    );
}

fn page_cmd(slug: &str, title: &str) -> CreatePageCmd {
    CreatePageCmd {
        slug: Some(slug.into()),
        title: title.into(),
        content: format!("# {title}\n\n正文，包含 **加粗**。"),
        visibility: PageVisibility::Public,
    }
}

#[tokio::test]
async fn page_is_public_only_while_published_and_public() {
    let _g = SERIAL.lock().await;
    let s = stack().await;

    let _created = s
        .pages
        .create(&s.editor, page_cmd("about", "关于"))
        .await
        .unwrap();

    // 草稿：根路径不可访问（404），且不能泄漏正文。
    let (status, body) = get(&s.router, "/about").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "草稿页面不可匿名读取");
    assert!(!body.contains("关于"));

    // 发布后可访问，Markdown 已渲染。
    s.pages.publish(&s.editor, _created.id, None).await.unwrap();
    let (status, body) = get(&s.router, "/about").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("关于"));
    assert!(body.contains("<strong>加粗</strong>"), "{body}");

    // 撤回后立即不可访问（无页面缓存）。
    s.pages
        .withdraw(&s.editor, _created.id, None)
        .await
        .unwrap();
    let (status, _) = get(&s.router, "/about").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "撤回后不可访问");

    // 重新发布再改 private：退出匿名读取，但后台仍可见。
    s.pages.publish(&s.editor, _created.id, None).await.unwrap();
    s.pages
        .edit(
            &s.editor,
            application::page::EditPageCmd {
                id: _created.id,
                visibility: Some(PageVisibility::Private),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let (status, _) = get(&s.router, "/about").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "private 页面不公开");
}

#[tokio::test]
async fn reserved_root_paths_are_not_shadowed_by_pages() {
    let _g = SERIAL.lock().await;
    let s = stack().await;

    // 领域层直接拒绝保留 slug；即便绕过，路由层固定入口也必须仍然生效。
    let err = s
        .pages
        .create(&s.editor, page_cmd("healthz", "伪健康检查"))
        .await
        .unwrap_err();
    assert!(
        matches!(err, application::error::UseCaseError::Invalid(_)),
        "保留路径必须被拒绝：{err:?}"
    );

    // 直接写库模拟历史坏数据，公开读取仍必须 404（不顶掉 /healthz）。
    sqlx::raw_sql(
        "INSERT INTO pages (id, title, slug, content, status, visibility, published_at, version, content_html, content_render_version) \
         VALUES (gen_random_uuid(), '伪健康检查', 'healthz', '不应出现', 'published', 'public', now(), 1, '<p>不应出现</p>', 1)",
    )
    .execute(&s.pool)
    .await
    .unwrap();
    let (status, body) = get(&s.router, "/healthz").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "ok", "固定路由优先，Page 不能顶掉 /healthz");

    // 多段未知路径仍走 404 fallback，不会被 Page 的根参数吞掉。
    let (status, _) = get(&s.router, "/about/extra").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// 公开标签页：/tags/{slug} 分页与可见性
// ---------------------------------------------------------------------------

/// 预置标签并返回 id。
async fn seed_tag(stack: &Stack, name: &str, slug: &str) -> uuid::Uuid {
    let tag = domain::content::Tag::new(
        name.into(),
        domain::content::Slug::new(slug).unwrap(),
        time::OffsetDateTime::now_utc(),
    )
    .unwrap();
    let snapshot = tag.snapshot();
    stack
        .tags
        .insert(
            &domain::content::Tag::reconstitute(snapshot.clone()).unwrap(),
            None,
        )
        .await
        .unwrap();
    snapshot.id
}

#[tokio::test]
async fn tag_page_lists_public_posts_and_hides_drafts_and_private() {
    let _g = SERIAL.lock().await;
    let stack = stack().await;
    let rust = seed_tag(&stack, "Rust", "rust").await;

    // 公开发布、草稿、发布但 private 各一篇挂同一标签。
    let _created = stack
        .posts
        .create(
            &stack.author,
            CreatePostCmd {
                slug: Some("tag-visible".into()),
                title: "公开的标签文章".into(),
                excerpt: None,
                content: "# 公开\n正文".into(),
                visibility: application::content::PostVisibility::Public,
                tag_ids: vec![rust],
                category_id: None,
                series: vec![],
                cover_media_id: None,
            },
        )
        .await
        .unwrap();
    stack
        .posts
        .publish(&stack.author, _created.id, None)
        .await
        .unwrap();

    let _created = stack
        .posts
        .create(
            &stack.author,
            CreatePostCmd {
                slug: Some("tag-draft".into()),
                title: "草稿不外泄".into(),
                excerpt: None,
                content: "草稿".into(),
                visibility: application::content::PostVisibility::Public,
                tag_ids: vec![rust],
                category_id: None,
                series: vec![],
                cover_media_id: None,
            },
        )
        .await
        .unwrap();

    let _created = stack
        .posts
        .create(
            &stack.author,
            CreatePostCmd {
                slug: Some("tag-private".into()),
                title: "私密不外泄".into(),
                excerpt: None,
                content: "私密".into(),
                visibility: application::content::PostVisibility::Private,
                tag_ids: vec![rust],
                category_id: None,
                series: vec![],
                cover_media_id: None,
            },
        )
        .await
        .unwrap();
    stack
        .posts
        .publish(&stack.author, _created.id, None)
        .await
        .unwrap();

    let (status, body) = get(&stack.router, "/tags/rust").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Rust"), "标签页显示标签名：{body}");
    assert!(body.contains("公开的标签文章"), "{body}");
    assert!(!body.contains("草稿不外泄"), "草稿不得出现在标签页：{body}");
    assert!(!body.contains("私密不外泄"), "私密不得出现在标签页：{body}");

    // 未知标签 404。
    let (status, _) = get(&stack.router, "/tags/ghost").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // 文章详情页展示标签链接。
    let (status, body) = get(&stack.router, "/posts/tag-visible").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body.contains(r#"href="/tags/rust""#),
        "详情页有标签链接：{body}"
    );
}

#[tokio::test]
async fn tag_page_paginates_public_posts() {
    let _g = SERIAL.lock().await;
    let stack = stack().await;
    let rust = seed_tag(&stack, "Rust", "rust").await;

    // 21 篇公开文章：每页 20 → 第 1 页 20 条，第 2 页 1 条。
    for i in 1..=21 {
        let slug = format!("page-{i:02}");
        let _created = stack
            .posts
            .create(
                &stack.author,
                CreatePostCmd {
                    slug: Some(slug.clone()),
                    title: format!("第 {i} 篇"),
                    excerpt: None,
                    content: "正文".into(),
                    visibility: application::content::PostVisibility::Public,
                    tag_ids: vec![rust],
                    category_id: None,
                    series: vec![],
                    cover_media_id: None,
                },
            )
            .await
            .unwrap();
        stack
            .posts
            .publish(&stack.author, _created.id, None)
            .await
            .unwrap();
    }

    let (status, body) = get(&stack.router, "/tags/rust").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("第 1 / 2 页"), "分页状态可见：{body}");
    assert!(
        body.contains(r#"href="/tags/rust?page=2""#),
        "下一页链接：{body}"
    );
    assert!(body.contains("第 21 篇"), "第 1 页含最新文章：{body}");

    let (status, body2) = get(&stack.router, "/tags/rust?page=2").await;
    assert_eq!(status, StatusCode::OK, "{body2}");
    assert!(body2.contains("第 2 / 2 页"), "第 2 页状态：{body2}");
    assert!(body2.contains("第 1 篇"), "第 2 页是最旧文章：{body2}");
    assert!(!body2.contains("第 21 篇"), "第 2 页不含最新文章：{body2}");

    // 页码 0/负数按第 1 页处理；远超总页数渲染空页而非报错。
    let (status, body_zero) = get(&stack.router, "/tags/rust?page=0").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body_zero.contains("第 21 篇"));
    let (status, body_far) = get(&stack.router, "/tags/rust?page=99").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body_far.contains("还没有公开文章"),
        "越界页为空页：{body_far}"
    );
}

#[tokio::test]
async fn category_page_lists_public_posts_and_hides_drafts() {
    let _g = SERIAL.lock().await;
    let stack = stack().await;
    // 直接经目录仓储预置分类（绕过权限装配）。
    let cat = domain::content::Category::new(
        "技术".into(),
        domain::content::Slug::new("tech").unwrap(),
        None,
        None,
        time::OffsetDateTime::now_utc(),
    )
    .unwrap();
    let cat_snapshot = cat.snapshot();
    stack
        .categories
        .insert(
            &domain::content::Category::reconstitute(cat_snapshot.clone()).unwrap(),
            None,
        )
        .await
        .unwrap();

    for (slug, title, publish) in [
        ("cat-visible", "公开的分类文章", true),
        ("cat-draft", "草稿不外泄", false),
    ] {
        let _created = stack
            .posts
            .create(
                &stack.author,
                CreatePostCmd {
                    slug: Some(slug.into()),
                    title: title.into(),
                    excerpt: None,
                    content: "正文".into(),
                    visibility: application::content::PostVisibility::Public,
                    tag_ids: Vec::new(),
                    category_id: Some(cat_snapshot.id),
                    series: vec![],
                    cover_media_id: None,
                },
            )
            .await
            .unwrap();
        if publish {
            stack
                .posts
                .publish(&stack.author, _created.id, None)
                .await
                .unwrap();
        }
    }

    let (status, body) = get(&stack.router, "/categories/tech").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("技术"), "{body}");
    assert!(body.contains("公开的分类文章"), "{body}");
    assert!(!body.contains("草稿不外泄"), "{body}");
    let (status, _) = get(&stack.router, "/categories/ghost").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // 详情页展示分类链接。
    let (status, body) = get(&stack.router, "/posts/cat-visible").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains(r#"href="/categories/tech""#), "{body}");
}

#[tokio::test]
async fn series_page_lists_public_posts_in_reading_order() {
    let _g = SERIAL.lock().await;
    let stack = stack().await;
    let series = domain::content::Series::new(
        "指南".into(),
        domain::content::Slug::new("guide").unwrap(),
        None,
        time::OffsetDateTime::now_utc(),
    )
    .unwrap();
    let s = series.snapshot();
    stack
        .series
        .insert(
            &domain::content::Series::reconstitute(s.clone()).unwrap(),
            None,
        )
        .await
        .unwrap();

    // 三篇挂系列：公开(序2)、草稿(序1)、公开(序3)。草稿占位但不出现。
    for (slug, order, publish) in [
        ("guide-draft", 1, false),
        ("guide-first", 2, true),
        ("guide-second", 3, true),
    ] {
        let _created = stack
            .posts
            .create(
                &stack.author,
                CreatePostCmd {
                    slug: Some(slug.into()),
                    title: format!("标题-{slug}"),
                    excerpt: None,
                    content: "正文".into(),
                    visibility: application::content::PostVisibility::Public,
                    tag_ids: Vec::new(),
                    category_id: None,
                    series: vec![application::content::SeriesPlacement {
                        series_id: s.id,
                        position: order,
                    }],
                    cover_media_id: None,
                },
            )
            .await
            .unwrap();
        if publish {
            stack
                .posts
                .publish(&stack.author, _created.id, None)
                .await
                .unwrap();
        }
    }

    let (status, body) = get(&stack.router, "/series/guide").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("指南"), "{body}");
    // 阅读序号按公开成员连续编号（1、2），草稿不出现、不透出空档。
    let first = body.find("标题-guide-first").unwrap();
    let second = body.find("标题-guide-second").unwrap();
    assert!(first < second, "按 series_order 升序：{body}");
    assert!(!body.contains("guide-draft"), "{body}");
    assert!(
        body.contains("value=\"1\"") && body.contains("value=\"2\""),
        "连续阅读序号：{body}"
    );

    let (status, _) = get(&stack.router, "/series/ghost").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // 详情页展示系列链接与阅读顺序。
    let (status, body) = get(&stack.router, "/posts/guide-first").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains(r#"href="/series/guide""#), "{body}");
}

#[tokio::test]
async fn paper_theme_functions_use_only_public_data() {
    let _g = SERIAL.lock().await;
    let s = stack_with_theme("../../themes/paper").await;
    let tag = seed_tag(&s, "主题标签", "paper-tag").await;
    let category = domain::content::Category::new(
        "主题分类".into(),
        domain::content::Slug::new("paper-category").unwrap(),
        None,
        None,
        time::OffsetDateTime::now_utc(),
    )
    .unwrap()
    .snapshot();
    s.categories
        .insert(
            &domain::content::Category::reconstitute(category.clone()).unwrap(),
            None,
        )
        .await
        .unwrap();
    for (slug, title, publish) in [
        ("paper-visible", "纸张主题可见文章", true),
        ("paper-related", "同类可见文章", true),
        ("paper-draft", "纸张主题不可见草稿", false),
    ] {
        let _created = s
            .posts
            .create(
                &s.author,
                CreatePostCmd {
                    slug: Some(slug.into()),
                    title: title.into(),
                    excerpt: None,
                    content: "# 正文".into(),
                    visibility: application::content::PostVisibility::Public,
                    tag_ids: vec![tag],
                    category_id: Some(category.id),
                    series: vec![],
                    cover_media_id: None,
                },
            )
            .await
            .unwrap();
        if publish {
            s.posts.publish(&s.author, _created.id, None).await.unwrap();
        }
    }
    let (status, index) = get(&s.router, "/").await;
    assert_eq!(status, StatusCode::OK, "{index}");
    assert!(index.contains("纸张主题可见文章"));
    assert!(!index.contains("纸张主题不可见草稿"));
    assert!(index.contains("/assets/paper/"));
    assert!(index.contains("/categories/paper-category"));
    assert!(index.contains("/tags/paper-tag"));
    let (status, detail) = get(&s.router, "/posts/paper-visible").await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert!(detail.contains("同类文章"));
    assert!(detail.contains("同类可见文章"));
    assert!(detail.contains("更新于"));
    assert!(!detail.contains("纸张主题不可见草稿"));
    for path in ["/tags/paper-tag", "/categories/paper-category"] {
        let (status, listing) = get(&s.router, path).await;
        assert_eq!(status, StatusCode::OK, "{path}: {listing}");
        assert!(listing.contains("纸张主题可见文章"));
        assert!(!listing.contains("纸张主题不可见草稿"));
    }
    let _created = s
        .pages
        .create(&s.editor, page_cmd("paper-page", "纸张主题页面"))
        .await
        .unwrap();
    s.pages.publish(&s.editor, _created.id, None).await.unwrap();
    let (status, page) = get(&s.router, "/paper-page").await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(page.contains("纸张主题页面"));
}

/// 在测试库里预置一张可用媒体：封面引用校验要求资产存在且可用。
async fn seed_media(pool: &PgPool, owner: uuid::Uuid) -> uuid::Uuid {
    let id = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO media (id, uploaded_by, path, filename, mime_type, size, \
         width, height, checksum_sha256, version) \
         VALUES ($1, $2, $3, 'cover.png', 'image/png', 16, 8, 8, $4, 1)",
    )
    .bind(id)
    .bind(owner)
    .bind(format!("objects/{id}.png"))
    .bind("a".repeat(64))
    .execute(pool)
    .await
    .unwrap();
    id
}

/// 文章封面出现在公开详情页，地址与正文图片同一个 `/media/{id}` 出口；
/// 撤回后页面整体 404，封面自然不再输出。
#[tokio::test]
async fn post_cover_is_rendered_on_the_public_detail_page() {
    let _g = SERIAL.lock().await;
    let s = stack().await;
    let cover = seed_media(&s.pool, s.author.user_id.0).await;

    let mut command = cmd("with-cover", "带封面的文章");
    command.cover_media_id = Some(cover);
    let _created = s.posts.create(&s.author, command).await.unwrap();
    s.posts.publish(&s.author, _created.id, None).await.unwrap();

    let (status, body) = get(&s.router, "/posts/with-cover").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body.contains(&format!("src=\"/media/{cover}\"")),
        "公开详情页必须渲染封面：{body}"
    );

    s.posts
        .withdraw(&s.author, _created.id, None)
        .await
        .unwrap();
    let (status, body) = get(&s.router, "/posts/with-cover").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(
        !body.contains(&cover.to_string()),
        "撤回后不得再输出封面地址"
    );
}

/// 系列封面出现在公开系列页；没有封面的系列不输出图片标签。
#[tokio::test]
async fn series_cover_is_rendered_on_the_public_series_page() {
    let _g = SERIAL.lock().await;
    let s = stack().await;
    let cover = seed_media(&s.pool, s.author.user_id.0).await;

    sqlx::query(
        "INSERT INTO series (id, name, slug, cover_media_id, version) \
         VALUES (gen_random_uuid(), '封面系列', 'cover-series', $1, 1)",
    )
    .bind(cover)
    .execute(&s.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO series (id, name, slug, version) \
         VALUES (gen_random_uuid(), '无封面系列', 'plain-series', 1)",
    )
    .execute(&s.pool)
    .await
    .unwrap();

    let (status, body) = get(&s.router, "/series/cover-series").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body.contains(&format!("src=\"/media/{cover}\"")),
        "公开系列页必须渲染封面：{body}"
    );

    let (status, body) = get(&s.router, "/series/plain-series").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        !body.contains("<img class=\"series-cover\""),
        "没有封面时不输出封面标签：{body}"
    );
}

/// 站点 logo（base 头部）与作者头像（署名行）出现在公开页面。
///
/// 两者都走 `/media/{id}` 出口：logo 读 settings.site 的值，头像读 users.avatar_media_id。
#[tokio::test]
async fn site_logo_and_author_avatar_are_rendered_on_public_pages() {
    let _g = SERIAL.lock().await;
    let s = stack().await;
    let logo = seed_media(&s.pool, s.author.user_id.0).await;
    let avatar = seed_media(&s.pool, s.author.user_id.0).await;

    // 站点 logo：直接写 settings.site 值（渲染只读值，不依赖引用行）。
    sqlx::query(
        "INSERT INTO settings (key, value, version, updated_at) \
         VALUES ('site', jsonb_build_object('schema_version', 1, 'title', '测试站点', \
                 'description', '集成测试', 'logo_media_id', $1::text), 1, now())",
    )
    .bind(logo.to_string())
    .execute(&s.pool)
    .await
    .unwrap();

    // 作者头像：本人自助设置（写 users.avatar_media_id 与引用行）。
    s.users
        .set_own_avatar(&s.author, Some(avatar))
        .await
        .unwrap();

    let _created = s
        .posts
        .create(&s.author, cmd("avatar-post", "带头像的文章"))
        .await
        .unwrap();
    s.posts.publish(&s.author, _created.id, None).await.unwrap();

    let (status, body) = get(&s.router, "/posts/avatar-post").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body.contains(&format!("src=\"/media/{avatar}\"")),
        "文章页必须渲染作者头像：{body}"
    );
    assert!(
        body.contains(&format!("src=\"/media/{logo}\"")),
        "base 头部必须渲染站点 logo：{body}"
    );

    let (status, index) = get(&s.router, "/").await;
    assert_eq!(status, StatusCode::OK, "{index}");
    assert!(
        index.contains(&format!("src=\"/media/{avatar}\"")),
        "首页列表必须渲染作者头像：{index}"
    );
}
