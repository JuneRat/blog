//! First-run lifecycle: durable local journal, atomic database bootstrap, then
//! in-place router activation. The listener stays open throughout installation.

use crate::config::{self, SavedConfig};
use application::{
    UseCaseError,
    audit::AuditContext,
    installation::{InitialOwner, InstallInfo, InstallInput, Installer, validate_database_url},
    ports::SecureRandom,
};
use axum::{Router, extract::Request};
use std::sync::{Arc, RwLock, Weak};
use tokio::sync::{Mutex, watch};
use tower::ServiceExt;

struct LiveSite {
    router: RwLock<Router>,
    pool: watch::Sender<Option<sqlx::PgPool>>,
}

struct Setup {
    saved: RwLock<Option<SavedConfig>>,
    config_path: std::path::PathBuf,
    bind: String,
    gate: Mutex<()>,
    live: Weak<LiveSite>,
}

#[async_trait::async_trait]
impl Installer for Setup {
    fn info(&self) -> InstallInfo {
        let saved = self.saved.read().expect("saved config lock");
        InstallInfo {
            database_configured: saved.is_some(),
            public_base_url: std::env::var("BLOG_PUBLIC_BASE_URL")
                .ok()
                .or_else(|| saved.as_ref().map(|s| s.public_base_url.clone())),
        }
    }

    async fn install(&self, input: InstallInput, audit: AuditContext) -> Result<(), UseCaseError> {
        let _gate = self
            .gate
            .try_lock()
            .map_err(|_| UseCaseError::RateLimited {
                retry_after_secs: 2,
            })?;
        let live = self
            .live
            .upgrade()
            .ok_or_else(|| UseCaseError::NotFound("安装入口".into()))?;
        if live.pool.borrow().is_some() {
            return Err(UseCaseError::NotFound("安装入口已关闭".into()));
        }
        let owner = InitialOwner::prepare(
            &input.username,
            &input.password,
            &infrastructure::Argon2PasswordHasher::with_defaults(),
            time::OffsetDateTime::now_utc(),
        )
        .await?;
        let previous = self.saved.read().expect("saved config lock").clone();
        let saved = match previous.clone() {
            Some(saved) => saved,
            None => {
                validate_database_url(&input.database_url)?;
                let public_base_url =
                    std::env::var("BLOG_PUBLIC_BASE_URL").unwrap_or(input.public_base_url);
                let public_base_url = application::seo::PublicBaseUrl::parse(&public_base_url)
                    .map_err(|e| UseCaseError::Invalid(e.to_string()))?
                    .as_str()
                    .to_owned();
                SavedConfig {
                    database_url: input.database_url,
                    public_base_url,
                    installation_id: infrastructure::SystemSecureRandom.token_hex()?,
                }
            }
        };
        let site = config::SiteConfig::from_env(Some(self.bind.clone()), Some(&saved))
            .map_err(UseCaseError::Invalid)?;
        // A successful setup must lead to an available login page.
        if !site.admin_dist.join("index.html").is_file() {
            return Err(UseCaseError::Invalid(
                "后台资源未构建，请先运行 pnpm --dir apps/admin build，或检查 BLOG_ADMIN_DIST"
                    .into(),
            ));
        }
        let pool = infrastructure::installation::connect(&saved.database_url).await?;
        let complete = previous.is_some()
            && infrastructure::installation::is_complete(&pool, &saved.installation_id).await?;
        if !complete {
            infrastructure::installation::check_target(&pool, previous.is_some()).await?;
            if previous.is_none() {
                config::save_new(&self.config_path, &saved).map_err(UseCaseError::Invalid)?;
                *self.saved.write().expect("saved config lock") = Some(saved.clone());
            }
            infrastructure::migrate_schema(
                &pool,
                config::DatabaseConfig::from_env(Some(&saved)).migrations_dir,
            )
            .await
            .map_err(|_| {
                UseCaseError::Invalid(
                    "初始化数据库结构失败；请检查迁移目录和建表权限，重试会继续使用已保存配置"
                        .into(),
                )
            })?;
        }
        // Preflight website assembly before creating any account. The runtime
        // pool uses normal connection settings, not installation query limits.
        let runtime_pool = infrastructure::connect(&saved.database_url)
            .await
            .map_err(|_| UseCaseError::Invalid("连接 PostgreSQL 失败，请检查网络后重试".into()))?;
        let app = crate::website::build_router(
            &runtime_pool,
            &site,
            crate::assembly::roles(&runtime_pool),
            Arc::new(infrastructure::RenderingRuntime::default()),
        )
        .await
        .map_err(|_| {
            UseCaseError::Invalid("加载站点主题失败，请检查 BLOG_THEME_DIR 和主题文件后重试".into())
        })?;
        if !complete {
            infrastructure::installation::initialize(&pool, &saved.installation_id, &owner, audit)
                .await?;
        }
        // No fallible work after the commit. The durable journal already exists;
        // a crash here boots directly into the completed site on the next run.
        *live.router.write().expect("live router lock") = app;
        live.pool.send_replace(Some(runtime_pool));
        println!(
            "安装完成，安装入口已关闭。登录地址：{}/admin/",
            site.public_base_url.as_str().trim_end_matches('/')
        );
        Ok(())
    }
}

pub async fn serve(addr: Option<String>, saved: Option<SavedConfig>) -> Result<(), String> {
    if crate::recovery::mode()? {
        return Err("恢复核验模式不能启动安装向导，请显式配置 DATABASE_URL".into());
    }
    let bind = config::bind_address(addr);
    // Validate deployment-owned options before opening the installer.
    let site = config::SiteConfig::from_env(Some(bind.clone()), saved.as_ref())?;
    let token = infrastructure::SystemSecureRandom
        .token_hex()
        .map_err(|e| e.to_string())?;
    let (pool, mut receiver) = watch::channel(None);
    let live = Arc::new(LiveSite {
        router: RwLock::new(Router::new()),
        pool,
    });
    let setup = Arc::new(Setup {
        saved: RwLock::new(saved),
        config_path: config::config_path(),
        bind: bind.clone(),
        gate: Mutex::new(()),
        live: Arc::downgrade(&live),
    });
    *live.router.write().expect("live router lock") =
        interfaces::http_install::install_router(interfaces::http_install::InstallState {
            installer: setup,
            token: token.clone(),
        })
        .layer(axum::Extension(interfaces::http_client_ip::TrustedProxies(
            site.trusted_proxies,
        )));
    let app = Router::new().fallback(move |request: Request| {
        let router = live.router.read().expect("live router lock").clone();
        async move { router.oneshot(request).await }
    });
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .map_err(|e| format!("绑定 {bind} 失败：{e}"))?;
    let address = listener.local_addr().map_err(|e| e.to_string())?;
    println!("首次安装：http://{address}/install");
    println!("安装码：{token}");
    let server = async {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(crate::shutdown_signal())
        .await
        .map_err(|e| format!("服务退出：{e}"))
    };
    let scheduler = async {
        let pool = receiver
            .wait_for(|pool| pool.is_some())
            .await
            .expect("live site retains sender")
            .clone()
            .expect("ready pool");
        crate::publish_scheduler(crate::assembly::publisher(&pool)).await
    };
    tokio::select! { result = server => result, _ = scheduler => unreachable!("scheduler loops") }
}
