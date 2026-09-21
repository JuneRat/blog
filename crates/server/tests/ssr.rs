//! 完整装配 + HTTP 集成测试：真实 PostgreSQL + 真实主题模板。
//! 验证公开 SSR 的可见性边界：发布可读、撤回/草稿/private/软删除不可访问。

mod common;

use std::sync::Arc;

use application::content::{CreatePostCmd, PostInteractor};
use application::identity::{Actor, CreateUserCmd, RoleInteractor, UserInteractor};
use application::ports::{PostRepository, PublishedPostQuery, UserRepository};
use application::public_site::{PublicSiteInteractor, SiteInfo};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use infrastructure::{
    MiniJinjaThemeRenderer, PostgresPostRepository, PostgresPublishedPostQuery, PostgresRbacStore,
    PostgresUserRepository, SanitizingMarkdownRenderer, SystemClock,
};
use interfaces::http::public_router_minimal;
use sqlx::PgPool;
use tower::ServiceExt;

/// 各测试重建同一个数据库，必须串行执行。
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Stack {
    router: axum::Router,
    posts: Arc<PostInteractor>,
    #[allow(dead_code)]
    users: Arc<UserInteractor>,
    pool: PgPool,
    author: Actor,
}

async fn actor_for(users: &Arc<UserInteractor>, username: &str) -> Actor {
    users.actor_for_username(username).await.unwrap()
}

async fn stack() -> Stack {
    let pool = common::fresh_database("blog_server_test").await;

    let clock = Arc::new(SystemClock);
    let user_repo: Arc<dyn UserRepository> = Arc::new(PostgresUserRepository::new(pool.clone()));
    let post_repo: Arc<dyn PostRepository> = Arc::new(PostgresPostRepository::new(pool.clone()));
    let rbac = Arc::new(PostgresRbacStore::new(pool.clone()));
    let roles = Arc::new(RoleInteractor::new(rbac.clone(), user_repo.clone()));
    roles.sync_registry().await.expect("同步权限目录失败");
    let public_query: Arc<dyn PublishedPostQuery> =
        Arc::new(PostgresPublishedPostQuery::new(pool.clone()));

    let theme = Arc::new(
        MiniJinjaThemeRenderer::load(std::path::Path::new("../../themes/default"))
            .expect("模板加载失败"),
    );
    let markdown = Arc::new(SanitizingMarkdownRenderer::new());

    let users = Arc::new(UserInteractor::new(user_repo, rbac, clock.clone()));
    let posts = Arc::new(PostInteractor::new(post_repo, clock));
    let public_site = Arc::new(PublicSiteInteractor::new(
        public_query,
        markdown,
        theme,
        SiteInfo {
            title: "测试站点".into(),
            description: "集成测试".into(),
        },
    ));

    users
        .create_user(
            &Actor::bootstrap_cli(),
            CreateUserCmd {
                username: "author".into(),
                email: None,
                display_name: Some("作者甲".into()),
            },
        )
        .await
        .unwrap();
    // 测试作者需要 author 角色才能创建/发布文章（RBAC 已接入用例）。
    roles
        .assign_to_username(&Actor::bootstrap_cli(), "author", "author")
        .await
        .unwrap();
    let author = actor_for(&users, "author").await;

    let router = public_router_minimal(public_site);
    Stack {
        router,
        posts,
        users,
        pool,
        author,
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
    s.posts
        .publish(&s.author, "acceptance-post", None)
        .await
        .unwrap();
    let (status, body) = get(&s.router, "/posts/acceptance-post").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("验收文章"));
    assert!(
        body.contains("<strong>加粗</strong>"),
        "Markdown 已渲染：{body}"
    );
    assert!(body.contains("作者甲"), "公开署名来自用户展示名");

    // 撤回后立即不可访问（无页面缓存）。
    s.posts
        .withdraw(&s.author, "acceptance-post", None)
        .await
        .unwrap();
    let (status, _) = get(&s.router, "/posts/acceptance-post").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "撤回后不可访问");
}

#[tokio::test]
async fn index_lists_only_public_posts() {
    let _g = SERIAL.lock().await;
    let s = stack().await;

    s.posts
        .create(&s.author, cmd("visible", "公开文章"))
        .await
        .unwrap();
    s.posts.publish(&s.author, "visible", None).await.unwrap();

    s.posts
        .create(&s.author, cmd("hidden-draft", "草稿文章"))
        .await
        .unwrap();

    let mut private = cmd("hidden-private", "私有文章");
    private.visibility = domain_visibility_private();
    s.posts.create(&s.author, private).await.unwrap();
    s.posts
        .publish(&s.author, "hidden-private", None)
        .await
        .unwrap();

    s.posts
        .create(&s.author, cmd("hidden-deleted", "回收站文章"))
        .await
        .unwrap();
    s.posts
        .publish(&s.author, "hidden-deleted", None)
        .await
        .unwrap();
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
    s.posts.create(&s.author, cmd).await.unwrap();
    s.posts.publish(&s.author, "xss-title", None).await.unwrap();

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
