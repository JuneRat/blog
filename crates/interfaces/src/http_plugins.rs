//! Independent plugin administration and immutable plugin resources.
use std::{collections::BTreeMap, sync::Arc};

use application::{UseCaseError, plugins as app};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, FromRef, Path, State, rejection::JsonRejection},
    http::{StatusCode, header},
    middleware,
    response::{IntoResponse, Response},
    routing::{get, put},
};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{
    http_admin::AdminAuth,
    http_auth::AdminState,
    http_support::{RequestId, admin_error, no_store},
};

#[derive(Clone)]
pub struct PluginsState {
    pub plugins: Arc<app::PluginsInteractor>,
    pub admin: AdminState,
}
impl FromRef<PluginsState> for AdminState {
    fn from_ref(state: &PluginsState) -> Self {
        state.admin.clone()
    }
}

pub fn plugins_router(state: PluginsState) -> Router {
    Router::new()
        .route("/api/admin/v1/plugins", get(view))
        .route("/api/admin/v1/plugins/{id}", put(save))
        .layer(DefaultBodyLimit::max(48 * 1024))
        .layer(middleware::from_fn(no_store))
        .with_state(state)
}

#[derive(Serialize, Deserialize, TS)]
#[serde(untagged)]
pub enum PluginConfigValue {
    Boolean(bool),
    Integer(i32),
    Text(String),
}

impl From<PluginConfigValue> for app::PluginConfigValue {
    fn from(value: PluginConfigValue) -> Self {
        match value {
            PluginConfigValue::Boolean(v) => Self::Boolean(v),
            PluginConfigValue::Integer(v) => Self::Integer(v),
            PluginConfigValue::Text(v) => Self::Text(v),
        }
    }
}
impl From<app::PluginConfigValue> for PluginConfigValue {
    fn from(value: app::PluginConfigValue) -> Self {
        match value {
            app::PluginConfigValue::Boolean(v) => Self::Boolean(v),
            app::PluginConfigValue::Integer(v) => Self::Integer(v),
            app::PluginConfigValue::Text(v) => Self::Text(v),
        }
    }
}

#[derive(Serialize, TS)]
pub struct PluginConfigField {
    pub key: String,
    pub label: String,
    pub description: String,
    pub default: PluginConfigValue,
}
#[derive(Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum PluginHook {
    Content,
    PageHead,
}

#[derive(Serialize, TS)]
pub struct PluginView {
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub hooks: Vec<PluginHook>,
    pub config_fields: Vec<PluginConfigField>,
    pub available: bool,
    pub enabled: bool,
    pub config: BTreeMap<String, PluginConfigValue>,
}

#[derive(Serialize, TS)]
pub struct PluginsView {
    #[ts(type = "number")]
    pub version: i64,
    pub plugins: Vec<PluginView>,
}

impl From<app::PluginsView> for PluginsView {
    fn from(value: app::PluginsView) -> Self {
        Self {
            version: value.version,
            plugins: value
                .plugins
                .into_iter()
                .map(|p| PluginView {
                    id: p.definition.id,
                    name: p.definition.name,
                    description: p.definition.description,
                    version: p.definition.version,
                    hooks: p
                        .definition
                        .hooks
                        .into_iter()
                        .map(|h| match h {
                            app::PluginHook::Content => PluginHook::Content,
                            app::PluginHook::PageHead => PluginHook::PageHead,
                        })
                        .collect(),
                    config_fields: p
                        .definition
                        .config_fields
                        .into_iter()
                        .map(|f| PluginConfigField {
                            key: f.key,
                            label: f.label,
                            description: f.description,
                            default: f.default.into(),
                        })
                        .collect(),
                    available: p.available,
                    enabled: p.enabled,
                    config: p.config.into_iter().map(|(k, v)| (k, v.into())).collect(),
                })
                .collect(),
        }
    }
}

#[derive(Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct SavePluginInput {
    pub enabled: bool,
    pub config: BTreeMap<String, PluginConfigValue>,
    #[ts(type = "number")]
    pub expected_version: i64,
}

async fn view(auth: AdminAuth, id: RequestId, State(state): State<PluginsState>) -> Response {
    match state.plugins.view(&auth.actor).await {
        Ok(view) => Json(PluginsView::from(view)).into_response(),
        Err(error) => admin_error(error, &id),
    }
}

async fn save(
    auth: AdminAuth,
    request_id: RequestId,
    State(state): State<PluginsState>,
    Path(id): Path<String>,
    body: Result<Json<SavePluginInput>, JsonRejection>,
) -> Response {
    if !auth.actor.has_permission("plugins.manage") {
        return admin_error(UseCaseError::Forbidden, &request_id);
    }
    let body = match body {
        Ok(Json(body)) => body,
        Err(error) if error.status() == StatusCode::PAYLOAD_TOO_LARGE => {
            return StatusCode::PAYLOAD_TOO_LARGE.into_response();
        }
        Err(_) => {
            return admin_error(
                UseCaseError::Invalid("插件配置请求无效".into()),
                &request_id,
            );
        }
    };
    let cmd = app::SavePluginCmd {
        id,
        enabled: body.enabled,
        config: body
            .config
            .into_iter()
            .map(|(k, v)| (k, v.into()))
            .collect(),
        expected_version: body.expected_version,
    };
    match state.plugins.save(&auth.actor, cmd).await {
        Ok(view) => Json(PluginsView::from(view)).into_response(),
        Err(error) => admin_error(error, &request_id),
    }
}

/// Only the registered snapshot is public; no filesystem traversal or uploads.
pub fn mount_plugin_assets(mut router: Router, plugins: Vec<app::PluginAssets>) -> Router {
    for plugin in plugins {
        let route = format!("/assets/plugins/{}/{}/{{*path}}", plugin.id, plugin.version);
        router = router.route(
            &route,
            get(move |Path(path): Path<String>| {
                let files = plugin.files.clone();
                async move {
                    let Some(bytes) = files.get(&path) else {
                        return StatusCode::NOT_FOUND.into_response();
                    };
                    (
                        [
                            (
                                header::CONTENT_TYPE,
                                mime_guess::from_path(path)
                                    .first_or_octet_stream()
                                    .to_string(),
                            ),
                            (
                                header::CACHE_CONTROL,
                                "public, max-age=31536000, immutable".into(),
                            ),
                            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".into()),
                            // Public immutable resources must also work in the admin's
                            // opaque-origin preview iframe (in particular web fonts).
                            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*".into()),
                        ],
                        axum::body::Bytes::from_owner(bytes.clone()),
                    )
                        .into_response()
                }
            }),
        );
    }
    router
}

pub(crate) fn export_contract(out: &mut Vec<String>) {
    use crate::http_contract::declare;
    declare::<PluginConfigValue>(out);
    declare::<PluginConfigField>(out);
    declare::<PluginHook>(out);
    declare::<PluginView>(out);
    declare::<PluginsView>(out);
    declare::<SavePluginInput>(out);
}
