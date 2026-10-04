//! Embedded management UI, intentionally independent of database-backed auth.
use crate::{
    http_client_ip::ClientAddress,
    http_support::{self, RequestId},
};
use application::{UseCaseError, backup::*};
use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, Path, Query, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use std::sync::Arc;
use tokio::io::AsyncReadExt;

#[derive(Clone)]
struct RecoveryState {
    control: Arc<dyn RecoveryControl>,
    secure: bool,
}

pub fn router(control: Arc<dyn RecoveryControl>, secure: bool) -> Router {
    let state = RecoveryState { control, secure };
    let uploads = Router::new()
        .route("/api/recovery/upload", post(upload))
        .layer(DefaultBodyLimit::max(4 * 1024 * 1024))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            upload_admission,
        ));
    Router::new()
        .route(
            "/recovery",
            get(|| async { Html(include_str!("recovery/index.html")) }),
        )
        .route(
            "/recovery/app.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("recovery/app.js"),
                )
            }),
        )
        .route(
            "/recovery/style.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("recovery/style.css"),
                )
            }),
        )
        .route(
            "/api/recovery/session",
            get(status).post(login).delete(logout),
        )
        .route("/api/recovery/action", post(command))
        .route("/api/recovery/download/{name}", get(download))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .merge(uploads)
        .with_state(state)
        .layer(middleware::from_fn(security))
        .layer(middleware::from_fn(http_support::request_context))
}
fn cookie_name(secure: bool) -> &'static str {
    if secure {
        "__Host-blog-recovery"
    } else {
        "blog-recovery"
    }
}
fn credential(state: &RecoveryState, headers: &HeaderMap) -> RecoveryCredential {
    RecoveryCredential {
        token: http_support::cookie_value(headers, cookie_name(state.secure)).unwrap_or_default(),
        csrf: headers
            .get("x-csrf-token")
            .and_then(|h| h.to_str().ok())
            .map(str::to_owned),
    }
}
fn cookie(state: &RecoveryState, token: &str, age: u32) -> HeaderValue {
    HeaderValue::from_str(&format!(
        "{}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age={age}{}",
        cookie_name(state.secure),
        token,
        if state.secure { "; Secure" } else { "" }
    ))
    .expect("generated cookie")
}
fn answer(result: Result<serde_json::Value, UseCaseError>, id: &RequestId) -> Response {
    match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => http_support::admin_error(error, id),
    }
}
async fn login(
    State(state): State<RecoveryState>,
    id: RequestId,
    ClientAddress(ip): ClientAddress,
    headers: HeaderMap,
    input: Result<Json<RecoveryLogin>, axum::extract::rejection::JsonRejection>,
) -> Response {
    if http_support::ensure_same_origin(&headers).is_err() {
        return http_support::admin_error(UseCaseError::Forbidden, &id);
    }
    let Ok(Json(input)) = input else {
        return http_support::admin_error(UseCaseError::Invalid("登录表单无效".into()), &id);
    };
    match state
        .control
        .login(input, ip.map(|ip| ip.to_string()))
        .await
    {
        Ok(session) => {
            let mut response = Json(serde_json::json!({"csrf": session.csrf})).into_response();
            response
                .headers_mut()
                .insert(header::SET_COOKIE, cookie(&state, &session.token, 3600));
            response
        }
        Err(error) => http_support::admin_error(error, &id),
    }
}
async fn status(State(state): State<RecoveryState>, id: RequestId, headers: HeaderMap) -> Response {
    answer(
        state.control.status(credential(&state, &headers)).await,
        &id,
    )
}
async fn logout(State(state): State<RecoveryState>, id: RequestId, headers: HeaderMap) -> Response {
    if http_support::ensure_same_origin(&headers).is_err() {
        return http_support::admin_error(UseCaseError::Forbidden, &id);
    }
    match state.control.logout(credential(&state, &headers)).await {
        Ok(()) => {
            let mut response = StatusCode::NO_CONTENT.into_response();
            response
                .headers_mut()
                .insert(header::SET_COOKIE, cookie(&state, "", 0));
            response
        }
        Err(error) => http_support::admin_error(error, &id),
    }
}
async fn command(
    State(state): State<RecoveryState>,
    id: RequestId,
    headers: HeaderMap,
    input: Result<Json<RecoveryCommand>, axum::extract::rejection::JsonRejection>,
) -> Response {
    if http_support::ensure_same_origin(&headers).is_err() {
        return http_support::admin_error(UseCaseError::Forbidden, &id);
    }
    let Ok(Json(input)) = input else {
        return http_support::admin_error(UseCaseError::Invalid("操作参数无效".into()), &id);
    };
    answer(
        state
            .control
            .execute(credential(&state, &headers), input)
            .await,
        &id,
    )
}
#[derive(Deserialize)]
struct UploadQuery {
    id: Option<String>,
    #[serde(default)]
    offset: u64,
    #[serde(default)]
    complete: bool,
}
async fn upload_admission(
    State(state): State<RecoveryState>,
    request: Request,
    next: Next,
) -> Response {
    // Authenticate before buffering a chunk; the handler rechecks after reading it.
    let id = request
        .extensions()
        .get::<RequestId>()
        .cloned()
        .unwrap_or_else(RequestId::generate);
    if http_support::ensure_same_origin(request.headers()).is_err() {
        return http_support::admin_error(UseCaseError::Forbidden, &id);
    }
    match state
        .control
        .authorize_upload(credential(&state, request.headers()))
        .await
    {
        Ok(()) => next.run(request).await,
        Err(error) => http_support::admin_error(error, &id),
    }
}
async fn upload(
    State(state): State<RecoveryState>,
    id: RequestId,
    headers: HeaderMap,
    Query(query): Query<UploadQuery>,
    bytes: Bytes,
) -> Response {
    if http_support::ensure_same_origin(&headers).is_err() {
        return http_support::admin_error(UseCaseError::Forbidden, &id);
    }
    answer(
        state
            .control
            .upload(
                credential(&state, &headers),
                query.id,
                query.offset,
                query.complete,
                bytes.to_vec(),
            )
            .await,
        &id,
    )
}
async fn download(
    State(state): State<RecoveryState>,
    id: RequestId,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    let file = match state
        .control
        .download(credential(&state, &headers), name)
        .await
    {
        Ok(file) => file,
        Err(error) => return http_support::admin_error(error, &id),
    };
    let stream = match tokio::fs::File::open(file.path).await {
        Ok(stream) => stream,
        Err(_) => return http_support::admin_error(UseCaseError::NotFound("备份".into()), &id),
    };
    let body = futures_util::stream::try_unfold(stream, |mut stream| async move {
        let mut bytes = vec![0u8; 64 * 1024];
        let read = stream.read(&mut bytes).await?;
        if read == 0 {
            return Ok::<_, std::io::Error>(None);
        }
        bytes.truncate(read);
        Ok(Some((bytes, stream)))
    });
    let mut response = Body::from_stream(body).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    response.headers_mut().insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&file.size.to_string()).expect("file size"),
    );
    if let Ok(value) = HeaderValue::from_str(&format!("attachment; filename=\"{}\"", file.name)) {
        response
            .headers_mut()
            .insert(header::CONTENT_DISPOSITION, value);
    }
    response
}
async fn security(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    for (key, value) in [
        ("cache-control", "no-store"),
        ("referrer-policy", "no-referrer"),
        ("x-content-type-options", "nosniff"),
        ("x-frame-options", "DENY"),
        (
            "content-security-policy",
            "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'",
        ),
    ] {
        response
            .headers_mut()
            .insert(key, HeaderValue::from_static(value));
    }
    response
}
