//! 公开站点 SSR 路由：只读，匿名可访问。
//! 草稿/private/回收站文章在应用层查询即被过滤，路由层不再重复判断。
//! M1 不提供任何写 HTTP；管理接口随 M2 与认证/CSRF 一起交付。

use std::path::PathBuf;
use std::sync::Arc;

use application::error::UseCaseError;
use application::ports::HealthCheck;
use application::public_site::PublicSiteInteractor;
use axum::Router;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use serde::Deserialize;
use tower_http::services::{ServeDir, ServeFile};

#[derive(Clone)]
pub struct PublicSiteState {
    pub site: Arc<PublicSiteInteractor>,
    /// 装配时可注入 readiness 探针（如 PgHealthCheck）；
    /// None 表示本路由无外部依赖可检，healthz 仅反映进程存活。
    pub health: Option<Arc<dyn HealthCheck>>,
}

/// 构建公开路由；assets_dir 提供时挂载 /assets/ 静态资源（主题 assets 目录）。
///
/// 根路径 `/{slug}` 是 Page 的公开地址（如 /about）。固定路由优先、Page 最后匹配：
/// matchit 让静态段（/healthz、/posts、/tags、/admin、/assets）胜过参数段，
/// 保留路径在领域校验与应用层 `render_page` 各拒绝一次，Page 不可能顶掉系统入口。
pub fn public_router(state: PublicSiteState, assets_dir: Option<PathBuf>) -> Router {
    let mut router = Router::new()
        .route("/", get(index))
        .route("/posts/{slug}", get(post_detail))
        .route("/tags/{slug}", get(tag_detail))
        .route("/categories/{slug}", get(category_detail))
        .route("/healthz", get(healthz))
        .route("/{slug}", get(page_detail))
        .fallback(not_found)
        .with_state(state);
    if let Some(dir) = assets_dir {
        router = router.nest_service("/assets", ServeDir::new(dir));
    }
    router
}

/// 兼容无静态资源/探针的调用方（如测试）。
pub fn public_router_minimal(state: Arc<PublicSiteInteractor>) -> Router {
    public_router(
        PublicSiteState {
            site: state,
            health: None,
        },
        None,
    )
}

/// 挂载后台 SPA（`apps/admin` 的构建产物）到 `/admin` 子树。
///
/// - `dist` 不存在时完全不注册 `/admin`（后端单独部署不报错）。
/// - SPA fallback 关在 `/admin` 内，结构上不可能遮挡 `/api`、`/auth`、
///   `/posts/{slug}` 等已注册路由（axum 按路径匹配，fallback 只在无路由命中时触发）。
/// - 缓存：`index.html`（含深链回退）`no-cache`，每次校验，发版即生效；
///   `/admin/assets/*` 的**成功**响应是 Vite 带指纹产物，`immutable` 长缓存。两者都不含秘密。
pub fn mount_admin_spa(router: Router, dist: Option<PathBuf>) -> Router {
    let Some(dist) = dist.filter(|dir| dir.is_dir()) else {
        return router;
    };
    let spa = Router::new()
        .nest_service("/assets", ServeDir::new(dist.join("assets")))
        .fallback_service(ServeFile::new(dist.join("index.html")))
        .layer(middleware::from_fn(admin_cache_headers));
    // nest_service（而非 nest）才能同时覆盖 `/admin` 与 `/admin/`（带斜杠的根路径）。
    router.nest_service("/admin", spa)
}

/// 后台静态资源缓存策略（路径已剥离 `/admin` 前缀）。
///
/// 只有**成功**的 `/assets/*` 响应才是可长缓存的带指纹产物；缺失资源是 404，
/// 若也带上 `immutable`，一次拼写错误就会被缓存层固化一年。
async fn admin_cache_headers(req: Request, next: Next) -> Response {
    let asset_path = req.uri().path().starts_with("/assets/");
    let mut response = next.run(req).await;
    let immutable = asset_path && response.status().is_success();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(if immutable {
            "public, max-age=31536000, immutable"
        } else {
            "no-cache"
        }),
    );
    response
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

/// 标签页查询参数：page 为 1 起的页码，缺省第 1 页；非数字由提取器回 400。
#[derive(Deserialize, Default)]
struct TagPageQuery {
    page: Option<i64>,
}

/// 公开标签页 /tags/{slug}?page=N：未知标签 404；
/// 文章列表只含 published+public+未删除（应用层过滤）。
async fn tag_detail(
    State(state): State<PublicSiteState>,
    Path(slug): Path<String>,
    Query(query): Query<TagPageQuery>,
) -> Response {
    match state.site.render_tag(&slug, query.page.unwrap_or(1)).await {
        Ok(html) => Html(html).into_response(),
        Err(UseCaseError::NotFound(_)) => {
            (StatusCode::NOT_FOUND, "<h1>404</h1><p>标签不存在。</p>").into_response()
        }
        Err(e) => server_error(e),
    }
}

/// 公开分类页 /categories/{slug}?page=N：未知分类 404；直接归属的公开文章分页。
async fn category_detail(
    State(state): State<PublicSiteState>,
    Path(slug): Path<String>,
    Query(query): Query<TagPageQuery>,
) -> Response {
    match state
        .site
        .render_category(&slug, query.page.unwrap_or(1))
        .await
    {
        Ok(html) => Html(html).into_response(),
        Err(UseCaseError::NotFound(_)) => {
            (StatusCode::NOT_FOUND, "<h1>404</h1><p>分类不存在。</p>").into_response()
        }
        Err(e) => server_error(e),
    }
}

/// 根路径页面（Page）：未发布、私有与保留路径一律 404。
async fn page_detail(State(state): State<PublicSiteState>, Path(slug): Path<String>) -> Response {
    match state.site.render_page(&slug).await {
        Ok(html) => Html(html).into_response(),
        Err(UseCaseError::NotFound(_)) => (
            StatusCode::NOT_FOUND,
            "<h1>404</h1><p>页面不存在或未公开。</p>",
        )
            .into_response(),
        Err(e) => server_error(e),
    }
}

async fn healthz(State(state): State<PublicSiteState>) -> Response {
    match state.health {
        Some(check) if !check.check().await => (
            StatusCode::SERVICE_UNAVAILABLE,
            "degraded: dependency check failed",
        )
            .into_response(),
        _ => (StatusCode::OK, "ok").into_response(),
    }
}

async fn not_found() -> Response {
    (StatusCode::NOT_FOUND, "<h1>404</h1><p>页面不存在。</p>").into_response()
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
