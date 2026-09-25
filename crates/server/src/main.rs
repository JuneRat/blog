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
    PublishedPostQuery, PublishedSeriesQuery, PublishedTagQuery, SeriesRepository, SettingsStore,
    TagRepository, ThemeSettingsStore, UserRepository,
};
use application::public_site::{PublicSiteInteractor, SiteInfo};
use application::seo::PublicBaseUrl;
use application::series::SeriesInteractor;
use application::settings::SettingsInteractor;
use application::tag::TagInteractor;
use application::themes::ThemeRegistry;
use infrastructure::{
    MiniJinjaThemeRenderer, PostgresCategoryRepository, PostgresPageRepository,
    PostgresPostRepository, PostgresPublishedCategoryQuery, PostgresPublishedPageQuery,
    PostgresPublishedPostQuery, PostgresPublishedSeriesQuery, PostgresPublishedTagQuery,
    PostgresRbacStore, PostgresSeriesRepository, PostgresTagRepository, PostgresUserRepository,
    SanitizingMarkdownRenderer, SystemClock,
};
use interfaces::cli::{CliDeps, Command, parse_args};

struct Config {
    database_url: String,
    theme_dir: PathBuf,
    site_title: String,
    site_description: String,
    /// 后台 SPA 构建产物（/admin/）；默认 apps/admin/dist，不存在时不注册。
    admin_dist: PathBuf,
    /// 媒体文件根目录：暂存与正式对象都在它下面。
    /// 备份必须与数据库一起覆盖它（docs/operations-and-recovery.md）。
    media_dir: PathBuf,
    /// 对外可达基础 URL：OAuth 回调、canonical、RSS 与 sitemap 共用。
    /// 装配期校验一次并失败即退出——错误地址会污染搜索索引，不能静默使用。
    public_base_url: PublicBaseUrl,
}

impl Config {
    fn from_env() -> Self {
        let public_base_url = std::env::var("BLOG_PUBLIC_BASE_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:8080".into());
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
            media_dir: std::env::var("BLOG_MEDIA_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("data/media")),
            public_base_url: PublicBaseUrl::parse(&public_base_url)
                .unwrap_or_else(|e| panic!("BLOG_PUBLIC_BASE_URL 无效：{e}")),
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
            let series_repo: Arc<dyn SeriesRepository> =
                Arc::new(PostgresSeriesRepository::new(pool.clone()));
            let public_series_query: Arc<dyn PublishedSeriesQuery> =
                Arc::new(PostgresPublishedSeriesQuery::new(pool.clone()));

            let theme_data = Arc::new(application::theme_data::ThemeData::new(
                public_query.clone(),
                public_tag_query.clone(),
                public_category_query.clone(),
            ));
            let fallback_renderer = MiniJinjaThemeRenderer::load(&config.theme_dir)
                .expect("加载默认主题模板失败")
                .with_data(theme_data.clone());
            assert_eq!(
                config.theme_dir.file_name().and_then(|name| name.to_str()),
                Some(fallback_renderer.slug()),
                "默认主题目录名必须与清单 slug 一致"
            );
            let fallback_slug = fallback_renderer.slug().to_string();
            let mut registry = ThemeRegistry::new(fallback_slug.clone());
            let mut theme_assets = Vec::new();
            theme_assets.push((fallback_slug.clone(), config.theme_dir.join("assets")));
            registry
                .add(
                    fallback_slug,
                    fallback_renderer.name().to_string(),
                    Arc::new(fallback_renderer),
                )
                .expect("默认主题清单无效");
            if let Some(parent) = config.theme_dir.parent() {
                for entry in std::fs::read_dir(parent).expect("读取主题目录失败") {
                    let entry = entry.expect("读取主题目录项失败");
                    let dir = entry.path();
                    if dir == config.theme_dir
                        || entry.file_type().is_ok_and(|kind| kind.is_symlink())
                        || !dir.is_dir()
                        || !dir.join("theme.json").is_file()
                    {
                        continue;
                    }
                    match MiniJinjaThemeRenderer::load(&dir) {
                        Ok(renderer)
                            if dir.file_name().and_then(|s| s.to_str())
                                == Some(renderer.slug()) =>
                        {
                            let slug = renderer.slug().to_string();
                            let name = renderer.name().to_string();
                            if registry
                                .add(
                                    slug.clone(),
                                    name,
                                    Arc::new(renderer.with_data(theme_data.clone())),
                                )
                                .is_ok()
                            {
                                theme_assets.push((slug, dir.join("assets")));
                            }
                        }
                        Ok(_) => eprintln!("跳过主题 {}：目录名与清单 slug 不一致", dir.display()),
                        Err(e) => eprintln!("跳过无效主题 {}：{e}", dir.display()),
                    }
                }
            }
            registry.validate().expect("默认主题未安装");
            let registry = Arc::new(registry);
            let theme_store: Arc<dyn ThemeSettingsStore> =
                Arc::new(infrastructure::PostgresSettingsStore::new(pool.clone()));
            let renderer = registry
                .renderer(registry.fallback())
                .expect("默认主题未安装");
            let markdown = Arc::new(SanitizingMarkdownRenderer::new());

            let users = Arc::new(UserInteractor::new(
                user_repo.clone(),
                rbac_store.clone(),
                clock.clone(),
            ));

            // 认证装配：会话落 PostgreSQL（重启后仍登录、跨进程共享撤销），
            // OAuth 尝试仍在内存（一次性 state，进程重启即作废是有意行为）。
            let secrets: Arc<dyn application::ports::SecretSource> =
                Arc::new(infrastructure::EnvSecretSource);
            let identity_client: Arc<dyn application::ports::ExternalIdentityClient> =
                Arc::new(infrastructure::ReqwestIdentityClient::new(secrets));
            let random: Arc<dyn application::ports::SecureRandom> =
                Arc::new(infrastructure::SystemSecureRandom);
            let session_store: Arc<dyn application::ports::SessionStore> = Arc::new(
                infrastructure::PostgresSessionStore::with_defaults(pool.clone()),
            );
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

            let base_url = config.public_base_url.as_str().to_string();
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
                series_repo.clone(),
                clock.clone(),
            ));
            let pages = Arc::new(PageInteractor::new(page_repo, clock.clone()));
            let tags = Arc::new(TagInteractor::new(tag_repo, clock.clone()));
            let categories = Arc::new(CategoryInteractor::new(category_repo, clock.clone()));
            let series = Arc::new(SeriesInteractor::new(series_repo, clock.clone()));
            // 媒体库：文件在本地随机 id 路径下，元数据与引用关系在 PostgreSQL。
            let media_repo: Arc<dyn application::ports::MediaRepository> =
                Arc::new(infrastructure::PostgresMediaRepository::new(pool.clone()));
            let media_storage: Arc<dyn application::ports::MediaStorage> = Arc::new(
                infrastructure::LocalMediaStorage::new(config.media_dir.clone()),
            );
            let media = Arc::new(application::media::MediaInteractor::new(
                media_repo,
                media_storage,
                clock.clone(),
            ));
            // 站点信息：数据库 settings.site > 装配回退值（环境变量/默认值）。
            // 同一存储实例供公开渲染与管理用例共享，保存后公开页面即时生效。
            let settings_store: Arc<dyn SettingsStore> =
                Arc::new(infrastructure::PostgresSettingsStore::new(pool.clone()));
            let site_fallback = SiteInfo {
                title: config.site_title,
                description: config.site_description,
                logo_url: None,
            };
            let settings = Arc::new(
                SettingsInteractor::new(
                    settings_store.clone(),
                    clock.clone(),
                    site_fallback.clone(),
                )
                .with_themes(theme_store.clone(), registry.clone()),
            );
            let public_site = Arc::new(
                PublicSiteInteractor::new(
                    public_query,
                    public_page_query,
                    public_tag_query,
                    public_category_query,
                    public_series_query,
                    markdown,
                    renderer,
                    settings_store,
                    site_fallback,
                    config.public_base_url.clone(),
                )
                .with_themes(theme_store, registry),
            );

            let deps = CliDeps {
                users,
                posts,
                pages,
                tags,
                categories,
                series,
                settings,
                roles,
                auth,
                passwords,
                media,
                secure_cookies,
                public_site,
                user_repo,
                assets_dir: None,
                theme_assets,
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
