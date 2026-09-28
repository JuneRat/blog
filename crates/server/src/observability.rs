//! Own the optional management listener; no business routes or database query
//! runs on a scrape. Pool state is shared with the installation transition.
use axum::{
    Router,
    http::{StatusCode, header},
    response::IntoResponse,
    routing::get,
};
use infrastructure::Database;
use interfaces::observability::{BuildInfo, Telemetry};
use std::net::SocketAddr;
use tokio::{net::TcpListener, sync::watch};

pub fn build_info() -> BuildInfo {
    BuildInfo {
        version: env!("CARGO_PKG_VERSION"),
        revision: env!("BLOG_BUILD_REVISION"),
    }
}

pub async fn bind(address: Option<SocketAddr>) -> Result<Option<TcpListener>, String> {
    match address {
        Some(address) => TcpListener::bind(address)
            .await
            .map(Some)
            .map_err(|error| format!("绑定指标端口 {address} 失败：{error}")),
        None => Ok(None),
    }
}

pub async fn serve(
    listener: Option<TcpListener>,
    telemetry: Telemetry,
    pool: watch::Receiver<Option<Database>>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), String> {
    let Some(listener) = listener else {
        return std::future::pending().await;
    };
    tracing::info!(address = %listener.local_addr().map_err(|error| error.to_string())?, "指标监听已启动");
    let app = Router::new().route(
        "/metrics",
        get(move || {
            let telemetry = telemetry.clone();
            let current = pool.borrow().clone();
            async move {
                telemetry.pool_snapshot(current.as_ref().map(|pool| {
                    let snapshot = pool.pool_snapshot();
                    (
                        snapshot.connections,
                        snapshot.idle_connections,
                        snapshot.max_connections,
                    )
                }));
                match telemetry.encode() {
                    Ok(body) => (
                        [
                            (
                                header::CONTENT_TYPE,
                                "text/plain; version=0.0.4; charset=utf-8",
                            ),
                            (header::CACHE_CONTROL, "no-store"),
                        ],
                        body,
                    )
                        .into_response(),
                    Err(error) => {
                        tracing::error!(%error, "指标编码失败");
                        StatusCode::INTERNAL_SERVER_ERROR.into_response()
                    }
                }
            }
        }),
    );
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = shutdown.wait_for(|closed| *closed).await;
        })
        .await
        .map_err(|error| format!("指标服务退出：{error}"))
}
