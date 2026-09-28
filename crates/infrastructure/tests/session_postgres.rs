//! PostgreSQL 会话存储集成测试：真实库上的跨进程共享、并发与过期语义。
//!
//! 独立测试库 `blog_session_test`（与 postgres.rs 的 `blog_test` 分离），
//! 让两个测试二进制可以并行运行而不互相 DROP 库；文件内测试用 `SERIAL` 串行。
//!
//! 时间由注入的**可推进逻辑时钟**控制，过期断言不需要 sleep。

use std::sync::{Arc, Mutex};

use application::ports::SessionStore;
use common::connect;
use infrastructure::sessions::SESSION_LOCK;
use infrastructure::{PostgresSessionStore, SessionStoreConfig};
use sha2::{Digest as _, Sha256};
use sqlx::{PgPool, Row};
use time::OffsetDateTime;
use uuid::Uuid;

mod common;

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const DB: &str = "blog_session_test";

type Clock = Box<dyn Fn() -> OffsetDateTime + Send + Sync>;

/// 可推进的逻辑时钟；多个存储/“进程”共享同一个读数。
fn shared_now() -> Arc<Mutex<OffsetDateTime>> {
    Arc::new(Mutex::new(OffsetDateTime::now_utc()))
}

fn reader(now: &Arc<Mutex<OffsetDateTime>>) -> Clock {
    let handle = Arc::clone(now);
    Box::new(move || *handle.lock().unwrap())
}

fn advance(now: &Arc<Mutex<OffsetDateTime>>, secs: i64) {
    *now.lock().unwrap() += time::Duration::seconds(secs);
}

fn config(idle_secs: i64, absolute_secs: i64, max_entries: usize) -> SessionStoreConfig {
    SessionStoreConfig {
        idle_secs,
        absolute_secs,
        max_entries,
    }
}

fn session_store(
    pool: PgPool,
    cfg: SessionStoreConfig,
    now: &Arc<Mutex<OffsetDateTime>>,
) -> PostgresSessionStore {
    PostgresSessionStore::new(common::database(pool), cfg, reader(now))
}

async fn session_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM sessions")
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn session_count_for(pool: &PgPool, user_id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM sessions WHERE user_id = $1")
        .bind(user_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

fn digest_hex(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// 跨进程读取与撤销：第二个连接池代表另一个进程（服务重启或运维 CLI）。
/// 这是持久会话相对内存实现的核心收益，必须逐条验证。
#[tokio::test]
async fn state_is_shared_across_pools_and_revocation_crosses_processes() {
    let _g = SERIAL.lock().await;
    let pool = common::fresh_database(DB).await;
    let user = common::seed_user(&pool, "cross-process").await;
    let other = common::seed_user(&pool, "cross-other").await;
    sqlx::query("UPDATE users SET auth_version=7 WHERE id=$1")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();

    let now = shared_now();
    let store_a = session_store(pool.clone(), config(3600, 86_400, 100), &now);
    let dsn = common::test_db_url(&common::admin_url(), DB);
    let pool_b = connect(&dsn).await.expect("第二个连接池");
    let store_b = session_store(pool_b, config(3600, 86_400, 100), &now);

    let token = store_a.create(user, 7).await.unwrap();
    let record = store_b
        .validate(&token)
        .await
        .unwrap()
        .expect("另一个进程看得到 A 签发的会话");
    assert_eq!(record.user_id, user);
    assert_eq!(record.auth_version, 7, "签发版本落库并跨进程读回");
    assert_eq!(record.csrf_token.len(), 64, "CSRF token 跨进程可读");

    store_b.revoke(&token).await.unwrap();
    assert!(
        store_a.validate(&token).await.unwrap().is_none(),
        "B 撤销后 A 立即可见"
    );

    // 按用户批量撤销同样跨进程，且不波及其他用户。
    let t1 = store_a.create(user, 7).await.unwrap();
    let t2 = store_a.create(user, 7).await.unwrap();
    let t_other = store_a.create(other, 1).await.unwrap();
    store_b.revoke_all_for_user(user).await.unwrap();
    assert!(store_a.validate(&t1).await.unwrap().is_none());
    assert!(store_a.validate(&t2).await.unwrap().is_none());
    assert!(
        store_a.validate(&t_other).await.unwrap().is_some(),
        "其他用户会话不受影响"
    );
}

/// 空闲过期看 `last_seen_at`：活动续期，空闲超时失效，且过期行被顺带清理。
#[tokio::test]
async fn only_valid_identity_snapshots_refresh_activity() {
    let _g = SERIAL.lock().await;
    let pool = common::fresh_database(DB).await;
    let user = common::seed_user(&pool, "auth-snapshot").await;
    let now = shared_now();
    let store = session_store(pool.clone(), config(3600, 86_400, 100), &now);
    let token = store.create(user, 1).await.unwrap();
    sqlx::query("UPDATE users SET version=version+1 WHERE id=$1")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    advance(&now, 10);
    assert!(
        store.validate(&token).await.unwrap().is_some(),
        "资料版本不参与鉴权"
    );
    let last_seen: OffsetDateTime = sqlx::query_scalar("SELECT last_seen_at FROM sessions")
        .fetch_one(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE users SET auth_version=auth_version+1 WHERE id=$1")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    advance(&now, 10);
    assert!(store.validate(&token).await.unwrap().is_none());
    let row: (i64, OffsetDateTime) =
        sqlx::query_as("SELECT auth_version, last_seen_at FROM sessions")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(row, (1, last_seen), "旧认证快照和活跃时间均不得刷新");
    store.revoke(&token).await.unwrap();
    let fresh = store.create(user, 2).await.unwrap();
    let last_seen: OffsetDateTime = sqlx::query_scalar("SELECT last_seen_at FROM sessions")
        .fetch_one(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE users SET status='disabled' WHERE id=$1")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    advance(&now, 10);
    assert!(store.validate(&fresh).await.unwrap().is_none());
    let unchanged: OffsetDateTime = sqlx::query_scalar("SELECT last_seen_at FROM sessions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(unchanged, last_seen, "禁用账号不得刷新活跃时间");
}

#[tokio::test]
async fn validate_refreshes_idle_window_and_idle_expiry_cleans_the_row() {
    let _g = SERIAL.lock().await;
    let pool = common::fresh_database(DB).await;
    let user = common::seed_user(&pool, "idle").await;
    let now = shared_now();
    let store = session_store(pool.clone(), config(60, 86_400, 100), &now);

    let token = store.create(user, 1).await.unwrap();
    assert!(store.validate(&token).await.unwrap().is_some());

    // 每次校验刷新 last_seen：累计 100 秒（> 60）依然有效。
    advance(&now, 50);
    assert!(store.validate(&token).await.unwrap().is_some(), "活动续期");
    advance(&now, 50);
    assert!(
        store.validate(&token).await.unwrap().is_some(),
        "续期后空闲窗口重新计时"
    );

    advance(&now, 61);
    assert!(
        store.validate(&token).await.unwrap().is_none(),
        "空闲超时必须失效"
    );
    assert_eq!(session_count(&pool).await, 0, "过期行被清理");
}

/// 绝对过期看签发时固定的 `expires_at`：活动续期不能逾越上限。
#[tokio::test]
async fn absolute_expiry_is_not_extended_by_activity() {
    let _g = SERIAL.lock().await;
    let pool = common::fresh_database(DB).await;
    let user = common::seed_user(&pool, "absolute").await;
    let now = shared_now();
    // 空闲 TTL 故意远大于绝对 TTL：只有绝对过期能解释最终的失效。
    let store = session_store(pool.clone(), config(100_000, 3600, 100), &now);

    let token = store.create(user, 1).await.unwrap();
    advance(&now, 3500);
    assert!(
        store.validate(&token).await.unwrap().is_some(),
        "绝对上限内仍有效"
    );

    let row = sqlx::query("SELECT created_at, expires_at FROM sessions")
        .fetch_one(&pool)
        .await
        .unwrap();
    let created: OffsetDateTime = row.get("created_at");
    let expires: OffsetDateTime = row.get("expires_at");
    assert_eq!(
        expires - created,
        time::Duration::seconds(3600),
        "expires_at = created_at + 绝对 TTL，签发时固定"
    );

    advance(&now, 200);
    assert!(
        store.validate(&token).await.unwrap().is_none(),
        "超过绝对过期必须失效，活动不能续命"
    );
    assert_eq!(session_count(&pool).await, 0);
}

/// 显式清理入口只删过期行，返回值即删除行数（供恢复后清空运行态）。
#[tokio::test]
async fn purge_expired_removes_only_stale_rows_and_reports_count() {
    let _g = SERIAL.lock().await;
    let pool = common::fresh_database(DB).await;
    let user = common::seed_user(&pool, "purge-a").await;
    let other = common::seed_user(&pool, "purge-b").await;
    let now = shared_now();
    let store = session_store(pool.clone(), config(60, 7200, 100), &now);

    let stale = store.create(user, 1).await.unwrap();
    advance(&now, 61);

    // 绕过 create 的顺带清理，直接造一行「刚活跃」的会话，
    // 这样 purge_expired 的取舍才是被单独验证的（而不是被 create 提前清掉）。
    let at = *now.lock().unwrap();
    sqlx::query(
        "INSERT INTO sessions \
             (token_hash, user_id, csrf_token, auth_version, created_at, last_seen_at, expires_at) \
         VALUES ($1, $2, $3, 1, $4, $4, $5)",
    )
    .bind("d".repeat(64))
    .bind(other)
    .bind("e".repeat(64))
    .bind(at)
    .bind(at + time::Duration::seconds(7200))
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(session_count(&pool).await, 2);

    assert_eq!(store.purge_expired().await.unwrap(), 1, "只清理过期那一行");
    assert_eq!(session_count_for(&pool, user).await, 0);
    assert_eq!(session_count_for(&pool, other).await, 1);
    assert_eq!(store.purge_expired().await.unwrap(), 0, "重复清理无效果");

    assert!(store.validate(&stale).await.unwrap().is_none());
    assert_eq!(
        session_count_for(&pool, other).await,
        1,
        "刚活跃的会话被保留"
    );
}

/// 库里只放摘要，不放明文；摘要形态与用户外键由表约束兜底。
#[tokio::test]
async fn row_stores_digest_not_plaintext_and_constraints_hold() {
    let _g = SERIAL.lock().await;
    let pool = common::fresh_database(DB).await;
    let user = common::seed_user(&pool, "digest").await;
    let now = shared_now();
    let store = session_store(pool.clone(), config(3600, 86_400, 100), &now);

    let token = store.create(user, 3).await.unwrap();
    let row = sqlx::query("SELECT token_hash, csrf_token, auth_version FROM sessions")
        .fetch_one(&pool)
        .await
        .unwrap();
    let hash: String = row.get("token_hash");
    assert_ne!(hash, token, "库中绝不保存明文令牌");
    assert_eq!(hash, digest_hex(&token), "落库的是 SHA-256 摘要");
    assert_eq!(row.get::<String, _>("csrf_token").len(), 64);
    assert_eq!(row.get::<i64, _>("auth_version"), 3);

    // CHECK 拒绝非 64 位 hex 的摘要。
    let bad_digest = sqlx::query(
        "INSERT INTO sessions \
             (token_hash, user_id, csrf_token, auth_version, created_at, last_seen_at, expires_at) \
         VALUES ('not-a-digest', $1, $2, 1, $3, $3, $4)",
    )
    .bind(user)
    .bind("a".repeat(64))
    .bind(OffsetDateTime::now_utc())
    .bind(OffsetDateTime::now_utc() + time::Duration::seconds(60))
    .execute(&pool)
    .await;
    assert!(bad_digest.is_err(), "非 hex 摘要被 CHECK 拒绝");

    // 外键拒绝不存在的用户。
    let orphan = sqlx::query(
        "INSERT INTO sessions \
             (token_hash, user_id, csrf_token, auth_version, created_at, last_seen_at, expires_at) \
         VALUES ($1, $2, $3, 1, $4, $4, $5)",
    )
    .bind("b".repeat(64))
    .bind(Uuid::now_v7())
    .bind("c".repeat(64))
    .bind(OffsetDateTime::now_utc())
    .bind(OffsetDateTime::now_utc() + time::Duration::seconds(60))
    .execute(&pool)
    .await;
    assert!(orphan.is_err(), "未知 user_id 被外键拒绝");
}

/// 容量上限在并发创建下也必须精确：靠事务级 advisory lock 串行「计数—淘汰—插入」。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_creates_never_exceed_capacity() {
    let _g = SERIAL.lock().await;
    let pool = common::fresh_database(DB).await;
    let user = common::seed_user(&pool, "capacity").await;
    let now = shared_now();
    let store = Arc::new(session_store(pool.clone(), config(86_400, 86_400, 5), &now));

    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..40 {
        let store = Arc::clone(&store);
        tasks.spawn(async move { store.create(user, 1).await });
    }
    let mut tokens = Vec::new();
    while let Some(joined) = tasks.join_next().await {
        tokens.push(joined.expect("任务未 panic").expect("创建不应失败"));
    }

    assert_eq!(
        session_count(&pool).await,
        5,
        "advisory lock 让容量上限精确成立"
    );
    let mut valid = 0;
    for token in &tokens {
        if store.validate(token).await.unwrap().is_some() {
            valid += 1;
        }
    }
    assert_eq!(valid, 5, "只有容量内的会话可用");
}

/// 未触顶时并发创建不得丢会话。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_creates_under_capacity_all_persist() {
    let _g = SERIAL.lock().await;
    let pool = common::fresh_database(DB).await;
    let user = common::seed_user(&pool, "no-loss").await;
    let now = shared_now();
    let store = Arc::new(session_store(
        pool.clone(),
        config(86_400, 86_400, 100),
        &now,
    ));

    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..30 {
        let store = Arc::clone(&store);
        tasks.spawn(async move { store.create(user, 1).await });
    }
    let mut tokens = Vec::new();
    while let Some(joined) = tasks.join_next().await {
        tokens.push(joined.expect("任务未 panic").expect("创建不应失败"));
    }

    assert_eq!(session_count(&pool).await, 30);
    for token in &tokens {
        assert!(
            store.validate(token).await.unwrap().is_some(),
            "并发创建不丢会话"
        );
    }
}

/// 并发创建与「撤销全部」混跑：不报错、不死锁，最终撤销后该用户一条不剩，
/// 其他用户的会话不受影响。
///
/// 这只验证收敛与无死锁；「撤销与创建的先后顺序」由
/// [`revoke_all_waits_for_in_flight_create_then_invalidates_it`] 单独验证——
/// 末尾这次静默撤销会掩盖顺序问题。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_creates_and_revoke_all_converge() {
    let _g = SERIAL.lock().await;
    let pool = common::fresh_database(DB).await;
    let user = common::seed_user(&pool, "race-user").await;
    let other = common::seed_user(&pool, "race-other").await;
    let now = shared_now();
    let store = Arc::new(session_store(
        pool.clone(),
        config(86_400, 86_400, 1000),
        &now,
    ));

    // 并发窗口之前先建一个别的用户的会话，结束时必须仍在。
    let untouched = store.create(other, 1).await.unwrap();

    let mut tasks = tokio::task::JoinSet::new();
    for i in 0..24 {
        let store = Arc::clone(&store);
        if i % 3 == 0 {
            tasks.spawn(async move {
                store
                    .revoke_all_for_user(user)
                    .await
                    .map(|()| None::<String>)
            });
        } else {
            tasks.spawn(async move { store.create(user, 1).await.map(Some) });
        }
    }
    let mut tokens = Vec::new();
    while let Some(joined) = tasks.join_next().await {
        if let Some(token) = joined.expect("任务未 panic").expect("并发操作不应失败") {
            tokens.push(token);
        }
    }

    // 静默后撤销：并发窗口里晚于某次撤销而新建的会话会被这一步清掉。
    // 关键不变量是「撤销返回后，之前存在的会话全部失效」，以及最终态收敛为零。
    store.revoke_all_for_user(user).await.unwrap();
    assert_eq!(session_count_for(&pool, user).await, 0);
    for token in &tokens {
        assert!(
            store.validate(token).await.unwrap().is_none(),
            "并发创建的会话最终全部失效"
        );
    }
    assert!(
        store.validate(&untouched).await.unwrap().is_some(),
        "其他用户不受影响"
    );
}

/// 批量撤销与创建共用会话锁：撤销必须排在「已开始但尚未提交的创建」之后，
/// 那个会话于是在撤销返回后同样失效。
///
/// 断言分两段：锁被占用时撤销**不能**提前返回；锁释放后，锁内插入的会话已被删除。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revoke_all_waits_for_in_flight_create_then_invalidates_it() {
    let _g = SERIAL.lock().await;
    let pool = common::fresh_database(DB).await;
    let user = common::seed_user(&pool, "revoke-order").await;
    let now = shared_now();
    let store = Arc::new(session_store(
        pool.clone(),
        config(86_400, 86_400, 100),
        &now,
    ));

    // 模拟「已拿到会话锁、尚未提交」的创建：此刻表里还没有它的会话行。
    let mut in_flight = pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock($1::int, $2::int)")
        .bind(SESSION_LOCK.0)
        .bind(SESSION_LOCK.1)
        .execute(&mut *in_flight)
        .await
        .unwrap();

    let revoke = tokio::spawn({
        let store = Arc::clone(&store);
        async move { store.revoke_all_for_user(user).await }
    });
    // 不共用锁的撤销此刻无事可等，会立刻返回；共用则必须等 in_flight 释放。
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    assert!(
        !revoke.is_finished(),
        "revoke_all 必须等待持有会话锁的创建，不能绕过锁提前返回"
    );

    // 这个「创建」在锁内插入并提交，模拟它先于撤销完成。
    let token = "f".repeat(64);
    let at = *now.lock().unwrap();
    sqlx::query(
        "INSERT INTO sessions \
             (token_hash, user_id, csrf_token, auth_version, created_at, last_seen_at, expires_at) \
         VALUES ($1, $2, $3, 1, $4, $4, $5)",
    )
    .bind(digest_hex(&token))
    .bind(user)
    .bind("a".repeat(64))
    .bind(at)
    .bind(at + time::Duration::seconds(86_400))
    .execute(&mut *in_flight)
    .await
    .unwrap();
    in_flight.commit().await.unwrap();

    revoke.await.unwrap().unwrap();
    assert!(
        store.validate(&token).await.unwrap().is_none(),
        "撤销返回后，排在它之前的创建必须已失效"
    );
    assert_eq!(session_count_for(&pool, user).await, 0);
}

/// 同一令牌被并发撤销：幂等，不报错，最终零行。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_revoke_of_same_token_is_idempotent() {
    let _g = SERIAL.lock().await;
    let pool = common::fresh_database(DB).await;
    let user = common::seed_user(&pool, "revoke-race").await;
    let now = shared_now();
    let store = Arc::new(session_store(pool.clone(), config(3600, 86_400, 100), &now));

    let token = store.create(user, 1).await.unwrap();
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..12 {
        let store = Arc::clone(&store);
        let token = token.clone();
        tasks.spawn(async move { store.revoke(&token).await });
    }
    while let Some(joined) = tasks.join_next().await {
        joined.expect("任务未 panic").expect("重复撤销不应报错");
    }

    assert!(store.validate(&token).await.unwrap().is_none());
    assert_eq!(session_count(&pool).await, 0, "同一令牌只应留下零行");
}
