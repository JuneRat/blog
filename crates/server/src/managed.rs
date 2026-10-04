//! A database-independent listener with a drainable business router. Maintenance
//! keeps the recovery routes alive and replaces the business runtime afterwards.
use crate::{
    config::{DeploymentConfig, InstallJournal},
    tasks::TaskSupervisor,
};
use axum::{
    Router,
    extract::Request,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use std::sync::{
    Arc, RwLock,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::{RwLock as AsyncRwLock, watch};
use tower::ServiceExt;

pub(crate) struct LiveSite {
    pub router: RwLock<Router>,
    pub pool: watch::Sender<Option<infrastructure::Database>>,
    pub telemetry: interfaces::observability::Telemetry,
    pub config: RwLock<DeploymentConfig>,
    pub admin: RwLock<Option<interfaces::http_auth::AdminState>>,
    pub tasks: RwLock<Arc<TaskSupervisor>>,
    pub gate: Arc<AsyncRwLock<()>>,
    pub paused: AtomicBool,
    pub installing: AtomicBool,
    pub installation_token: RwLock<Option<String>>,
    pub bind: String,
}

impl LiveSite {
    pub fn new(
        config: DeploymentConfig,
        bind: String,
        telemetry: interfaces::observability::Telemetry,
    ) -> Self {
        let (pool, _) = watch::channel(None);
        Self {
            router: RwLock::new(Router::new()),
            pool,
            telemetry,
            config: RwLock::new(config),
            admin: RwLock::new(None),
            tasks: RwLock::new(Arc::new(TaskSupervisor::default())),
            gate: Arc::new(AsyncRwLock::new(())),
            paused: AtomicBool::new(false),
            installing: AtomicBool::new(false),
            installation_token: RwLock::new(None),
            bind,
        }
    }
    pub fn tasks(&self) -> Arc<TaskSupervisor> {
        self.tasks.read().expect("tasks lock").clone()
    }
    pub fn config(&self) -> DeploymentConfig {
        self.config.read().expect("config lock").clone()
    }
    pub fn publish(
        &self,
        config: DeploymentConfig,
        app: crate::website::WebsiteSetup,
        pool: infrastructure::Database,
    ) {
        *self.config.write().expect("config lock") = config;
        *self.admin.write().expect("admin lock") = Some(app.admin);
        *self.router.write().expect("router lock") = app.router;
        self.pool.send_replace(Some(pool));
        self.tasks().activate(app.tasks);
        if !self.paused.load(Ordering::Acquire) {
            self.tasks().start();
        }
        self.installing.store(false, Ordering::Release);
        *self.installation_token.write().expect("installation lock") = None;
    }
    pub async fn dispatch(self: Arc<Self>, request: Request) -> Response {
        if self.paused.load(Ordering::Acquire) {
            return maintenance(request.uri().path());
        }
        let _reader = self.gate.read().await;
        if self.paused.load(Ordering::Acquire) {
            return maintenance(request.uri().path());
        }
        let router = self.router.read().expect("router lock").clone();
        router
            .oneshot(request)
            .await
            .unwrap_or_else(|never| match never {})
    }
    pub async fn stop(&self) {
        let tasks = self.tasks();
        tasks
            .shutdown(tokio::time::Instant::now() + std::time::Duration::from_secs(25))
            .await;
        tasks.close_maintenance_pool().await;
        let pool = self.pool.send_replace(None);
        *self.admin.write().expect("admin lock") = None;
        // Drop every route-held theme/package guard before assembling the next runtime.
        *self.router.write().expect("router lock") = Router::new();
        if let Some(pool) = pool {
            pool.close().await;
        }
        *self.tasks.write().expect("tasks lock") = Arc::new(TaskSupervisor::default());
    }
    pub async fn activate(&self) -> Result<(), String> {
        let config = self.config().reload()?;
        let database = config.http_database()?;
        let site = config.site(Some(self.bind.clone()))?;
        let pool = infrastructure::connect_with_config(&database.url, &database.pool)
            .await
            .map_err(|_| "数据库暂不可用，请检查数据库服务后重试")?;
        let result = async {
            if pool
                .is_recovery_isolated()
                .await
                .map_err(|_| "无法检查数据库恢复状态")?
            {
                return Err("该数据库仍处于旧版恢复隔离状态".to_string());
            }
            infrastructure::migrate_schema(&pool, &database.migrations_dir)
                .await
                .map_err(|_| "数据库版本校验失败，站点暂未启动")?;
            let roles = crate::assembly::roles(&pool);
            roles
                .sync_registry()
                .await
                .map_err(|_| "无法加载站点权限目录")?;
            crate::website::build_router(
                &pool,
                &site,
                roles,
                Arc::new(
                    infrastructure::RenderingRuntime::default()
                        .with_observer(Arc::new(self.telemetry.clone())),
                ),
                &self.telemetry,
                crate::website::TaskEnvironment {
                    supervisor: self.tasks(),
                    maintenance: crate::tasks::maintenance_pool(&config, &database.url, &pool)
                        .await,
                    recovery_mode: false,
                    installation_preflight: false,
                },
            )
            .await
        }
        .await;
        match result {
            Ok(app) => {
                self.publish(config, app, pool);
                Ok(())
            }
            Err(error) => {
                pool.close().await;
                Err(error)
            }
        }
    }
}

fn maintenance(path: &str) -> Response {
    if path == "/livez" {
        return StatusCode::OK.into_response();
    }
    (StatusCode::SERVICE_UNAVAILABLE,
     [("retry-after", "30"), ("cache-control", "no-store"), ("content-type", "text/html; charset=utf-8")],
     "<!doctype html><html lang=zh-CN><meta name=viewport content='width=device-width,initial-scale=1'><title>站点维护中</title><main><h1>站点暂时维护中</h1><p>备份或恢复完成后会自动开放访问。</p><p>管理员可前往 <a href='/recovery'>备份与恢复</a> 查看状态。</p></main></html>").into_response()
}

pub async fn serve(mut config: DeploymentConfig, addr: Option<String>) -> Result<(), String> {
    let recovery_root = std::env::var_os("BLOG_BACKUP_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            config
                .path
                .parent()
                .unwrap_or(std::path::Path::new("."))
                .join("recovery")
        });
    let journal = if recovery_root.join("RECOVERY_REQUIRED").exists() {
        None
    } else {
        InstallJournal::read(&config.path)?
    };
    if let Some(saved) = &journal {
        config = saved.recover_config(&config)?;
    }
    let site = config.site(addr)?;
    let telemetry = interfaces::observability::Telemetry::new(&crate::observability::build_info());
    let live = Arc::new(LiveSite::new(
        config.clone(),
        site.bind.clone(),
        telemetry.clone(),
    ));
    let controller = crate::backup::Controller::new(live.clone()).await?;
    let pending_restore = controller.recovery_required();
    if pending_restore {
        live.paused.store(true, Ordering::Release);
    } else {
        let install_needed = match config.configured_database_url()? {
            None => Ok(true),
            Some(url) => {
                match infrastructure::connect_with_config(&url, &config.http_database_pool()?).await
                {
                    Ok(pool) => {
                        let result = if let Some(saved) = &journal {
                            infrastructure::installation::is_complete(&pool, &saved.installation_id)
                                .await
                                .map(|v| !v)
                        } else {
                            infrastructure::installation::needs_installation(&pool).await
                        };
                        pool.close().await;
                        result.map_err(|_| "无法检查安装状态".to_owned())
                    }
                    Err(_) => Err("数据库暂不可用".to_string()),
                }
            }
        };
        match install_needed {
            Ok(true) => crate::installation::prepare_router(live.clone(), config, journal)?,
            Ok(false) => {
                if let Some(saved) = &journal {
                    crate::installation::cleanup_completed(&config, saved);
                }
                if let Err(error) = live.activate().await {
                    tracing::warn!(%error, "站点启动失败，应急恢复入口仍可访问");
                    live.paused.store(true, Ordering::Release);
                }
            }
            Err(error) => {
                tracing::warn!(%error, "站点启动失败，应急恢复入口仍可访问");
                live.paused.store(true, Ordering::Release);
            }
        }
    }
    let fallback = live.clone();
    let app = interfaces::http_backup::router(controller.clone(), site.secure_cookies)
        .fallback(move |request: Request| fallback.clone().dispatch(request))
        .layer(axum::Extension(interfaces::http_client_ip::TrustedProxies(
            site.trusted_proxies.clone(),
        )));
    let listener = tokio::net::TcpListener::bind(&site.bind)
        .await
        .map_err(|e| format!("绑定失败：{e}"))?;
    let metrics = crate::observability::bind(configured_metrics(&live)?).await?;
    crate::notice(format_args!(
        "站点已启动：http://{}；备份与恢复：/recovery",
        listener.local_addr().map_err(|e| e.to_string())?
    ));
    let (stop, stopped) = watch::channel(false);
    let mut running = tokio::task::JoinSet::new();
    running.spawn(crate::transport::serve(
        listener,
        app,
        stopped.clone(),
        site.http,
    ));
    if let Some(listener) = metrics {
        running.spawn(crate::observability::serve(
            listener,
            telemetry,
            live.pool.subscribe(),
            stopped.clone(),
            site.http,
        ));
    }
    let scheduled = controller.clone();
    let schedule = tokio::spawn(async move {
        scheduled.schedule(stopped).await;
    });
    let result = tokio::select! {
        _ = crate::shutdown_signal() => Ok(()),
        done = running.join_next() => match done {
            Some(Ok(result)) => result, Some(Err(_)) => Err("HTTP 服务意外退出".into()), None => Ok(())
        }
    };
    live.paused.store(true, Ordering::Release);
    stop.send_replace(true);
    schedule.abort();
    controller.shutdown().await;
    live.stop().await;
    let deadline = site.http.shutdown;
    if tokio::time::timeout(deadline, async {
        while running.join_next().await.is_some() {}
    })
    .await
    .is_err()
    {
        running.abort_all();
    }
    result
}
fn configured_metrics(live: &LiveSite) -> Result<Option<std::net::SocketAddr>, String> {
    live.config().metrics_bind()
}
