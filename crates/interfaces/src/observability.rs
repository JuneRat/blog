//! HTTP/runtime telemetry. Labels contain only bounded methods, route templates
//! and status codes; never request IDs, users, URLs, queries or credentials.
mod workloads;
use axum::{
    Extension, Json,
    http::{Method, StatusCode, header},
    response::IntoResponse,
};
use prometheus::{
    HistogramOpts, HistogramVec, IntCounterVec, IntGauge, IntGaugeVec, Opts, Registry, TextEncoder,
};
use serde::Serialize;
use std::time::Instant;

#[derive(Clone, Serialize)]
pub struct BuildInfo {
    pub version: &'static str,
    pub revision: &'static str,
}

pub async fn version(info: Option<Extension<BuildInfo>>) -> impl IntoResponse {
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(info.map(|info| info.0).unwrap_or(BuildInfo {
            version: env!("CARGO_PKG_VERSION"),
            revision: "unknown",
        })),
    )
}

pub async fn livez() -> impl IntoResponse {
    ([(header::CACHE_CONTROL, "no-store")], "ok")
}

#[derive(Clone)]
pub struct Telemetry {
    registry: Registry,
    requests: IntCounterVec,
    duration: HistogramVec,
    inflight: IntGauge,
    cancelled: IntCounterVec,
    pool: IntGaugeVec,
    installed: IntGauge,
    workloads: workloads::WorkloadMetrics,
}

impl Telemetry {
    pub fn new(build: &BuildInfo) -> Self {
        let registry = Registry::new();
        let requests = IntCounterVec::new(
            Opts::new("blog_http_requests_total", "Completed HTTP responses"),
            &["method", "route", "status"],
        )
        .unwrap();
        let duration = HistogramVec::new(
            HistogramOpts::new(
                "blog_http_request_duration_seconds",
                "Time until HTTP response headers, excluding body streaming",
            )
            .buckets(vec![
                0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
            ]),
            &["method", "route"],
        )
        .unwrap();
        let inflight = IntGauge::new(
            "blog_http_requests_in_flight",
            "HTTP handlers currently running",
        )
        .unwrap();
        let cancelled = IntCounterVec::new(
            Opts::new(
                "blog_http_requests_cancelled_total",
                "HTTP handlers dropped before producing a response",
            ),
            &["method", "route"],
        )
        .unwrap();
        let pool = IntGaugeVec::new(
            Opts::new(
                "blog_database_pool_connections",
                "Runtime database pool snapshot",
            ),
            &["state"],
        )
        .unwrap();
        let installed = IntGauge::new(
            "blog_installation_complete",
            "One after a runtime pool is installed, not a readiness check",
        )
        .unwrap();
        let info = IntGaugeVec::new(
            Opts::new("blog_build_info", "Running binary build information"),
            &["version", "revision"],
        )
        .unwrap();
        info.with_label_values(&[build.version, build.revision])
            .set(1);
        for metric in [
            Box::new(requests.clone()) as Box<dyn prometheus::core::Collector>,
            Box::new(duration.clone()),
            Box::new(inflight.clone()),
            Box::new(cancelled.clone()),
            Box::new(pool.clone()),
            Box::new(installed.clone()),
            Box::new(info),
        ] {
            registry
                .register(metric)
                .expect("unique static metric names");
        }
        Self {
            workloads: workloads::WorkloadMetrics::new(&registry),
            registry,
            requests,
            duration,
            inflight,
            cancelled,
            pool,
            installed,
        }
    }

    pub fn begin(&self, method: &Method, route: &str) -> RequestMeasurement {
        let method = match method.as_str() {
            "GET" | "HEAD" | "POST" | "PUT" | "PATCH" | "DELETE" | "OPTIONS" | "CONNECT"
            | "TRACE" => method.as_str(),
            _ => "OTHER",
        }
        .to_owned();
        self.inflight.inc();
        RequestMeasurement {
            metrics: self.clone(),
            method,
            route: route.to_owned(),
            started: Instant::now(),
            completed: false,
        }
    }

    pub fn pool_snapshot(&self, snapshot: Option<(u32, usize, u32)>) {
        let (size, idle, max) = snapshot.unwrap_or_default();
        self.installed.set(i64::from(snapshot.is_some()));
        for (state, value) in [
            ("size", size as i64),
            ("idle", idle as i64),
            ("in_use", size.saturating_sub(idle as u32) as i64),
            ("max", max as i64),
        ] {
            self.pool.with_label_values(&[state]).set(value);
        }
    }

    pub fn encode(&self) -> Result<String, prometheus::Error> {
        let mut output = String::new();
        TextEncoder::new().encode_utf8(&self.registry.gather(), &mut output)?;
        Ok(output)
    }

    pub fn publication_run(&self, elapsed: std::time::Duration, success: bool) {
        self.workloads.publication_run(elapsed, success);
    }
}

impl application::rendering_observer::RenderingObserver for Telemetry {
    fn observe(
        &self,
        kind: application::rendering_observer::RenderKind,
        event: application::rendering_observer::RenderingEvent,
    ) {
        self.workloads.observe(kind, event);
    }
}

pub struct RequestMeasurement {
    metrics: Telemetry,
    method: String,
    route: String,
    started: Instant,
    completed: bool,
}

impl RequestMeasurement {
    pub fn complete(mut self, status: StatusCode) {
        self.metrics
            .requests
            .with_label_values(&[&self.method, &self.route, status.as_str()])
            .inc();
        self.metrics
            .duration
            .with_label_values(&[&self.method, &self.route])
            .observe(self.started.elapsed().as_secs_f64());
        self.completed = true;
    }
}

impl Drop for RequestMeasurement {
    fn drop(&mut self) {
        self.metrics.inflight.dec();
        if !self.completed {
            self.metrics
                .cancelled
                .with_label_values(&[&self.method, &self.route])
                .inc();
        }
    }
}
