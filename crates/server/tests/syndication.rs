//! RSS / sitemap / robots 与基础 SEO 的集成测试：真实 PostgreSQL + 真实主题模板。
//!
//! 四类断言对应这一段的验收边界：
//! 1. **不泄漏**：草稿、私密、已撤回、软删除内容不出现在任何机器可读入口；
//! 2. **绝对且合法**：链接基于可信站点地址，XML 转义正确（含 Unicode slug 编码）；
//! 3. **收录规则**：sitemap 收录首页/公开文章/公开 Page，目录页只在非空时收录；
//! 4. **及时变化**：新增、撤回与站点设置变更后，下一次请求立刻反映。

mod common;

use std::sync::Arc;

use application::content::{CreatePostCmd, PostInteractor};
use application::identity::{Actor, CreateUserCmd, RoleInteractor, UserInteractor};
use application::page::{CreatePageCmd, PageInteractor, PageVisibility};
use application::ports::{
    CategoryRepository, PageRepository, PostRepository, SeriesRepository, SettingsStore,
    SiteSettingsValue, TagRepository, UserRepository,
};
use application::public_site::{PublicSiteInteractor, SiteInfo};
use application::seo::PublicBaseUrl;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use http_body_util::BodyExt;
use infrastructure::{
    MiniJinjaThemeRenderer, PostgresCategoryRepository, PostgresPageRepository,
    PostgresPostRepository, PostgresPublishedCategoryQuery, PostgresPublishedPageQuery,
    PostgresPublishedPostQuery, PostgresPublishedSeriesQuery, PostgresPublishedTagQuery,
    PostgresRbacStore, PostgresSeriesRepository, PostgresSettingsStore, PostgresTagRepository,
    PostgresUserRepository, SanitizingMarkdownRenderer, SystemClock,
};
use interfaces::http::public_router_minimal;
use sqlx::PgPool;
use tower::ServiceExt;

/// 测试用可信站点地址：所有绝对链接都应基于它，而不是请求的 Host 头。
const BASE: &str = "https://blog.test";

/// 各测试重建同一个数据库，必须串行执行。
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Stack {
    router: axum::Router,
    posts: Arc<PostInteractor>,
    pages: Arc<PageInteractor>,
    tags: Arc<dyn TagRepository>,
    categories: Arc<dyn CategoryRepository>,
    series: Arc<dyn SeriesRepository>,
    settings: Arc<dyn SettingsStore>,
    pool: PgPool,
    author: Actor,
    editor: Actor,
}

async fn stack() -> Stack {
    let pool = common::fresh_database("blog_syndication_test").await;
    let clock = Arc::new(SystemClock);

    let user_repo: Arc<dyn UserRepository> = Arc::new(PostgresUserRepository::new(pool.clone()));
    let post_repo: Arc<dyn PostRepository> = Arc::new(PostgresPostRepository::new(pool.clone()));
    let page_repo: Arc<dyn PageRepository> = Arc::new(PostgresPageRepository::new(pool.clone()));
    let rbac = Arc::new(PostgresRbacStore::new(pool.clone()));
    let roles = Arc::new(RoleInteractor::new(rbac.clone(), user_repo.clone()));
    roles.sync_registry().await.expect("同步权限目录失败");

    let tag_repo: Arc<dyn TagRepository> = Arc::new(PostgresTagRepository::new(pool.clone()));
    let category_repo: Arc<dyn CategoryRepository> =
        Arc::new(PostgresCategoryRepository::new(pool.clone()));
    let series_repo: Arc<dyn SeriesRepository> =
        Arc::new(PostgresSeriesRepository::new(pool.clone()));
    let settings: Arc<dyn SettingsStore> = Arc::new(PostgresSettingsStore::new(pool.clone()));

    let theme = Arc::new(
        MiniJinjaThemeRenderer::load(std::path::Path::new("../../themes/default"))
            .expect("模板加载失败"),
    );
    let public_site = Arc::new(PublicSiteInteractor::new(
        Arc::new(PostgresPublishedPostQuery::new(pool.clone())),
        Arc::new(PostgresPublishedPageQuery::new(pool.clone())),
        Arc::new(PostgresPublishedTagQuery::new(pool.clone())),
        Arc::new(PostgresPublishedCategoryQuery::new(pool.clone())),
        Arc::new(PostgresPublishedSeriesQuery::new(pool.clone())),
        Arc::new(SanitizingMarkdownRenderer::new()),
        theme,
        settings.clone(),
        SiteInfo {
            title: "测试站点".into(),
            description: "集成测试".into(),
            logo_url: None,
        },
        PublicBaseUrl::parse(BASE).unwrap(),
    ));

    let users = Arc::new(UserInteractor::new(
        user_repo,
        rbac,
        clock.clone(),
        common::media_guard(pool.clone()),
    ));
    for username in ["author", "editor"] {
        users
            .create_user(
                &Actor::bootstrap_cli(),
                CreateUserCmd {
                    username: username.into(),
                    email: None,
                    display_name: Some(format!("{username} 的展示名")),
                },
            )
            .await
            .unwrap();
    }
    roles
        .assign_to_username(&Actor::bootstrap_cli(), "author", "author")
        .await
        .unwrap();
    roles
        .assign_to_username(&Actor::bootstrap_cli(), "editor", "editor")
        .await
        .unwrap();
    let author = users.actor_for_username("author").await.unwrap();
    let editor = users.actor_for_username("editor").await.unwrap();

    let posts = Arc::new(PostInteractor::new(
        post_repo,
        tag_repo.clone(),
        category_repo.clone(),
        series_repo.clone(),
        clock.clone(),
        common::media_guard(pool.clone()),
    ));
    let pages = Arc::new(PageInteractor::new(page_repo, clock));

    Stack {
        router: public_router_minimal(public_site),
        posts,
        pages,
        tags: tag_repo,
        categories: category_repo,
        series: series_repo,
        settings,
        pool,
        author,
        editor,
    }
}

/// 发一次 GET，返回状态、响应头与响应体。
async fn get(router: &axum::Router, uri: &str) -> (StatusCode, HeaderMap, String) {
    let response = router
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, headers, String::from_utf8_lossy(&body).to_string())
}

fn cmd(slug: &str, title: &str) -> CreatePostCmd {
    CreatePostCmd {
        slug: Some(slug.into()),
        title: title.into(),
        excerpt: Some(format!("{title} 的摘要")),
        content: format!("# {title}\n\n正文。"),
        visibility: application::content::PostVisibility::Public,
        tag_ids: Vec::new(),
        category_id: None,
        series: None,
        cover_media_id: None,
    }
}

fn page_cmd(slug: &str, title: &str) -> CreatePageCmd {
    CreatePageCmd {
        slug: Some(slug.into()),
        title: title.into(),
        content: format!("# {title}\n\n正文。"),
        visibility: PageVisibility::Public,
    }
}

async fn seed_tag(stack: &Stack, name: &str, slug: &str) -> uuid::Uuid {
    let tag = domain::content::Tag::new(
        name.into(),
        domain::content::post::Slug::new(slug).unwrap(),
        time::OffsetDateTime::now_utc(),
    )
    .unwrap();
    let snapshot = tag.snapshot();
    stack.tags.insert(&snapshot).await.unwrap();
    snapshot.id
}

async fn seed_category(stack: &Stack, slug: &str) -> uuid::Uuid {
    let category = domain::content::Category::new(
        slug.into(),
        domain::content::post::Slug::new(slug).unwrap(),
        None,
        None,
        time::OffsetDateTime::now_utc(),
    )
    .unwrap();
    let snapshot = category.snapshot();
    stack.categories.insert(&snapshot).await.unwrap();
    snapshot.id
}

async fn seed_series(stack: &Stack, slug: &str) -> uuid::Uuid {
    let series = domain::content::Series::new(
        slug.into(),
        domain::content::post::Slug::new(slug).unwrap(),
        None,
        time::OffsetDateTime::now_utc(),
    )
    .unwrap();
    let snapshot = series.snapshot();
    stack.series.insert(&snapshot).await.unwrap();
    snapshot.id
}

/// 建文章；`publish` 为 true 时立刻发布。
async fn post(stack: &Stack, cmd: CreatePostCmd, publish: bool) -> String {
    let slug = cmd.slug.clone().unwrap();
    stack.posts.create(&stack.author, cmd).await.unwrap();
    if publish {
        stack
            .posts
            .publish(&stack.author, &slug, None)
            .await
            .unwrap();
    }
    slug
}

fn content_type(headers: &HeaderMap) -> String {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

#[tokio::test]
async fn feed_exposes_only_public_posts_and_follows_content_changes() {
    let _g = SERIAL.lock().await;
    let s = stack().await;

    post(&s, cmd("feed-visible", "公开文章"), true).await;
    post(&s, cmd("feed-draft", "草稿文章"), false).await;

    let mut private = cmd("feed-private", "私密文章");
    private.visibility = application::content::PostVisibility::Private;
    post(&s, private, true).await;

    post(&s, cmd("feed-deleted", "回收站文章"), true).await;
    sqlx::raw_sql("UPDATE posts SET deleted_at = now() WHERE slug = 'feed-deleted'")
        .execute(&s.pool)
        .await
        .unwrap();

    let (status, headers, body) = get(&s.router, "/feed.xml").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(content_type(&headers), "application/rss+xml; charset=utf-8");
    assert!(body.contains("<link>https://blog.test/</link>"), "{body}");
    assert!(
        body.contains("<link>https://blog.test/posts/feed-visible</link>"),
        "条目链接必须是绝对地址：{body}"
    );
    assert!(
        body.contains("<guid isPermaLink=\"true\">https://blog.test/posts/feed-visible</guid>"),
        "稳定标识取 canonical URL：{body}"
    );
    assert!(body.contains("<pubDate>"), "条目必须带发布时间：{body}");
    for hidden in ["feed-draft", "feed-private", "feed-deleted"] {
        assert!(!body.contains(hidden), "{hidden} 不得出现在 feed：{body}");
    }
    for hidden in ["草稿文章", "私密文章", "回收站文章"] {
        assert!(!body.contains(hidden), "{hidden} 不得出现在 feed：{body}");
    }

    // 新发布 → 下一次请求立刻出现（无缓存、不预热）。
    post(&s, cmd("feed-later", "后发布"), true).await;
    let (_, _, body) = get(&s.router, "/feed.xml").await;
    assert!(body.contains("feed-later"), "新文章立即进入 feed：{body}");
    let newer = body.find("feed-later").unwrap();
    let older = body.find("feed-visible").unwrap();
    assert!(newer < older, "按发布时间倒序：{body}");

    // 撤回 → 下一次请求立刻消失。
    s.posts
        .withdraw(&s.author, "feed-visible", None)
        .await
        .unwrap();
    let (_, _, body) = get(&s.router, "/feed.xml").await;
    assert!(
        !body.contains("feed-visible"),
        "撤回后立即退出 feed：{body}"
    );
}

#[tokio::test]
async fn feed_escapes_markup_titles() {
    let _g = SERIAL.lock().await;
    let s = stack().await;

    post(&s, cmd("esc-a", "A & B"), true).await;
    post(&s, cmd("esc-b", "C < D"), true).await;

    let (_, _, body) = get(&s.router, "/feed.xml").await;
    assert!(body.contains("A &amp; B"), "标题中的 & 必须转义：{body}");
    assert!(body.contains("C &lt; D"), "标题中的 < 必须转义：{body}");
    assert!(!body.contains("A & B"), "不得出现未转义的裸 &：{body}");
    assert!(!body.contains("C < D"), "不得出现未转义的裸 <：{body}");
}

#[tokio::test]
async fn feed_and_sitemap_percent_encode_unicode_slugs() {
    let _g = SERIAL.lock().await;
    let s = stack().await;

    post(&s, cmd("关于", "关于页面"), true).await;

    let (_, _, feed) = get(&s.router, "/feed.xml").await;
    assert!(
        feed.contains("https://blog.test/posts/%E5%85%B3%E4%BA%8E"),
        "Unicode slug 必须以百分号编码进入 feed：{feed}"
    );
    let (_, _, sitemap) = get(&s.router, "/sitemap.xml").await;
    assert!(
        sitemap.contains("<loc>https://blog.test/posts/%E5%85%B3%E4%BA%8E</loc>"),
        "Unicode slug 必须以百分号编码进入 sitemap：{sitemap}"
    );
}

#[tokio::test]
async fn sitemap_covers_public_urls_and_skips_empty_directories() {
    let _g = SERIAL.lock().await;
    let s = stack().await;

    // 非空目录：各挂一篇公开文章；同名空目录用于验证「非空才收录」。
    let rust = seed_tag(&s, "Rust", "rust").await;
    seed_tag(&s, "空标签", "empty-tag").await;
    let tech = seed_category(&s, "tech").await;
    seed_category(&s, "empty-cat").await;
    let guide = seed_series(&s, "guide").await;
    seed_series(&s, "empty-series").await;

    post(
        &s,
        CreatePostCmd {
            tag_ids: vec![rust],
            category_id: Some(tech),
            series: Some((guide, 1)),
            ..cmd("site-post", "公开文章")
        },
        true,
    )
    .await;
    post(&s, cmd("site-draft", "草稿文章"), false).await;

    s.pages
        .create(&s.editor, page_cmd("about", "关于"))
        .await
        .unwrap();
    s.pages.publish(&s.editor, "about", None).await.unwrap();
    s.pages
        .create(&s.editor, page_cmd("draft-page", "草稿页面"))
        .await
        .unwrap();

    let (status, headers, body) = get(&s.router, "/sitemap.xml").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(content_type(&headers), "application/xml; charset=utf-8");

    for loc in [
        "https://blog.test/",
        "https://blog.test/posts/site-post",
        "https://blog.test/about",
        "https://blog.test/tags/rust",
        "https://blog.test/categories/tech",
        "https://blog.test/series/guide",
    ] {
        assert!(
            body.contains(&format!("<loc>{loc}</loc>")),
            "缺少 {loc}：{body}"
        );
    }
    assert!(body.contains("<lastmod>"), "文章/Page 应带 lastmod：{body}");

    for absent in [
        "site-draft",
        "draft-page",
        "empty-tag",
        "empty-cat",
        "empty-series",
    ] {
        assert!(!body.contains(absent), "不该收录 {absent}：{body}");
    }

    // 撤回文章后立即退出 sitemap（与 feed 同一公开谓词）。
    s.posts
        .withdraw(&s.author, "site-post", None)
        .await
        .unwrap();
    let (_, _, body) = get(&s.router, "/sitemap.xml").await;
    assert!(
        !body.contains("site-post"),
        "撤回后立即退出 sitemap：{body}"
    );
}

#[tokio::test]
async fn html_pages_expose_title_description_and_canonical() {
    let _g = SERIAL.lock().await;
    let s = stack().await;

    seed_tag(&s, "Rust", "rust").await;
    post(&s, cmd("seo-post", "SEO 文章"), true).await;
    s.pages
        .create(&s.editor, page_cmd("about", "关于"))
        .await
        .unwrap();
    s.pages.publish(&s.editor, "about", None).await.unwrap();

    // 首页：标题即站点标题，canonical 是站点根，带 feed 自动发现。
    let (_, _, home) = get(&s.router, "/").await;
    assert!(home.contains("<title>测试站点</title>"), "{home}");
    assert!(
        home.contains(r#"<link rel="canonical" href="https://blog.test/">"#),
        "{home}"
    );
    assert!(
        home.contains(r#"<meta name="description" content="集成测试">"#),
        "{home}"
    );
    assert!(
        home.contains(r#"type="application/rss+xml""#)
            && home.contains(r#"href="https://blog.test/feed.xml""#),
        "首页必须声明 RSS 自动发现：{home}"
    );

    // 文章：标题带站点后缀，canonical 指向文章地址，描述取摘要。
    let (_, _, post_html) = get(&s.router, "/posts/seo-post").await;
    assert!(
        post_html.contains("<title>SEO 文章 - 测试站点</title>"),
        "{post_html}"
    );
    assert!(
        post_html.contains(r#"href="https://blog.test/posts/seo-post""#),
        "{post_html}"
    );
    assert!(
        post_html.contains(r#"<meta name="description" content="SEO 文章 的摘要">"#),
        "{post_html}"
    );
    assert!(
        post_html.contains(r#"<meta property="og:type" content="article">"#),
        "{post_html}"
    );

    // Page：canonical 是根路径地址。
    let (_, _, page_html) = get(&s.router, "/about").await;
    assert!(
        page_html.contains("<title>关于 - 测试站点</title>"),
        "{page_html}"
    );
    assert!(
        page_html.contains(r#"href="https://blog.test/about""#),
        "{page_html}"
    );

    // 列表页：第 1 页 canonical 不带查询串，第 2 页自指带 ?page=2。
    let (_, _, tag_page) = get(&s.router, "/tags/rust").await;
    assert!(
        tag_page.contains(r#"href="https://blog.test/tags/rust""#),
        "{tag_page}"
    );
    let (_, _, tag_page2) = get(&s.router, "/tags/rust?page=2").await;
    assert!(
        tag_page2.contains(r#"href="https://blog.test/tags/rust?page=2""#),
        "分页 canonical 必须自指分页地址：{tag_page2}"
    );
}

#[tokio::test]
async fn site_settings_change_is_reflected_in_feed_and_html() {
    let _g = SERIAL.lock().await;
    let s = stack().await;

    let (_, _, feed) = get(&s.router, "/feed.xml").await;
    assert!(feed.contains("<title>测试站点</title>"), "{feed}");

    // 直接经存储端口写 site 行（管理 API 的授权边界由 settings 测试覆盖）。
    s.settings
        .save_site(
            &SiteSettingsValue {
                title: Some("改名站点".into()),
                description: Some("改名描述".into()),
                logo_media_id: None,
            },
            0,
            time::OffsetDateTime::now_utc(),
        )
        .await
        .unwrap();

    let (_, _, feed) = get(&s.router, "/feed.xml").await;
    assert!(
        feed.contains("<title>改名站点</title>"),
        "feed 必须立即反映设置：{feed}"
    );
    assert!(
        feed.contains("<description>改名描述</description>"),
        "{feed}"
    );
    let (_, _, home) = get(&s.router, "/").await;
    assert!(home.contains("<title>改名站点</title>"), "{home}");
    assert!(
        home.contains(r#"<meta name="description" content="改名描述">"#),
        "{home}"
    );
}

#[tokio::test]
async fn robots_allows_public_content_and_points_at_sitemap() {
    let _g = SERIAL.lock().await;
    let s = stack().await;

    let (status, headers, body) = get(&s.router, "/robots.txt").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(content_type(&headers), "text/plain; charset=utf-8");
    assert!(body.contains("User-agent: *"), "{body}");
    assert!(body.contains("Allow: /"), "{body}");
    assert!(body.contains("Disallow: /admin"), "{body}");
    assert!(
        body.contains("Sitemap: https://blog.test/sitemap.xml"),
        "必须声明 sitemap 地址：{body}"
    );
}

/// 回归：50,000 条上限是**整个文件**的预算，不是每个来源各自的额度。
///
/// 修复前的实现按来源各取 50,000，仅 50,000 篇文章加首页就产出 50,001 条，
/// 而超限 sitemap 会被抓取器整体拒绝。这里真的插入 50,001 篇公开文章，
/// 断言输出停在上限内、首页仍在（最先占预算，绝不能被截掉），
/// 且排在文章之后的 Page/目录被整体省略（预算先到先得，顺序写进文档）。
#[tokio::test]
async fn sitemap_respects_the_whole_file_url_budget() {
    let _g = SERIAL.lock().await;
    let s = stack().await;

    // Page 排在文章之后：文章吃满预算时它应被省略。
    s.pages
        .create(&s.editor, page_cmd("budget-page", "预算页面"))
        .await
        .unwrap();
    s.pages
        .publish(&s.editor, "budget-page", None)
        .await
        .unwrap();

    // generate_series 一次插入 50,001 篇：单条语句，比循环建文章快得多。
    sqlx::raw_sql(
        r#"
        INSERT INTO posts (id, author_id, title, slug, content, status, visibility,
                           published_at, version, created_at, updated_at)
        SELECT gen_random_uuid(), u.id, '批量文章 ' || i, 'bulk-' || i, '正文',
               'published', 'public', now(), 1, now(), now()
        FROM generate_series(1, 50001) AS i
        CROSS JOIN (SELECT id FROM users WHERE username = 'author') AS u
        "#,
    )
    .execute(&s.pool)
    .await
    .unwrap();

    let (status, _, body) = get(&s.router, "/sitemap.xml").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.matches("<loc>").count(),
        50_000,
        "整个 sitemap 必须停在上限内"
    );
    assert!(
        body.contains("<loc>https://blog.test/</loc>"),
        "首页最先占预算，不能被截掉"
    );
    // 50,001 篇里只有 49,999 篇进得来（1 条首页 + 49,999 = 50,000）；
    // 具体丢哪一篇由排序决定，这里只钉住「丢了一篇、没有溢出」。
    assert_eq!(
        body.matches("<loc>https://blog.test/posts/").count(),
        49_999,
        "文章占用剩余预算，多出的那篇应被丢弃"
    );
    assert!(
        !body.contains("budget-page"),
        "预算被文章用尽后，排在后面的 Page 应被整体省略"
    );
}
