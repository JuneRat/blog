//! 账号状态变更与真实 PostgreSQL 身份锁、会话、审计的一致性。
mod common;

use application::error::UseCaseError;
use application::identity::{Actor, ActorChannel, BUILTIN_ROLES, PERMISSION_REGISTRY};
use application::ports::{
    AccountAdministration, PasswordCredentialStore, RbacStore, SessionStore, UserQuery,
};
use domain::identity::{UserId, UserSnapshot, UserStatus};
use infrastructure::{PostgresRbacStore, PostgresSessionStore, PostgresUserRepository};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn status_change_does_not_deadlock_with_session_eviction_and_late_creation() {
    let _g = SERIAL.lock().await;
    let pool = database().await;
    let (owner, _) = account(&pool, "owner", "owner").await;
    let (_, target) = account(&pool, "author", "author").await;
    let sessions = PostgresSessionStore::with_defaults(pool.clone());
    sessions
        .create(target.id, target.auth_version)
        .await
        .unwrap();

    // 会话创建事务先淘汰旧会话，再插入带 users 外键的新行。
    let mut creation = pool.begin().await.unwrap();
    sqlx::query("DELETE FROM sessions WHERE user_id=$1")
        .bind(target.id)
        .execute(&mut *creation)
        .await
        .unwrap();
    let status_pool = pool.clone();
    let status_target = target.clone();
    let status_change = tokio::spawn(async move {
        change(
            &PostgresUserRepository::new(status_pool),
            &owner,
            &status_target,
            UserStatus::Disabled,
        )
        .await
    });
    // 确认状态事务已锁住用户，正在等待刚被淘汰的会话行。
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE datname=current_database() \
                 AND query='DELETE FROM sessions WHERE user_id=$1' AND wait_event_type='Lock')",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            if waiting {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("状态事务应进入会话删除");
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        sqlx::query("INSERT INTO sessions (token_hash,user_id,csrf_token,auth_version,created_at,last_seen_at,expires_at) \
            VALUES (repeat('a',64),$1,repeat('b',64),$2,now(),now(),now()+interval '1 hour')")
            .bind(target.id).bind(target.auth_version).execute(&mut *creation).await.unwrap();
        creation.commit().await.unwrap();
        status_change.await.unwrap().unwrap();
    }).await.expect("会话外键检查与状态变更不能互相等待");
    // 并发迟到的会话行可以残留，但旧认证快照永远不能通过认证。
    let active_stale: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sessions s JOIN users u ON u.id=s.user_id \
         WHERE u.id=$1 AND u.status='active' AND s.auth_version=u.auth_version",
    )
    .bind(target.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(active_stale, 0);
}

async fn database() -> PgPool {
    let pool = common::fresh_database("blog_user_status_test").await;
    let rbac = PostgresRbacStore::new(pool.clone());
    rbac.sync_permission_registry(PERMISSION_REGISTRY)
        .await
        .unwrap();
    rbac.sync_builtin_roles(BUILTIN_ROLES).await.unwrap();
    pool
}

async fn account(pool: &PgPool, name: &str, role: &str) -> (Actor, UserSnapshot) {
    let id = common::seed_user(pool, name).await;
    let users = PostgresUserRepository::new(pool.clone());
    users
        .set_password_hash(id, "$test-credential-retained", None.into())
        .await
        .unwrap();
    let rbac = PostgresRbacStore::new(pool.clone());
    rbac.assign_role(id, role, None.into()).await.unwrap();
    let actor = Actor::new(
        UserId(id),
        ActorChannel::Session,
        rbac.permissions_of_user(id).await.unwrap(),
    )
    .with_audit_ip(Some("198.51.100.23".parse().unwrap()));
    (actor, users.find_by_id(id).await.unwrap().unwrap())
}

async fn change(
    repo: &PostgresUserRepository,
    actor: &Actor,
    user: &UserSnapshot,
    status: UserStatus,
) -> Result<UserSnapshot, UseCaseError> {
    repo.change_status(
        user.id,
        status,
        user.version,
        OffsetDateTime::now_utc(),
        actor,
    )
    .await
}

#[tokio::test]
async fn status_changes_revoke_sessions_and_never_revive_old_tokens() {
    let _g = SERIAL.lock().await;
    let pool = database().await;
    let (owner, _) = account(&pool, "owner", "owner").await;
    let (_, user) = account(&pool, "author", "author").await;
    let repo = PostgresUserRepository::new(pool.clone());
    let sessions = PostgresSessionStore::with_defaults(pool.clone());
    let cookie = sessions.create(user.id, user.auth_version).await.unwrap();
    let disabled = change(&repo, &owner, &user, UserStatus::Disabled)
        .await
        .unwrap();
    assert_eq!(disabled.version, user.version + 1);
    assert_eq!(disabled.auth_version, user.auth_version + 1);
    assert!(sessions.validate(&cookie).await.unwrap().is_none());
    assert!(
        repo.find_password_credential("author")
            .await
            .unwrap()
            .is_none()
    );
    let late = sessions.create(user.id, user.auth_version).await.unwrap();
    let while_disabled = sessions
        .create(user.id, disabled.auth_version)
        .await
        .unwrap();
    let enabled = change(&repo, &owner, &disabled, UserStatus::Active)
        .await
        .unwrap();
    assert_eq!(enabled.version, disabled.version + 1);
    assert_eq!(enabled.auth_version, disabled.auth_version + 1);
    // 模拟在重新启用之后才持旧快照签发的会话，不能复活。
    let late_after_enable = sessions
        .create(user.id, disabled.auth_version)
        .await
        .unwrap();
    for token in [&cookie, &late, &while_disabled, &late_after_enable] {
        assert!(sessions.validate(token).await.unwrap().is_none());
    }
    let fresh = sessions
        .create(user.id, enabled.auth_version)
        .await
        .unwrap();
    assert!(sessions.validate(&fresh).await.unwrap().is_some());
    assert_eq!(
        repo.find_password_credential("author")
            .await
            .unwrap()
            .unwrap()
            .password_hash,
        "$test-credential-retained"
    );
    assert_eq!(
        PostgresRbacStore::new(pool.clone())
            .roles_of_user(user.id)
            .await
            .unwrap(),
        vec!["author"]
    );
    assert_eq!(
        change(&repo, &owner, &enabled, UserStatus::Active)
            .await
            .unwrap(),
        enabled
    );
    assert!(
        sessions.validate(&fresh).await.unwrap().is_some(),
        "无变化不撤销会话"
    );
    assert!(matches!(
        change(&repo, &owner, &user, UserStatus::Active).await,
        Err(UseCaseError::VersionConflict)
    ));
    let audits: Vec<(Uuid, String, serde_json::Value)> = sqlx::query_as(
        "SELECT actor_id, host(ip_address), metadata FROM audit_logs WHERE action='user.status.update' ORDER BY created_at,id"
    ).fetch_all(&pool).await.unwrap();
    assert_eq!(audits.len(), 2, "无变化不追加审计");
    assert_eq!(
        audits[0],
        (
            owner.user_id.0,
            "198.51.100.23".into(),
            serde_json::json!({
                "from":"active", "to":"disabled", "version":disabled.version, "auth_version":disabled.auth_version
            })
        )
    );
    let rows = repo.list_admin(50, 0).await.unwrap();
    let row = rows.iter().find(|row| row.id == user.id).unwrap();
    assert_eq!(
        (row.status, row.version),
        (UserStatus::Active, enabled.version)
    );
}

#[tokio::test]
async fn owner_and_current_operator_permissions_are_checked_under_the_lock() {
    let _g = SERIAL.lock().await;
    let pool = database().await;
    let (owner, owner_user) = account(&pool, "owner", "owner").await;
    let (admin, admin_user) = account(&pool, "admin", "admin").await;
    let (_, target) = account(&pool, "author", "author").await;
    let repo = PostgresUserRepository::new(pool.clone());
    assert!(matches!(
        change(&repo, &admin, &owner_user, UserStatus::Disabled).await,
        Err(UseCaseError::Forbidden)
    ));
    assert!(matches!(
        change(&repo, &owner, &owner_user, UserStatus::Disabled).await,
        Err(UseCaseError::LastOwnerProtected)
    ));
    let disabled = change(&repo, &admin, &target, UserStatus::Disabled)
        .await
        .unwrap();
    change(&repo, &owner, &admin_user, UserStatus::Disabled)
        .await
        .unwrap();
    // Actor 是停用前读出的；不能依靠它缓存的权限绕过停用。
    assert!(matches!(
        change(&repo, &admin, &disabled, UserStatus::Active).await,
        Err(UseCaseError::Forbidden)
    ));
    let (other_admin, other) = account(&pool, "other-admin", "admin").await;
    PostgresRbacStore::new(pool.clone())
        .remove_role(other.id, "admin", None.into())
        .await
        .unwrap();
    assert!(matches!(
        change(&repo, &other_admin, &disabled, UserStatus::Active).await,
        Err(UseCaseError::Forbidden)
    ));
}

#[tokio::test]
async fn two_owners_cannot_disable_themselves_concurrently() {
    let _g = SERIAL.lock().await;
    let pool = database().await;
    let (actor_a, a) = account(&pool, "owner-a", "owner").await;
    let (actor_b, b) = account(&pool, "owner-b", "owner").await;
    let repo = PostgresUserRepository::new(pool.clone());
    let (a, b) = tokio::join!(
        change(&repo, &actor_a, &a, UserStatus::Disabled),
        change(&repo, &actor_b, &b, UserStatus::Disabled),
    );
    assert!(a.is_ok() ^ b.is_ok());
    assert!(matches!(
        a.err().or(b.err()),
        Some(UseCaseError::LastOwnerProtected)
    ));
    assert_eq!(
        PostgresRbacStore::new(pool)
            .loginable_owner_count()
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn disabling_and_role_removal_share_last_owner_protection() {
    let _g = SERIAL.lock().await;
    let pool = database().await;
    let (_, a) = account(&pool, "owner-a", "owner").await;
    let (_, b) = account(&pool, "owner-b", "owner").await;
    let repo = PostgresUserRepository::new(pool.clone());
    let rbac = PostgresRbacStore::new(pool);
    let operator = Actor::bootstrap_cli();
    let (disable, remove) = tokio::join!(
        change(&repo, &operator, &a, UserStatus::Disabled),
        rbac.remove_role(b.id, "owner", None.into()),
    );
    assert!(disable.is_ok() ^ remove.is_ok());
    assert!(matches!(
        disable.err().or(remove.err()),
        Some(UseCaseError::LastOwnerProtected)
    ));
    assert_eq!(rbac.loginable_owner_count().await.unwrap(), 1);
}

#[tokio::test]
async fn failed_audit_rolls_back_status_versions_and_session_deletion() {
    let _g = SERIAL.lock().await;
    let pool = database().await;
    let (owner, _) = account(&pool, "owner", "owner").await;
    let (_, target) = account(&pool, "author", "author").await;
    let repo = PostgresUserRepository::new(pool.clone());
    let sessions = PostgresSessionStore::with_defaults(pool.clone());
    let cookie = sessions
        .create(target.id, target.auth_version)
        .await
        .unwrap();
    sqlx::query("ALTER TABLE audit_logs ADD CONSTRAINT reject_status_audit CHECK (action <> 'user.status.update')")
        .execute(&pool).await.unwrap();
    assert!(
        change(&repo, &owner, &target, UserStatus::Disabled)
            .await
            .is_err()
    );
    assert_eq!(repo.find_by_id(target.id).await.unwrap().unwrap(), target);
    assert!(sessions.validate(&cookie).await.unwrap().is_some());
}

#[tokio::test]
async fn enabling_never_restores_deleted_accounts_or_bypasses_owner_permissions() {
    let _g = SERIAL.lock().await;
    let pool = database().await;
    let (owner, _) = account(&pool, "owner", "owner").await;
    let (_, other) = account(&pool, "owner-other", "owner").await;
    let (admin, _) = account(&pool, "admin", "admin").await;
    let repo = PostgresUserRepository::new(pool.clone());
    let disabled = change(&repo, &owner, &other, UserStatus::Disabled)
        .await
        .unwrap();
    assert!(matches!(
        change(&repo, &admin, &disabled, UserStatus::Active).await,
        Err(UseCaseError::Forbidden)
    ));
    sqlx::query("UPDATE users SET deleted_at=now() WHERE id=$1")
        .bind(other.id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        change(&repo, &owner, &disabled, UserStatus::Active).await,
        Err(UseCaseError::NotFound(_))
    ));
}

#[tokio::test]
async fn owner_noop_still_checks_permissions_and_version_without_side_effects() {
    let _g = SERIAL.lock().await;
    let pool = database().await;
    let (owner, target) = account(&pool, "owner", "owner").await;
    let (admin, _) = account(&pool, "admin", "admin").await;
    let repo = PostgresUserRepository::new(pool.clone());
    let sessions = PostgresSessionStore::with_defaults(pool.clone());
    let token = sessions
        .create(target.id, target.auth_version)
        .await
        .unwrap();
    assert!(matches!(
        change(&repo, &admin, &target, UserStatus::Active).await,
        Err(UseCaseError::Forbidden)
    ));
    assert!(matches!(
        repo.change_status(
            target.id,
            UserStatus::Active,
            target.version + 1,
            OffsetDateTime::now_utc(),
            &owner
        )
        .await,
        Err(UseCaseError::VersionConflict)
    ));
    assert_eq!(
        change(&repo, &owner, &target, UserStatus::Active)
            .await
            .unwrap(),
        target
    );
    assert_eq!(repo.find_by_id(target.id).await.unwrap().unwrap(), target);
    assert!(sessions.validate(&token).await.unwrap().is_some());
    let audits: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_logs WHERE action='user.status.update'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(audits, 0);
}
