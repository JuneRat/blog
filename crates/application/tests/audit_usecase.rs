use application::{
    audit::{AuditFilter, AuditInteractor, AuditPage, AuditQuery, AuditQueryStore},
    error::UseCaseError,
    identity::{Actor, ActorChannel, BUILTIN_ROLES},
};
use domain::identity::{PermissionSet, UserId};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use uuid::Uuid;

struct Store(AtomicUsize);
#[async_trait::async_trait]
impl AuditQueryStore for Store {
    async fn list(&self, _: &AuditFilter) -> Result<AuditPage, UseCaseError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(AuditPage {
            items: vec![],
            next_cursor: None,
        })
    }
}
fn actor(permissions: &[&str]) -> Actor {
    Actor::new(
        UserId(Uuid::nil()),
        ActorChannel::Session,
        PermissionSet::from_keys(permissions.iter().copied()),
    )
}

#[tokio::test]
async fn audit_permission_is_independent_and_checked_before_validation_or_storage() {
    let store = Arc::new(Store(AtomicUsize::new(0)));
    let service = AuditInteractor::new(store.clone());
    assert!(matches!(
        service
            .list(
                &actor(&["settings.manage"]),
                AuditQuery {
                    limit: Some(0),
                    ..Default::default()
                }
            )
            .await,
        Err(UseCaseError::Forbidden)
    ));
    assert!(matches!(
        service
            .list(
                &actor(&["audit.read"]),
                AuditQuery {
                    limit: Some(0),
                    ..Default::default()
                }
            )
            .await,
        Err(UseCaseError::Invalid(_))
    ));
    assert_eq!(store.0.load(Ordering::SeqCst), 0);
    service
        .list(&actor(&["audit.read"]), AuditQuery::default())
        .await
        .unwrap();
    assert_eq!(store.0.load(Ordering::SeqCst), 1);
    for role in BUILTIN_ROLES {
        assert_eq!(
            role.permissions.contains(&"audit.read"),
            role.slug == "admin"
        );
    }
}

#[test]
fn invalid_or_ambiguous_filters_are_rejected_and_cursor_preserves_precision() {
    for query in [
        AuditQuery {
            limit: Some(101),
            ..Default::default()
        },
        AuditQuery {
            actor_id: Some(Uuid::nil()),
            without_actor: true,
            ..Default::default()
        },
        AuditQuery {
            from: Some("2026-09-27T10:00:00".into()),
            ..Default::default()
        },
        AuditQuery {
            from: Some("2026-09-27T10:00:00Z".into()),
            until: Some("2026-09-27T18:00:00+08:00".into()),
            ..Default::default()
        },
        AuditQuery {
            cursor: Some("2026-09-27T10:00:00Z|invalid".into()),
            ..Default::default()
        },
        AuditQuery {
            cursor: Some("x".repeat(101)),
            ..Default::default()
        },
        AuditQuery {
            action: Some("\n".into()),
            ..Default::default()
        },
    ] {
        assert!(AuditFilter::try_from(query).is_err());
    }
    let timestamp = "2026-09-27T10:00:00.123456Z";
    let filter = AuditFilter::try_from(AuditQuery {
        action: Some(" post.update ".into()),
        cursor: Some(format!("{timestamp}|{}", Uuid::nil())),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(filter.limit, 50);
    assert_eq!(filter.action.as_deref(), Some("post.update"));
    let (at, id) = filter.before.unwrap();
    assert_eq!(at.microsecond(), 123456);
    assert_eq!(id, Uuid::nil());
}
