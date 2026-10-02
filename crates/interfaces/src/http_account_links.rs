//! Anonymous password recovery and administrator invitations.
use crate::{
    http_admin::AdminAuth,
    http_auth::{AdminState, AuthState},
    http_client_ip::ClientAddress,
    http_contract::MessageResult,
    http_support::{RequestId, admin_error, ensure_same_origin, no_store},
};
use application::account_links::LinkTarget;
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, StatusCode},
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post},
};

#[derive(serde::Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct RecoveryEmailInput {
    pub email: String,
}
#[derive(serde::Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ResetPasswordInput {
    pub token: String,
    pub password: String,
}
#[derive(serde::Serialize, ts_rs::TS)]
pub struct RecoveryStatus {
    pub enabled: bool,
}

pub fn public_router(state: AuthState) -> Router {
    Router::new()
        .route("/auth/password/recovery", get(status).post(request))
        .route("/auth/password/reset", post(reset))
        .route_layer(middleware::from_fn_with_state(
            (
                state.admission.clone(),
                application::ports::PublicRequest::PasswordRecovery,
            ),
            crate::http_limits::admit,
        ))
        .layer(DefaultBodyLimit::max(4096))
        .layer(middleware::from_fn(no_store))
        .with_state(state)
}
async fn status(State(state): State<AuthState>) -> Json<RecoveryStatus> {
    Json(RecoveryStatus {
        enabled: state.passwords.account_links().is_ok(),
    })
}
async fn request(
    State(state): State<AuthState>,
    id: RequestId,
    headers: HeaderMap,
    Json(input): Json<RecoveryEmailInput>,
) -> Response {
    if let Err(error) = ensure_same_origin(&headers) {
        return admin_error(error, &id);
    }
    let links = match state.passwords.account_links() {
        Ok(links) => links,
        Err(error) => return admin_error(error, &id),
    };
    // Admission is bounded before spawning. Both known and unknown addresses get
    // the same response before account lookup/SMTP, including transport failures.
    tokio::spawn(async move {
        if let Err(error) = links
            .request(LinkTarget::Recovery(input.email.trim()))
            .await
        {
            tracing::warn!(request_id = id.as_str(), error = %error, "password recovery delivery failed");
        }
    });
    (
        StatusCode::ACCEPTED,
        Json(MessageResult {
            message: "如果该邮箱关联可找回的账号，将收到密码重置邮件；请检查收件箱和垃圾邮件。"
                .into(),
        }),
    )
        .into_response()
}
async fn reset(
    State(state): State<AuthState>,
    id: RequestId,
    headers: HeaderMap,
    client: ClientAddress,
    Json(input): Json<ResetPasswordInput>,
) -> Response {
    if let Err(error) = ensure_same_origin(&headers) {
        return admin_error(error, &id);
    }
    let result = match state.passwords.account_links() {
        Ok(links) => links.reset(&input.token, &input.password, client.0).await,
        Err(error) => Err(error),
    };
    match result {
        Ok(()) => Json(MessageResult {
            message: "密码已设置，旧会话已撤销，请重新登录。".into(),
        })
        .into_response(),
        Err(error) => admin_error(error, &id),
    }
}
pub fn admin_router(state: AdminState) -> Router {
    Router::new()
        .route("/api/admin/v1/users/{user_id}/invitation", post(invite))
        .layer(DefaultBodyLimit::max(4096))
        .layer(middleware::from_fn(no_store))
        .with_state(state)
}
async fn invite(
    State(state): State<AdminState>,
    auth: AdminAuth,
    id: RequestId,
    Path(user_id): Path<uuid::Uuid>,
) -> Response {
    if !auth.actor.has_permission("user.manage") {
        return admin_error(application::UseCaseError::Forbidden, &id);
    }
    let result = match state.passwords.account_links() {
        Ok(links) => {
            links
                .request(LinkTarget::Invitation {
                    user_id,
                    actor: &auth.actor,
                })
                .await
        }
        Err(error) => Err(error),
    };
    match result {
        Ok(()) => Json(MessageResult {
            message: "邀请邮件已交给邮件服务，链接在 30 分钟内有效。".into(),
        })
        .into_response(),
        Err(error) => admin_error(error, &id),
    }
}
pub(crate) fn export_contract(out: &mut Vec<String>) {
    crate::http_contract::declare::<RecoveryEmailInput>(out);
    crate::http_contract::declare::<ResetPasswordInput>(out);
    crate::http_contract::declare::<RecoveryStatus>(out);
}
