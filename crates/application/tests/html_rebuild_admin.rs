use std::sync::{Arc, Mutex};

use application::{
    UseCaseError,
    audit::AuditContext,
    html_rebuild::{RebuildCounts, RebuildReport},
    html_rebuild_admin::{
        HtmlRebuildAdminInteractor, HtmlRebuildJob, HtmlRebuildJobStatus, HtmlRebuildJobs,
        HtmlRebuildView,
    },
    identity::{Actor, ActorChannel},
};
use async_trait::async_trait;
use domain::identity::{PermissionSet, UserId};
use uuid::Uuid;

#[derive(Debug, PartialEq, Eq)]
enum Call {
    View,
    Start(AuditContext),
}

struct Jobs {
    calls: Mutex<Vec<Call>>,
    view: HtmlRebuildView,
    fail: bool,
}

impl Jobs {
    fn new(fail: bool) -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(vec![]),
            view: HtmlRebuildView {
                pending: None,
                job: Some(HtmlRebuildJob {
                    id: Uuid::from_u128(42),
                    status: HtmlRebuildJobStatus::Running,
                    report: RebuildReport {
                        rebuilt: RebuildCounts {
                            posts: 7,
                            ..Default::default()
                        },
                        has_more: true,
                        ..Default::default()
                    },
                }),
                available: false,
            },
            fail,
        })
    }
}

#[async_trait]
impl HtmlRebuildJobs for Jobs {
    async fn view(&self) -> Result<HtmlRebuildView, UseCaseError> {
        self.calls.lock().unwrap().push(Call::View);
        if self.fail {
            Err(UseCaseError::Repository("job read unavailable".into()))
        } else {
            Ok(self.view.clone())
        }
    }

    async fn start(&self, audit: AuditContext) -> Result<HtmlRebuildJob, UseCaseError> {
        self.calls.lock().unwrap().push(Call::Start(audit));
        if self.fail {
            Err(UseCaseError::Invalid("恢复隔离期间禁止 HTML 重建".into()))
        } else {
            Ok(self.view.job.clone().unwrap())
        }
    }
}

fn actor(permissions: &[&str]) -> Actor {
    Actor::new(
        UserId(Uuid::from_u128(17)),
        ActorChannel::Session,
        PermissionSet::from_keys(permissions.iter().copied()),
    )
    .with_audit_ip(Some("2001:db8::17".parse().unwrap()))
}

#[tokio::test]
async fn authorization_precedes_any_job_reads_or_start() {
    let jobs = Jobs::new(true);
    let admin = HtmlRebuildAdminInteractor::new(jobs.clone());
    for permissions in [
        &[][..],
        &["oauth.manage"][..],
        &["post.update_any", "page.update", "audit.read"][..],
    ] {
        let actor = actor(permissions);
        assert!(matches!(
            admin.view(&actor).await,
            Err(UseCaseError::Forbidden)
        ));
        assert!(matches!(
            admin.start(&actor).await,
            Err(UseCaseError::Forbidden)
        ));
    }
    assert!(jobs.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn delegated_settings_permission_reads_the_exact_job_snapshot() {
    let jobs = Jobs::new(false);
    let admin = HtmlRebuildAdminInteractor::new(jobs.clone());
    let view = admin.view(&actor(&["settings.manage"])).await.unwrap();
    assert_eq!(view.pending, None);
    assert!(!view.available);
    let job = view.job.unwrap();
    assert_eq!(job.id, Uuid::from_u128(42));
    assert_eq!(job.status, HtmlRebuildJobStatus::Running);
    assert_eq!(job.report.rebuilt.posts, 7);
    assert_eq!(job.report.pending, None);
    assert_eq!(*jobs.calls.lock().unwrap(), [Call::View]);
}

#[tokio::test]
async fn start_uses_the_trusted_actor_and_address_as_audit_context() {
    let jobs = Jobs::new(false);
    let admin = HtmlRebuildAdminInteractor::new(jobs.clone());
    let actor = actor(&["settings.manage"]);
    let job = admin.start(&actor).await.unwrap();
    assert_eq!(job.id, Uuid::from_u128(42));
    assert_eq!(job.report.rebuilt.posts, 7);
    assert_eq!(
        *jobs.calls.lock().unwrap(),
        [Call::Start(AuditContext {
            actor_id: Some(actor.user_id.0),
            ip_address: Some("2001:db8::17".parse().unwrap()),
        })]
    );
}

#[tokio::test]
async fn authorized_job_port_errors_are_preserved() {
    let jobs = Jobs::new(true);
    let admin = HtmlRebuildAdminInteractor::new(jobs.clone());
    let actor = actor(&["settings.manage"]);
    assert!(matches!(
        admin.view(&actor).await,
        Err(UseCaseError::Repository(_))
    ));
    assert!(matches!(
        admin.start(&actor).await,
        Err(UseCaseError::Invalid(_))
    ));
    assert_eq!(
        *jobs.calls.lock().unwrap(),
        [Call::View, Call::Start(actor.audit_context())]
    );
}
