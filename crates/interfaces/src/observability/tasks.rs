//! Fixed task kinds and states only. Persistent gauges are sampled by the owner;
//! counters and execution timings describe transitions acknowledged here.
use application::tasks::{TaskKind, TaskStatus};
use prometheus::{
    GaugeVec, HistogramOpts, HistogramVec, IntCounterVec, IntGauge, IntGaugeVec, Opts, Registry,
};
use std::time::Duration;

const KINDS: [TaskKind; 3] = [
    TaskKind::HtmlRebuild,
    TaskKind::Retention,
    TaskKind::PublishDue,
];
const TERMINAL: [TaskStatus; 4] = [
    TaskStatus::Completed,
    TaskStatus::Failed,
    TaskStatus::Interrupted,
    TaskStatus::Cancelled,
];

pub struct TaskHealth {
    pub kind: TaskKind,
    pub queued: i64,
    pub running: i64,
    pub expired: i64,
    pub due_wait_seconds: f64,
    pub last_success_timestamp: i64,
    pub consecutive_failures: i64,
    pub schedule_enabled: bool,
    pub schedule_interval_seconds: i64,
    pub schedule_next_run_timestamp: i64,
}

#[derive(Clone, Copy)]
pub enum TaskSchedulerCheck {
    Success,
    Error,
    Unavailable,
}

#[derive(Clone)]
pub(super) struct TaskMetrics {
    started: IntCounterVec,
    finished: IntCounterVec,
    queue_duration: HistogramVec,
    execution_duration: HistogramVec,
    runs: IntGaugeVec,
    due_wait: GaugeVec,
    last_success: IntGaugeVec,
    failures: IntGaugeVec,
    schedule_enabled: IntGaugeVec,
    schedule_interval: IntGaugeVec,
    schedule_next_run: IntGaugeVec,
    expirations: IntCounterVec,
    scheduler_available: IntGauge,
    scheduler_checks: IntCounterVec,
    scheduler_last_success: IntGauge,
    snapshot_last_success: IntGauge,
}

impl TaskMetrics {
    pub(super) fn new(registry: &Registry) -> Self {
        let started = IntCounterVec::new(
            Opts::new(
                "blog_task_started_total",
                "Durable task claims executed by this process",
            ),
            &["kind"],
        )
        .unwrap();
        let finished = IntCounterVec::new(
            Opts::new(
                "blog_task_finished_total",
                "Durable task terminal transitions acknowledged by this process",
            ),
            &["kind", "status"],
        )
        .unwrap();
        let queue_duration = HistogramVec::new(HistogramOpts::new("blog_task_queue_duration_seconds", "Time from task becoming due to durable claim, excluding intentional future scheduling").buckets(vec![0.1, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 300.0, 3600.0]), &["kind"]).unwrap();
        let execution_duration = HistogramVec::new(HistogramOpts::new("blog_task_execution_duration_seconds", "Local execution time through acknowledged terminal write, excluding expired-lease recovery and cancellation").buckets(vec![0.1, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 300.0, 3600.0]), &["kind", "status"]).unwrap();
        let runs = IntGaugeVec::new(Opts::new("blog_task_runs", "Cached durable queued, running and expired lease counts; expired is a subset of running"), &["kind", "status"]).unwrap();
        let due_wait = GaugeVec::new(
            Opts::new(
                "blog_task_due_wait_seconds",
                "Cached lateness of the oldest due queued task; zero if none",
            ),
            &["kind"],
        )
        .unwrap();
        let last_success = IntGaugeVec::new(Opts::new("blog_task_last_success_timestamp_seconds", "Cached latest retained durable successful completion timestamp; zero if none retained"), &["kind"]).unwrap();
        let failures = IntGaugeVec::new(Opts::new("blog_task_consecutive_failures", "Cached latest failed or interrupted completion streak, ignoring cancellations, capped by retained history"), &["kind"]).unwrap();
        let schedule_enabled = IntGaugeVec::new(
            Opts::new(
                "blog_task_schedule_enabled",
                "Cached periodic schedule enabled flag; zero for manual-only kinds",
            ),
            &["kind"],
        )
        .unwrap();
        let schedule_interval = IntGaugeVec::new(
            Opts::new(
                "blog_task_schedule_interval_seconds",
                "Cached periodic schedule interval; zero for manual-only kinds",
            ),
            &["kind"],
        )
        .unwrap();
        let schedule_next_run = IntGaugeVec::new(
            Opts::new(
                "blog_task_schedule_next_run_timestamp_seconds",
                "Cached next periodic schedule due timestamp; zero when disabled or absent",
            ),
            &["kind"],
        )
        .unwrap();
        let expirations = IntCounterVec::new(
            Opts::new(
                "blog_task_lease_expirations_total",
                "Expired task leases durably recovered by this process",
            ),
            &["kind"],
        )
        .unwrap();
        let scheduler_available = IntGauge::new("blog_task_scheduler_available", "Latest supervisor check: one on success, zero on error or shutdown, minus one before activation").unwrap();
        scheduler_available.set(-1);
        let scheduler_checks = IntCounterVec::new(
            Opts::new(
                "blog_task_scheduler_checks_total",
                "Supervisor checks in this process",
            ),
            &["result"],
        )
        .unwrap();
        let scheduler_last_success = IntGauge::new(
            "blog_task_scheduler_last_success_timestamp_seconds",
            "Latest successful writable supervisor check in this process; zero before success",
        )
        .unwrap();
        let snapshot_last_success = IntGauge::new("blog_task_health_snapshot_last_success_timestamp_seconds", "Latest successful durable task health sample in this process; zero before first sample").unwrap();
        for kind in KINDS {
            let label = kind.as_str();
            started.with_label_values(&[label]);
            queue_duration.with_label_values(&[label]);
            expirations.with_label_values(&[label]);
            for status in TERMINAL {
                finished.with_label_values(&[label, status.as_str()]);
                if status != TaskStatus::Cancelled {
                    execution_duration.with_label_values(&[label, status.as_str()]);
                }
            }
            for state in ["queued", "running", "expired"] {
                runs.with_label_values(&[label, state]);
            }
            due_wait.with_label_values(&[label]);
            for metric in [
                &last_success,
                &failures,
                &schedule_enabled,
                &schedule_interval,
                &schedule_next_run,
            ] {
                metric.with_label_values(&[label]);
            }
        }
        for result in ["success", "error", "unavailable"] {
            scheduler_checks.with_label_values(&[result]);
        }
        for metric in [
            Box::new(started.clone()) as Box<dyn prometheus::core::Collector>,
            Box::new(finished.clone()),
            Box::new(queue_duration.clone()),
            Box::new(execution_duration.clone()),
            Box::new(runs.clone()),
            Box::new(due_wait.clone()),
            Box::new(last_success.clone()),
            Box::new(failures.clone()),
            Box::new(schedule_enabled.clone()),
            Box::new(schedule_interval.clone()),
            Box::new(schedule_next_run.clone()),
            Box::new(expirations.clone()),
            Box::new(scheduler_available.clone()),
            Box::new(scheduler_checks.clone()),
            Box::new(scheduler_last_success.clone()),
            Box::new(snapshot_last_success.clone()),
        ] {
            registry
                .register(metric)
                .expect("unique static task metric names");
        }
        Self {
            started,
            finished,
            queue_duration,
            execution_duration,
            runs,
            due_wait,
            last_success,
            failures,
            schedule_enabled,
            schedule_interval,
            schedule_next_run,
            expirations,
            scheduler_available,
            scheduler_checks,
            scheduler_last_success,
            snapshot_last_success,
        }
    }

    pub(super) fn started(&self, kind: TaskKind, wait: Duration) {
        self.started.with_label_values(&[kind.as_str()]).inc();
        self.queue_duration
            .with_label_values(&[kind.as_str()])
            .observe(wait.as_secs_f64());
    }

    pub(super) fn finished(&self, kind: TaskKind, status: TaskStatus, elapsed: Option<Duration>) {
        if !status.is_terminal() {
            return;
        }
        self.finished
            .with_label_values(&[kind.as_str(), status.as_str()])
            .inc();
        if status != TaskStatus::Cancelled
            && let Some(elapsed) = elapsed
        {
            self.execution_duration
                .with_label_values(&[kind.as_str(), status.as_str()])
                .observe(elapsed.as_secs_f64());
        }
    }

    pub(super) fn lease_expirations(&self, kind: TaskKind, count: u64) {
        self.expirations
            .with_label_values(&[kind.as_str()])
            .inc_by(count);
        self.finished
            .with_label_values(&[kind.as_str(), TaskStatus::Interrupted.as_str()])
            .inc_by(count);
    }

    pub(super) fn health(&self, health: TaskHealth) {
        let kind = health.kind.as_str();
        for (status, value) in [
            ("queued", health.queued),
            ("running", health.running),
            ("expired", health.expired),
        ] {
            self.runs.with_label_values(&[kind, status]).set(value);
        }
        self.due_wait
            .with_label_values(&[kind])
            .set(health.due_wait_seconds);
        self.last_success
            .with_label_values(&[kind])
            .set(health.last_success_timestamp);
        self.failures
            .with_label_values(&[kind])
            .set(health.consecutive_failures);
        self.schedule_enabled
            .with_label_values(&[kind])
            .set(i64::from(health.schedule_enabled));
        self.schedule_interval
            .with_label_values(&[kind])
            .set(health.schedule_interval_seconds);
        self.schedule_next_run
            .with_label_values(&[kind])
            .set(health.schedule_next_run_timestamp);
    }

    pub(super) fn scheduler_check(&self, result: TaskSchedulerCheck, timestamp: i64) {
        let (label, available) = match result {
            TaskSchedulerCheck::Success => ("success", true),
            TaskSchedulerCheck::Error => ("error", false),
            TaskSchedulerCheck::Unavailable => ("unavailable", false),
        };
        self.scheduler_available.set(i64::from(available));
        self.scheduler_checks.with_label_values(&[label]).inc();
        if available {
            self.scheduler_last_success.set(timestamp);
        }
    }

    pub(super) fn scheduler_stopped(&self) {
        self.scheduler_available.set(0);
    }
    pub(super) fn snapshot_success(&self, timestamp: i64) {
        self.snapshot_last_success.set(timestamp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observability::{BuildInfo, Telemetry};

    fn telemetry() -> Telemetry {
        Telemetry::new(&BuildInfo {
            version: "test",
            revision: "test",
        })
    }

    fn sample(metrics: &Telemetry, name: &str) -> f64 {
        metrics
            .encode()
            .unwrap()
            .lines()
            .find_map(|line| {
                let (key, value) = line.rsplit_once(' ')?;
                (key == name).then(|| value.parse().unwrap())
            })
            .unwrap_or_else(|| panic!("missing {name}"))
    }

    #[test]
    fn acknowledgements_have_bounded_labels_and_recovery_has_no_local_duration() {
        let metrics = telemetry();
        metrics.task_started(TaskKind::HtmlRebuild, Duration::from_secs(4));
        metrics.task_finished(
            TaskKind::HtmlRebuild,
            TaskStatus::Completed,
            Some(Duration::from_secs(7)),
        );
        metrics.task_finished(
            TaskKind::HtmlRebuild,
            TaskStatus::Running,
            Some(Duration::from_secs(9)),
        );
        metrics.task_finished(
            TaskKind::Retention,
            TaskStatus::Cancelled,
            Some(Duration::from_secs(9)),
        );
        metrics.task_lease_expirations(TaskKind::PublishDue, 2);
        assert_eq!(
            sample(&metrics, "blog_task_started_total{kind=\"html_rebuild\"}"),
            1.0
        );
        assert_eq!(
            sample(
                &metrics,
                "blog_task_queue_duration_seconds_sum{kind=\"html_rebuild\"}"
            ),
            4.0
        );
        assert_eq!(
            sample(
                &metrics,
                "blog_task_execution_duration_seconds_sum{kind=\"html_rebuild\",status=\"completed\"}"
            ),
            7.0
        );
        assert_eq!(
            sample(
                &metrics,
                "blog_task_finished_total{kind=\"publish_due\",status=\"interrupted\"}"
            ),
            2.0
        );
        assert_eq!(
            sample(
                &metrics,
                "blog_task_lease_expirations_total{kind=\"publish_due\"}"
            ),
            2.0
        );
        assert_eq!(
            sample(
                &metrics,
                "blog_task_execution_duration_seconds_count{kind=\"publish_due\",status=\"interrupted\"}"
            ),
            0.0
        );
        let encoded = metrics.encode().unwrap();
        assert!(!encoded.contains("status=\"cancelled\"} 9"));
        assert!(!encoded.contains(
            "blog_task_execution_duration_seconds_count{kind=\"retention\",status=\"cancelled\"}"
        ));
        assert!(
            !encoded.contains("blog_task_finished_total{kind=\"html_rebuild\",status=\"running\"}")
        );
        assert_eq!(
            encoded
                .lines()
                .filter(|line| line.starts_with("blog_task_finished_total{"))
                .count(),
            12
        );
    }

    #[test]
    fn persistent_health_is_cached_while_process_health_and_counters_reset() {
        let metrics = telemetry();
        assert_eq!(sample(&metrics, "blog_task_scheduler_available"), -1.0);
        metrics.task_health(TaskHealth {
            kind: TaskKind::Retention,
            queued: 1,
            running: 0,
            expired: 0,
            due_wait_seconds: 12.5,
            last_success_timestamp: 123,
            consecutive_failures: 3,
            schedule_enabled: true,
            schedule_interval_seconds: 3600,
            schedule_next_run_timestamp: 456,
        });
        metrics.task_health_snapshot_success(500);
        metrics.task_scheduler_check(TaskSchedulerCheck::Success, 501);
        metrics.task_scheduler_check(TaskSchedulerCheck::Error, 502);
        assert_eq!(sample(&metrics, "blog_task_scheduler_available"), 0.0);
        assert_eq!(
            sample(
                &metrics,
                "blog_task_scheduler_last_success_timestamp_seconds"
            ),
            501.0
        );
        assert_eq!(
            sample(
                &metrics,
                "blog_task_health_snapshot_last_success_timestamp_seconds"
            ),
            500.0
        );
        assert_eq!(
            sample(
                &metrics,
                "blog_task_last_success_timestamp_seconds{kind=\"retention\"}"
            ),
            123.0
        );
        assert_eq!(
            sample(
                &metrics,
                "blog_task_consecutive_failures{kind=\"retention\"}"
            ),
            3.0
        );
        assert_eq!(
            sample(&metrics, "blog_task_due_wait_seconds{kind=\"retention\"}"),
            12.5
        );
        let restarted = telemetry();
        assert_eq!(
            sample(
                &restarted,
                "blog_task_health_snapshot_last_success_timestamp_seconds"
            ),
            0.0
        );
        assert_eq!(
            sample(
                &restarted,
                "blog_task_scheduler_last_success_timestamp_seconds"
            ),
            0.0
        );
        metrics.task_scheduler_check(TaskSchedulerCheck::Success, 503);
        metrics.task_scheduler_stopped();
        assert_eq!(sample(&metrics, "blog_task_scheduler_available"), 0.0);
    }
}
