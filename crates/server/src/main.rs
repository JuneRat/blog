//! server：装配入口。读取配置、构建连接池与适配器、注入用例，交给入站适配器。
//! 本 crate 不实现业务规则。

use std::path::PathBuf;
use std::sync::Arc;

use application::category::CategoryInteractor;
use application::content::PostInteractor;
use application::identity::{RoleInteractor, UserInteractor};
use application::page::PageInteractor;
use application::ports::{
    CategoryRepository, PageRepository, PostRepository, PublishedCategoryQuery, PublishedPageQuery,
    PublishedPostQuery, PublishedTagQuery, TagRepository, UserRepository,
};
use application::public_site::{PublicSiteInteractor, SiteInfo};
use application::tag::TagInteractor;
use infrastructure::{
    MiniJinjaThemeRenderer, PostgresCategoryRepository, PostgresPageRepository,
    PostgresPostRepository, PostgresPublishedCategoryQuery, PostgresPublishedPageQuery,
    PostgresPublishedPostQuery, PostgresPublishedTagQuery, PostgresRbacStore,
    PostgresTagRepository, PostgresUserRepository, SanitizingMarkdownRenderer, SystemClock,
};
use interfaces::cli::{CliDeps, Command, parse_args};

struct Config {
    database_url: String,
    theme_dir: PathBuf,
    site_title: String,
    site_description: String,
    /// 后台 SPA 构建产物（/admin/）；默认 apps/admin/dist，不存在时不注册。
    admin_dist: PathBuf,
}

impl Config {
    fn from_env() -> Self {
        Self {
            database_url: std::env::var("DATABASE_URL")
                .unwrap_or_else(|_| "postgres://blog:blog@127.0.0.1:5432/blog".into()),
            theme_dir: std::env::var("BLOG_THEME_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("themes/default")),
            site_title: std::env::var("BLOG_SITE_TITLE").unwrap_or_else(|_| "Sun's Blog".into()),
            site_description: std::env::var("BLOG_SITE_DESCRIPTION")
                .unwrap_or_else(|_| "一个 Rust 博客".into()),
            admin_dist: std::env::var("BLOG_ADMIN_DIST")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("apps/admin/dist")),
        }
    }
}

fn migrations_dir() -> PathBuf {
    std::env::var("BLOG_MIGRATIONS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("migrations/postgres"))
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,sqlx=warn".into()),
        )
        .init();

    let cli = parse_args();
    let config = Config::from_env();

    let pool = infrastructure::connect(&config.database_url)
        .await
        .expect("连接 PostgreSQL 失败");

    match cli.command {
        Command::Migrate => {
            infrastructure::migrate(&pool, migrations_dir())
                .await
                .expect("执行迁移失败");
            println!("迁移完成。");
        }
        command => {
            // 启动/写命令前确保 schema 就绪（幂等，未应用时才执行）。
            infrastructure::migrate(&pool, migrations_dir())
                .await
                .expect("执行迁移失败");

            let clock = Arc::new(SystemClock);
            let user_repo: Arc<dyn UserRepository> =
                Arc::new(PostgresUserRepository::new(pool.clone()));
            let post_repo: Arc<dyn PostRepository> =
                Arc::new(PostgresPostRepository::new(pool.clone()));
            let page_repo: Arc<dyn PageRepository> =
                Arc::new(PostgresPageRepository::new(pool.clone()));
            let rbac_store = Arc::new(PostgresRbacStore::new(pool.clone()));
            let roles = Arc::new(RoleInteractor::new(rbac_store.clone(), user_repo.clone()));
            // 迁移后同步权限目录与内置角色（幂等；受控初始化命令的一部分）。
            roles.sync_registry().await.expect("同步权限目录失败");
            let public_query: Arc<dyn PublishedPostQuery> =
                Arc::new(PostgresPublishedPostQuery::new(pool.clone()));
            let public_page_query: Arc<dyn PublishedPageQuery> =
                Arc::new(PostgresPublishedPageQuery::new(pool.clone()));
            let tag_repo: Arc<dyn TagRepository> =
                Arc::new(PostgresTagRepository::new(pool.clone()));
            let public_tag_query: Arc<dyn PublishedTagQuery> =
                Arc::new(PostgresPublishedTagQuery::new(pool.clone()));
            let category_repo: Arc<dyn CategoryRepository> =
                Arc::new(PostgresCategoryRepository::new(pool.clone()));
            let public_category_query: Arc<dyn PublishedCategoryQuery> =
                Arc::new(PostgresPublishedCategoryQuery::new(pool.clone()));

            let renderer = Arc::new(
                MiniJinjaThemeRenderer::load(&config.theme_dir).expect("加载主题模板失败"),
            );
            let markdown = Arc::new(SanitizingMarkdownRenderer::new());

            let users = Arc::new(UserInteractor::new(
                user_repo.clone(),
                rbac_store.clone(),
                clock.clone(),
            ));

            // 认证装配：单实例内存会话/尝试 + OAuth 客户端（秘密经环境变量 secret_ref 读取）。
            let secrets: Arc<dyn application::ports::SecretSource> =
                Arc::new(infrastructure::EnvSecretSource);
            let identity_client: Arc<dyn application::ports::ExternalIdentityClient> =
                Arc::new(infrastructure::ReqwestIdentityClient::new(secrets));
            let random: Arc<dyn application::ports::SecureRandom> =
                Arc::new(infrastructure::SystemSecureRandom);
            let session_store: Arc<dyn application::ports::SessionStore> =
                Arc::new(infrastructure::InMemorySessionStore::with_defaults());
            let attempt_store: Arc<dyn application::ports::OAuthAttemptStore> =
                Arc::new(infrastructure::InMemoryOAuthAttemptStore::with_defaults());
            let oauth_configs: Arc<dyn application::ports::OAuthConfigStore> =
                Arc::new(infrastructure::PostgresOAuthConfigStore::new(pool.clone()));
            let oauth_accounts: Arc<dyn application::ports::OAuthAccountStore> =
                Arc::new(infrastructure::PostgresOAuthAccountStore::new(pool.clone()));

            // 本地密码：Argon2id 哈希（限并发）+ 内存失败限流；与 OAuth 共用会话存储。
            let hasher: Arc<dyn application::ports::PasswordHasher> =
                Arc::new(infrastructure::Argon2PasswordHasher::with_defaults());
            let login_throttle: Arc<dyn application::ports::LoginThrottle> =
                Arc::new(infrastructure::InMemoryLoginThrottle::with_defaults());
            let passwords = Arc::new(application::password::PasswordInteractor::new(
                application::password::PasswordDeps {
                    users: user_repo.clone(),
                    hasher,
                    throttle: login_throttle,
                    sessions: session_store.clone(),
                },
            ));

            let base_url = std::env::var("BLOG_PUBLIC_BASE_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:8080".into());
            // Secure cookie 默认跟随公开基础 URL 的 scheme，避免 HTTPS 部署漏设；
            // BLOG_SECURE_COOKIES 仅作显式覆盖（如 TLS 终止代理场景）。
            let secure_cookies = match std::env::var("BLOG_SECURE_COOKIES") {
                Ok(v) => v == "1" || v.eq_ignore_ascii_case("true"),
                Err(_) => base_url.starts_with("https://"),
            };
            let auth = Arc::new(application::auth::AuthInteractor::new(
                application::auth::AuthDeps {
                    sessions: session_store,
                    attempts: attempt_store,
                    configs: oauth_configs,
                    accounts: oauth_accounts,
                    identity_client,
                    random,
                },
                users.clone(),
                clock.clone(),
                base_url,
            ));

            let posts = Arc::new(PostInteractor::new(
                post_repo.clone(),
                tag_repo.clone(),
                category_repo.clone(),
                clock.clone(),
            ));
            let pages = Arc::new(PageInteractor::new(page_repo, clock.clone()));
            let tags = Arc::new(TagInteractor::new(tag_repo, clock.clone()));
            let categories = Arc::new(CategoryInteractor::new(category_repo, clock.clone()));
            let public_site = Arc::new(PublicSiteInteractor::new(
                public_query,
                public_page_query,
                public_tag_query,
                public_category_query,
                markdown,
                renderer,
                SiteInfo {
                    title: config.site_title,
                    description: config.site_description,
                },
            ));

            let deps = CliDeps {
                users,
                posts,
                pages,
                tags,
                categories,
                roles,
                auth,
                passwords,
                secure_cookies,
                public_site,
                user_repo,
                assets_dir: Some(config.theme_dir.join("assets")),
                admin_dist: Some(config.admin_dist),
                health: Some(Arc::new(infrastructure::PgHealthCheck::new(pool.clone()))),
            };

            if let Err(e) = interfaces::cli::run(deps, command).await {
                eprintln!("错误：{e}");
                std::process::exit(1);
            }
        }
    }
}
