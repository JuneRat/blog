//! Embedded first-run UI, available before a database or admin bundle is loaded.

use crate::{
    http_client_ip::ClientAddress,
    http_support::{self, RequestId},
};
use application::{
    UseCaseError,
    audit::AuditContext,
    installation::{InstallInput, Installer},
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Redirect, Response},
    routing::get,
};
use std::sync::Arc;

#[derive(Clone)]
pub struct InstallState {
    pub installer: Arc<dyn Installer>,
    pub token: String,
}

pub fn install_router(state: InstallState) -> Router {
    Router::new()
        .route(
            "/install",
            get(|| async { Html(include_str!("install/index.html")) }),
        )
        .route(
            "/install/app.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("install/app.js"),
                )
            }),
        )
        .route(
            "/install/style.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("install/style.css"),
                )
            }),
        )
        .route("/api/install", get(info).post(install))
        .route("/livez", get(crate::observability::livez))
        .route("/version", get(crate::observability::version))
        .route(
            "/readyz",
            get(|| async { (StatusCode::SERVICE_UNAVAILABLE, "installation required") }),
        )
        .route(
            "/healthz",
            get(|| async { (StatusCode::SERVICE_UNAVAILABLE, "installation required") }),
        )
        .fallback(|method: Method| async move {
            if method == Method::GET || method == Method::HEAD {
                Redirect::to("/install").into_response()
            } else {
                StatusCode::NOT_FOUND.into_response()
            }
        })
        .with_state(state)
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(middleware::from_fn(security_headers))
        .layer(middleware::from_fn(http_support::request_context))
}

fn authorized(state: &InstallState, headers: &HeaderMap) -> bool {
    let token = headers
        .get("x-install-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    http_support::csrf_token_matches(token, &state.token)
        && http_support::ensure_same_origin(headers).is_ok()
}

async fn info(
    State(state): State<InstallState>,
    request_id: RequestId,
    headers: HeaderMap,
) -> Response {
    if !authorized(&state, &headers) {
        return http_support::admin_error(UseCaseError::Forbidden, &request_id);
    }
    Json(state.installer.info()).into_response()
}

async fn install(
    State(state): State<InstallState>,
    request_id: RequestId,
    ClientAddress(ip_address): ClientAddress,
    headers: HeaderMap,
    input: Result<Json<InstallInput>, axum::extract::rejection::JsonRejection>,
) -> Response {
    if !authorized(&state, &headers) {
        return http_support::admin_error(UseCaseError::Forbidden, &request_id);
    }
    let Json(input) = match input {
        Ok(input) => input,
        Err(_) => {
            return http_support::admin_error(
                UseCaseError::Invalid("安装表单无效或过大".into()),
                &request_id,
            );
        }
    };
    match state
        .installer
        .install(
            input,
            AuditContext {
                actor_id: None,
                ip_address,
                ..Default::default()
            },
        )
        .await
    {
        Ok(()) => Json(serde_json::json!({"redirect":"/admin/"})).into_response(),
        Err(error) => http_support::admin_error(error, &request_id),
    }
}

async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    for (key, value) in [
        ("cache-control", "no-store"),
        (
            "content-security-policy",
            "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'",
        ),
        ("referrer-policy", "no-referrer"),
        ("x-content-type-options", "nosniff"),
        ("x-frame-options", "DENY"),
    ] {
        response
            .headers_mut()
            .insert(key, HeaderValue::from_static(value));
    }
    response
}
