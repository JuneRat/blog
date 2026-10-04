//! Theme settings wire contract; package/app validation is authoritative.
use crate::{
    http_admin::AdminAuth,
    http_auth::AdminState,
    http_support::{RequestId, admin_error},
};
use application::theme_config as app;
use axum::{
    Json,
    extract::{Path, State, rejection::JsonRejection},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS)]
#[serde(untagged)]
pub enum ThemeConfigValue {
    Boolean(bool),
    Integer(#[ts(type = "number")] i64),
    Text(String),
    Null,
}
impl From<ThemeConfigValue> for app::ThemeValue {
    fn from(v: ThemeConfigValue) -> Self {
        match v {
            ThemeConfigValue::Boolean(v) => Self::Boolean(v),
            ThemeConfigValue::Integer(v) => Self::Integer(v),
            ThemeConfigValue::Text(v) => Self::Text(v),
            ThemeConfigValue::Null => Self::Null,
        }
    }
}
impl From<app::ThemeValue> for ThemeConfigValue {
    fn from(v: app::ThemeValue) -> Self {
        match v {
            app::ThemeValue::Boolean(v) => Self::Boolean(v),
            app::ThemeValue::Integer(v) => Self::Integer(v),
            app::ThemeValue::Text(v) => Self::Text(v),
            app::ThemeValue::Null => Self::Null,
        }
    }
}
#[derive(Serialize, TS)]
pub struct ThemeConfigChoice {
    pub value: String,
    pub label: String,
}
#[derive(Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ThemeConfigType {
    Text,
    Textarea,
    Integer,
    Boolean,
    Select,
    Color,
    Media,
}
#[derive(Serialize, TS)]
pub struct ThemeConfigField {
    pub key: String,
    #[serde(rename = "type")]
    pub kind: ThemeConfigType,
    pub label: String,
    pub description: String,
    pub group: String,
    pub default: ThemeConfigValue,
    pub min_length: Option<usize>,
    pub max_length: Option<usize>,
    #[ts(type = "number | null")]
    pub min: Option<i64>,
    #[ts(type = "number | null")]
    pub max: Option<i64>,
    pub options: Vec<ThemeConfigChoice>,
}
impl From<app::ThemeField> for ThemeConfigField {
    fn from(f: app::ThemeField) -> Self {
        Self {
            key: f.key,
            kind: match f.kind {
                app::ThemeFieldType::Text => ThemeConfigType::Text,
                app::ThemeFieldType::Textarea => ThemeConfigType::Textarea,
                app::ThemeFieldType::Integer => ThemeConfigType::Integer,
                app::ThemeFieldType::Boolean => ThemeConfigType::Boolean,
                app::ThemeFieldType::Select => ThemeConfigType::Select,
                app::ThemeFieldType::Color => ThemeConfigType::Color,
                app::ThemeFieldType::Media => ThemeConfigType::Media,
            },
            label: f.label,
            description: f.description,
            group: f.group,
            default: f.default.into(),
            min_length: f.min_length,
            max_length: f.max_length,
            min: f.min,
            max: f.max,
            options: f
                .options
                .into_iter()
                .map(|o| ThemeConfigChoice {
                    value: o.value,
                    label: o.label,
                })
                .collect(),
        }
    }
}
#[derive(Serialize, TS)]
pub struct ThemeConfigSettings {
    pub id: uuid::Uuid,
    pub slug: String,
    pub release: String,
    pub fields: Vec<ThemeConfigField>,
    pub config: BTreeMap<String, ThemeConfigValue>,
    pub overrides: BTreeMap<String, ThemeConfigValue>,
    pub config_schema_version: u32,
    #[ts(type = "number")]
    pub version: i64,
}
impl From<app::ThemeConfigView> for ThemeConfigSettings {
    fn from(v: app::ThemeConfigView) -> Self {
        Self {
            id: v.id,
            slug: v.slug,
            release: v.release,
            fields: v.fields.into_iter().map(Into::into).collect(),
            config: v.config.into_iter().map(|(k, v)| (k, v.into())).collect(),
            overrides: v
                .overrides
                .into_iter()
                .map(|(k, v)| (k, v.into()))
                .collect(),
            config_schema_version: v.config_schema_version,
            version: v.version,
        }
    }
}
#[derive(Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct SaveThemeConfigInput {
    pub id: uuid::Uuid,
    pub expected_release: String,
    pub config_schema_version: u32,
    #[ts(type = "number")]
    pub expected_version: i64,
    pub config: BTreeMap<String, ThemeConfigValue>,
}

pub(crate) async fn get(
    auth: AdminAuth,
    id: RequestId,
    State(state): State<AdminState>,
    Path(slug): Path<String>,
) -> Response {
    match state.settings.theme_config(&auth.actor, &slug).await {
        Ok(v) => Json(ThemeConfigSettings::from(v)).into_response(),
        Err(e) => admin_error(e, &id),
    }
}
pub(crate) async fn put(
    auth: AdminAuth,
    id: RequestId,
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    body: Result<Json<SaveThemeConfigInput>, JsonRejection>,
) -> Response {
    let body = match body {
        Ok(Json(v)) => v,
        Err(e) => return admin_error(application::UseCaseError::Invalid(e.body_text()), &id),
    };
    let cmd = app::SaveThemeConfigCmd {
        id: body.id,
        expected_release: body.expected_release,
        config_schema_version: body.config_schema_version,
        expected_version: body.expected_version,
        config: body
            .config
            .into_iter()
            .map(|(k, v)| (k, v.into()))
            .collect(),
    };
    match state
        .settings
        .save_theme_config(&auth.actor, &slug, cmd)
        .await
    {
        Ok(v) => Json(ThemeConfigSettings::from(v)).into_response(),
        Err(e) => admin_error(e, &id),
    }
}
pub(crate) fn export_contract(out: &mut Vec<String>) {
    crate::http_contract::declare::<ThemeConfigValue>(out);
    crate::http_contract::declare::<ThemeConfigType>(out);
    crate::http_contract::declare::<ThemeConfigChoice>(out);
    crate::http_contract::declare::<ThemeConfigField>(out);
    crate::http_contract::declare::<ThemeConfigSettings>(out);
    crate::http_contract::declare::<SaveThemeConfigInput>(out);
}
