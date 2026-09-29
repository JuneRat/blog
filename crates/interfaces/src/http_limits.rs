//! Transport admission and request deadlines. Connection/read/write lifetimes
//! are owned by the server, including time before headers reach this middleware.
use crate::{
    http_client_ip::ClientRateLimitKey,
    http_support::{RequestId, admin_error},
};
use application::ports::{PublicRequest, RequestAdmission};
use axum::{
    Json,
    extract::{Request, State},
    http::{Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::{sync::Arc, time::Duration};

pub async fn admit(
    State((limits, action)): State<(Arc<dyn RequestAdmission>, PublicRequest)>,
    client: ClientRateLimitKey,
    id: RequestId,
    request: Request,
    next: Next,
) -> Response {
    if (action == PublicRequest::OAuthStart || request.method() == Method::POST)
        && let Err(error) = limits.admit(action, client.0.as_deref())
    {
        return admin_error(error, &id);
    }
    next.run(request).await
}

#[derive(Clone, Copy, Debug)]
pub struct RequestTimeouts {
    pub request: Duration,
    pub upload: Duration,
}
impl Default for RequestTimeouts {
    fn default() -> Self {
        Self {
            request: Duration::from_secs(30),
            upload: Duration::from_secs(120),
        }
    }
}
pub async fn run(request: Request, next: Next, id: &RequestId) -> Response {
    let limits = request
        .extensions()
        .get::<RequestTimeouts>()
        .copied()
        .unwrap_or_default();
    let budget =
        if request.method() == Method::POST && request.uri().path() == "/api/admin/v1/media" {
            limits.upload
        } else {
            limits.request
        };
    match tokio::time::timeout(budget, next.run(request)).await {
        Ok(response) => response,
        Err(_) => (StatusCode::REQUEST_TIMEOUT, [("cache-control", "no-store")], Json(serde_json::json!({
            "code": "request_timeout", "error": "请求超时，请检查操作结果后重试", "request_id": id.as_str()
        }))).into_response(),
    }
}
