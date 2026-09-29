//! First-run lifecycle: durable local journal, atomic database bootstrap, then
//! in-place router activation. The listener stays open throughout installation.

use crate::config::{DeploymentConfig, InstallJournal};
use application::{
    UseCaseError,
    audit::AuditContext,
    installation::{InitialAdmin, InstallInfo, InstallInput, Installer, validate_database_url},
    ports::SecureRandom,
};
use axum::{Router, extract::Request};
use std::sync::{Arc, RwLock, Weak};
use tokio::sync::{Mutex, watch};
use tower::ServiceExt;

struct LiveSite {
    router: RwLock<Router>,
    pool: watch::Sender<Option<infrastructure::Database>>,
    telemetry: interfaces::observability::Telemetry,
}

struct Setup {
    saved: RwLock<Option<InstallJournal>>,
    config: DeploymentConfig,
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
            public_base_url: self
                .config
                .configured_public_url()
                .ok()
                .flatten()
                .or_else(|| {
                    saved
                        .as_ref()
                        .and_then(|saved| saved.deployment(&self.config).ok())
                        .and_then(|config| config.configured_public_url().ok().flatten())
                }),
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
        let admin = InitialAdmin::prepare(
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
                let public_base_url = self
                    .config
                    .configured_public_url()
                    .map_err(UseCaseError::Invalid)?
                    .unwrap_or(input.public_base_url);
                let public_base_url = application::seo::PublicBaseUrl::parse(&public_base_url)
                    .map_err(|e| UseCaseError::Invalid(e.to_string()))?
                    .as_str()
                    .to_owned();
                InstallJournal::prepare(
                    &self.config,
                    &input.database_url,
                    &public_base_url,
                    infrastructure::SystemSecureRandom.token_hex()?,
                )
                .map_err(UseCaseError::Invalid)?
            }
        };
        let deployment = if self
            .config
            .configured_database_url()
            .map_err(UseCaseError::Invalid)?
            .is_some()
        {
            self.config.clone()
        } else {
            saved
                .deployment(&self.config)
                .map_err(UseCaseError::Invalid)?
        };
        let site = deployment
            .site(Some(self.bind.clone()))
            .map_err(UseCaseError::Invalid)?;
        let initial_site = deployment.bootstrap_site().map_err(UseCaseError::Invalid)?;
        let database_url = saved
            .pending_database_url()
            .map_err(UseCaseError::Invalid)?;
        // A successful setup must lead to an available login page.
        if !site.admin_dist.join("index.html").is_file() {
            return Err(UseCaseError::Invalid(
                "后台资源未构建，请先运行 pnpm --dir apps/admin build，或检查 BLOG_ADMIN_DIST"
                    .into(),
            ));
        }
        let database = deployment.database().map_err(UseCaseError::Invalid)?;
        let schema_contract =
            infrastructure::schema_contract::SchemaContract::load(&database.migrations_dir)?;
        let pool = infrastructure::installation::connect(&database_url).await?;
        let complete = previous.is_some()
            && infrastructure::installation::is_complete(&pool, &saved.installation_id).await?;
        if !complete {
            infrastructure::installation::check_target(&pool, &schema_contract, previous.is_some())
                .await?;
            if previous.is_none() {
                saved.publish(&self.config).map_err(UseCaseError::Invalid)?;
                *self.saved.write().expect("saved config lock") = Some(saved.clone());
            }
            infrastructure::migrate_schema(&pool, database.migrations_dir)
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
        let runtime_pool = infrastructure::connect_with_config(&database_url, &database.pool)
            .await
            .map_err(|_| UseCaseError::Invalid("连接 PostgreSQL 失败，请检查网络后重试".into()))?;
        let app = crate::website::build_router(
            &runtime_pool,
            &site,
            crate::assembly::roles(&runtime_pool),
            Arc::new(
                infrastructure::RenderingRuntime::default()
                    .with_observer(Arc::new(live.telemetry.clone())),
            ),
            &live.telemetry,
        )
        .await
        .map_err(|_| {
            UseCaseError::Invalid("加载站点主题失败，请检查 BLOG_THEME_DIR 和主题文件后重试".into())
        })?;
        if !complete {
            infrastructure::installation::initialize(
                &pool,
                &schema_contract,
                &saved.installation_id,
                &admin,
                &initial_site,
                audit,
            )
            .await?;
        }
        // Cleanup must not turn a committed installation into an HTTP failure.
        // A crash or failed unlink leaves the journal for startup to retry.
        cleanup_completed(&deployment, &saved);
        *live.router.write().expect("live router lock") = app;
        live.pool.send_replace(Some(runtime_pool));
        crate::notice(format_args!(
            "安装完成，安装入口已关闭。登录地址：{}/admin/",
            site.public_base_url.as_str().trim_end_matches('/')
        ));
        Ok(())
    }
}

pub fn cleanup_completed(config: &DeploymentConfig, saved: &InstallJournal) {
    if let Err(error) = saved.remove_completed(config) {
        tracing::warn!(%error, "安装已完成，临时安装日志清理失败；下次启动会重试残留日志");
    }
}

pub async fn serve(
    config: DeploymentConfig,
    addr: Option<String>,
    saved: Option<InstallJournal>,
) -> Result<(), String> {
    if config.recovery_mode()? {
        return Err("恢复核验模式不能启动安装向导，请显式配置 DATABASE_URL".into());
    }
    // Validate deployment-owned options before opening the installer.
    let site = config.site(addr)?;
    config.bootstrap_site()?;
    config.database_pool()?;
    let bind = site.bind.clone();
    let metrics_listener = crate::observability::bind(config.metrics_bind()?).await?;
    let telemetry = interfaces::observability::Telemetry::new(&crate::observability::build_info());
    let token = infrastructure::SystemSecureRandom
        .token_hex()
        .map_err(|e| e.to_string())?;
    let (pool, _) = watch::channel(None);
    let live = Arc::new(LiveSite {
        router: RwLock::new(Router::new()),
        pool,
        telemetry: telemetry.clone(),
    });
    let setup = Arc::new(Setup {
        saved: RwLock::new(saved),
        config,
        bind: bind.clone(),
        gate: Mutex::new(()),
        live: Arc::downgrade(&live),
    });
    let metrics_pool = live.pool.subscribe();
    *live.router.write().expect("live router lock") =
        interfaces::http_install::install_router(interfaces::http_install::InstallState {
            installer: setup,
            token: token.clone(),
        })
        .layer(axum::Extension(telemetry.clone()))
        .layer(axum::Extension(crate::observability::build_info()))
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
    crate::notice(format_args!("首次安装：http://{address}/install"));
    crate::notice(format_args!("安装码：{token}"));
    let server = crate::serve_http(
        listener,
        app,
        metrics_listener,
        telemetry.clone(),
        metrics_pool,
        site.http,
        true,
    );
    server.await
}
