//! Full HTTP assembly. Theme files, static assets, public URLs and browser
//! authentication belong exclusively to `serve`.

use std::path::Path;
use std::sync::Arc;

use application::category::CategoryInteractor;
use application::identity::RoleInteractor;
use application::page::PageInteractor;
use application::public_site::PublicSiteInteractor;
use application::series::SeriesInteractor;
use application::settings::SettingsInteractor;
use application::tag::TagInteractor;
use application::theme_data::ThemeData;
use application::themes::ThemeRegistry;
use infrastructure::Database;
use infrastructure::{
    MiniJinjaThemeRenderer, PostgresCategoryRepository, PostgresPageRepository,
    PostgresPublishedCategoryQuery, PostgresPublishedPageQuery, PostgresPublishedPostQuery,
    PostgresPublishedSeriesQuery, PostgresPublishedTagQuery, PostgresSeriesRepository,
    PostgresTagRepository, RenderingRuntime, SystemClock,
};
use interfaces::http::{AppState, HttpAssets, HttpConfig, PublicSiteState};
use interfaces::http_auth::{AdminState, AuthState};

use crate::assembly;
use crate::config::SiteConfig;

pub async fn build_router(
    pool: &Database,
    config: &SiteConfig,
    roles: Arc<RoleInteractor>,
    runtime: Arc<RenderingRuntime>,
    telemetry: &interfaces::observability::Telemetry,
) -> Result<axum::Router, String> {
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
    let installed = load_themes(&config.theme_dir, theme_data, &runtime).await?;
    let fallback = installed
        .registry
        .renderer(installed.registry.fallback())
        .map_err(|error| format!("默认主题未安装：{error}"))?;

    let clock = Arc::new(SystemClock);
    let media_guard = Arc::new(infrastructure::PostgresMediaRepository::new(pool.clone()));
    let settings_store = Arc::new(infrastructure::PostgresSettingsStore::new(pool.clone()));
    let settings = Arc::new(
        SettingsInteractor::new(
            settings_store.clone(),
            clock.clone(),
            config.site.clone(),
            media_guard.clone(),
        )
        .with_themes(settings_store.clone(), installed.registry.clone()),
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
        .with_themes(settings_store, installed.registry),
    );

    let users = assembly::users(pool);
    let sessions = assembly::sessions(pool);
    let passwords = assembly::passwords(pool, sessions.clone());
    let auth = assembly::auth(
        pool,
        users.clone(),
        sessions,
        config.public_base_url.as_str().into(),
    );
    let auth_state = AuthState {
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
        runtime,
    ));
    let retention = Arc::new(application::retention::RetentionInteractor::new(Arc::new(
        infrastructure::retention::PostgresRetentionStore::new(pool.clone()),
    )));
    let audit = Arc::new(application::audit::AuditInteractor::new(Arc::new(
        infrastructure::audit::PostgresAuditQuery::new(pool.clone()),
    )));
    Ok(interfaces::http::app_router(
        AppState {
            public: PublicSiteState {
                site: public_site,
                health: Some(Arc::new(infrastructure::PgHealthCheck::new(pool.clone()))),
            },
            auth: auth_state,
            admin,
            comments,
            retention,
            audit,
        },
        HttpAssets {
            themes: installed.assets,
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
    .layer(axum::Extension(crate::observability::build_info())))
}

struct InstalledThemes {
    registry: Arc<ThemeRegistry>,
    assets: Vec<application::themes::ThemeAssets>,
}

async fn load_themes(
    theme_dir: &Path,
    data: Arc<ThemeData>,
    runtime: &Arc<RenderingRuntime>,
) -> Result<InstalledThemes, String> {
    let fallback = MiniJinjaThemeRenderer::load_checked(theme_dir, runtime)
        .await
        .map_err(|error| format!("加载默认主题模板失败：{error}"))?;
    if theme_dir.file_name().and_then(|name| name.to_str()) != Some(fallback.slug()) {
        return Err("默认主题目录名必须与清单 slug 一致".into());
    }
    let fallback_slug = fallback.slug().to_string();
    let mut registry = ThemeRegistry::new(fallback_slug.clone());
    let mut assets = vec![fallback.assets()];
    registry
        .add(
            fallback_slug,
            fallback.name().to_string(),
            runtime.theme_renderer(fallback.with_data(data.clone())),
        )
        .map_err(|error| format!("默认主题清单无效：{error}"))?;
    if let Some(parent) = theme_dir.parent() {
        for entry in
            std::fs::read_dir(parent).map_err(|error| format!("读取主题目录失败：{error}"))?
        {
            let entry = entry.map_err(|error| format!("读取主题目录项失败：{error}"))?;
            let dir = entry.path();
            if dir == theme_dir
                || entry.file_type().is_ok_and(|kind| kind.is_symlink())
                || !dir.is_dir()
                || !dir.join("theme.json").is_file()
            {
                continue;
            }
            match MiniJinjaThemeRenderer::load_checked(&dir, runtime).await {
                Ok(renderer)
                    if dir.file_name().and_then(|name| name.to_str()) == Some(renderer.slug()) =>
                {
                    let slug = renderer.slug().to_string();
                    let name = renderer.name().to_string();
                    let release_assets = renderer.assets();
                    if registry
                        .add(
                            slug.clone(),
                            name,
                            runtime.theme_renderer(renderer.with_data(data.clone())),
                        )
                        .is_ok()
                    {
                        assets.push(release_assets);
                    }
                }
                Ok(_) => eprintln!("跳过主题 {}：目录名与清单 slug 不一致", dir.display()),
                Err(error) => eprintln!("跳过无效主题 {}：{error}", dir.display()),
            }
        }
    }
    registry
        .validate()
        .map_err(|error| format!("默认主题未安装：{error}"))?;
    Ok(InstalledThemes {
        registry: Arc::new(registry),
        assets,
    })
}
