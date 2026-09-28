use application::rendering_observer::{RenderKind, RenderingEvent};
use prometheus::{
    Histogram, HistogramOpts, HistogramVec, IntCounterVec, IntGauge, IntGaugeVec, Opts, Registry,
};
use std::time::Duration;

#[derive(Clone)]
pub(super) struct WorkloadMetrics {
    waiting: IntGaugeVec,
    active: IntGaugeVec,
    queue_duration: HistogramVec,
    queues: IntCounterVec,
    render_duration: HistogramVec,
    renders: IntCounterVec,
    timeouts: IntCounterVec,
    publication_runs: IntCounterVec,
    publication_duration: Histogram,
    publication_last_success: IntGauge,
}

impl WorkloadMetrics {
    pub(super) fn new(registry: &Registry) -> Self {
        let waiting = IntGaugeVec::new(
            Opts::new(
                "blog_render_waiting",
                "Tasks waiting for a rendering permit",
            ),
            &["kind"],
        )
        .unwrap();
        let active = IntGaugeVec::new(
            Opts::new(
                "blog_render_active",
                "Actually running rendering workers, including workers whose caller timed out",
            ),
            &["kind"],
        )
        .unwrap();
        let queue_duration = HistogramVec::new(
            HistogramOpts::new(
                "blog_render_queue_duration_seconds",
                "Rendering permit wait time, including cancelled and rejected waits",
            )
            .buckets(vec![0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0]),
            &["kind"],
        )
        .unwrap();
        let queues = IntCounterVec::new(
            Opts::new("blog_render_queue_total", "Finished rendering permit waits"),
            &["kind", "result"],
        )
        .unwrap();
        let render_duration = HistogramVec::new(
            HistogramOpts::new(
                "blog_render_execution_duration_seconds",
                "Actual worker duration, excluding blocking pool scheduling",
            ),
            &["kind"],
        )
        .unwrap();
        let renders = IntCounterVec::new(
            Opts::new(
                "blog_render_completed_total",
                "Rendering workers that exited",
            ),
            &["kind", "result"],
        )
        .unwrap();
        let timeouts = IntCounterVec::new(
            Opts::new(
                "blog_render_execution_timeouts_total",
                "Callers timing out while waiting for their rendering worker",
            ),
            &["kind"],
        )
        .unwrap();
        let publication_runs = IntCounterVec::new(
            Opts::new(
                "blog_scheduled_publication_runs_total",
                "Completed scheduled publication polling runs",
            ),
            &["result"],
        )
        .unwrap();
        let publication_duration = Histogram::with_opts(HistogramOpts::new(
            "blog_scheduled_publication_run_duration_seconds",
            "Time spent processing due publications per polling run",
        ))
        .unwrap();
        let publication_last_success = IntGauge::new(
            "blog_scheduled_publication_last_success_timestamp_seconds",
            "Unix time of the last successful publication poll; zero before the first success",
        )
        .unwrap();
        for metric in [
            Box::new(waiting.clone()) as Box<dyn prometheus::core::Collector>,
            Box::new(active.clone()),
            Box::new(queue_duration.clone()),
            Box::new(queues.clone()),
            Box::new(render_duration.clone()),
            Box::new(renders.clone()),
            Box::new(timeouts.clone()),
            Box::new(publication_runs.clone()),
            Box::new(publication_duration.clone()),
            Box::new(publication_last_success.clone()),
        ] {
            registry
                .register(metric)
                .expect("unique static metric names");
        }
        for kind in ["content", "comment", "theme"] {
            waiting.with_label_values(&[kind]).set(0);
            active.with_label_values(&[kind]).set(0);
            timeouts.with_label_values(&[kind]).inc_by(0);
        }
        Self {
            waiting,
            active,
            queue_duration,
            queues,
            render_duration,
            renders,
            timeouts,
            publication_runs,
            publication_duration,
            publication_last_success,
        }
    }

    pub(super) fn observe(&self, kind: RenderKind, event: RenderingEvent) {
        let kind = kind.as_str();
        match event {
            RenderingEvent::Queued => self.waiting.with_label_values(&[kind]).inc(),
            RenderingEvent::QueueFinished { elapsed, outcome } => {
                self.waiting.with_label_values(&[kind]).dec();
                self.queue_duration
                    .with_label_values(&[kind])
                    .observe(elapsed.as_secs_f64());
                self.queues
                    .with_label_values(&[kind, outcome.as_str()])
                    .inc();
            }
            RenderingEvent::Started => self.active.with_label_values(&[kind]).inc(),
            RenderingEvent::Finished { elapsed, success } => {
                self.active.with_label_values(&[kind]).dec();
                self.render_duration
                    .with_label_values(&[kind])
                    .observe(elapsed.as_secs_f64());
                self.renders
                    .with_label_values(&[kind, if success { "success" } else { "error" }])
                    .inc();
            }
            RenderingEvent::ExecutionTimeout => self.timeouts.with_label_values(&[kind]).inc(),
        }
    }

    pub(super) fn publication_run(&self, elapsed: Duration, success: bool) {
        self.publication_duration.observe(elapsed.as_secs_f64());
        self.publication_runs
            .with_label_values(&[if success { "success" } else { "error" }])
            .inc();
        if success {
            self.publication_last_success
                .set(time::OffsetDateTime::now_utc().unix_timestamp());
        }
    }
}
