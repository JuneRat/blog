//! 站点和主题设置路由。

use super::AdminAuth;
use crate::http_auth::AdminState;
use crate::http_support::{RequestId, admin_error, no_store};
use application::settings::{SaveSiteSettingsCmd, SaveThemeSettingsCmd, SiteSettingsView};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router, middleware};
use serde::Deserialize;
use uuid::Uuid;

/// 站点与主题设置的请求体限制。
const SETTINGS_BODY_LIMIT: usize = 16 * 1024;

#[derive(serde::Serialize, ts_rs::TS)]
#[ts(rename = "SiteSettings")]
struct SiteSettingsJson {
    time_zone: String,
    time_zones: Vec<String>,
    title: String,
    description: String,
    /// 站点 logo 的媒体资产 id（None = 无 logo）。
    logo_media_id: Option<Uuid>,
    /// 站点 logo 的站内地址（None = 无 logo）。
    logo_url: Option<String>,
    /// "database"（settings.site 行）或 "fallback"（内置默认值，version=0）。
    source: &'static str,
    version: i64,
}

impl From<&SiteSettingsView> for SiteSettingsJson {
    fn from(view: &SiteSettingsView) -> Self {
        Self {
            time_zone: view.time_zone.clone(),
            time_zones: view.time_zones.clone(),
            title: view.title.clone(),
            description: view.description.clone(),
            logo_media_id: view.logo_media_id,
            logo_url: view.logo_url.clone(),
            source: match view.source {
                application::settings::SiteSettingsSource::Database => "database",
                application::settings::SiteSettingsSource::Fallback => "fallback",
            },
            version: view.version,
        }
    }
}

#[derive(Deserialize, Default, ts_rs::TS)]
#[ts(rename = "SaveSiteSettingsInput", optional_fields = nullable)]
pub struct SaveSiteSettingsBody {
    pub time_zone: Option<String>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub description: String,
    /// 站点 logo（PUT 是整组替换：缺省/null = 清除 logo）。
    #[serde(default)]
    pub logo_media_id: Option<Uuid>,
    pub expected_version: Option<i64>,
}

/// 站点设置路由：site 与 theme 分组。oauth 等受保护分组
/// 使用专用权限（oauth.manage）与独立入口，不暴露在本 API 面上，
/// 未知分组（含猜测 `/settings/oauth`）由路由层直接 404。
pub fn settings_router(state: AdminState) -> Router {
    Router::new()
        .route(
            "/api/admin/v1/settings/site",
            get(get_site_settings).put(put_site_settings),
        )
        .route(
            "/api/admin/v1/settings/theme",
            get(get_theme_settings).put(put_theme_settings),
        )
        .layer(axum::extract::DefaultBodyLimit::max(SETTINGS_BODY_LIMIT))
        .layer(middleware::from_fn(no_store))
        .with_state(state)
}

#[derive(Deserialize, ts_rs::TS)]
#[ts(rename = "SaveThemeSettingsInput", optional_fields = nullable)]
pub struct SaveThemeSettingsBody {
    pub slug: String,
    pub expected_version: Option<i64>,
}

async fn get_theme_settings(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
) -> Response {
    match state.settings.theme_view(&actor).await {
        Ok(view) => (
            StatusCode::OK,
            Json(crate::http_contract::ThemeSettings::from(view)),
        )
            .into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

async fn put_theme_settings(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Json(body): Json<SaveThemeSettingsBody>,
) -> Response {
    match state
        .settings
        .save_theme(
            &actor,
            SaveThemeSettingsCmd {
                slug: body.slug,
                expected_version: body.expected_version,
            },
        )
        .await
    {
        Ok(view) => (
            StatusCode::OK,
            Json(crate::http_contract::ThemeSettings::from(view)),
        )
            .into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

async fn get_site_settings(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
) -> Response {
    match state.settings.site_view(&actor).await {
        Ok(view) => (StatusCode::OK, Json(SiteSettingsJson::from(&view))).into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

/// 全量替换 site 分组（title/description 必填；PUT 语义）。
/// 读写都要求 `settings.manage`；expected_version 过期是 409 version_conflict。
async fn put_site_settings(
    AdminAuth { actor }: AdminAuth,
    request_id: RequestId,
    State(state): State<AdminState>,
    Json(body): Json<SaveSiteSettingsBody>,
) -> Response {
    match state
        .settings
        .save_site(
            &actor,
            SaveSiteSettingsCmd {
                time_zone: body.time_zone,
                title: body.title,
                description: body.description,
                logo_media_id: body.logo_media_id,
                expected_version: body.expected_version,
            },
        )
        .await
    {
        Ok(view) => (StatusCode::OK, Json(SiteSettingsJson::from(&view))).into_response(),
        Err(e) => admin_error(e, &request_id),
    }
}

pub(crate) fn export_contract(out: &mut Vec<String>) {
    crate::http_contract::declare::<SiteSettingsJson>(out);
    crate::http_contract::declare::<SaveSiteSettingsBody>(out);
    crate::http_contract::declare::<SaveThemeSettingsBody>(out);
}
