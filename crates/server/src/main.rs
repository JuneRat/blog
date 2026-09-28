//! Composition root: select a command, build its dependencies, and own the
//! process listener and shutdown lifecycle. Business rules live in application.

mod assembly;
mod config;
mod installation;
mod recovery;
mod website;

use std::sync::Arc;

use infrastructure::RenderingRuntime;
use interfaces::cli::{Command, parse_args};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,sqlx=warn".into()),
        )
        .init();
    if let Err(error) = run(parse_args().command).await {
        eprintln!("错误：{error}");
        std::process::exit(1);
    }
}

async fn run(command: Command) -> Result<(), String> {
    if let Command::Maintenance {
        batch_size,
        max_batches,
        dry_run,
    } = command
    {
        let url = std::env::var("BLOG_MAINTENANCE_DATABASE_URL")
            .map_err(|_| "请设置独立维护连接 BLOG_MAINTENANCE_DATABASE_URL")?;
        let pool = infrastructure::connect(&url)
            .await
            .map_err(|e| e.to_string())?;
        if recovery::mode()? || recovery::is_isolated(&pool).await? {
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
    let saved = config::read_saved(&config::config_path())?;
    if let Command::Serve { addr } = &command
        && std::env::var_os("DATABASE_URL").is_none()
        && saved.is_none()
    {
        return installation::serve(addr.clone(), None).await;
    }
    let database = config::DatabaseConfig::from_env(saved.as_ref());
    let pool = infrastructure::connect(&database.url)
        .await
        .map_err(|error| format!("连接 PostgreSQL 失败：{error}"))?;
    let recovery_mode = recovery::mode()?;
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
        && std::env::var_os("DATABASE_URL").is_none()
        && let Some(saved) = saved.as_ref()
        && !infrastructure::installation::is_complete(&pool, &saved.installation_id)
            .await
            .map_err(|e| e.to_string())?
    {
        pool.close().await;
        return installation::serve(addr.clone(), Some(saved.clone())).await;
    }
    // Validate the recovery listener before any migration or permission writes.
    let site_config = if let Command::Serve { addr } = &command {
        let site = config::SiteConfig::from_env(addr.clone(), saved.as_ref())?;
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
            interfaces::cli::run_media(&assembly::media(&pool, config::media_dir()), action).await
        }
        Command::Serve { .. } => {
            let site = site_config.expect("serve configuration was validated above");
            let app =
                website::build_router(&pool, &site, roles, Arc::new(RenderingRuntime::default()))
                    .await?;
            serve(app, &site.bind, assembly::publisher(&pool), recovery_mode).await
        }
    }
}

async fn serve(
    app: axum::Router,
    bind: &str,
    publisher: application::publishing::PublishDueInteractor,
    recovery_mode: bool,
) -> Result<(), String> {
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|error| format!("绑定 {bind} 失败：{error}"))?;
    println!(
        "公开站点已启动：http://{}",
        listener.local_addr().map_err(|e| e.to_string())?
    );
    let server = async {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|error| format!("服务退出：{error}"))
    };
    let scheduler = publish_scheduler(publisher);
    if recovery_mode {
        println!("恢复核验模式：预约发布任务已停用。");
        return server.await;
    }
    tokio::select! { result=server=>result, _=scheduler=>unreachable!("scheduler loops until server shuts down") }
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
    println!("收到退出信号，正在关闭…");
}
