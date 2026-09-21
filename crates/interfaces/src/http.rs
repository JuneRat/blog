//! 公开站点 SSR 路由：只读，匿名可访问。
//! 草稿/private/回收站文章在应用层查询即被过滤，路由层不再重复判断。
//! M1 不提供任何写 HTTP；管理接口随 M2 与认证/CSRF 一起交付。

use std::path::PathBuf;
use std::sync::Arc;

use application::error::UseCaseError;
use application::public_site::PublicSiteInteractor;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use tower_http::services::ServeDir;

#[derive(Clone)]
pub struct PublicSiteState {
    pub site: Arc<PublicSiteInteractor>,
}

/// 构建公开路由；assets_dir 提供时挂载 /assets/ 静态资源（主题 assets 目录）。
pub fn public_router(state: PublicSiteState, assets_dir: Option<PathBuf>) -> Router {
    let mut router = Router::new()
        .route("/", get(index))
        .route("/posts/{slug}", get(post_detail))
        .route("/healthz", get(healthz))
        .fallback(not_found)
        .with_state(state);
    if let Some(dir) = assets_dir {
        router = router.nest_service("/assets", ServeDir::new(dir));
    }
    router
}

/// 兼容无静态资源的调用方（如测试）。
pub fn public_router_minimal(state: PublicSiteState) -> Router {
    public_router(state, None)
}

async fn index(State(state): State<PublicSiteState>) -> Response {
    match state.site.render_index(50).await {
        Ok(html) => Html(html).into_response(),
        Err(e) => server_error(e),
    }
}

async fn post_detail(State(state): State<PublicSiteState>, Path(slug): Path<String>) -> Response {
    match state.site.render_post(&slug).await {
        Ok(html) => Html(html).into_response(),
        Err(UseCaseError::NotFound(_)) => (
            StatusCode::NOT_FOUND,
            "<h1>404</h1><p>页面不存在或未公开。</p>",
        )
            .into_response(),
        Err(e) => server_error(e),
    }
}

async fn healthz() -> &'static str {
    "ok"
}

async fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        "<h1>404</h1><p>页面不存在。</p>",
    )
        .into_response()
}

fn server_error(e: UseCaseError) -> Response {
    // 不向匿名访问泄漏内部错误细节。
    tracing::error!(error = %e, "公开站点渲染失败");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "<h1>500</h1><p>服务暂时不可用。</p>",
    )
        .into_response()
}
