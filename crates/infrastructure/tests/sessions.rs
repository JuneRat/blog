//! 内存会话/尝试存储与安全随机源的行为测试（无数据库依赖）。

use application::ports::{OAuthAttempt, OAuthAttemptStore, SecureRandom, SessionStore};
use infrastructure::{
    AttemptStoreConfig, InMemoryOAuthAttemptStore, InMemorySessionStore, SessionStoreConfig,
    SystemSecureRandom,
};
use time::OffsetDateTime;
use uuid::Uuid;

fn mutable_now() -> (
    std::sync::Arc<std::sync::Mutex<OffsetDateTime>>,
    impl Fn() -> OffsetDateTime + Send + Sync + 'static,
) {
    let now = std::sync::Arc::new(std::sync::Mutex::new(OffsetDateTime::now_utc()));
    let reader = now.clone();
    (now, move || *reader.lock().unwrap())
}

#[tokio::test]
async fn session_validate_refreshes_idle_and_respects_absolute_expiry() {
    let (now, clock) = mutable_now();
    let store = InMemorySessionStore::new(
        SessionStoreConfig {
            idle_secs: 60,
            absolute_secs: 3600,
            max_entries: 100,
        },
        Box::new(clock),
    );

    let token = store.create(Uuid::now_v7()).await.unwrap();
    assert!(
        store.validate(&token).await.unwrap().is_some(),
        "刚创建可校验"
    );

    // 空闲 61 秒后过期。
    *now.lock().unwrap() += time::Duration::seconds(61);
    assert!(store.validate(&token).await.unwrap().is_none(), "空闲过期");

    // 空闲窗口内活动可续期，但绝对上限不可逾越。
    let token2 = store.create(Uuid::now_v7()).await.unwrap();
    let mut elapsed = 0;
    while elapsed < 3500 {
        *now.lock().unwrap() += time::Duration::seconds(50);
        assert!(
            store.validate(&token2).await.unwrap().is_some(),
            "活动续期应保持会话"
        );
        elapsed += 50;
    }
    *now.lock().unwrap() += time::Duration::seconds(200);
    assert!(
        store.validate(&token2).await.unwrap().is_none(),
        "超过绝对过期必须失效"
    );
}

#[tokio::test]
async fn session_revoke_and_revoke_all_for_user() {
    let (_now, clock) = mutable_now();
    let store = InMemorySessionStore::with_defaults();
    let _ = clock; // 默认系统时钟即可
    let user = Uuid::now_v7();

    let t1 = store.create(user).await.unwrap();
    let t2 = store.create(user).await.unwrap();
    let t3 = store.create(Uuid::now_v7()).await.unwrap();

    store.revoke(&t1).await.unwrap();
    assert!(store.validate(&t1).await.unwrap().is_none());
    assert!(store.validate(&t2).await.unwrap().is_some());

    store.revoke_all_for_user(user).await.unwrap();
    assert!(
        store.validate(&t2).await.unwrap().is_none(),
        "用户会话全部撤销"
    );
    assert!(
        store.validate(&t3).await.unwrap().is_some(),
        "其他用户不受影响"
    );
}

#[tokio::test]
async fn session_capacity_evicts_least_recently_active() {
    let (now, clock) = mutable_now();
    let store = InMemorySessionStore::new(
        SessionStoreConfig {
            idle_secs: 10_000,
            absolute_secs: 10_000,
            max_entries: 2,
        },
        Box::new(clock),
    );

    let t1 = store.create(Uuid::now_v7()).await.unwrap();
    *now.lock().unwrap() += time::Duration::seconds(5);
    let _t2 = store.create(Uuid::now_v7()).await.unwrap();
    *now.lock().unwrap() += time::Duration::seconds(5);
    // 第三个会话触发容量淘汰：t1 最久未活跃。
    let _t3 = store.create(Uuid::now_v7()).await.unwrap();

    assert!(
        store.validate(&t1).await.unwrap().is_none(),
        "容量满时淘汰最久未活跃会话"
    );
}

#[tokio::test]
async fn oauth_attempt_state_is_consumed_once_and_expires() {
    let (now, clock) = mutable_now();
    let store = InMemoryOAuthAttemptStore::new(
        AttemptStoreConfig {
            ttl_secs: 60,
            max_entries: 10,
        },
        Box::new(clock),
    );

    let created = *now.lock().unwrap();
    let attempt = OAuthAttempt {
        provider_id: "fake".into(),
        verifier: Some("verifier".into()),
        nonce: Some("nonce".into()),
        redirect_uri: "http://localhost/auth/callback/fake".into(),
        next: "/admin".into(),
        created_at: created,
    };
    store.save("state-1".into(), attempt).await.unwrap();

    assert!(
        store.consume("state-1").await.unwrap().is_some(),
        "首次消费成功"
    );
    assert!(
        store.consume("state-1").await.unwrap().is_none(),
        "state 只能消费一次（防重放）"
    );

    // 注意：await 调用参数中的临时 MutexGuard 会存活到 await 结束，
    // 与存储内部 clock() 的锁形成自死锁；必须先在独立语句完成构造。
    let created2 = *now.lock().unwrap();
    let attempt2 = OAuthAttempt {
        created_at: created2,
        ..sample_attempt()
    };
    store.save("state-2".into(), attempt2).await.unwrap();
    *now.lock().unwrap() += time::Duration::seconds(61);
    assert!(
        store.consume("state-2").await.unwrap().is_none(),
        "过期尝试无效"
    );
}

fn sample_attempt() -> OAuthAttempt {
    OAuthAttempt {
        provider_id: "fake".into(),
        verifier: None,
        nonce: None,
        redirect_uri: "http://localhost/auth/callback/fake".into(),
        next: "/".into(),
        created_at: OffsetDateTime::now_utc(),
    }
}

#[tokio::test]
async fn oauth_attempt_store_capacity_bound() {
    let (_now, clock) = mutable_now();
    let store = InMemoryOAuthAttemptStore::new(
        AttemptStoreConfig {
            ttl_secs: 60,
            max_entries: 1,
        },
        Box::new(clock),
    );
    store.save("a".into(), sample_attempt()).await.unwrap();
    let err = store.save("b".into(), sample_attempt()).await.unwrap_err();
    assert!(
        err.to_string().contains("已满"),
        "容量上限拒绝新尝试：{err}"
    );
}

#[test]
fn pkce_s256_matches_rfc7636_vector() {
    // RFC 7636 Appendix B 测试向量。
    let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    let expected = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
    let random = SystemSecureRandom;
    assert_eq!(random.pkce_s256(verifier).unwrap(), expected);
}

#[test]
fn token_hex_shape() {
    let random = SystemSecureRandom;
    let token = random.token_hex().unwrap();
    assert_eq!(token.len(), 64, "32 字节 hex");
    assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
    assert_ne!(token, random.token_hex().unwrap(), "不重复");
}
