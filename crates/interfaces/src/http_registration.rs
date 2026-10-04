use crate::{
    http_admin::AdminAuth,
    http_auth::{AdminState, AuthState},
    http_client_ip::ClientAddress,
    http_support::{RequestId, admin_error, ensure_same_origin, no_store},
};
use application::registration::{AccessPolicy, RegisterAccount, RegistrationInteractor};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, FromRef, State},
    http::{HeaderMap, StatusCode},
    middleware,
    response::{IntoResponse, Response},
    routing::get,
};
use std::sync::Arc;

#[derive(serde::Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields = nullable)]
pub struct RegistrationInput {
    pub username: String,
    pub display_name: Option<String>,
    pub email: String,
    pub password: String,
}
#[derive(serde::Serialize, ts_rs::TS)]
pub struct RegistrationStatus {
    pub enabled: bool,
}
#[derive(serde::Deserialize, serde::Serialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct AccessSettings {
    pub registration_enabled: bool,
    pub guest_comments_enabled: bool,
    pub version: i64,
}
impl From<AccessPolicy> for AccessSettings {
    fn from(value: AccessPolicy) -> Self {
        Self {
            registration_enabled: value.registration_enabled,
            guest_comments_enabled: value.guest_comments_enabled,
            version: value.version,
        }
    }
}

pub fn public_router(state: AuthState) -> Router {
    Router::new()
        .route("/auth/register", get(status).post(register))
        .route_layer(middleware::from_fn_with_state(
            (
                state.admission.clone(),
                application::ports::PublicRequest::Registration,
            ),
            crate::http_limits::admit,
        ))
        .layer(DefaultBodyLimit::max(4096))
        .layer(middleware::from_fn(no_store))
        .with_state(state)
}
async fn status(State(state): State<AuthState>, id: RequestId) -> Response {
    match state.registration.enabled().await {
        Ok(enabled) => Json(RegistrationStatus { enabled }).into_response(),
        Err(error) => admin_error(error, &id),
    }
}
async fn register(
    State(state): State<AuthState>,
    id: RequestId,
    headers: HeaderMap,
    client: ClientAddress,
    Json(input): Json<RegistrationInput>,
) -> Response {
    if let Err(error) = ensure_same_origin(&headers) {
        return admin_error(error, &id);
    }
    match state
        .registration
        .register(
            RegisterAccount {
                username: input.username,
                display_name: input.display_name,
                email: input.email,
                password: input.password,
            },
            client.0,
        )
        .await
    {
        Ok(()) => (
            StatusCode::CREATED,
            Json(crate::http_contract::MessageResult {
                message: "注册成功，请登录".into(),
            }),
        )
            .into_response(),
        Err(error) => admin_error(error, &id),
    }
}
#[derive(Clone)]
pub struct AccessState {
    pub registration: Arc<RegistrationInteractor>,
    pub admin: AdminState,
}
impl FromRef<AccessState> for AdminState {
    fn from_ref(state: &AccessState) -> Self {
        state.admin.clone()
    }
}
pub fn admin_router(state: AccessState) -> Router {
    Router::new()
        .route(
            "/api/admin/v1/access-settings",
            get(get_policy).put(save_policy),
        )
        .layer(DefaultBodyLimit::max(4096))
        .layer(middleware::from_fn(no_store))
        .with_state(state)
}
async fn get_policy(State(state): State<AccessState>, auth: AdminAuth, id: RequestId) -> Response {
    match state.registration.policy(&auth.actor, None).await {
        Ok(value) => Json(AccessSettings::from(value)).into_response(),
        Err(error) => admin_error(error, &id),
    }
}
async fn save_policy(
    State(state): State<AccessState>,
    auth: AdminAuth,
    id: RequestId,
    client: ClientAddress,
    Json(input): Json<AccessSettings>,
) -> Response {
    let actor = auth.actor.with_audit_ip(client.0);
    let update = AccessPolicy {
        registration_enabled: input.registration_enabled,
        guest_comments_enabled: input.guest_comments_enabled,
        version: input.version,
    };
    match state.registration.policy(&actor, Some(update)).await {
        Ok(value) => Json(AccessSettings::from(value)).into_response(),
        Err(error) => admin_error(error, &id),
    }
}
pub(crate) fn export_contract(out: &mut Vec<String>) {
    crate::http_contract::declare::<RegistrationInput>(out);
    crate::http_contract::declare::<RegistrationStatus>(out);
    crate::http_contract::declare::<AccessSettings>(out);
}
