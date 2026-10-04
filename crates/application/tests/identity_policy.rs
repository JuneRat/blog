use application::UseCaseError;
use application::identity::policy::{
    StatusChangePlan, ensure_admin_removal_allowed, plan_status_change,
};
use domain::identity::{PermissionSet, User, UserSnapshot, UserStatus};
use time::OffsetDateTime;

fn user() -> UserSnapshot {
    User::new("target", None, None, OffsetDateTime::UNIX_EPOCH)
        .unwrap()
        .snapshot()
}
fn permissions(keys: &[&str]) -> PermissionSet {
    PermissionSet::from_keys(keys.iter().copied())
}

#[test]
fn owner_permission_and_account_permission_are_both_required_even_for_noops() {
    let target = user();
    for keys in [vec![], vec!["admin.manage"], vec!["user.manage"]] {
        for desired in [UserStatus::Active, UserStatus::Disabled] {
            for version in [target.version, target.version + 1] {
                assert!(matches!(
                    plan_status_change(&permissions(&keys), &target, true, desired, version),
                    Err(UseCaseError::Forbidden)
                ));
            }
        }
    }
}

#[test]
fn matching_status_is_idempotent_only_after_version_check() {
    let target = user();
    let manager = permissions(&["user.manage", "admin.manage"]);
    assert!(matches!(
        plan_status_change(&manager, &target, true, target.status, target.version + 1),
        Err(UseCaseError::VersionConflict)
    ));
    assert_eq!(
        plan_status_change(&manager, &target, true, target.status, target.version).unwrap(),
        StatusChangePlan::Unchanged
    );
}

#[test]
fn only_disabling_an_owner_needs_a_global_owner_check() {
    let mut target = user();
    let manager = permissions(&["user.manage", "admin.manage"]);
    for is_owner in [false, true] {
        assert_eq!(
            plan_status_change(
                &manager,
                &target,
                is_owner,
                UserStatus::Disabled,
                target.version
            )
            .unwrap(),
            StatusChangePlan::Update {
                requires_admin_check: is_owner
            }
        );
    }
    target.status = UserStatus::Disabled;
    assert_eq!(
        plan_status_change(&manager, &target, true, UserStatus::Active, target.version).unwrap(),
        StatusChangePlan::Update {
            requires_admin_check: false
        }
    );
    assert_eq!(
        plan_status_change(
            &manager,
            &target,
            true,
            UserStatus::Disabled,
            target.version
        )
        .unwrap(),
        StatusChangePlan::Unchanged
    );
}

#[test]
fn status_changes_cannot_restore_deleted_accounts() {
    let mut target = user();
    target.deleted_at = Some(OffsetDateTime::UNIX_EPOCH);
    let manager = permissions(&["user.manage", "admin.manage"]);
    for desired in [UserStatus::Active, UserStatus::Disabled] {
        assert!(matches!(
            plan_status_change(&manager, &target, false, desired, target.version),
            Err(UseCaseError::NotFound(_))
        ));
    }
}

#[test]
fn owner_count_includes_the_target_and_missing_counts_fail_closed() {
    for count in [0, 1] {
        assert!(matches!(
            ensure_admin_removal_allowed(count),
            Err(UseCaseError::LastAdminProtected)
        ));
    }
    for count in [2, 3, 100] {
        assert!(ensure_admin_removal_allowed(count).is_ok());
    }
}
