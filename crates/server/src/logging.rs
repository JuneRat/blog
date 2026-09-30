//! 日志装配和不受过滤级别影响的运维提示；stdout 留给 CLI 结果。
use std::{fmt, io, sync::OnceLock};

use infrastructure::SiteTimeZone;
use time::OffsetDateTime;
use tracing::Dispatch;
use tracing_subscriber::{
    EnvFilter,
    fmt::{MakeWriter, format::Writer, time::FormatTime},
    util::SubscriberInitExt,
};

static OUTPUT: OnceLock<LogOutput> = OnceLock::new();

#[derive(Clone)]
struct LogTimer(SiteTimeZone);

impl FormatTime for LogTimer {
    fn format_time(&self, writer: &mut Writer<'_>) -> fmt::Result {
        writer.write_str(&self.0.log_timestamp(OffsetDateTime::now_utc()))
    }
}

struct LogOutput {
    zone: SiteTimeZone,
    json: bool,
}

impl LogOutput {
    fn notice(&self, message: fmt::Arguments<'_>, at: OffsetDateTime) -> String {
        let timestamp = self.zone.log_timestamp(at);
        if self.json {
            serde_json::json!({
                "timestamp": timestamp, "level": "INFO", "target": "blog",
                "fields": {"message": message.to_string()}
            })
            .to_string()
        } else {
            format!("{timestamp}  INFO {message}")
        }
    }
}

fn subscriber<W>(filter: EnvFilter, output: &LogOutput, writer: W) -> Dispatch
where
    W: for<'a> MakeWriter<'a> + Send + Sync + 'static,
{
    let builder = tracing_subscriber::fmt()
        .with_writer(writer)
        .with_timer(LogTimer(output.zone.clone()))
        .with_ansi(false)
        .with_env_filter(filter);
    if output.json {
        Dispatch::new(
            builder
                .json()
                .with_current_span(true)
                .with_span_list(false)
                .finish(),
        )
    } else {
        // Request events already say what happened; Rust module paths add noise.
        Dispatch::new(builder.with_target(false).finish())
    }
}

pub fn init(config: &crate::config::DeploymentConfig) -> Result<(), String> {
    let output = LogOutput {
        zone: match std::env::var("TZ") {
            Ok(name) => SiteTimeZone::parse(&name).map_err(|error| format!("TZ {error}"))?,
            Err(std::env::VarError::NotPresent) => SiteTimeZone::default(),
            Err(_) => return Err("TZ 必须是有效 UTF-8 的 IANA 时区名称".into()),
        },
        json: config.log_json()?,
    };
    subscriber(config.log_filter()?, &output, io::stderr)
        .try_init()
        .map_err(|error| format!("初始化日志失败：{error}"))?;
    OUTPUT.set(output).map_err(|_| "日志已初始化".to_owned())
}

/// 安装码和监听地址必须可见，即使 RUST_LOG=warn；格式与运行日志一致。
pub fn notice(message: fmt::Arguments<'_>) {
    let output = OUTPUT
        .get()
        .expect("logging initialized before operator notices");
    eprintln!("{}", output.notice(message, OffsetDateTime::now_utc()));
}

/// 记录轮询失败与恢复；空闲轮询成功时保持安静，不推断是否有文章发布失败。
#[derive(Default)]
pub(crate) struct PublicationLog {
    consecutive_failures: u64,
}

impl PublicationLog {
    pub(crate) fn record(
        &mut self,
        result: &Result<usize, application::UseCaseError>,
        elapsed: std::time::Duration,
        pool: infrastructure::PoolSnapshot,
    ) {
        let elapsed_ms = elapsed.as_millis() as u64;
        match result {
            Err(error) => {
                self.consecutive_failures = self.consecutive_failures.saturating_add(1);
                tracing::error!(
                    %error,
                    consecutive_failures = self.consecutive_failures,
                    elapsed_ms,
                    pool_size = pool.connections,
                    pool_idle = pool.idle_connections,
                    pool_max = pool.max_connections,
                    "预约发布轮询失败，下次轮询重试"
                );
            }
            Ok(published_count) if self.consecutive_failures > 0 => {
                let failed_polls = std::mem::take(&mut self.consecutive_failures);
                tracing::info!(
                    failed_polls,
                    published_count = *published_count,
                    elapsed_ms,
                    pool_size = pool.connections,
                    pool_idle = pool.idle_connections,
                    pool_max = pool.max_connections,
                    "预约发布轮询已恢复"
                );
            }
            Ok(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        body::Body,
        http::{Request, StatusCode},
        middleware,
        routing::get,
    };
    use interfaces::http_support::{RequestId, request_context};
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;

    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);
    impl io::Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl<'a> MakeWriter<'a> for Capture {
        type Writer = Self;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    #[test]
    fn publication_logs_count_failures_and_report_recovery_once_even_without_due_content() {
        let capture = Capture::default();
        let dispatch = subscriber(
            EnvFilter::new("info"),
            &LogOutput {
                zone: SiteTimeZone::default(),
                json: true,
            },
            capture.clone(),
        );
        let failed_pool = infrastructure::PoolSnapshot {
            connections: 5,
            idle_connections: 0,
            max_connections: 5,
        };
        let recovered_pool = infrastructure::PoolSnapshot {
            connections: 2,
            idle_connections: 2,
            max_connections: 5,
        };
        let error = Err(application::UseCaseError::Repository(
            "pool timed out".into(),
        ));
        let slow = std::time::Duration::from_millis(5000);
        let fast = std::time::Duration::from_millis(3);
        tracing::dispatcher::with_default(&dispatch, || {
            let mut log = PublicationLog::default();
            log.record(&Ok(0), fast, recovered_pool);
            log.record(&error, slow, failed_pool);
            log.record(&error, slow, failed_pool);
            log.record(&Ok(0), fast, recovered_pool);
            log.record(&Ok(0), fast, recovered_pool);
            log.record(&error, slow, failed_pool);
            log.record(&Ok(3), fast, recovered_pool);
        });
        let text = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
        let events: Vec<serde_json::Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(events.len(), 5, "{text}");
        for (index, count) in [(0, 1), (1, 2), (3, 1)] {
            let event = &events[index];
            assert_eq!(event["level"], "ERROR");
            let fields = &event["fields"];
            assert_eq!(fields["message"], "预约发布轮询失败，下次轮询重试");
            assert!(fields["error"].as_str().unwrap().contains("pool timed out"));
            assert_eq!(fields["consecutive_failures"], count);
            assert_eq!(fields["elapsed_ms"], 5000);
            assert_eq!(fields["pool_size"], 5);
            assert_eq!(fields["pool_idle"], 0);
            assert_eq!(fields["pool_max"], 5);
        }
        for (index, failures, published) in [(2, 2, 0), (4, 1, 3)] {
            let event = &events[index];
            assert_eq!(event["level"], "INFO");
            let fields = &event["fields"];
            assert_eq!(fields["message"], "预约发布轮询已恢复");
            assert_eq!(fields["failed_polls"], failures);
            assert_eq!(fields["published_count"], published);
            assert_eq!(fields["elapsed_ms"], 3);
            assert_eq!(fields["pool_size"], 2);
            assert_eq!(fields["pool_idle"], 2);
            assert_eq!(fields["pool_max"], 5);
        }
    }

    fn request_log(json: bool, filter: &str, status: StatusCode) -> (String, String) {
        let output = LogOutput {
            zone: SiteTimeZone::parse("Asia/Shanghai").unwrap(),
            json,
        };
        let capture = Capture::default();
        let dispatch = subscriber(EnvFilter::new(filter), &output, capture.clone());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let app = Router::new()
            .route(
                "/items/{id}",
                get(move |id: RequestId| async move {
                    id.set_actor(uuid::Uuid::from_u128(1));
                    tracing::info!("处理请求");
                    status
                }),
            )
            .layer(middleware::from_fn(request_context));
        let response = tracing::dispatcher::with_default(&dispatch, || {
            runtime.block_on(
                app.oneshot(
                    Request::builder()
                        .uri("/items/42?token=secret-query")
                        .header("cookie", "secret-cookie")
                        .body(Body::from("secret-body"))
                        .unwrap(),
                ),
            )
        })
        .unwrap();
        let request_id = response.headers()["x-request-id"]
            .to_str()
            .unwrap()
            .to_owned();
        let text = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
        for secret in ["secret-query", "secret-cookie", "secret-body"] {
            assert!(!text.contains(secret), "{text}");
        }
        (text, request_id)
    }

    #[test]
    fn text_completion_has_configured_log_time_and_one_copy_of_request_fields() {
        let (text, id) = request_log(false, "info", StatusCode::OK);
        let line = text.lines().find(|line| line.contains("请求完成")).unwrap();
        assert!(
            line.contains("+08:00  INFO 请求完成 method=GET path=/items/42 status=200 elapsed_ms="),
            "{line}"
        );
        assert_eq!(line.matches("request_id=").count(), 1);
        assert_eq!(line.matches("method=").count(), 1);
        assert_eq!(line.matches("path=").count(), 1);
        assert_eq!(line.matches("route=").count(), 1);
        assert!(line.contains(&id));
        assert!(line.contains("actor_id="));
        assert!(!line.contains("http_request{"));
        assert!(!line.contains("interfaces::"));
        // Handler events retain their span correlation for diagnosing intermediate failures.
        assert!(
            text.lines()
                .any(|line| line.contains("处理请求") && line.contains(&id))
        );
    }

    #[test]
    fn json_completion_stays_structured_and_warn_filter_keeps_correlation() {
        for filter in ["info", "warn"] {
            let (text, id) = request_log(true, filter, StatusCode::INTERNAL_SERVER_ERROR);
            let events: Vec<serde_json::Value> = text
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            let event = events
                .iter()
                .find(|event| event["fields"]["message"] == "请求完成")
                .unwrap();
            assert_eq!(event["level"], "WARN");
            assert_eq!(event["fields"]["request_id"], id);
            assert_eq!(event["fields"]["method"], "GET");
            assert_eq!(event["fields"]["path"], "/items/42");
            assert_eq!(event["fields"]["route"], "/items/{id}");
            assert_eq!(event["fields"]["status"], 500);
            assert!(event["fields"]["elapsed_ms"].is_u64());
            assert!(event["fields"]["actor_id"].is_string());
            assert!(event.get("span").is_none());
            let timestamp = OffsetDateTime::parse(
                event["timestamp"].as_str().unwrap(),
                &time::format_description::well_known::Rfc3339,
            )
            .unwrap();
            assert_eq!(timestamp.offset().whole_hours(), 8);
            if filter == "warn" {
                assert_eq!(events.len(), 1);
            } else {
                let handler = events
                    .iter()
                    .find(|event| event["fields"]["message"] == "处理请求")
                    .unwrap();
                assert_eq!(handler["span"]["request_id"], id);
            }
        }
    }

    #[test]
    fn unfiltered_operator_notices_share_the_timestamp_and_output_format() {
        let at = time::macros::datetime!(2026-09-28 13:26:12.044045 UTC);
        let mut output = LogOutput {
            zone: SiteTimeZone::parse("Asia/Shanghai").unwrap(),
            json: false,
        };
        assert_eq!(
            output.notice(format_args!("监听已启动"), at),
            "2026-09-28T21:26:12.044+08:00  INFO 监听已启动"
        );
        output.json = true;
        let value: serde_json::Value =
            serde_json::from_str(&output.notice(format_args!("监听已启动"), at)).unwrap();
        assert_eq!(value["timestamp"], "2026-09-28T21:26:12.044+08:00");
        assert_eq!(value["fields"]["message"], "监听已启动");
    }
}
