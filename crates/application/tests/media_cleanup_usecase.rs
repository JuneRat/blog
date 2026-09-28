use application::{UseCaseError, media_cleanup::*, ports::Clock};
use async_trait::async_trait;
use std::{
    collections::BTreeSet,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use time::OffsetDateTime;
use uuid::Uuid;

struct Fake {
    plan: PurgePlan,
    committed: AtomicBool,
    uncertain_commit: AtomicBool,
    bad_file: AtomicBool,
    refuse_delete: AtomicBool,
    fail_unlink: Mutex<BTreeSet<Uuid>>,
    removed: Mutex<BTreeSet<Uuid>>,
    events: Mutex<Vec<&'static str>>,
}
impl Fake {
    fn fixture() -> (Arc<Self>, MediaCleanup) {
        let fake = Arc::new(Self {
            plan: PurgePlan {
                format: PLAN_FORMAT,
                operation_id: Uuid::now_v7(),
                database: DatabaseIdentity {
                    name: "test".into(),
                    oid: "42".into(),
                    endpoint: DatabaseEndpoint::Tcp {
                        host: "localhost".into(),
                        port: "5432".into(),
                    },
                },
                media_root: "/media".into(),
                created_at: None,
                items: (0..2)
                    .map(|i| PurgeItem {
                        id: Uuid::now_v7(),
                        path: format!("objects/{i}.png"),
                        size: 4,
                        sha256: "0".repeat(64),
                        version: 2,
                        deleted_at: Some("2026-01-01T00:00:00.000000Z".into()),
                    })
                    .collect(),
            },
            committed: AtomicBool::new(false),
            uncertain_commit: AtomicBool::new(false),
            bad_file: AtomicBool::new(false),
            refuse_delete: AtomicBool::new(false),
            fail_unlink: Mutex::new(BTreeSet::new()),
            removed: Mutex::new(BTreeSet::new()),
            events: Mutex::new(vec![]),
        });
        let service = MediaCleanup::new(fake.clone(), fake.clone(), fake.clone(), fake.clone());
        (fake, service)
    }
    fn event(&self, event: &'static str) {
        self.events.lock().unwrap().push(event);
    }
}
fn failure() -> UseCaseError {
    UseCaseError::Repository("injected failure".into())
}
impl Clock for Fake {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH
    }
}
#[async_trait]
impl MediaPurgePlans for Fake {
    async fn load(&self, _: &Path) -> Result<VerifiedPlan, UseCaseError> {
        self.event("load");
        Ok(VerifiedPlan {
            plan: self.plan.clone(),
            sha256: "verified".into(),
        })
    }
    async fn save(&self, _: &Path, _: &PurgePlan) -> Result<(), UseCaseError> {
        self.event("save");
        Ok(())
    }
}
#[async_trait]
impl MediaPurgeStore for Fake {
    async fn identity(&self) -> Result<DatabaseIdentity, UseCaseError> {
        Ok(self.plan.database.clone())
    }
    async fn candidates(&self, ids: &[Uuid]) -> Result<Vec<PurgeItem>, UseCaseError> {
        Ok(self
            .plan
            .items
            .iter()
            .filter(|i| ids.contains(&i.id))
            .cloned()
            .collect())
    }
    async fn has_receipt(&self, _: &VerifiedPlan, _: &PurgeItem) -> Result<bool, UseCaseError> {
        Ok(self.committed.load(Ordering::SeqCst))
    }
    async fn commit(&self, _: &VerifiedPlan) -> Result<(), UseCaseError> {
        self.event("commit");
        self.committed.store(true, Ordering::SeqCst);
        if self.uncertain_commit.swap(false, Ordering::SeqCst) {
            Err(failure())
        } else {
            Ok(())
        }
    }
    async fn may_remove_file(&self, _: &VerifiedPlan, _: &PurgeItem) -> Result<bool, UseCaseError> {
        Ok(self.committed.load(Ordering::SeqCst) && !self.refuse_delete.load(Ordering::SeqCst))
    }
}
#[async_trait]
impl MediaPurgeFiles for Fake {
    async fn root(&self) -> Result<String, UseCaseError> {
        Ok(self.plan.media_root.clone())
    }
    async fn validate(
        &self,
        _: &str,
        item: &PurgeItem,
        missing_ok: bool,
    ) -> Result<(), UseCaseError> {
        self.event("validate");
        if self.bad_file.load(Ordering::SeqCst)
            || (!missing_ok && self.removed.lock().unwrap().contains(&item.id))
        {
            Err(failure())
        } else {
            Ok(())
        }
    }
    async fn remove(&self, _: &str, item: &PurgeItem) -> Result<bool, UseCaseError> {
        self.event("remove");
        if self.fail_unlink.lock().unwrap().contains(&item.id) {
            return Err(failure());
        }
        Ok(self.removed.lock().unwrap().insert(item.id))
    }
}

#[tokio::test]
async fn confirmations_and_all_files_are_checked_before_any_commit() {
    let (fake, service) = Fake::fixture();
    for flags in [(false, true), (true, false), (false, false)] {
        assert!(
            service
                .apply(Path::new("plan"), flags.0, flags.1)
                .await
                .is_err()
        );
    }
    assert!(fake.events.lock().unwrap().is_empty());
    fake.bad_file.store(true, Ordering::SeqCst);
    assert!(service.apply(Path::new("plan"), true, true).await.is_err());
    assert!(!fake.committed.load(Ordering::SeqCst));
    assert!(fake.removed.lock().unwrap().is_empty());
}

#[tokio::test]
async fn uncertain_commit_preserves_files_and_original_plan_resumes() {
    let (fake, service) = Fake::fixture();
    fake.uncertain_commit.store(true, Ordering::SeqCst);
    assert!(service.apply(Path::new("plan"), true, true).await.is_err());
    assert!(fake.committed.load(Ordering::SeqCst));
    assert!(fake.removed.lock().unwrap().is_empty());
    let result = service.apply(Path::new("plan"), true, true).await.unwrap();
    assert_eq!(result.files_deleted, 2);
    assert!(result.failures.is_empty());
}

#[tokio::test]
async fn partial_unlink_failure_and_lost_receipt_are_not_success() {
    let (fake, service) = Fake::fixture();
    let failed = fake.plan.items[1].id;
    fake.fail_unlink.lock().unwrap().insert(failed);
    let result = service.apply(Path::new("plan"), true, true).await.unwrap();
    assert_eq!((result.files_deleted, result.files_already_absent), (1, 0));
    assert_eq!(result.failures[0].id, failed);
    fake.fail_unlink.lock().unwrap().clear();
    fake.refuse_delete.store(true, Ordering::SeqCst);
    let denied = service.apply(Path::new("plan"), true, true).await.unwrap();
    assert_eq!(denied.failures.len(), 2);
    assert_eq!(fake.removed.lock().unwrap().len(), 1);
    fake.refuse_delete.store(false, Ordering::SeqCst);
    let result = service.apply(Path::new("plan"), true, true).await.unwrap();
    assert_eq!((result.files_deleted, result.files_already_absent), (1, 1));
    let result = service.apply(Path::new("plan"), true, true).await.unwrap();
    assert_eq!((result.files_deleted, result.files_already_absent), (0, 2));
}

#[tokio::test]
async fn plan_requires_selected_existing_trashed_objects_and_saves_after_validation() {
    let (fake, service) = Fake::fixture();
    assert!(service.plan(vec![], Path::new("plan")).await.is_err());
    assert!(
        service
            .plan(vec![Uuid::now_v7()], Path::new("plan"))
            .await
            .is_err()
    );
    let id = fake.plan.items[0].id;
    let plan = service.plan(vec![id, id], Path::new("plan")).await.unwrap();
    assert_eq!(plan.items.len(), 1);
    assert_eq!(&*fake.events.lock().unwrap(), &["validate", "save"]);
    let mut invalid = plan.clone();
    invalid.items[0].deleted_at = None;
    assert!(invalid.validate().is_err());
    invalid = plan.clone();
    invalid.items[0].path = "../outside".into();
    assert!(invalid.validate().is_err());
    invalid = plan;
    invalid.items.push(invalid.items[0].clone());
    assert!(invalid.validate().is_err());
}
