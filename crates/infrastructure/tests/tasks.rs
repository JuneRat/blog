//! Persistent maintenance queue and execution-fence PostgreSQL regressions.
mod common;

use application::{
    UseCaseError,
    audit::AuditContext,
    html_rebuild::{HtmlKind, HtmlRebuildStore},
    publishing::{PublicationResult, ScheduledPublicationStore},
    retention::RetentionCleanupStore,
    tasks::{
        TaskExecutionStore, TaskFilter, TaskKind, TaskLease, TaskListQuery, TaskReport,
        TaskScheduleInput, TaskStatus, TaskStore, TaskTrigger,
    },
};
use infrastructure::{
    PostgresHtmlRebuildStore, PostgresScheduledPublicationStore, PostgresTaskStore,
    RenderingRuntime,
    retention::PostgresRetentionCleanupStore,
    tasks::{guard_execution, supports_retention_execution},
};
use sqlx::PgPool;
use std::{future::Future, sync::Arc};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

async fn isolated<F, R>(scenario: F)
where
    F: FnOnce(PgPool) -> R + Send + 'static,
    R: Future<Output = ()> + Send + 'static,
{
    let name = format!("blog_tasks_{}", Uuid::now_v7().simple());
    let pool = common::fresh_database(&name).await;
    let result = tokio::spawn(scenario(pool.clone())).await;
    pool.close().await;
    let admin = common::connect(&common::admin_url()).await.unwrap();
    sqlx::raw_sql(&format!("DROP DATABASE {name} WITH (FORCE)"))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    if let Err(error) = result {
        std::panic::resume_unwind(error.into_panic());
    }
}
fn store(pool: &PgPool) -> PostgresTaskStore {
    PostgresTaskStore::new(common::database(pool.clone()))
}
async fn now(pool: &PgPool) -> OffsetDateTime {
    sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(pool)
        .await
        .unwrap()
}
async fn enqueue(
    pool: &PgPool,
    kind: TaskKind,
    audit: AuditContext,
) -> application::tasks::TaskRun {
    store(pool)
        .enqueue(kind, now(pool).await, TaskTrigger::Manual, None, audit)
        .await
        .unwrap()
}
async fn lease(pool: &PgPool, kind: TaskKind, audit: AuditContext) -> TaskLease {
    enqueue(pool, kind, audit).await;
    store(pool)
        .claim(&[kind], Uuid::now_v7(), 180)
        .await
        .unwrap()
        .unwrap()
}
async fn expire(pool: &PgPool, id: Uuid) {
    sqlx::query(
        "UPDATE task_runs SET lease_expires_at=clock_timestamp()-interval '1 second' WHERE id=$1",
    )
    .bind(id)
    .execute(pool)
    .await
    .unwrap();
}
fn schedule(enabled: bool, version: i64) -> TaskScheduleInput {
    TaskScheduleInput {
        enabled,
        interval_seconds: 3600,
        next_run_at: None,
        version,
    }
}

#[tokio::test]
async fn health_snapshot_is_read_only_and_uses_completion_order_for_failure_streaks() {
    isolated(|pool| async move {
        let store = store(&pool);
        let empty = store.health_snapshot().await.unwrap();
        assert_eq!(empty.kinds.len(), 3);
        assert!(empty.kinds.iter().all(|kind| kind.queued == 0 && kind.running == 0 && !kind.schedule_enabled));
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM task_schedules").fetch_one(&pool).await.unwrap(), 0, "sampling never seeds schedules");
        store.seed_schedules().await.unwrap();
        let reference = now(&pool).await;
        // Creation order deliberately differs from completion order, as with a
        // future plan that executes after tasks created more recently.
        for (status, created, finished) in [("failed", 1, 50), ("completed", 70, 40), ("failed", 60, 30), ("cancelled", 20, 20), ("interrupted", 80, 10)] {
            sqlx::query("INSERT INTO task_runs(id,kind,status,trigger,run_at,created_at,finished_at) VALUES($1,'html_rebuild',$2,'manual',$3,$4,$5)")
                .bind(Uuid::now_v7()).bind(status).bind(reference-Duration::seconds(created))
                .bind(reference-Duration::seconds(created)).bind(reference-Duration::seconds(finished))
                .execute(&pool).await.unwrap();
        }
        let queued = enqueue(&pool, TaskKind::HtmlRebuild, AuditContext::system()).await;
        sqlx::query("UPDATE task_runs SET created_at=$2,run_at=$3 WHERE id=$1").bind(queued.id)
            .bind(reference-Duration::seconds(300)).bind(reference-Duration::seconds(15)).execute(&pool).await.unwrap();
        let running = lease(&pool, TaskKind::Retention, AuditContext::system()).await;
        expire(&pool, running.run.id).await;
        let snapshot = store.health_snapshot().await.unwrap();
        assert!(snapshot.observed_at >= reference.unix_timestamp());
        let html = snapshot.kinds.iter().find(|kind| kind.kind == TaskKind::HtmlRebuild).unwrap();
        assert_eq!(html.queued, 1);
        assert_eq!(html.consecutive_failures, 2, "cancel does not hide the latest failed and interrupted tasks");
        assert_eq!(html.last_success_timestamp, (reference-Duration::seconds(40)).unix_timestamp());
        assert!((15.0..25.0).contains(&html.due_wait_seconds), "intentional wait before due time must be excluded");
        let retention = snapshot.kinds.iter().find(|kind| kind.kind == TaskKind::Retention).unwrap();
        assert_eq!((retention.running,retention.expired), (1,1));
        assert!(!retention.schedule_enabled);
        assert_eq!(retention.schedule_next_run_timestamp, 0);
        let publishing = snapshot.kinds.iter().find(|kind| kind.kind == TaskKind::PublishDue).unwrap();
        assert!(publishing.schedule_enabled);
        assert_eq!(publishing.schedule_interval_seconds, 30);
        assert!(publishing.schedule_next_run_timestamp > 0);
        assert_eq!(sqlx::query_scalar::<_,String>("SELECT status FROM task_runs WHERE id=$1").bind(running.run.id).fetch_one(&pool).await.unwrap(), "running", "sampling never recovers leases");
        assert_eq!(store.recover_expired_by_kind().await.unwrap(), vec![(TaskKind::Retention,1)]);
        assert!(store.recover_expired_by_kind().await.unwrap().is_empty());
        let snapshot = store.health_snapshot().await.unwrap();
        let retention = snapshot.kinds.iter().find(|kind| kind.kind == TaskKind::Retention).unwrap();
        assert_eq!((retention.running,retention.expired,retention.consecutive_failures), (0,0,1));
    }).await;
}

#[tokio::test]
async fn concurrent_enqueue_and_claim_are_atomic_with_single_source_audit() {
    isolated(|pool| async move {
        let actor = common::seed_user(&pool, "tasks-actor").await;
        let audit = AuditContext {
            actor_id: Some(actor),
            ip_address: Some("2001:db8::17".parse().unwrap()),
            ..Default::default()
        };
        let store = Arc::new(store(&pool));
        let due = now(&pool).await;
        let mut requests = tokio::task::JoinSet::new();
        for _ in 0..16 {
            let store = store.clone();
            let audit = audit.clone();
            requests.spawn(async move {
                store
                    .enqueue(TaskKind::HtmlRebuild, due, TaskTrigger::Manual, None, audit)
                    .await
                    .unwrap()
                    .id
            });
        }
        let first = requests.join_next().await.unwrap().unwrap();
        while let Some(result) = requests.join_next().await {
            assert_eq!(result.unwrap(), first);
        }
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM task_runs")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM audit_logs WHERE action='task.enqueue'"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            1
        );
        for _ in 0..16 {
            let store = store.clone();
            requests.spawn(async move {
                store
                    .claim(&[TaskKind::HtmlRebuild], Uuid::now_v7(), 180)
                    .await
                    .unwrap()
                    .map(|lease| lease.run.id)
                    .unwrap_or(Uuid::nil())
            });
        }
        let mut claimed = 0;
        while let Some(result) = requests.join_next().await {
            let id = result.unwrap();
            if !id.is_nil() {
                assert_eq!(id, first);
                claimed += 1;
            }
        }
        assert_eq!(claimed, 1);
        let sources: Vec<(Option<Uuid>, Option<String>)> = sqlx::query_as(
            "SELECT actor_id,host(ip_address) FROM audit_logs WHERE action LIKE 'task.%'",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(sources, vec![(Some(actor), Some("2001:db8::17".into())); 2]);
        assert!(
            store
                .claim(&[TaskKind::Retention], Uuid::now_v7(), 180)
                .await
                .unwrap()
                .is_none()
        );
    })
    .await;
}

#[tokio::test]
async fn expired_read_is_pure_and_retry_preserves_history_and_rejects_old_lease() {
    isolated(|pool| async move {
        let store = store(&pool);
        let original = lease(&pool, TaskKind::HtmlRebuild, AuditContext::system()).await;
        let report = TaskReport {
            html: Some(application::html_rebuild::RebuildReport {
                rebuilt: application::html_rebuild::RebuildCounts {
                    posts: 3,
                    ..Default::default()
                },
                has_more: true,
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(store.progress(&original, &report).await.unwrap());
        let mut wrong = original.clone();
        wrong.token = Uuid::now_v7();
        assert!(!store.renew(&wrong, 180).await.unwrap());
        assert!(
            !store
                .progress(&wrong, &TaskReport::default())
                .await
                .unwrap()
        );
        assert!(
            !store
                .finish(&wrong, TaskStatus::Completed, &TaskReport::default())
                .await
                .unwrap()
        );
        expire(&pool, original.run.id).await;
        let shown = store.get(original.run.id).await.unwrap().unwrap();
        assert_eq!(shown.status, TaskStatus::Interrupted);
        assert!(!shown.can_retry);
        assert!(!shown.can_cancel);
        assert_eq!(shown.report.html.unwrap().rebuilt.posts, 3);
        assert!(shown.report.error.is_some());
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT status FROM task_runs")
                .fetch_one(&pool)
                .await
                .unwrap(),
            "running"
        );
        assert!(
            matches!(
                store.retry(original.run.id, AuditContext::system()).await,
                Err(UseCaseError::Invalid(_))
            ),
            "raw expired-running still needs recovery before retry"
        );
        assert!(!store.renew(&original, 180).await.unwrap());
        assert!(
            !store
                .progress(&original, &TaskReport::default())
                .await
                .unwrap()
        );
        assert!(
            !store
                .finish(&original, TaskStatus::Completed, &TaskReport::default())
                .await
                .unwrap()
        );
        assert_eq!(store.recover_expired().await.unwrap(), 1);
        assert_eq!(store.recover_expired().await.unwrap(), 0);
        assert!(store.get(original.run.id).await.unwrap().unwrap().can_retry);
        let retry = store
            .retry(original.run.id, AuditContext::system())
            .await
            .unwrap();
        assert_ne!(retry.id, original.run.id);
        assert_eq!(retry.retry_of, Some(original.run.id));
        assert_eq!(retry.trigger, TaskTrigger::Retry);
        assert!(!store.get(original.run.id).await.unwrap().unwrap().can_retry);
        assert!(matches!(
            store.retry(original.run.id, AuditContext::system()).await,
            Err(UseCaseError::Conflict(
                application::error::ConflictKind::Unknown
            ))
        ));
        let claimed = store
            .claim(&[TaskKind::HtmlRebuild], Uuid::now_v7(), 180)
            .await
            .unwrap()
            .unwrap();
        assert_ne!(claimed.token, original.token);
        assert!(
            !store
                .finish(&original, TaskStatus::Completed, &TaskReport::default())
                .await
                .unwrap()
        );
        assert!(
            store
                .finish(&claimed, TaskStatus::Completed, &report)
                .await
                .unwrap()
        );
        assert_eq!(
            store.get(retry.id).await.unwrap().unwrap().status,
            TaskStatus::Completed
        );
        assert!(
            !store
                .finish(&claimed, TaskStatus::Failed, &report)
                .await
                .unwrap()
        );
    })
    .await;
}

#[tokio::test]
async fn schedules_are_read_only_by_default_versioned_and_coalesce_missed_ticks() {
    isolated(|pool|async move {
        let name:String=sqlx::query_scalar("SELECT current_database()").fetch_one(&pool).await.unwrap();
        let readonly=common::connect(&format!("{}?options=-c%20default_transaction_read_only%3Don",common::test_db_url(&common::admin_url(),&name))).await.unwrap();
        let defaults=store(&readonly).schedules().await.unwrap();
        assert_eq!(defaults.len(),2);assert!(defaults.iter().all(|schedule|schedule.version==0));
        assert!(defaults.iter().find(|schedule|schedule.kind==TaskKind::PublishDue).unwrap().enabled);
        assert!(!defaults.iter().find(|schedule|schedule.kind==TaskKind::Retention).unwrap().enabled);
        assert!(store(&readonly).list(TaskFilter::try_from(TaskListQuery::default()).unwrap()).await.unwrap().items.is_empty());
        assert!(store(&readonly).latest().await.unwrap().is_empty());
        readonly.close().await;
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM task_schedules").fetch_one(&pool).await.unwrap(),0);
        let store=store(&pool);
        let saved=store.save_retention_schedule(schedule(true,0),AuditContext::system()).await.unwrap();
        assert_eq!(saved.version,1);assert!(saved.next_run_at.unwrap()>now(&pool).await);
        assert!(matches!(store.save_retention_schedule(schedule(false,0),AuditContext::system()).await,Err(UseCaseError::VersionConflict)));
        store.seed_schedules().await.unwrap();store.seed_schedules().await.unwrap();
        assert!(store.schedules().await.unwrap().iter().find(|s|s.kind==TaskKind::Retention).unwrap().enabled,"seed cannot overwrite configured policy");
        sqlx::query("UPDATE task_schedules SET next_run_at=clock_timestamp()-interval '1 week' WHERE enabled").execute(&pool).await.unwrap();
        store.tick_schedules().await.unwrap();
        let periodic=store.latest().await.unwrap();assert_eq!(periodic.len(),2);
        for run in &periodic {assert_eq!(run.trigger,TaskTrigger::Periodic);assert!(!run.can_cancel);assert!(store.cancel(run.id,AuditContext::system()).await.is_err());}
        let checked_at = now(&pool).await;
        assert!(store.schedules().await.unwrap().iter().all(|s|s.next_run_at.unwrap()>checked_at));
        sqlx::query("UPDATE task_schedules SET next_run_at=clock_timestamp()-interval '1 week' WHERE enabled").execute(&pool).await.unwrap();
        store.tick_schedules().await.unwrap();
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM task_runs").fetch_one(&pool).await.unwrap(),2,"missed cycles never queue a backlog");
        let disabled=store.save_retention_schedule(schedule(false,1),AuditContext::system()).await.unwrap();
        assert_eq!(disabled.version,2);assert_eq!(disabled.next_run_at,None);
        assert!(sqlx::query("UPDATE task_schedules SET interval_seconds=31 WHERE kind='publish_due'").execute(&pool).await.is_err());
    }).await;
}

#[tokio::test]
async fn future_plans_survive_history_pagination_and_terminal_history_is_bounded() {
    isolated(|pool|async move {
        let store=store(&pool);
        let planned=store.enqueue(TaskKind::HtmlRebuild,now(&pool).await+Duration::days(7),TaskTrigger::Once,None,AuditContext::system()).await.unwrap();
        assert!(planned.can_cancel);
        assert!(store.claim(&[TaskKind::HtmlRebuild],Uuid::now_v7(),180).await.unwrap().is_none());
        // Enough frequent publisher records to push the plan outside a default page.
        sqlx::query("INSERT INTO task_runs(id,kind,status,trigger,run_at,created_at,finished_at) SELECT gen_random_uuid(),'publish_due','completed','periodic',clock_timestamp(),clock_timestamp()+n*interval '1 millisecond',clock_timestamp() FROM generate_series(1,505) n")
            .execute(&pool).await.unwrap();
        let page=store.list(TaskFilter::try_from(TaskListQuery::default()).unwrap()).await.unwrap();
        assert_eq!(page.items.len(),20);assert!(page.items.iter().all(|run|run.kind==TaskKind::PublishDue));
        let cursor=page.next_cursor.unwrap();
        let second=store.list(TaskFilter::try_from(TaskListQuery{cursor:Some(cursor),..Default::default()}).unwrap()).await.unwrap();
        assert!(second.items.iter().all(|run|!page.items.iter().any(|prior|prior.id==run.id)));
        assert_eq!(store.latest().await.unwrap().iter().find(|run|run.kind==TaskKind::HtmlRebuild).unwrap().id,planned.id);
        let active=lease(&pool,TaskKind::PublishDue,AuditContext::system()).await;
        assert!(store.finish(&active,TaskStatus::Completed,&TaskReport::default()).await.unwrap());
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM task_runs WHERE kind='publish_due'").fetch_one(&pool).await.unwrap(),500);
        assert_eq!(store.get(planned.id).await.unwrap().unwrap().status,TaskStatus::Queued);
        let cancelled=store.cancel(planned.id,AuditContext::system()).await.unwrap();
        assert_eq!(cancelled.status,TaskStatus::Cancelled);assert!(!cancelled.can_retry);assert!(!cancelled.can_cancel);
        assert!(matches!(store.cancel(planned.id,AuditContext::system()).await,
            Err(UseCaseError::Conflict(application::error::ConflictKind::Unknown))));
    }).await;
}

#[tokio::test]
async fn task_writes_and_business_execution_fail_closed_in_recovery_isolation() {
    isolated(|pool| async move {
        let store = store(&pool);
        let running = lease(&pool, TaskKind::PublishDue, AuditContext::system()).await;
        let queued = enqueue(&pool, TaskKind::HtmlRebuild, AuditContext::system()).await;
        let failed = lease(&pool, TaskKind::Retention, AuditContext::system()).await;
        store
            .finish(&failed, TaskStatus::Failed, &TaskReport::default())
            .await
            .unwrap();
        let name: String = sqlx::query_scalar("SELECT current_database()")
            .fetch_one(&pool)
            .await
            .unwrap();
        sqlx::raw_sql(&format!(
            "COMMENT ON DATABASE {name} IS 'blog:recovery-isolated:tasks-test'"
        ))
        .execute(&pool)
        .await
        .unwrap();
        assert!(store.seed_schedules().await.is_err());
        assert!(store.tick_schedules().await.is_err());
        assert!(
            store
                .claim(&[TaskKind::HtmlRebuild], Uuid::now_v7(), 180)
                .await
                .is_err()
        );
        assert!(store.renew(&running, 180).await.is_err());
        assert!(
            store
                .progress(&running, &TaskReport::default())
                .await
                .is_err()
        );
        assert!(
            store
                .finish(&running, TaskStatus::Interrupted, &TaskReport::default())
                .await
                .is_err()
        );
        assert!(store.recover_expired().await.is_err());
        assert!(
            store
                .enqueue(
                    TaskKind::Retention,
                    now(&pool).await,
                    TaskTrigger::Manual,
                    None,
                    AuditContext::system()
                )
                .await
                .is_err()
        );
        assert!(
            store
                .retry(failed.run.id, AuditContext::system())
                .await
                .is_err()
        );
        assert!(
            store
                .cancel(queued.id, AuditContext::system())
                .await
                .is_err()
        );
        assert!(
            store
                .save_retention_schedule(schedule(true, 0), AuditContext::system())
                .await
                .is_err()
        );
        let mut tx = pool.begin().await.unwrap();
        assert!(guard_execution(&mut tx, &running).await.is_err());
        tx.rollback().await.unwrap();
        assert!(store.get(queued.id).await.unwrap().is_some());
        assert_eq!(store.latest().await.unwrap().len(), 3);
        assert_eq!(store.schedules().await.unwrap().len(), 2);
        assert_eq!(
            store
                .list(TaskFilter::try_from(TaskListQuery::default()).unwrap())
                .await
                .unwrap()
                .items
                .len(),
            3
        );
        assert!(
            PostgresScheduledPublicationStore::new(common::database(pool.clone()))
                .publish_batch(now(&pool).await, 100)
                .await
                .is_err()
        );
        assert!(
            PostgresRetentionCleanupStore::new(common::database(pool.clone()))
                .cleanup_batch(100, false)
                .await
                .is_err()
        );
        assert!(
            PostgresRetentionCleanupStore::new(common::database(pool.clone()))
                .cleanup_batch(100, true)
                .await
                .is_ok(),
            "CLI dry-run remains read only"
        );
    })
    .await;
}

#[tokio::test]
async fn audit_failure_rolls_back_enqueue_cancel_and_finish() {
    isolated(|pool|async move {
        let store=store(&pool);
        sqlx::raw_sql("CREATE FUNCTION reject_tasks_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action IN ('task.enqueue','task.cancel','task.finish') THEN RAISE EXCEPTION 'test audit unavailable'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_tasks_audit BEFORE INSERT ON audit_logs FOR EACH ROW EXECUTE FUNCTION reject_tasks_audit()")
            .execute(&pool).await.unwrap();
        assert!(store.enqueue(TaskKind::HtmlRebuild,now(&pool).await,TaskTrigger::Manual,None,AuditContext::system()).await.is_err());
        assert!(store.latest().await.unwrap().is_empty());
        sqlx::query("ALTER TABLE audit_logs DISABLE TRIGGER reject_tasks_audit").execute(&pool).await.unwrap();
        let queued=enqueue(&pool,TaskKind::HtmlRebuild,AuditContext::system()).await;
        let running=lease(&pool,TaskKind::Retention,AuditContext::system()).await;
        sqlx::query("ALTER TABLE audit_logs ENABLE TRIGGER reject_tasks_audit").execute(&pool).await.unwrap();
        assert!(store.cancel(queued.id,AuditContext::system()).await.is_err());
        assert_eq!(store.get(queued.id).await.unwrap().unwrap().status,TaskStatus::Queued);
        assert!(store.finish(&running,TaskStatus::Completed,&TaskReport::default()).await.is_err());
        assert_eq!(store.get(running.run.id).await.unwrap().unwrap().status,TaskStatus::Running);
        assert!(store.renew(&running,180).await.unwrap(),"audit failure cannot drop live ownership");
    }).await;
}

#[tokio::test]
async fn lease_is_checked_after_row_lock_wait_before_business_or_report_writes() {
    isolated(|pool| async move {
        let store = Arc::new(store(&pool));
        enqueue(&pool, TaskKind::HtmlRebuild, AuditContext::system()).await;
        let lease = store
            .claim(&[TaskKind::HtmlRebuild], Uuid::now_v7(), 1)
            .await
            .unwrap()
            .unwrap();
        let mut held = pool.begin().await.unwrap();
        sqlx::query("SELECT id FROM task_runs WHERE id=$1 FOR UPDATE")
            .bind(lease.run.id)
            .fetch_one(&mut *held)
            .await
            .unwrap();
        let guard_pool = pool.clone();
        let guard_lease = lease.clone();
        let guarded = tokio::spawn(async move {
            let mut tx = guard_pool.begin().await.unwrap();
            let result = guard_execution(&mut tx, &guard_lease).await;
            tx.rollback().await.unwrap();
            result
        });
        let renewal_store = store.clone();
        let renewal_lease = lease.clone();
        let renewed =
            tokio::spawn(async move { renewal_store.renew(&renewal_lease, 180).await.unwrap() });
        for _ in 0..200 {
            let expired: bool = sqlx::query_scalar(
                "SELECT lease_expires_at<=clock_timestamp() FROM task_runs WHERE id=$1",
            )
            .bind(lease.run.id)
            .fetch_one(&pool)
            .await
            .unwrap();
            if expired {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(!guarded.is_finished());
        assert!(!renewed.is_finished());
        held.commit().await.unwrap();
        assert!(matches!(
            guarded.await.unwrap(),
            Err(UseCaseError::Invalid(_))
        ));
        assert!(!renewed.await.unwrap());
        assert!(
            !store
                .progress(&lease, &TaskReport::default())
                .await
                .unwrap()
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT status FROM task_runs")
                .fetch_one(&pool)
                .await
                .unwrap(),
            "running"
        );
    })
    .await;
}

#[tokio::test]
async fn business_adapters_fence_stale_workers_and_preserve_trusted_audit_source() {
    isolated(|pool|async move {
        let actor=common::seed_user(&pool,"task-business-owner").await;
        let audit=AuditContext{actor_id:Some(actor),ip_address:Some("192.0.2.17".parse().unwrap()), ..Default::default()};
        let post=Uuid::now_v7();
        sqlx::query("INSERT INTO posts(id,author_id,slug,title,content,content_html,content_render_version,status,published_at) VALUES($1,$2,'task-fence','Task fence','**source**','stale',$3,'scheduled',clock_timestamp()-interval '1 minute')")
            .bind(post).bind(actor).bind(infrastructure::CONTENT_RENDER_VERSION+1).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO comments(id,post_id,author_name,content,content_html,content_render_version,ip_address,created_at) VALUES(gen_random_uuid(),$1,'Reader','body','body',$2,'192.0.2.99',clock_timestamp()-interval '400 days')")
            .bind(post).bind(infrastructure::COMMENT_RENDER_VERSION).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO audit_logs(id,action,target_type,target_id,created_at) VALUES(gen_random_uuid(),'old_fixture','system','fixture',clock_timestamp()-interval '400 days')").execute(&pool).await.unwrap();
        let store=store(&pool);
        for kind in [TaskKind::HtmlRebuild,TaskKind::Retention,TaskKind::PublishDue] {
            let lease=lease(&pool,kind,audit.clone()).await;
            let mut wrong=lease.clone();wrong.token=Uuid::now_v7();
            match kind {
                TaskKind::HtmlRebuild=>{
                    let runtime=Arc::new(RenderingRuntime::default());
                    let adapter=PostgresHtmlRebuildStore::new(common::database(pool.clone()),runtime.clone(),runtime.clone()).with_task_lease(wrong);
                    assert!(adapter.rebuild_batch(HtmlKind::Post,None,100).await.is_err());
                    assert_eq!(sqlx::query_scalar::<_,String>("SELECT content_html FROM posts WHERE id=$1").bind(post).fetch_one(&pool).await.unwrap(),"stale");
                    assert_eq!(PostgresHtmlRebuildStore::new(common::database(pool.clone()),runtime.clone(),runtime).with_task_lease(lease.clone()).rebuild_batch(HtmlKind::Post,None,100).await.unwrap().rebuilt,1);
                }
                TaskKind::Retention=>{
                    assert!(PostgresRetentionCleanupStore::new(common::database(pool.clone())).with_task_lease(wrong).cleanup_batch(100,false).await.is_err());
                    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM comments WHERE ip_address IS NOT NULL").fetch_one(&pool).await.unwrap(),1);
                    let cleaned=PostgresRetentionCleanupStore::new(common::database(pool.clone())).with_task_lease(lease.clone()).cleanup_batch(100,false).await.unwrap();
                    assert_eq!((cleaned.comment_ips,cleaned.audit_logs),(1,1));
                }
                TaskKind::PublishDue=>{
                    assert!(PostgresScheduledPublicationStore::new(common::database(pool.clone())).with_task_lease(wrong).publish_batch(now(&pool).await,100).await.is_err());
                    assert_eq!(sqlx::query_scalar::<_,String>("SELECT status FROM posts WHERE id=$1").bind(post).fetch_one(&pool).await.unwrap(),"scheduled");
                    assert_eq!(PostgresScheduledPublicationStore::new(common::database(pool.clone())).with_task_lease(lease.clone()).publish_batch(now(&pool).await,100).await.unwrap(),1);
                }
            }
            assert!(store.finish(&lease,TaskStatus::Completed,&TaskReport::default()).await.unwrap());
            let mut tx=pool.begin().await.unwrap();assert!(guard_execution(&mut tx,&lease).await.is_err());tx.rollback().await.unwrap();
        }
        let business:Vec<(String,Option<Uuid>,Option<String>)>=sqlx::query_as("SELECT action,actor_id,host(ip_address) FROM audit_logs WHERE action IN ('post.html.rebuild','maintenance.retention','post.publish_due') ORDER BY action").fetch_all(&pool).await.unwrap();
        assert_eq!(business.len(),3);
        for (_,source,ip) in business {assert_eq!(source,Some(actor));assert_eq!(ip.as_deref(),Some("192.0.2.17"));}
        assert!(supports_retention_execution(&common::database(pool.clone())).await.unwrap());
        // Finished reports can carry all three stable typed summaries without raw backend errors.
        let publication=lease(&pool,TaskKind::PublishDue,audit).await;
        let report=TaskReport{publication:Some(PublicationResult{published:1,batches:1,has_more:false}),..Default::default()};
        assert!(store.finish(&publication,TaskStatus::Completed,&report).await.unwrap());
        assert_eq!(store.get(publication.run.id).await.unwrap().unwrap().report.publication.unwrap().published,1);
    }).await;
}

#[tokio::test]
async fn existing_app_and_maintenance_permissions_are_detected_without_broadening_grants() {
    isolated(|pool| async move {
        let app_role=format!("blog_tasks_app_{}",Uuid::now_v7().simple());
        let maintenance_role=format!("blog_tasks_maint_{}",Uuid::now_v7().simple());
        // Roles are cluster scoped, so clean them even when a scenario assertion panics.
        sqlx::raw_sql(&format!("CREATE ROLE {app_role}; CREATE ROLE {maintenance_role}; GRANT USAGE ON SCHEMA public TO {app_role},{maintenance_role};
            GRANT SELECT ON settings TO {app_role},{maintenance_role};
            GRANT SELECT,UPDATE ON comments TO {app_role}; GRANT SELECT(id,created_at,ip_address),UPDATE(ip_address) ON comments TO {maintenance_role};
            GRANT SELECT,INSERT ON audit_logs TO {app_role}; GRANT SELECT(id,created_at),INSERT,DELETE ON audit_logs TO {maintenance_role};
            GRANT SELECT,INSERT,UPDATE,DELETE ON task_runs TO {app_role}; GRANT SELECT,UPDATE ON task_runs TO {maintenance_role};
            GRANT SELECT,INSERT,UPDATE ON task_schedules TO {app_role}; GRANT SELECT ON task_schedules TO {maintenance_role}"))
            .execute(&pool).await.unwrap();
        let fixture_pool=pool.clone();let app=app_role.clone();let maintenance=maintenance_role.clone();
        let result=tokio::spawn(async move {
            let name:String=sqlx::query_scalar("SELECT current_database()").fetch_one(&fixture_pool).await.unwrap();
            let dsn=common::test_db_url(&common::admin_url(),&name);
            let app_pool=common::connect(&format!("{dsn}?options=-c%20role%3D{app}")).await.unwrap();
            let maintenance_pool=common::connect(&format!("{dsn}?options=-c%20role%3D{maintenance}")).await.unwrap();
            assert!(!supports_retention_execution(&common::database(app_pool.clone())).await.unwrap(),"application role lacks audit DELETE");
            assert!(supports_retention_execution(&common::database(maintenance_pool.clone())).await.unwrap());
            let task=lease(&fixture_pool,TaskKind::Retention,AuditContext::system()).await;
            // Fencing works with the narrow maintenance grant; no audit UPDATE is necessary.
            assert!(PostgresRetentionCleanupStore::new(common::database(maintenance_pool.clone())).with_task_lease(task.clone()).cleanup_batch(100,false).await.is_ok());
            assert!(sqlx::query("UPDATE audit_logs SET action='forbidden'").execute(&maintenance_pool).await.is_err());
            assert!(sqlx::query("DELETE FROM audit_logs").execute(&app_pool).await.is_err());
            assert!(sqlx::query("DELETE FROM task_runs").execute(&maintenance_pool).await.is_err());
            assert!(sqlx::query("UPDATE task_schedules SET enabled=false").execute(&maintenance_pool).await.is_err());
            let mut tx=maintenance_pool.begin().await.unwrap();
            assert!(guard_execution(&mut tx,&task).await.is_ok());tx.rollback().await.unwrap();
            assert!(store(&app_pool).finish(&task,TaskStatus::Completed,&TaskReport::default()).await.unwrap());
            app_pool.close().await;maintenance_pool.close().await;
        }).await;
        sqlx::raw_sql(&format!("DROP OWNED BY {app_role},{maintenance_role}; DROP ROLE {app_role}; DROP ROLE {maintenance_role}"))
            .execute(&pool).await.unwrap();
        if let Err(error)=result { std::panic::resume_unwind(error.into_panic()); }
    }).await;
}

#[tokio::test]
async fn lease_from_another_database_cannot_authorize_progress_or_business_writes() {
    isolated(|source| async move {
        let foreign = lease(&source, TaskKind::Retention, AuditContext::system()).await;
        isolated(move |target| async move {
            let mut tx = target.begin().await.unwrap();
            assert!(matches!(
                guard_execution(&mut tx, &foreign).await,
                Err(UseCaseError::Invalid(_))
            ));
            tx.rollback().await.unwrap();
            let store = store(&target);
            assert!(!store.renew(&foreign, 180).await.unwrap());
            assert!(
                !store
                    .progress(&foreign, &TaskReport::default())
                    .await
                    .unwrap()
            );
            assert!(
                !store
                    .finish(&foreign, TaskStatus::Completed, &TaskReport::default())
                    .await
                    .unwrap()
            );
            assert!(
                PostgresRetentionCleanupStore::new(common::database(target.clone()))
                    .with_task_lease(foreign)
                    .cleanup_batch(100, false)
                    .await
                    .is_err()
            );
            assert!(store.latest().await.unwrap().is_empty());
        })
        .await;
    })
    .await;
}
