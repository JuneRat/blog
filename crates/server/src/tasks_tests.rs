//! Reproduce worker self-contention and delayed PostgreSQL rollback with real SQL.
use super::*;
use infrastructure::test_support::pool;
use sqlx::PgPool;
use std::future::Future;

#[path = "../tests/common/mod.rs"]
mod common;

async fn isolated<F, Fut>(scenario: F)
where
    F: FnOnce(Database, PgPool, String) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let name = format!("blog_worker_{}", Uuid::now_v7().simple());
    let fixture = common::fresh_database(&name).await;
    let policy = infrastructure::DatabasePoolConfig {
        statement_timeout_ms: 20_000,
        lock_timeout_ms: 250,
        ..Default::default()
    };
    let database = infrastructure::connect_with_config(
        &common::test_db_url(&common::admin_url(), &name),
        &policy,
    )
    .await
    .unwrap();
    let runtime_pool = pool(database.clone());
    let result = tokio::spawn(scenario(database.clone(), runtime_pool, name.clone())).await;
    database.close().await;
    fixture.close().await;
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

async fn slow_cleanup(
    database: &Database,
    pool: &PgPool,
    seconds: u32,
) -> (Arc<TaskRuntime>, TaskLease, Uuid) {
    let old = Uuid::now_v7();
    sqlx::query("INSERT INTO audit_logs(id,action,target_type,target_id,metadata,created_at) VALUES($1,'fixture','task','old','{}',clock_timestamp()-interval '400 days')")
        .bind(old).execute(pool).await.unwrap();
    // Only cleanup deletes this row; enqueue/claim/result append audit entries.
    sqlx::raw_sql(&format!("CREATE FUNCTION slow_cleanup() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_sleep({seconds}); RETURN OLD; END $$; CREATE TRIGGER slow_cleanup BEFORE DELETE ON audit_logs FOR EACH ROW EXECUTE FUNCTION slow_cleanup();"))
        .execute(pool).await.unwrap();
    let runtime = Arc::new(TaskRuntime::new(
        database.clone(),
        Some(database.clone()),
        Arc::new(RenderingRuntime::default()),
        interfaces::observability::Telemetry::new(&crate::observability::build_info()),
        false,
    ));
    runtime
        .store
        .enqueue(
            TaskKind::Retention,
            time::OffsetDateTime::now_utc(),
            TaskTrigger::Manual,
            None,
            AuditContext::system(),
        )
        .await
        .unwrap();
    let lease = runtime
        .store
        .claim(&[TaskKind::Retention], Uuid::now_v7(), LEASE_SECONDS)
        .await
        .unwrap()
        .unwrap();
    (runtime, lease, old)
}

fn metric(runtime: &TaskRuntime, name: &str) -> f64 {
    runtime
        .telemetry
        .encode()
        .unwrap()
        .lines()
        .find_map(|line| {
            let (key, value) = line.rsplit_once(' ')?;
            (key == name).then(|| value.parse().unwrap())
        })
        .unwrap_or_else(|| panic!("missing {name}"))
}

#[tokio::test]
async fn slow_valid_business_survives_its_own_progress_and_heartbeat_lock_timeouts() {
    isolated(|database, pool, _| async move {
        let (runtime, lease, old) = slow_cleanup(&database, &pool, 12).await;
        let (updates, _) = watch::channel(lease.run.report.clone());
        let (_stop, stopping) = watch::channel(false);
        tokio::time::timeout(
            Duration::from_secs(25),
            execute_lease(runtime.clone(), lease.clone(), updates, stopping),
        )
        .await
        .unwrap();
        let run = runtime.store.get(lease.run.id).await.unwrap().unwrap();
        assert_eq!(run.status, TaskStatus::Completed);
        assert_eq!(run.report.retention.unwrap().audit_logs, 1);
        let remaining: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_logs WHERE id=$1")
            .bind(old)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(remaining, 0);
        assert_eq!(metric(&runtime, "blog_task_finished_total{kind=\"retention\",status=\"completed\"}"), 1.0);
        assert_eq!(metric(&runtime, "blog_task_execution_duration_seconds_count{kind=\"retention\",status=\"completed\"}"), 1.0);
        assert_eq!(metric(&runtime, "blog_task_finished_total{kind=\"retention\",status=\"interrupted\"}"), 0.0);
    })
    .await;
}

#[tokio::test]
async fn shutdown_retries_terminal_write_until_cancelled_sql_releases_its_lock() {
    isolated(|database, pool, name| async move {
        let (runtime, lease, old) = slow_cleanup(&database, &pool, 5).await;
        let (updates, _) = watch::channel(lease.run.report.clone());
        let (stop, stopping) = watch::channel(false);
        let job = tokio::spawn(execute_lease(runtime.clone(), lease.clone(), updates, stopping));
        let started = Instant::now();
        loop {
            let sleeping: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=$1 AND wait_event='PgSleep')").bind(&name).fetch_one(&pool).await.unwrap();
            if sleeping { break; }
            assert!(started.elapsed() < Duration::from_secs(5), "business never reached slow SQL");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        stop.send_replace(true);
        tokio::time::timeout(Duration::from_secs(12), job).await.unwrap().unwrap();
        let run = runtime.store.get(lease.run.id).await.unwrap().unwrap();
        assert_eq!(run.status, TaskStatus::Interrupted);
        assert!(run.can_retry);
        let report = run.report.retention.unwrap();
        assert_eq!(report.audit_logs, 0, "cancelled batch must not count as committed");
        assert!(report.has_more);
        let remaining: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_logs WHERE id=$1").bind(old).fetch_one(&pool).await.unwrap();
        assert_eq!(remaining, 1, "cancelled cleanup rolls back before the task finishes");
        assert_eq!(metric(&runtime, "blog_task_finished_total{kind=\"retention\",status=\"interrupted\"}"), 1.0, "terminal write retries do not duplicate metrics");
        assert_eq!(metric(&runtime, "blog_task_execution_duration_seconds_count{kind=\"retention\",status=\"interrupted\"}"), 1.0);
        runtime.recover_expired().await.unwrap();
        assert_eq!(metric(&runtime, "blog_task_lease_expirations_total{kind=\"retention\"}"), 0.0);
    }).await;
}

#[tokio::test]
async fn lost_lease_is_counted_only_by_committed_recovery_and_health_survives_restart() {
    isolated(|database, pool, _| async move {
        let runtime = Arc::new(TaskRuntime::new(database.clone(), Some(database.clone()), Arc::new(RenderingRuntime::default()), interfaces::observability::Telemetry::new(&crate::observability::build_info()), false));
        runtime.store.enqueue(TaskKind::HtmlRebuild, time::OffsetDateTime::now_utc(), TaskTrigger::Manual, None, AuditContext::system()).await.unwrap();
        let lease = runtime.store.claim(&[TaskKind::HtmlRebuild],Uuid::now_v7(),LEASE_SECONDS).await.unwrap().unwrap();
        sqlx::query("UPDATE task_runs SET lease_expires_at=clock_timestamp()-interval '1 second' WHERE id=$1").bind(lease.run.id).execute(&pool).await.unwrap();
        let (updates, _) = watch::channel(lease.run.report.clone());
        let (_stop, stopping) = watch::channel(false);
        tokio::time::timeout(Duration::from_secs(5),execute_lease(runtime.clone(),lease,updates,stopping)).await.unwrap();
        assert_eq!(metric(&runtime, "blog_task_finished_total{kind=\"html_rebuild\",status=\"completed\"}"), 0.0, "false terminal acknowledgement is not a completion");
        runtime.recover_expired().await.unwrap();
        runtime.recover_expired().await.unwrap();
        assert_eq!(metric(&runtime, "blog_task_finished_total{kind=\"html_rebuild\",status=\"interrupted\"}"), 1.0);
        assert_eq!(metric(&runtime, "blog_task_lease_expirations_total{kind=\"html_rebuild\"}"), 1.0);
        assert_eq!(metric(&runtime, "blog_task_execution_duration_seconds_count{kind=\"html_rebuild\",status=\"interrupted\"}"), 0.0, "lease recovery has no local completed duration");
        let restarted = TaskRuntime::new(database.clone(), None, Arc::new(RenderingRuntime::default()), interfaces::observability::Telemetry::new(&crate::observability::build_info()), false);
        restarted.sample_health().await.unwrap();
        assert_eq!(metric(&restarted, "blog_task_finished_total{kind=\"html_rebuild\",status=\"interrupted\"}"), 0.0);
        assert_eq!(metric(&restarted, "blog_task_consecutive_failures{kind=\"html_rebuild\"}"), 1.0, "new process restores durable history without counter backfill");
        assert!(metric(&restarted, "blog_task_health_snapshot_last_success_timestamp_seconds") > 0.0);
        database.close().await;
        assert_eq!(metric(&restarted, "blog_task_consecutive_failures{kind=\"html_rebuild\"}"), 1.0, "scrape reads cached health after the database closes");
    }).await;
}

#[tokio::test]
async fn repeated_cancel_does_not_duplicate_confirmed_terminal_count() {
    isolated(|database, _, _| async move {
        let runtime = Arc::new(TaskRuntime::new(
            database.clone(),
            None,
            Arc::new(RenderingRuntime::default()),
            interfaces::observability::Telemetry::new(&crate::observability::build_info()),
            false,
        ));
        let admin = WebsiteTasks::new(runtime.clone(), Arc::new(TaskSupervisor::default()));
        let run = runtime
            .store
            .enqueue(
                TaskKind::HtmlRebuild,
                time::OffsetDateTime::now_utc() + time::Duration::hours(1),
                TaskTrigger::Once,
                None,
                AuditContext::system(),
            )
            .await
            .unwrap();
        admin.cancel(run.id, AuditContext::system()).await.unwrap();
        assert!(admin.cancel(run.id, AuditContext::system()).await.is_err());
        assert_eq!(
            metric(
                &runtime,
                "blog_task_finished_total{kind=\"html_rebuild\",status=\"cancelled\"}"
            ),
            1.0
        );
        assert_eq!(
            metric(&runtime, "blog_task_started_total{kind=\"html_rebuild\"}"),
            0.0
        );
    })
    .await;
}
