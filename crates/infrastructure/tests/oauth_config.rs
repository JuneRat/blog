//! OAuth 设置版本、配置层面可登录 Owner 与真实 PostgreSQL 身份锁。
mod common;

use application::error::UseCaseError;
use application::identity::{BUILTIN_ROLES, PERMISSION_REGISTRY};
use application::ports::{
    ClearPasswordOutcome, OAuthAccountStore, OAuthConfigStore, PasswordCredentialStore,
    ProviderConfig, ProviderKind, RbacStore, UserQuery,
};
use infrastructure::{
    PostgresOAuthAccountStore, PostgresOAuthConfigStore, PostgresRbacStore, PostgresUserRepository,
};
use sqlx::PgPool;
use uuid::Uuid;

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn provider(id: &str, issuer: &str) -> ProviderConfig {
    ProviderConfig {
        id: id.into(),
        name: None,
        kind: ProviderKind::Oidc,
        issuer: Some(issuer.into()),
        client_id: "private-client".into(),
        secret_ref: "PRIVATE_SECRET_REFERENCE".into(),
        scopes: vec![],
    }
}

async fn database() -> PgPool {
    let pool = common::fresh_database("blog_oauth_config_test").await;
    let rbac = PostgresRbacStore::new(common::database(pool.clone()));
    rbac.sync_permission_registry(PERMISSION_REGISTRY)
        .await
        .unwrap();
    rbac.sync_builtin_roles(BUILTIN_ROLES).await.unwrap();
    pool
}

fn configs(pool: &PgPool) -> PostgresOAuthConfigStore {
    PostgresOAuthConfigStore::new(common::database(pool.clone()))
}

fn rbac(pool: &PgPool) -> PostgresRbacStore {
    PostgresRbacStore::new(common::database(pool.clone()))
}

async fn owner(pool: &PgPool, name: &str, password: bool, issuer: Option<&str>) -> Uuid {
    let id = common::seed_user(pool, name).await;
    if password {
        PostgresUserRepository::new(common::database(pool.clone()))
            .set_password_hash(id, "$test-only-password", None.into())
            .await
            .unwrap();
    }
    if let Some(issuer) = issuer {
        PostgresOAuthAccountStore::new(common::database(pool.clone()))
            .bind(id, issuer, &id.to_string(), None, None.into())
            .await
            .unwrap();
    }
    rbac(pool)
        .assign_role(id, "owner", None.into())
        .await
        .unwrap();
    id
}

async fn audit_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM audit_logs WHERE action='settings.oauth'")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn concurrent_configuration_updates_conflict_and_noops_keep_version_and_audit() {
    let _guard = SERIAL.lock().await;
    let pool = database().await;
    let a = configs(&pool);
    let b = configs(&pool);
    assert_eq!(a.read().await.unwrap().version, 0);
    assert_eq!(a.save(&[], 0, None.into()).await.unwrap(), 0);
    assert_eq!(audit_count(&pool).await, 0);

    let initial_a = [provider("a", "https://a.example")];
    let initial_b = [provider("b", "https://b.example")];
    let (one, two) = tokio::join!(
        a.save(&initial_a, 0, None.into()),
        b.save(&initial_b, 0, None.into()),
    );
    assert_eq!(
        [one.is_ok(), two.is_ok()]
            .into_iter()
            .filter(|ok| *ok)
            .count(),
        1
    );
    assert!(
        matches!(one, Err(UseCaseError::VersionConflict))
            || matches!(two, Err(UseCaseError::VersionConflict))
    );
    let current = a.read().await.unwrap();
    assert_eq!(current.version, 1);
    assert_eq!(current.providers.len(), 1);
    assert_eq!(audit_count(&pool).await, 1);
    assert!(
        matches!(
            a.save(&current.providers, 0, None.into()).await,
            Err(UseCaseError::VersionConflict)
        ),
        "同值旧版本也必须冲突"
    );
    assert_eq!(a.save(&current.providers, 1, None.into()).await.unwrap(), 1);
    assert_eq!(audit_count(&pool).await, 1);

    // 重新读取后显式合并，才能提交另一提供商；失败的旧快照不会悄悄覆盖。
    let all = [initial_a[0].clone(), initial_b[0].clone()];
    assert_eq!(b.save(&all, current.version, None.into()).await.unwrap(), 2);
    assert_eq!(a.read().await.unwrap().providers, all);
    let metadata: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT metadata FROM audit_logs WHERE action='settings.oauth' ORDER BY created_at,id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    for value in metadata {
        assert_eq!(value.as_object().unwrap().len(), 2);
        assert!(value.get("version").is_some());
        assert!(value.get("provider_count").is_some());
    }
}

#[tokio::test]
async fn removing_or_changing_the_last_owners_namespace_rolls_back() {
    let _guard = SERIAL.lock().await;
    let pool = database().await;
    let store = configs(&pool);
    let first = provider("idp", "https://idp.example");
    store
        .save(std::slice::from_ref(&first), 0, None.into())
        .await
        .unwrap();
    let owner_id = owner(&pool, "owner", false, first.issuer.as_deref()).await;
    let before = store.read().await.unwrap();
    for next in [vec![], vec![provider("idp", "https://replacement.example")]] {
        assert!(matches!(
            store.save(&next, before.version, None.into()).await,
            Err(UseCaseError::LastOwnerProtected)
        ));
        assert_eq!(store.read().await.unwrap(), before);
        assert_eq!(audit_count(&pool).await, 1);
    }
    // 同一 issuer 的另一入口仍然能够匹配原绑定；更换入口 id 不改变身份键。
    let alias = provider("other-entry", "https://idp.example");
    store.save(&[alias], 1, None.into()).await.unwrap();
    assert_eq!(rbac(&pool).loginable_owner_count().await.unwrap(), 1);
    PostgresUserRepository::new(common::database(pool.clone()))
        .set_password_hash(owner_id, "$fallback-password", None.into())
        .await
        .unwrap();
    store.save(&[], 2, None.into()).await.unwrap();
    assert_eq!(rbac(&pool).loginable_owner_count().await.unwrap(), 1);
}

#[tokio::test]
async fn disabled_deleted_and_unconfigured_owners_are_not_fallbacks() {
    let _guard = SERIAL.lock().await;
    let pool = database().await;
    let store = configs(&pool);
    store
        .save(&[provider("idp", "https://idp.example")], 0, None.into())
        .await
        .unwrap();
    let live = owner(&pool, "live", false, Some("https://idp.example")).await;
    let disabled = owner(&pool, "disabled", true, None).await;
    let deleted = owner(&pool, "deleted", true, None).await;
    let broken = owner(&pool, "unconfigured", false, Some("https://absent.example")).await;
    sqlx::query("UPDATE users SET status='disabled' WHERE id=$1")
        .bind(disabled)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE users SET deleted_at=now() WHERE id=$1")
        .bind(deleted)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(rbac(&pool).loginable_owner_count().await.unwrap(), 1);
    assert!(matches!(
        store.save(&[], 1, None.into()).await,
        Err(UseCaseError::LastOwnerProtected)
    ));
    assert!(matches!(
        rbac(&pool).remove_role(live, "owner", None.into()).await,
        Err(UseCaseError::LastOwnerProtected)
    ));
    let rows = PostgresUserRepository::new(common::database(pool.clone()))
        .list_admin(20, 0)
        .await
        .unwrap();
    let broken = rows.iter().find(|row| row.id == broken).unwrap();
    assert_eq!(broken.external_identities, 0);
    assert!(!broken.can_login());
}

#[tokio::test]
async fn provider_removal_and_owner_role_removal_share_the_identity_lock() {
    let _guard = SERIAL.lock().await;
    let pool = database().await;
    let store = configs(&pool);
    store
        .save(&[provider("idp", "https://idp.example")], 0, None.into())
        .await
        .unwrap();
    owner(&pool, "external", false, Some("https://idp.example")).await;
    let local = owner(&pool, "local", true, None).await;
    let roles = rbac(&pool);
    let (config, role) = tokio::join!(
        store.save(&[], 1, None.into()),
        roles.remove_role(local, "owner", None.into()),
    );
    assert_ne!(
        config.is_ok(),
        role.is_ok(),
        "只允许一个破坏备用入口：{config:?}, {role:?}"
    );
    assert!(
        matches!(config, Err(UseCaseError::LastOwnerProtected))
            || matches!(role, Err(UseCaseError::LastOwnerProtected))
    );
    assert_eq!(roles.loginable_owner_count().await.unwrap(), 1);
}

#[tokio::test]
async fn configuration_waits_for_identity_changes_and_rechecks_the_committed_owner_state() {
    let _guard = SERIAL.lock().await;
    let pool = database().await;
    configs(&pool)
        .save(&[provider("idp", "https://idp.example")], 0, None.into())
        .await
        .unwrap();
    owner(&pool, "external", false, Some("https://idp.example")).await;
    let local = owner(&pool, "local", true, None).await;

    // 在另一连接模拟先拿到身份锁的账号停用。配置写入必须等待，并在取得锁后重读。
    let mut identity = pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(2048001, 1)")
        .execute(&mut *identity)
        .await
        .unwrap();
    let store = configs(&pool);
    let pending = tokio::spawn(async move { store.save(&[], 1, None.into()).await });
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_locks \
                 WHERE locktype='advisory' AND classid=2048001 AND objid=1 AND NOT granted \
                   AND database=(SELECT oid FROM pg_database WHERE datname=current_database()))",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            if waiting {
                break;
            }
            assert!(!pending.is_finished(), "配置保存必须等待同一身份锁");
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("配置事务应等待身份锁");
    sqlx::query("UPDATE users SET status='disabled' WHERE id=$1")
        .bind(local)
        .execute(&mut *identity)
        .await
        .unwrap();
    identity.commit().await.unwrap();
    assert!(matches!(
        pending.await.unwrap(),
        Err(UseCaseError::LastOwnerProtected)
    ));
    assert_eq!(configs(&pool).read().await.unwrap().version, 1);
    assert_eq!(rbac(&pool).loginable_owner_count().await.unwrap(), 1);
}

#[tokio::test]
async fn provider_removal_and_password_clear_cannot_remove_all_owner_access() {
    let _guard = SERIAL.lock().await;
    let pool = database().await;
    let store = configs(&pool);
    store
        .save(&[provider("idp", "https://idp.example")], 0, None.into())
        .await
        .unwrap();
    let id = owner(&pool, "owner", true, Some("https://idp.example")).await;
    let users = PostgresUserRepository::new(common::database(pool.clone()));
    let (config, password) = tokio::join!(
        store.save(&[], 1, None.into()),
        users.clear_password_hash_guarded(id, None.into()),
    );
    assert_ne!(
        config.is_ok(),
        matches!(password, Ok(ClearPasswordOutcome::Cleared))
    );
    assert_eq!(rbac(&pool).loginable_owner_count().await.unwrap(), 1);
}

#[tokio::test]
async fn unconfigured_binding_does_not_allow_clearing_the_last_usable_method() {
    let _guard = SERIAL.lock().await;
    let pool = database().await;
    configs(&pool)
        .save(&[provider("idp", "https://idp.example")], 0, None.into())
        .await
        .unwrap();
    let id = owner(&pool, "owner", false, Some("https://idp.example")).await;
    let accounts = PostgresOAuthAccountStore::new(common::database(pool.clone()));
    accounts
        .bind(id, "https://absent.example", "absent", None, None.into())
        .await
        .unwrap();
    assert!(matches!(
        accounts
            .unbind(id, "https://idp.example", &id.to_string(), None.into())
            .await,
        Err(UseCaseError::Forbidden)
    ));
    let users = PostgresUserRepository::new(common::database(pool.clone()));
    users
        .set_password_hash(id, "$fallback-password", None.into())
        .await
        .unwrap();
    accounts
        .unbind(id, "https://idp.example", &id.to_string(), None.into())
        .await
        .unwrap();
    assert!(matches!(
        users
            .clear_password_hash_guarded(id, None.into())
            .await
            .unwrap(),
        ClearPasswordOutcome::LastLoginMethod
    ));
}

#[tokio::test]
async fn initial_configuration_and_audit_failure_preserve_installation_and_atomicity() {
    let _guard = SERIAL.lock().await;
    let pool = database().await;
    let store = configs(&pool);
    // 分步安装还没有 Owner，不阻止修改或撤销初始提供商配置。
    store
        .save(&[provider("idp", "https://idp.example")], 0, None.into())
        .await
        .unwrap();
    store.save(&[], 1, None.into()).await.unwrap();
    let before = store.read().await.unwrap();
    let audits = audit_count(&pool).await;
    sqlx::raw_sql("CREATE FUNCTION reject_oauth_audit() RETURNS trigger LANGUAGE plpgsql AS $$ \
        BEGIN IF NEW.action='settings.oauth' THEN RAISE EXCEPTION 'blocked audit'; END IF; RETURN NEW; END $$; \
        CREATE TRIGGER reject_oauth_audit BEFORE INSERT ON audit_logs FOR EACH ROW EXECUTE FUNCTION reject_oauth_audit();")
        .execute(&pool).await.unwrap();
    assert!(
        store
            .save(
                &[provider("new", "https://new.example")],
                before.version,
                None.into()
            )
            .await
            .is_err()
    );
    assert_eq!(store.read().await.unwrap(), before);
    assert_eq!(audit_count(&pool).await, audits);
}
