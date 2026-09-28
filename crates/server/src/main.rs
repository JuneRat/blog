//! Composition root: select a command, build its dependencies, and own the
//! process listener and shutdown lifecycle. Business rules live in application.

mod assembly;
mod config;
mod installation;
mod observability;
mod recovery;
mod website;

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

static JSON_LOGS: AtomicBool = AtomicBool::new(false);

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
    let log_filter = match config.log_filter() {
        Ok(filter) => filter,
        Err(error) => {
            eprintln!("错误：{error}");
            std::process::exit(1);
        }
    };
    let json = match config.log_json() {
        Ok(json) => json,
        Err(error) => {
            eprintln!("错误：{error}");
            std::process::exit(1);
        }
    };
    JSON_LOGS.store(json, Ordering::Relaxed);
    let subscriber = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_env_filter(log_filter);
    if json {
        subscriber
            .json()
            .with_current_span(true)
            .with_span_list(false)
            .init();
    } else {
        subscriber.init();
    }
    if let Err(error) = run(cli.command, config).await {
        tracing::error!(%error, "命令执行失败");
        std::process::exit(1);
    }
}

// Installation codes and listening addresses must remain visible even with
// RUST_LOG=warn. In JSON mode these finite operator notices are JSON on stderr;
// command result stdout remains owned by the CLI contract.
pub fn notice(message: std::fmt::Arguments<'_>) {
    if JSON_LOGS.load(Ordering::Relaxed) {
        eprintln!(
            "{}",
            serde_json::json!({
                "timestamp": time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339).unwrap_or_default(),
                "level":"INFO", "target":"blog", "fields":{"message": message.to_string()}
            })
        );
    } else {
        println!("{message}");
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
        if config.recovery_mode()? || recovery::is_isolated(&pool).await? {
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
    let database = config.database()?;
    if matches!(command, Command::Media { .. }) {
        config.media_dir()?;
    }
    let pool = infrastructure::connect_with_config(&database.url, &database.pool)
        .await
        .map_err(|error| format!("连接 PostgreSQL 失败：{error}"))?;
    let recovery_mode = config.recovery_mode()?;
    let isolated = recovery::is_isolated(&pool).await?;
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
    // Keep automatic schema initialization for schema owners; restricted runtime
    // roles verify the applied migrations. Derived HTML is rebuilt only by the
    // explicit maintenance command, never as a startup or migration side effect.
    infrastructure::migrate_schema(&pool, database.migrations_dir)
        .await
        .map_err(|error| format!("执行迁移失败：{error}"))?;
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
            let app = website::build_router(
                &pool,
                &site,
                roles,
                Arc::new(RenderingRuntime::default()),
                &telemetry,
            )
            .await?;
            serve(
                app,
                &site.bind,
                pool,
                telemetry,
                metrics_listener,
                recovery_mode,
            )
            .await
        }
    }
}

async fn serve(
    app: axum::Router,
    bind: &str,
    pool: sqlx::PgPool,
    telemetry: interfaces::observability::Telemetry,
    metrics_listener: Option<tokio::net::TcpListener>,
    recovery_mode: bool,
) -> Result<(), String> {
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|error| format!("绑定 {bind} 失败：{error}"))?;
    notice(format_args!(
        "公开站点已启动：http://{}",
        listener.local_addr().map_err(|e| e.to_string())?
    ));
    let (_pool_sender, pool_receiver) = tokio::sync::watch::channel(Some(pool.clone()));
    let server = serve_http(listener, app, metrics_listener, telemetry, pool_receiver);
    let scheduler = publish_scheduler(assembly::publisher(&pool));
    if recovery_mode {
        tracing::info!("恢复核验模式：预约发布任务已停用");
        return server.await;
    }
    tokio::select! { result=server=>result, _=scheduler=>unreachable!("scheduler loops until server shuts down") }
}

async fn serve_http(
    listener: tokio::net::TcpListener,
    app: axum::Router,
    metrics_listener: Option<tokio::net::TcpListener>,
    telemetry: interfaces::observability::Telemetry,
    pool: tokio::sync::watch::Receiver<Option<sqlx::PgPool>>,
) -> Result<(), String> {
    let (shutdown, mut receiver) = tokio::sync::watch::channel(false);
    let enabled = metrics_listener.is_some();
    let management = observability::serve(metrics_listener, telemetry, pool, receiver.clone());
    let public = async {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            let _ = receiver.wait_for(|closed| *closed).await;
        })
        .await
        .map_err(|error| format!("服务退出：{error}"))
    };
    tokio::pin!(public, management);
    tokio::select! {
        result = &mut public => result,
        result = &mut management => result,
        _ = shutdown_signal() => {
            shutdown.send_replace(true);
            public.await?;
            if enabled { management.await?; }
            Ok(())
        }
    }
}

async fn publish_scheduler(publisher: application::publishing::PublishDueInteractor) {
    let mut ticks = tokio::time::interval(std::time::Duration::from_secs(30));
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticks.tick().await;
        if let Err(error) = publisher.run().await {
            tracing::error!(%error, "到期内容发布失败，下次轮询重试");
        }
    }
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
