//! HTTP/runtime telemetry. Labels contain only bounded methods, route templates
//! and status codes; never request IDs, users, URLs, queries or credentials.
mod tasks;
mod workloads;
use axum::{
    Extension, Json,
    http::{Method, StatusCode, header},
    response::IntoResponse,
};
use prometheus::{
    HistogramOpts, HistogramVec, IntCounter, IntCounterVec, IntGauge, IntGaugeVec, Opts, Registry,
    TextEncoder,
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
    settings_reads: IntCounterVec,
    settings_available: IntGauge,
    settings_recoveries: IntCounter,
    tasks: tasks::TaskMetrics,
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
        let settings_reads = IntCounterVec::new(
            Opts::new(
                "blog_site_settings_reads_total",
                "Site settings reads, including failures hidden by public fallback",
            ),
            &["result"],
        )
        .unwrap();
        let settings_available = IntGauge::new(
            "blog_site_settings_read_available",
            "Latest site settings read succeeded: one, zero on failure, minus one before any read",
        )
        .unwrap();
        settings_available.set(-1);
        let settings_recoveries = IntCounter::new(
            "blog_site_settings_read_recoveries_total",
            "Successful reads following a site settings read failure",
        )
        .unwrap();
        for metric in [
            Box::new(requests.clone()) as Box<dyn prometheus::core::Collector>,
            Box::new(duration.clone()),
            Box::new(inflight.clone()),
            Box::new(cancelled.clone()),
            Box::new(pool.clone()),
            Box::new(installed.clone()),
            Box::new(info),
            Box::new(settings_reads.clone()),
            Box::new(settings_available.clone()),
            Box::new(settings_recoveries.clone()),
        ] {
            registry
                .register(metric)
                .expect("unique static metric names");
        }
        Self {
            tasks: tasks::TaskMetrics::new(&registry),
            workloads: workloads::WorkloadMetrics::new(&registry),
            registry,
            requests,
            duration,
            inflight,
            cancelled,
            pool,
            installed,
            settings_reads,
            settings_available,
            settings_recoveries,
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

    pub fn task_started(&self, kind: application::tasks::TaskKind, wait: std::time::Duration) {
        self.tasks.started(kind, wait);
    }

    /// Observe only a terminal transition acknowledged by the durable store.
    /// Cancelled and recovered leases have no execution duration in this process.
    pub fn task_finished(
        &self,
        kind: application::tasks::TaskKind,
        status: application::tasks::TaskStatus,
        elapsed: Option<std::time::Duration>,
    ) {
        self.tasks.finished(kind, status, elapsed);
    }

    pub fn task_lease_expirations(&self, kind: application::tasks::TaskKind, count: u64) {
        self.tasks.lease_expirations(kind, count);
    }

    /// Called by the supervisor, never by the metrics HTTP handler.
    pub fn task_health(&self, health: TaskHealth) {
        self.tasks.health(health);
    }

    pub fn task_health_snapshot_success(&self, timestamp: i64) {
        self.tasks.snapshot_success(timestamp);
    }

    pub fn task_scheduler_check(&self, result: TaskSchedulerCheck, timestamp: i64) {
        self.tasks.scheduler_check(result, timestamp);
    }

    pub fn task_scheduler_stopped(&self) {
        self.tasks.scheduler_stopped();
    }
}

pub use tasks::{TaskHealth, TaskSchedulerCheck};

impl application::rendering_observer::RenderingObserver for Telemetry {
    fn observe(
        &self,
        kind: application::rendering_observer::RenderKind,
        event: application::rendering_observer::RenderingEvent,
    ) {
        self.workloads.observe(kind, event);
    }
}

impl application::ports::SettingsReadObserver for Telemetry {
    fn observe_site_read(
        &self,
        outcome: application::ports::SiteSettingsReadOutcome,
        recovered: bool,
    ) {
        use application::ports::SiteSettingsReadOutcome;
        let result = match outcome {
            SiteSettingsReadOutcome::Configured => "configured",
            SiteSettingsReadOutcome::Missing => "missing",
            SiteSettingsReadOutcome::Failed => "error",
        };
        self.settings_reads.with_label_values(&[result]).inc();
        self.settings_available
            .set(i64::from(outcome != SiteSettingsReadOutcome::Failed));
        if recovered {
            self.settings_recoveries.inc();
        }
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
