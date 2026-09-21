//! 请求编号与请求结果日志：响应头、JSON 错误体、完成日志三者必须一致。
//!
//! 捕获日志到内存缓冲（线程级 subscriber），验证：
//! - 每个响应（含预期 4xx）都带 `x-request-id`；
//! - 管理 JSON 错误体里的 `request_id` 等于响应头；
//! - 完成日志带同一编号、状态码与已验证 actor，匿名请求不记 actor。

use std::io::Write;
use std::sync::{Arc, Mutex};

use application::error::UseCaseError;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Router, middleware};
use http_body_util::BodyExt;
use interfaces::http_support::{REQUEST_ID_HEADER, RequestId, admin_error, request_context};
use tower::ServiceExt;
use tracing_subscriber::fmt::MakeWriter;

/// 已验证身份的固定用户 id（模拟认证成功后的补录）。
const ACTOR: &str = "018f0000-0000-7000-8000-000000000001";

#[derive(Clone, Default)]
struct LogBuffer(Arc<Mutex<Vec<u8>>>);

impl LogBuffer {
    fn contents(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).to_string()
    }
}

impl Write for LogBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for LogBuffer {
    type Writer = LogBuffer;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

async fn echo_id(request_id: RequestId) -> String {
    request_id.as_str().to_string()
}

async fn forbidden(request_id: RequestId) -> Response {
    admin_error(UseCaseError::Forbidden, &request_id)
}

async fn as_actor(request_id: RequestId) -> Response {
    request_id.set_actor(uuid::Uuid::parse_str(ACTOR).unwrap());
    (StatusCode::OK, "ok").into_response()
}

fn app() -> Router {
    Router::new()
        .route("/ok", get(echo_id))
        .route("/forbidden", get(forbidden))
        .route("/as-actor", get(as_actor))
        .layer(middleware::from_fn(request_context))
}

async fn fetch(app: &Router, uri: &str) -> (StatusCode, Option<String>, String) {
    let response = app
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let request_id = response
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let body = String::from_utf8_lossy(&response.into_body().collect().await.unwrap().to_bytes())
        .to_string();
    (status, request_id, body)
}

#[tokio::test]
async fn request_id_header_body_and_log_agree() {
    let buffer = LogBuffer::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(buffer.clone())
        .with_ansi(false)
        .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let app = app();

    // 1. 正常请求：响应头有编号，且是 UUIDv7 形态。
    let (status, id, body) = fetch(&app, "/ok").await;
    assert_eq!(status, StatusCode::OK);
    let id = id.expect("响应头必须带 x-request-id");
    assert_eq!(body, id, "演示路由回显编号");
    assert!(uuid::Uuid::parse_str(&id).is_ok(), "编号应为 UUID：{id}");

    // 2. 预期 4xx：编号同时出现在响应头、JSON 错误体与日志中。
    let (status, header_id, body) = fetch(&app, "/forbidden").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let header_id = header_id.expect("403 也必须带编号");
    assert!(
        body.contains(&format!("\"request_id\":\"{header_id}\"")),
        "错误体 request_id 必须等于响应头：{body}"
    );
    assert!(body.contains("\"code\":\"forbidden\""), "{body}");

    // 3. 每个请求编号唯一。
    assert_ne!(id, header_id);

    // 4. 已验证身份的请求补录 actor；匿名请求不带。
    let (status, actor_request_id, _) = fetch(&app, "/as-actor").await;
    assert_eq!(status, StatusCode::OK);
    let actor_request_id = actor_request_id.expect("编号");

    let logs = buffer.contents();
    assert!(logs.contains("请求完成"), "必须有完成日志：{logs}");
    for request_id in [&id, &header_id, &actor_request_id] {
        assert!(
            logs.contains(&format!("request_id={request_id}")),
            "日志缺少编号 {request_id}：{logs}"
        );
    }
    assert!(logs.contains("status=403"), "4xx 也要留下记录：{logs}");
    assert!(logs.contains("status=200"), "{logs}");
    assert!(logs.contains("elapsed_ms="), "完成日志应含耗时：{logs}");
    assert_eq!(
        logs.matches("actor_id=").count(),
        1,
        "只有已验证身份的请求记录 actor：{logs}"
    );
    assert!(
        logs.contains(&format!("actor_id={ACTOR}")),
        "actor 必须来自补录的身份：{logs}"
    );
}
