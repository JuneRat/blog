//! Composition root: select a command, build its dependencies, and own the
//! process listener and shutdown lifecycle. Business rules live in application.

mod assembly;
mod config;
mod website;

use std::sync::Arc;

use infrastructure::RenderingRuntime;
use interfaces::cli::{Command, parse_args};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
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
    let database = config::DatabaseConfig::from_env();
    let pool = infrastructure::connect(&database.url)
        .await
        .map_err(|error| format!("连接 PostgreSQL 失败：{error}"))?;
    // Keep automatic schema initialization, while website-specific configuration
    // and dependencies are evaluated only in the serve branch below.
    let migration = match &command {
        Command::Migrate | Command::PublishDue | Command::Post { .. } | Command::Serve { .. } => {
            infrastructure::migrate(&pool, database.migrations_dir).await
        }
        Command::User { .. }
        | Command::Role { .. }
        | Command::Oauth { .. }
        | Command::Media { .. } => {
            infrastructure::migrate_schema(&pool, database.migrations_dir).await
        }
    };
    migration.map_err(|error| format!("执行迁移失败：{error}"))?;
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
        Command::Migrate => unreachable!("migration returned above"),
        Command::PublishDue => {
            let mut total = 0;
            loop {
                let count = infrastructure::publish_due_content(
                    &pool,
                    time::OffsetDateTime::now_utc(),
                    100,
                )
                .await
                .map_err(|e| e.to_string())?;
                total += count;
                // Either table may have filled its 100-row batch on its own.
                if count < 100 {
                    break;
                }
            }
            println!("已发布 {total} 条到期内容。");
            Ok(())
        }
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
        Command::Serve { addr } => {
            let site = config::SiteConfig::from_env(addr)?;
            let app =
                website::build_router(&pool, &site, roles, Arc::new(RenderingRuntime::default()))
                    .await?;
            serve(app, &site.bind, pool).await
        }
    }
}

async fn serve(app: axum::Router, bind: &str, pool: sqlx::PgPool) -> Result<(), String> {
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|error| format!("绑定 {bind} 失败：{error}"))?;
    println!("公开站点已启动：http://{bind}");
    let server = async {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|error| format!("服务退出：{error}"))
    };
    let scheduler = async {
        let mut ticks = tokio::time::interval(std::time::Duration::from_secs(30));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticks.tick().await;
            loop {
                match infrastructure::publish_due_content(
                    &pool,
                    time::OffsetDateTime::now_utc(),
                    100,
                )
                .await
                {
                    Ok(count) if count >= 100 => continue,
                    Ok(_) => break,
                    Err(error) => {
                        tracing::error!(%error,"到期内容发布失败，下次轮询重试");
                        break;
                    }
                }
            }
        }
    };
    tokio::select! { result=server=>result, _=scheduler=>unreachable!("scheduler loops until server shuts down") }
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
