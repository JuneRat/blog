//! Full HTTP assembly. Theme files, static assets, public URLs and browser
//! authentication belong exclusively to `serve`.

use std::sync::Arc;

use application::category::CategoryInteractor;
use application::identity::RoleInteractor;
use application::page::PageInteractor;
use application::public_site::PublicSiteInteractor;
use application::series::SeriesInteractor;
use application::settings::SettingsInteractor;
use application::tag::TagInteractor;
use application::theme_data::ThemeData;
use infrastructure::Database;
use infrastructure::{
    PostgresCategoryRepository, PostgresPageRepository, PostgresPublishedCategoryQuery,
    PostgresPublishedPageQuery, PostgresPublishedPostQuery, PostgresPublishedSeriesQuery,
    PostgresPublishedTagQuery, PostgresSeriesRepository, PostgresTagRepository, RenderingRuntime,
    SystemClock,
};
use interfaces::http::{AppState, HttpAssets, HttpConfig, PublicSiteState};
use interfaces::http_auth::{AdminState, AuthState};

use crate::assembly;
use crate::config::SiteConfig;

pub struct TaskEnvironment {
    pub supervisor: Arc<crate::tasks::TaskSupervisor>,
    pub maintenance: Option<Database>,
    pub recovery_mode: bool,
    pub installation_preflight: bool,
}
pub struct WebsiteSetup {
    pub router: axum::Router,
    pub tasks: Arc<crate::tasks::TaskRuntime>,
    pub theme_packages: Arc<infrastructure::theme_packages::LocalThemePackages>,
}
pub async fn build_router(
    pool: &Database,
    config: &SiteConfig,
    roles: Arc<RoleInteractor>,
    runtime: Arc<RenderingRuntime>,
    telemetry: &interfaces::observability::Telemetry,
    environment: TaskEnvironment,
) -> Result<WebsiteSetup, String> {
    let plugins = assembly::plugins(pool);
    let runtime = Arc::new(runtime.as_ref().clone().with_plugins(plugins.clone()));
    let time_zones = Arc::new(infrastructure::IanaTimeZones);
    let public_posts = Arc::new(PostgresPublishedPostQuery::new(pool.clone()));
    let public_pages = Arc::new(PostgresPublishedPageQuery::new(pool.clone()));
    let public_tags = Arc::new(PostgresPublishedTagQuery::new(pool.clone()));
    let public_categories = Arc::new(PostgresPublishedCategoryQuery::new(pool.clone()));
    let public_series = Arc::new(PostgresPublishedSeriesQuery::new(pool.clone()));
    let theme_data = Arc::new(ThemeData::new(
        public_posts.clone(),
        public_tags.clone(),
        public_categories.clone(),
    ));
    let theme_configs = Arc::new(infrastructure::themes::PostgresThemesStore::new(
        pool.clone(),
    ));
    let packages = if environment.installation_preflight {
        infrastructure::theme_packages::LocalThemePackages::load_for_installation(
            &config.theme_dir,
            theme_data,
            runtime.clone(),
            theme_configs.as_ref().clone(),
        )
        .await
    } else {
        infrastructure::theme_packages::LocalThemePackages::load_persistent(
            &config.theme_dir,
            theme_data,
            runtime.clone(),
            theme_configs.as_ref().clone(),
            !environment.recovery_mode,
        )
        .await
    };
    let theme_packages = Arc::new(
        packages
            .map_err(|error| format!("加载默认主题模板失败：{error}"))?
            .with_mutations_enabled(!environment.recovery_mode),
    );
    let theme_registry = theme_packages.registry();
    let fallback = theme_registry
        .renderer(theme_registry.fallback())
        .map_err(|error| format!("默认主题未安装：{error}"))?;

    let clock = Arc::new(SystemClock);
    let media_guard = Arc::new(infrastructure::PostgresMediaRepository::new(pool.clone()));
    let settings_store = Arc::new(
        infrastructure::PostgresSettingsStore::new(pool.clone())
            .with_read_observer(Arc::new(telemetry.clone())),
    );
    let settings = Arc::new(
        SettingsInteractor::new(
            settings_store.clone(),
            clock.clone(),
            config.site.clone(),
            media_guard.clone(),
        )
        .with_themes(settings_store.clone(), theme_registry.clone())
        .with_theme_packages(theme_packages.clone())
        .with_theme_configs(theme_configs.clone())
        .with_time_zones(time_zones.clone()),
    );
    let public_site = Arc::new(
        PublicSiteInteractor::new(
            public_posts,
            public_pages,
            public_tags,
            public_categories,
            public_series,
            fallback,
            settings_store.clone(),
            config.site.clone(),
            config.public_base_url.clone(),
        )
        .with_discovery(Arc::new(
            infrastructure::persistence::PostgresPublicDiscoveryQuery::new(pool.clone()),
        ))
        .with_themes(settings_store, theme_registry.clone())
        .with_theme_configs(theme_configs)
        .with_time_zones(time_zones),
    );

    let users = assembly::users(pool);
    let sessions = assembly::sessions(pool);
    let passwords = if environment.recovery_mode {
        assembly::passwords(pool, sessions.clone())
    } else {
        assembly::passwords_with_mail(pool, sessions.clone(), config)?
    };
    let auth = assembly::auth(
        pool,
        users.clone(),
        sessions,
        config.public_base_url.as_str().into(),
    );
    let auth_state = AuthState {
        registration: assembly::registration(pool),
        admission: Arc::new(infrastructure::InMemoryRequestAdmission::default()),
        auth: auth.clone(),
        passwords: passwords.clone(),
        secure_cookies: config.secure_cookies,
    };
    let admin = AdminState {
        content_queries: assembly::content_queries(pool),
        auth,
        users,
        passwords,
        posts: assembly::posts(pool, runtime.clone()),
        pages: Arc::new(PageInteractor::new(
            Arc::new(PostgresPageRepository::new(pool.clone(), runtime.clone())),
            clock.clone(),
        )),
        tags: Arc::new(TagInteractor::new(
            Arc::new(PostgresTagRepository::new(pool.clone())),
            clock.clone(),
        )),
        categories: Arc::new(CategoryInteractor::new(
            Arc::new(PostgresCategoryRepository::new(pool.clone())),
            clock.clone(),
        )),
        series: Arc::new(SeriesInteractor::new(
            Arc::new(PostgresSeriesRepository::new(pool.clone())),
            clock,
            media_guard,
        )),
        settings,
        roles,
        media: assembly::media(pool, config.media_dir.clone()),
        secure_cookies: config.secure_cookies,
    };
    let comments = Arc::new(application::comments::CommentInteractor::new(
        Arc::new(infrastructure::comments::PostgresCommentRepository::new(
            pool.clone(),
            runtime.clone(),
        )),
        runtime.clone(),
    ));
    let retention = Arc::new(application::retention::RetentionInteractor::new(Arc::new(
        infrastructure::retention::PostgresRetentionStore::new(pool.clone()),
    )));
    let audit = Arc::new(application::audit::AuditInteractor::new(Arc::new(
        infrastructure::audit::PostgresAuditQuery::new(pool.clone()),
    )));
    let task_runtime = Arc::new(crate::tasks::TaskRuntime::new(
        pool.clone(),
        environment.maintenance,
        runtime.clone(),
        telemetry.clone(),
        environment.recovery_mode,
    ));
    let task_admin = Arc::new(crate::tasks::WebsiteTasks::new(
        task_runtime.clone(),
        environment.supervisor,
    ));
    let tasks = Arc::new(application::tasks::TasksInteractor::new(
        task_admin.clone(),
        Arc::new(SystemClock),
    ));
    let html_rebuild = Arc::new(
        application::html_rebuild_admin::HtmlRebuildAdminInteractor::new(Arc::new(
            crate::html_rebuild::WebsiteHtmlRebuildJobs::new(task_admin),
        )),
    );
    Ok(WebsiteSetup {
        theme_packages,
        tasks: task_runtime,
        router: interfaces::http::app_router(
            AppState {
                public: PublicSiteState {
                    site: public_site,
                    health: Some(Arc::new(infrastructure::PgHealthCheck::new(pool.clone()))),
                },
                auth: auth_state,
                admin,
                comments,
                content_preview: Arc::new(application::content_preview::ContentPreview::new(
                    runtime,
                )),
                retention,
                audit,
                html_rebuild,
                tasks,
                plugins: plugins.manager.clone(),
            },
            HttpAssets {
                themes: theme_registry,
                plugins: plugins.catalog.assets(),
                admin_dist: config.admin_dist.clone(),
            },
            HttpConfig {
                public_origin: config
                    .public_base_url
                    .as_str()
                    .trim_end_matches('/')
                    .to_string(),
                trusted_proxies: config.trusted_proxies.clone(),
            },
        )
        .layer(axum::Extension(telemetry.clone()))
        .layer(axum::Extension(crate::observability::build_info())),
    })
}
