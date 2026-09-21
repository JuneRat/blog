//! server：装配入口。读取配置、构建连接池与适配器、注入用例，交给入站适配器。
//! 本 crate 不实现业务规则。

use std::path::PathBuf;
use std::sync::Arc;

use application::content::PostInteractor;
use application::identity::{RoleInteractor, UserInteractor};
use application::ports::{PostRepository, PublishedPostQuery, UserRepository};
use application::public_site::{PublicSiteInteractor, SiteInfo};
use infrastructure::{
    MiniJinjaThemeRenderer, PostgresPostRepository, PostgresPublishedPostQuery, PostgresRbacStore,
    PostgresUserRepository, SanitizingMarkdownRenderer, SystemClock,
};
use interfaces::cli::{CliDeps, Command, parse_args};

struct Config {
    database_url: String,
    theme_dir: PathBuf,
    site_title: String,
    site_description: String,
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
            let rbac_store = Arc::new(PostgresRbacStore::new(pool.clone()));
            let roles = Arc::new(RoleInteractor::new(rbac_store.clone(), user_repo.clone()));
            // 迁移后同步权限目录与内置角色（幂等；受控初始化命令的一部分）。
            roles.sync_registry().await.expect("同步权限目录失败");
            let public_query: Arc<dyn PublishedPostQuery> =
                Arc::new(PostgresPublishedPostQuery::new(pool.clone()));

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
            let base_url = std::env::var("BLOG_PUBLIC_BASE_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:8080".into());
            let secure_cookies = std::env::var("BLOG_SECURE_COOKIES")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false);
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

            let posts = Arc::new(PostInteractor::new(post_repo.clone(), clock.clone()));
            let public_site = Arc::new(PublicSiteInteractor::new(
                public_query,
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
                roles,
                auth,
                secure_cookies,
                public_site,
                user_repo,
                assets_dir: Some(config.theme_dir.join("assets")),
                health: Some(Arc::new(infrastructure::PgHealthCheck::new(pool.clone()))),
            };

            if let Err(e) = interfaces::cli::run(deps, command).await {
                eprintln!("错误：{e}");
                std::process::exit(1);
            }
        }
    }
}
