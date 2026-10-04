use std::sync::{Arc, Mutex};

use application::{
    UseCaseError,
    audit::AuditContext,
    identity::{Actor, ActorChannel},
    ports::Clock,
    tasks::{
        TaskAdmin, TaskFilter, TaskKind, TaskListQuery, TaskRun, TaskRunPage, TaskSchedule,
        TaskScheduleInput, TaskStartInput, TaskStatus, TaskTrigger, TaskView, TasksInteractor,
        parse_time,
    },
};
use async_trait::async_trait;
use domain::identity::{PermissionSet, UserId};
use time::{Duration, OffsetDateTime, UtcOffset, format_description::well_known::Rfc3339};
use uuid::Uuid;

struct FixedClock;
impl Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        parse_time("2026-10-01T00:00:00Z").unwrap()
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Call {
    View,
    Enqueue(AuditContext),
    Retry(Uuid, AuditContext),
    Cancel(Uuid, AuditContext),
    Schedule(AuditContext),
}
#[derive(Default)]
struct Admin(Mutex<Vec<Call>>);
fn run() -> TaskRun {
    TaskRun {
        id: Uuid::from_u128(42),
        kind: TaskKind::HtmlRebuild,
        status: TaskStatus::Queued,
        trigger: TaskTrigger::Manual,
        run_at: FixedClock.now(),
        created_at: FixedClock.now(),
        started_at: None,
        finished_at: None,
        retry_of: None,
        report: Default::default(),
        can_retry: false,
        can_cancel: true,
    }
}
#[async_trait]
impl TaskAdmin for Admin {
    async fn view(&self, _: TaskListQuery) -> Result<TaskView, UseCaseError> {
        self.0.lock().unwrap().push(Call::View);
        Ok(TaskView {
            available: false,
            retention_available: false,
            pending_html: None,
            schedules: vec![],
            latest: vec![run()],
            runs: TaskRunPage::default(),
        })
    }
    async fn enqueue(
        &self,
        _: TaskStartInput,
        audit: AuditContext,
    ) -> Result<TaskRun, UseCaseError> {
        self.0.lock().unwrap().push(Call::Enqueue(audit));
        Ok(run())
    }
    async fn retry(&self, id: Uuid, audit: AuditContext) -> Result<TaskRun, UseCaseError> {
        self.0.lock().unwrap().push(Call::Retry(id, audit));
        Ok(run())
    }
    async fn cancel(&self, id: Uuid, audit: AuditContext) -> Result<TaskRun, UseCaseError> {
        self.0.lock().unwrap().push(Call::Cancel(id, audit));
        Ok(run())
    }
    async fn save_retention_schedule(
        &self,
        input: TaskScheduleInput,
        audit: AuditContext,
    ) -> Result<TaskSchedule, UseCaseError> {
        self.0.lock().unwrap().push(Call::Schedule(audit));
        Ok(TaskSchedule {
            kind: TaskKind::Retention,
            enabled: input.enabled,
            interval_seconds: input.interval_seconds,
            next_run_at: input.resolve_next_run_at(FixedClock.now())?,
            version: input.version + 1,
        })
    }
}
fn actor(permission: bool, channel: ActorChannel) -> Actor {
    Actor::new(
        UserId(Uuid::from_u128(17)),
        channel,
        PermissionSet::from_keys(if permission {
            vec!["settings.manage"]
        } else {
            vec!["post.update_any"]
        }),
    )
    .with_audit_ip(Some("2001:db8::17".parse().unwrap()))
}
fn schedule() -> TaskScheduleInput {
    TaskScheduleInput {
        enabled: true,
        interval_seconds: 86400,
        next_run_at: None,
        version: 0,
    }
}
fn start() -> TaskStartInput {
    TaskStartInput {
        kind: TaskKind::HtmlRebuild,
        run_at: None,
    }
}

#[tokio::test]
async fn authorization_precedes_validation_and_all_port_calls() {
    let port = Arc::new(Admin::default());
    let admin = TasksInteractor::new(port.clone(), Arc::new(FixedClock));
    let untrusted = actor(false, ActorChannel::Session);
    assert!(matches!(
        admin
            .view(
                &untrusted,
                TaskListQuery {
                    limit: Some(0),
                    ..Default::default()
                }
            )
            .await,
        Err(UseCaseError::Forbidden)
    ));
    assert!(matches!(
        admin
            .enqueue(
                &untrusted,
                TaskStartInput {
                    kind: TaskKind::Retention,
                    run_at: Some("invalid".into())
                }
            )
            .await,
        Err(UseCaseError::Forbidden)
    ));
    assert!(matches!(
        admin.retry(&untrusted, Uuid::nil()).await,
        Err(UseCaseError::Forbidden)
    ));
    assert!(matches!(
        admin.cancel(&untrusted, Uuid::nil()).await,
        Err(UseCaseError::Forbidden)
    ));
    assert!(matches!(
        admin
            .save_retention_schedule(
                &untrusted,
                TaskScheduleInput {
                    interval_seconds: 0,
                    ..schedule()
                }
            )
            .await,
        Err(UseCaseError::Forbidden)
    ));
    assert!(port.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn settings_manager_reads_unavailable_state_and_writes_with_trusted_actor_ip() {
    let port = Arc::new(Admin::default());
    let admin = TasksInteractor::new(port.clone(), Arc::new(FixedClock));
    let actor = actor(true, ActorChannel::Session);
    let view = admin.view(&actor, Default::default()).await.unwrap();
    assert!(!view.available);
    assert_eq!(view.latest[0].id, Uuid::from_u128(42));
    admin.enqueue(&actor, start()).await.unwrap();
    admin.retry(&actor, Uuid::from_u128(1)).await.unwrap();
    admin.cancel(&actor, Uuid::from_u128(2)).await.unwrap();
    admin
        .save_retention_schedule(&actor, schedule())
        .await
        .unwrap();
    assert_eq!(
        *port.0.lock().unwrap(),
        [
            Call::View,
            Call::Enqueue(actor.audit_context()),
            Call::Retry(Uuid::from_u128(1), actor.audit_context()),
            Call::Cancel(Uuid::from_u128(2), actor.audit_context()),
            Call::Schedule(actor.audit_context())
        ]
    );
}

#[tokio::test]
async fn controlled_cli_bootstrap_keeps_system_audit_identity() {
    let port = Arc::new(Admin::default());
    let admin = TasksInteractor::new(port.clone(), Arc::new(FixedClock));
    admin
        .enqueue(&Actor::bootstrap_cli(), start())
        .await
        .unwrap();
    assert_eq!(
        *port.0.lock().unwrap(),
        [Call::Enqueue(AuditContext::system())]
    );
}

#[test]
fn scheduling_validates_kind_future_window_and_normalizes_iso_offsets() {
    let now = FixedClock.now();
    assert_eq!(start().resolve_run_at(now).unwrap(), now);
    for kind in [TaskKind::Retention, TaskKind::PublishDue] {
        assert!(
            TaskStartInput {
                kind,
                run_at: Some("2026-10-02T00:00:00Z".into())
            }
            .resolve_run_at(now)
            .is_err()
        );
    }
    for value in [
        "invalid",
        "2026-10-01T00:00:00",
        "2026-09-30T23:59:59Z",
        "2026-10-01T00:00:00Z",
        "2027-10-01T00:00:01Z",
    ] {
        assert!(
            TaskStartInput {
                kind: TaskKind::HtmlRebuild,
                run_at: Some(value.into())
            }
            .resolve_run_at(now)
            .is_err(),
            "{value}"
        );
    }
    let boundary = TaskStartInput {
        kind: TaskKind::HtmlRebuild,
        run_at: Some((now + Duration::days(365)).format(&Rfc3339).unwrap()),
    };
    assert_eq!(
        boundary.resolve_run_at(now).unwrap(),
        now + Duration::days(365)
    );
    let offset = TaskStartInput {
        kind: TaskKind::HtmlRebuild,
        run_at: Some("2026-10-01T09:00:00+08:00".into()),
    }
    .resolve_run_at(now)
    .unwrap();
    assert_eq!(offset.offset(), UtcOffset::UTC);
    assert_eq!(offset, now + Duration::hours(1));
}

#[test]
fn retention_schedule_defaults_future_time_and_rejects_invalid_interval_version() {
    let now = FixedClock.now();
    assert_eq!(
        schedule().resolve_next_run_at(now).unwrap(),
        Some(now + Duration::days(1))
    );
    assert_eq!(
        TaskScheduleInput {
            enabled: false,
            ..schedule()
        }
        .resolve_next_run_at(now)
        .unwrap(),
        None
    );
    for seconds in [3599, 2_592_001] {
        assert!(
            TaskScheduleInput {
                interval_seconds: seconds,
                ..schedule()
            }
            .resolve_next_run_at(now)
            .is_err()
        );
    }
    for seconds in [3600, 2_592_000] {
        assert!(
            TaskScheduleInput {
                interval_seconds: seconds,
                ..schedule()
            }
            .resolve_next_run_at(now)
            .is_ok()
        );
    }
    assert!(
        TaskScheduleInput {
            version: -1,
            ..schedule()
        }
        .resolve_next_run_at(now)
        .is_err()
    );
    assert!(
        TaskScheduleInput {
            next_run_at: Some("2026-10-01T00:00:00Z".into()),
            ..schedule()
        }
        .resolve_next_run_at(now)
        .is_err()
    );
}

#[test]
fn history_filter_validates_bounds_and_stable_timestamp_uuid_cursor() {
    assert_eq!(
        TaskFilter::try_from(TaskListQuery::default())
            .unwrap()
            .limit,
        20
    );
    for limit in [0, 101] {
        assert!(
            TaskFilter::try_from(TaskListQuery {
                limit: Some(limit),
                ..Default::default()
            })
            .is_err()
        );
    }
    for cursor in [
        "garbage".into(),
        "2026-10-01T00:00:00Z|invalid".into(),
        "a".repeat(101),
    ] {
        assert!(
            TaskFilter::try_from(TaskListQuery {
                cursor: Some(cursor),
                ..Default::default()
            })
            .is_err()
        );
    }
    let filter = TaskFilter::try_from(TaskListQuery {
        kind: Some(TaskKind::Retention),
        cursor: Some(format!("2026-10-01T08:00:00+08:00|{}", Uuid::from_u128(4))),
        limit: Some(100),
    })
    .unwrap();
    assert_eq!(filter.before, Some((FixedClock.now(), Uuid::from_u128(4))));
    assert_eq!(filter.kind, Some(TaskKind::Retention));
}

#[tokio::test]
async fn invalid_authorized_input_never_reaches_runtime() {
    let port = Arc::new(Admin::default());
    let admin = TasksInteractor::new(port.clone(), Arc::new(FixedClock));
    let actor = actor(true, ActorChannel::Session);
    assert!(
        admin
            .view(
                &actor,
                TaskListQuery {
                    limit: Some(101),
                    ..Default::default()
                }
            )
            .await
            .is_err()
    );
    assert!(
        admin
            .enqueue(
                &actor,
                TaskStartInput {
                    kind: TaskKind::HtmlRebuild,
                    run_at: Some("yesterday".into())
                }
            )
            .await
            .is_err()
    );
    assert!(
        admin
            .save_retention_schedule(
                &actor,
                TaskScheduleInput {
                    interval_seconds: 0,
                    ..schedule()
                }
            )
            .await
            .is_err()
    );
    assert!(port.0.lock().unwrap().is_empty());
}
