//! Composition root: select a command, build its dependencies, and own the
//! process listener and shutdown lifecycle. Business rules live in application.

mod assembly;
mod config;
mod html_rebuild;
mod installation;
mod logging;
mod observability;
mod recovery;
mod tasks;
mod transport;
mod website;

use std::sync::Arc;

pub use logging::notice;

use infrastructure::RenderingRuntime;
use interfaces::cli::{Command, parse_args};

#[tokio::main]
async fn main() {
    let cli = parse_args();
    let config = match config::DeploymentConfig::load(cli.config) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("错误：{error}");
            std::process::exit(1);
        }
    };
    let command = cli.command.unwrap_or_default();
    if let Err(error) = logging::init(&config) {
        eprintln!("错误：{error}");
        std::process::exit(1);
    }
    if let Err(error) = run(command, config).await {
        tracing::error!(%error, "命令执行失败");
        std::process::exit(1);
    }
}

async fn run(command: Command, mut config: config::DeploymentConfig) -> Result<(), String> {
    if let Command::Config { action } = command {
        return config::run_command(&config, action);
    }
    if let Command::Maintenance {
        batch_size,
        max_batches,
        dry_run,
    } = command
    {
        let url = config.maintenance_url()?;
        let pool = infrastructure::connect_with_config(&url, &config.database_pool()?)
            .await
            .map_err(|e| e.to_string())?;
        if config.recovery_mode()?
            || pool
                .is_recovery_isolated()
                .await
                .map_err(|error| error.to_string())?
        {
            return Err("恢复隔离期间禁止保留期清理".into());
        }
        return interfaces::cli::run_maintenance(
            &assembly::retention_maintenance(&pool),
            batch_size,
            max_batches,
            dry_run,
        )
        .await;
    }
    let journal = if matches!(command, Command::Serve { .. }) {
        config::InstallJournal::read(&config.path)?
    } else {
        None
    };
    if let Some(journal) = &journal {
        config = journal.recover_config(&config)?;
    }
    if let Command::Serve { addr } = &command
        && config.configured_database_url()?.is_none()
    {
        if journal.is_some() {
            return Err("安装记录存在但数据库地址缺失，请修复 TOML 配置".into());
        }
        return installation::serve(config, addr.clone(), None).await;
    }
    let database = if matches!(command, Command::Serve { .. }) {
        config.http_database()?
    } else {
        config.database()?
    };
    if matches!(command, Command::Media { .. }) {
        config.media_dir()?;
    }
    let pool = infrastructure::connect_with_config(&database.url, &database.pool)
        .await
        .map_err(|error| format!("连接 PostgreSQL 失败：{error}"))?;
    let recovery_mode = config.recovery_mode()?;
    let isolated = pool
        .is_recovery_isolated()
        .await
        .map_err(|error| error.to_string())?;
    if matches!(command, Command::PublishDue) && (isolated || recovery_mode) {
        return Err("恢复隔离期间禁止预约发布任务".into());
    }
    if matches!(command, Command::RebuildHtml { .. }) && (isolated || recovery_mode) {
        return Err("恢复隔离期间禁止 HTML 重建".into());
    }
    if matches!(command, Command::Serve { .. }) && isolated && !recovery_mode {
        return Err("恢复数据库尚未解除隔离；核验请设置 BLOG_RECOVERY_MODE=1，完成后用 recovery.py release 解除".into());
    }
    if let Command::Serve { addr } = &command
        && let Some(saved) = journal.as_ref()
    {
        if !infrastructure::installation::is_complete(&pool, &saved.installation_id)
            .await
            .map_err(|e| e.to_string())?
        {
            if database.url != saved.pending_database_url()? {
                return Err(
                    "未完成安装的数据库目标已改变，拒绝续装；请恢复原连接或为独立部署使用新的配置路径"
                        .into(),
                );
            }
            pool.close().await;
            return installation::serve(config, addr.clone(), Some(saved.clone())).await;
        }
        installation::cleanup_completed(&config, saved);
    }
    // Validate the recovery listener before any migration or permission writes.
    let site_config = if let Command::Serve { addr } = &command {
        let site = config.site(addr.clone())?;
        if recovery_mode {
            recovery::check_bind(&site.bind)?;
        }
        Some(site)
    } else {
        None
    };
    if let Command::RebuildHtml {
        batch_size,
        max_batches,
        dry_run,
    } = command
    {
        let options = application::html_rebuild::RebuildOptions {
            batch_size,
            max_batches,
            dry_run,
        };
        options.validate().map_err(|error| error.to_string())?;
        if dry_run {
            infrastructure::verify_schema(&pool, &database.migrations_dir).await
        } else {
            infrastructure::migrate_schema(&pool, &database.migrations_dir).await
        }
        .map_err(|error| format!("校验或准备迁移失败：{error}"))?;
        return interfaces::cli::run_html_rebuild(&assembly::html_rebuilder(&pool), options).await;
    }
    if let Command::Media {
        action:
            interfaces::cli::MediaAction::Purge {
                legacy_container,
                action,
            },
    } = command
    {
        if isolated || recovery_mode {
            return Err("恢复隔离期间禁止媒体物理清理".into());
        }
        infrastructure::verify_schema(&pool, &database.migrations_dir)
            .await
            .map_err(|e| e.to_string())?;
        let root = match &action {
            interfaces::cli::MediaPurgeAction::Plan {
                media_dir: Some(root),
                ..
            } => root.clone(),
            _ => config.media_dir()?,
        };
        return interfaces::cli::run_media_purge(
            &assembly::media_cleanup(&pool, root, legacy_container),
            action,
        )
        .await;
    }
    // Keep automatic schema initialization for schema owners; restricted runtime
    // roles verify the applied migrations. Derived HTML is rebuilt only by the
    // explicit maintenance command, never as a startup or migration side effect.
    if recovery_mode {
        // Recovery verification must keep the backed-up schema even when the
        // default site account owns it. Upgrade only after releasing isolation.
        infrastructure::verify_schema(&pool, &database.migrations_dir).await
    } else {
        infrastructure::migrate_schema(&pool, &database.migrations_dir).await
    }
    .map_err(|error| format!("执行或校验迁移失败：{error}"))?;
    if matches!(command, Command::Migrate) {
        println!("迁移完成。");
        return Ok(());
    }
    let roles = assembly::roles(&pool);
    roles
        .sync_registry()
        .await
        .map_err(|error| format!("同步权限目录失败：{error}"))?;
    match command {
        Command::Config { .. } => unreachable!("config command returned above"),
        Command::Maintenance { .. } => unreachable!("maintenance returned above"),
        Command::Migrate => unreachable!("migration returned above"),
        Command::RebuildHtml { .. } => unreachable!("HTML rebuild returned above"),
        Command::PublishDue => interfaces::cli::run_publish_due(&assembly::publisher(&pool)).await,
        Command::User { action } => {
            interfaces::cli::run_user(assembly::user_commands(&pool), action).await
        }
        Command::Role { action } => interfaces::cli::run_role(&roles, action).await,
        Command::Oauth { action } => {
            interfaces::cli::run_oauth(&assembly::oauth_commands(&pool), action).await
        }
        Command::Post { action } => {
            interfaces::cli::run_post(
                assembly::post_commands(&pool, Arc::new(RenderingRuntime::default())),
                action,
            )
            .await
        }
        Command::Media { action } => {
            interfaces::cli::run_media(&assembly::media(&pool, config.media_dir()?), action).await
        }
        Command::Serve { .. } => {
            let site = site_config.expect("serve configuration was validated above");
            let telemetry = interfaces::observability::Telemetry::new(&observability::build_info());
            let metrics_listener = observability::bind(config.metrics_bind()?).await?;
            let tasks = Arc::new(tasks::TaskSupervisor::default());
            let maintenance = if recovery_mode {
                None
            } else {
                tasks::maintenance_pool(&config, &database.url, &pool).await
            };
            let app = website::build_router(
                &pool,
                &site,
                roles,
                Arc::new(RenderingRuntime::default().with_observer(Arc::new(telemetry.clone()))),
                &telemetry,
                website::TaskEnvironment {
                    supervisor: tasks.clone(),
                    maintenance,
                    recovery_mode,
                },
            )
            .await?;
            tasks.activate(app.tasks);
            serve(
                app.router,
                &site.bind,
                pool,
                telemetry,
                metrics_listener,
                HttpBackground {
                    tasks,
                    scheduler_enabled: !recovery_mode,
                },
                site.http,
            )
            .await
        }
    }
}

struct HttpBackground {
    tasks: Arc<tasks::TaskSupervisor>,
    scheduler_enabled: bool,
}

async fn serve(
    app: axum::Router,
    bind: &str,
    pool: infrastructure::Database,
    telemetry: interfaces::observability::Telemetry,
    metrics_listener: Option<tokio::net::TcpListener>,
    background: HttpBackground,
    limits: transport::HttpLimits,
) -> Result<(), String> {
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|error| format!("绑定 {bind} 失败：{error}"))?;
    notice(format_args!(
        "公开站点已启动：http://{}",
        listener.local_addr().map_err(|e| e.to_string())?
    ));
    let (_pool_sender, pool_receiver) = tokio::sync::watch::channel(Some(pool.clone()));
    let server = serve_http(
        listener,
        app,
        metrics_listener,
        telemetry.clone(),
        pool_receiver,
        limits,
        background,
    );
    server.await
}

async fn serve_http(
    listener: tokio::net::TcpListener,
    app: axum::Router,
    metrics_listener: Option<tokio::net::TcpListener>,
    telemetry: interfaces::observability::Telemetry,
    pool: tokio::sync::watch::Receiver<Option<infrastructure::Database>>,
    limits: transport::HttpLimits,
    background: HttpBackground,
) -> Result<(), String> {
    let (shutdown, receiver) = tokio::sync::watch::channel(false);
    let mut tasks = tokio::task::JoinSet::new();
    tasks.spawn(transport::serve(listener, app, receiver.clone(), limits));
    if let Some(listener) = metrics_listener {
        tasks.spawn(observability::serve(
            listener,
            telemetry.clone(),
            pool.clone(),
            receiver.clone(),
            limits,
        ));
    }
    if background.scheduler_enabled {
        background.tasks.start();
    }
    let result = tokio::select! {
        _ = shutdown_signal() => Ok(()),
        result = tasks.join_next() => match result {
            Some(Ok(result)) => result,
            Some(Err(error)) => Err(format!("服务任务退出：{error}")),
            None => Ok(()),
        },
    };
    let deadline = tokio::time::Instant::now() + limits.shutdown;
    background.tasks.close();
    shutdown.send_replace(true);
    background.tasks.shutdown(deadline).await;
    let drain = async {
        while let Some(result) = tasks.join_next().await {
            if let Err(error) = result {
                tracing::warn!(%error, "关闭服务任务失败");
            }
        }
        background.tasks.close_maintenance_pool().await;
        let database = pool.borrow().clone();
        if let Some(database) = database {
            database.close().await;
        }
    };
    if tokio::time::timeout_at(deadline, drain).await.is_err() {
        tasks.abort_all();
        tracing::warn!("关闭总期限已到，停止剩余任务");
    }
    result
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = terminate => {} }
    tracing::info!("收到退出信号，正在关闭");
}
