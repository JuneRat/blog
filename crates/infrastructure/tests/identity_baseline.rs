//! 新库基线、身份修订与事务审计的边界验证。
mod common;

use application::ports::{
    AccountAdministration, OAuthAccountStore, PasswordCredentialStore, SessionStore, UserQuery,
};
use infrastructure::audit::{AuditEntry, append_audit_log};
use infrastructure::{PostgresOAuthAccountStore, PostgresSessionStore, PostgresUserRepository};

const DB: &str = "blog_identity_baseline_test";
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn legacy_schema_is_rejected_without_deleting_data() {
    let _g = SERIAL.lock().await;
    let pool = common::fresh_database(DB).await;
    // 仅在本文件独立测试库里构造旧结构，保留哨兵确认迁移失败无破坏性动作。
    sqlx::raw_sql(
        "DROP SCHEMA public CASCADE; CREATE SCHEMA public; \
        CREATE TABLE users (username text PRIMARY KEY); INSERT INTO users VALUES ('keep-me');",
    )
    .execute(&pool)
    .await
    .unwrap();
    let error = infrastructure::migrate_schema(&pool, "../../migrations/postgres")
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("新初始迁移仅支持空库"),
        "{error}"
    );
    let rows: Vec<String> = sqlx::query_scalar("SELECT username FROM users")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(rows, vec!["keep-me"]);
}

#[tokio::test]
async fn authentication_changes_invalidate_even_late_stale_sessions() {
    let _g = SERIAL.lock().await;
    let pool = common::fresh_database(DB).await;
    let user = common::seed_user(&pool, "identity-changes").await;
    let users = PostgresUserRepository::new(pool.clone());
    let accounts = PostgresOAuthAccountStore::new(pool.clone());
    let sessions = PostgresSessionStore::with_defaults(pool.clone());
    let old = sessions.create(user, 1).await.unwrap();
    users
        .set_password_hash(user, "$argon2id$test", None.into())
        .await
        .unwrap();
    let snapshot = users.find_by_id(user).await.unwrap().unwrap();
    assert_eq!((snapshot.version, snapshot.auth_version), (2, 2));
    assert!(sessions.validate(&old).await.unwrap().is_none());
    // 模拟另一个进程持有旧用户快照，恰好在凭据事务提交后才写入会话。
    let late = sessions.create(user, 1).await.unwrap();
    assert!(sessions.validate(&late).await.unwrap().is_none());

    let token = sessions.create(user, 2).await.unwrap();
    accounts
        .bind(
            user,
            "github",
            "123",
            Some("unused@example.com".into()),
            None.into(),
        )
        .await
        .unwrap();
    assert!(sessions.validate(&token).await.unwrap().is_none());
    let bindings = accounts.list_for_user(user).await.unwrap();
    assert_eq!(bindings[0].provider_user_id, "123");
    assert!(bindings[0].email.is_none(), "绑定表不持久化提供商邮箱");
    let token = sessions.create(user, 3).await.unwrap();
    accounts
        .unbind(user, "github", "missing", None.into())
        .await
        .unwrap();
    assert!(
        sessions.validate(&token).await.unwrap().is_some(),
        "无变化不撤销登录"
    );
    accounts
        .unbind(user, "github", "123", None.into())
        .await
        .unwrap();
    assert!(sessions.validate(&token).await.unwrap().is_none());
    let before = users.find_by_id(user).await.unwrap().unwrap();
    let token = sessions.create(user, before.auth_version).await.unwrap();
    users
        .revoke_authentication(user, None.into())
        .await
        .unwrap();
    let after = users.find_by_id(user).await.unwrap().unwrap();
    assert_eq!(after.auth_version, before.auth_version + 1);
    assert_eq!(after.version, before.version, "撤销登录不算资料编辑");
    assert!(sessions.validate(&token).await.unwrap().is_none());
    sqlx::query("UPDATE users SET status='disabled' WHERE id=$1")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        users
            .find_password_credential("identity-changes")
            .await
            .unwrap()
            .is_none()
    );
    assert!(!users.find_by_id(user).await.unwrap().unwrap().is_active());
}

#[tokio::test]
async fn audit_and_business_changes_commit_or_roll_back_together() {
    let _g = SERIAL.lock().await;
    let pool = common::fresh_database(DB).await;
    let user = common::seed_user(&pool, "atomic-audit").await;
    let target = user.to_string();
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("UPDATE users SET bio='saved', version=version+1 WHERE id=$1")
        .bind(user)
        .execute(&mut *tx)
        .await
        .unwrap();
    append_audit_log(
        &mut tx,
        AuditEntry {
            actor_id: Some(user),
            ip_address: Some("127.0.0.1".parse().unwrap()),
            action: "user.profile.update",
            target_type: "user",
            target_id: &target,
            metadata: serde_json::json!({"version": 2}),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    // 审计 CHECK 失败时，前面的业务 UPDATE 也必须回滚。
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("UPDATE users SET bio='should-roll-back', version=version+1 WHERE id=$1")
        .bind(user)
        .execute(&mut *tx)
        .await
        .unwrap();
    assert!(
        append_audit_log(
            &mut tx,
            AuditEntry {
                actor_id: Some(user),
                ip_address: None,
                action: "",
                target_type: "user",
                target_id: &target,
                metadata: serde_json::json!({"version": 3}),
            }
        )
        .await
        .is_err()
    );
    tx.rollback().await.unwrap();
    let row: (String, i64) = sqlx::query_as("SELECT bio, version FROM users WHERE id=$1")
        .bind(user)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row, ("saved".into(), 2));
    let audits: Vec<(String, serde_json::Value)> =
        sqlx::query_as("SELECT host(ip_address), metadata FROM audit_logs WHERE target_id=$1")
            .bind(target)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        audits,
        vec![("127.0.0.1".into(), serde_json::json!({"version": 2}))]
    );
}
