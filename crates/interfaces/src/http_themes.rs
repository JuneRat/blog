//! Theme package lifecycle shares settings authorization and activation versions.
use crate::{
    http_admin::AdminAuth,
    http_auth::AdminState,
    http_support::{RequestId, admin_error, no_store},
};
use application::{UseCaseError, themes::MAX_THEME_PACKAGE_BYTES};
use axum::{
    Json, Router,
    extract::{Path, Request, State},
    http::StatusCode,
    middleware,
    response::{IntoResponse, Response},
    routing::post,
};

#[derive(serde::Serialize, ts_rs::TS)]
pub struct ThemePackageReport {
    pub slug: String,
    pub name: String,
    pub release: String,
    pub template_count: usize,
    pub asset_count: usize,
}
impl From<application::themes::ThemePackageReport> for ThemePackageReport {
    fn from(value: application::themes::ThemePackageReport) -> Self {
        Self {
            slug: value.slug,
            name: value.name,
            release: value.release,
            template_count: value.template_count,
            asset_count: value.asset_count,
        }
    }
}
#[derive(serde::Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct UninstallThemeInput {
    pub expected_version: i64,
    pub expected_release: String,
}

pub fn themes_router(state: AdminState) -> Router {
    Router::new()
        .route("/api/admin/v1/themes", post(install))
        .route(
            "/api/admin/v1/themes/validate-package",
            post(validate_package),
        )
        .route(
            "/api/admin/v1/themes/{slug}/validate",
            post(validate_installed),
        )
        .route(
            "/api/admin/v1/themes/{slug}",
            axum::routing::delete(uninstall),
        )
        .layer(axum::extract::DefaultBodyLimit::max(1024))
        .layer(middleware::from_fn(no_store))
        .with_state(state)
}

async fn package(
    auth: AdminAuth,
    id: RequestId,
    state: AdminState,
    request: Request,
    install: bool,
) -> Response {
    if !auth.actor.has_permission("settings.manage") {
        return admin_error(UseCaseError::Forbidden, &id);
    }
    let bytes = match axum::body::to_bytes(request.into_body(), MAX_THEME_PACKAGE_BYTES).await {
        Ok(bytes) => bytes.to_vec(),
        Err(error) => {
            use std::error::Error;
            if error
                .source()
                .is_some_and(|source| source.is::<http_body_util::LengthLimitError>())
            {
                return (StatusCode::PAYLOAD_TOO_LARGE, Json(serde_json::json!({"error":"主题 ZIP 包不能超过 10 MiB", "code":"payload_too_large"}))).into_response();
            }
            return admin_error(UseCaseError::Invalid("读取主题包失败".into()), &id);
        }
    };
    let result = if install {
        state.settings.install_theme(&auth.actor, bytes).await
    } else {
        state
            .settings
            .validate_theme_package(&auth.actor, bytes)
            .await
    };
    match result {
        Ok(report) => (
            if install {
                StatusCode::CREATED
            } else {
                StatusCode::OK
            },
            Json(ThemePackageReport::from(report)),
        )
            .into_response(),
        Err(error) => admin_error(error, &id),
    }
}
async fn install(
    auth: AdminAuth,
    id: RequestId,
    State(state): State<AdminState>,
    request: Request,
) -> Response {
    package(auth, id, state, request, true).await
}
async fn validate_package(
    auth: AdminAuth,
    id: RequestId,
    State(state): State<AdminState>,
    request: Request,
) -> Response {
    package(auth, id, state, request, false).await
}
async fn validate_installed(
    auth: AdminAuth,
    id: RequestId,
    State(state): State<AdminState>,
    Path(slug): Path<String>,
) -> Response {
    match state
        .settings
        .validate_installed_theme(&auth.actor, &slug)
        .await
    {
        Ok(report) => Json(ThemePackageReport::from(report)).into_response(),
        Err(error) => admin_error(error, &id),
    }
}
async fn uninstall(
    auth: AdminAuth,
    id: RequestId,
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    Json(body): Json<UninstallThemeInput>,
) -> Response {
    match state
        .settings
        .uninstall_theme(
            &auth.actor,
            &slug,
            Some(body.expected_version),
            &body.expected_release,
        )
        .await
    {
        Ok(view) => Json(crate::http_contract::ThemeSettings::from(view)).into_response(),
        Err(error) => admin_error(error, &id),
    }
}
pub(crate) fn export_contract(out: &mut Vec<String>) {
    crate::http_contract::declare::<ThemePackageReport>(out);
    crate::http_contract::declare::<UninstallThemeInput>(out);
}
