//! 会话与 OAuth 尝试存储 + 安全随机源。
//!
//! 会话有两个适配器，共享同一套令牌形态（32 字节 hex）、SHA-256 摘要与
//! 空闲/绝对过期语义，区别只在状态放哪里：
//! - [`InMemorySessionStore`]：单实例内存，重启全部失效（测试与无库场景）；
//! - [`PostgresSessionStore`]：状态落库，重启后仍登录，且多个进程共享同一份会话。
//!
//! 其余共同约定：
//! - 服务端只保存令牌的 SHA-256 摘要；明文令牌仅在签发时返回一次。
//! - 有 TTL 与容量上限；容量上限是「清理 + 淘汰 + 插入」的原子更新。
//! - OAuth state 一次性消费：consume 即删除。

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use base64::Engine as _;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use time::OffsetDateTime;
use uuid::Uuid;

use application::error::UseCaseError;
use application::ports::{
    OAuthAttempt, OAuthAttemptStore, SecureRandom, SessionRecord, SessionStore,
};

fn sha256_hex(input: &[u8]) -> String {
    let digest = Sha256::digest(input);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// 32 字节不透明随机令牌（小写 hex，64 字符）。
///
/// 会话令牌与 CSRF token 用同一种形态；明文只在签发时返回一次，
/// 服务端只落 [`sha256_hex`] 摘要。两个会话适配器共用它，保证摘要算法一致。
fn opaque_token() -> Result<String, UseCaseError> {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf)
        .map_err(|e| UseCaseError::Repository(format!("系统随机源失败：{e}")))?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

/// 会话表上的适配器把 sqlx 技术错误统一映射为端口错误。
fn repo_err(e: sqlx::Error) -> UseCaseError {
    UseCaseError::Repository(format!("会话存储失败：{e}"))
}

/// 从用户索引里摘掉一个会话摘要；列表空了顺带删键，避免索引无限累积。
fn unindex(by_user: &mut HashMap<Uuid, Vec<String>>, user_id: Uuid, digest: &str) {
    if let Some(list) = by_user.get_mut(&user_id) {
        list.retain(|d| d != digest);
        if list.is_empty() {
            by_user.remove(&user_id);
        }
    }
}

/// 安全随机源：系统 CSPRNG；PKCE S256 = base64url(SHA-256(verifier)) 无填充。
pub struct SystemSecureRandom;

impl SecureRandom for SystemSecureRandom {
    fn token_hex(&self) -> Result<String, UseCaseError> {
        let mut buf = [0u8; 32];
        getrandom::fill(&mut buf)
            .map_err(|e| UseCaseError::Repository(format!("系统随机源失败：{e}")))?;
        Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
    }

    fn pkce_s256(&self, verifier: &str) -> Result<String, UseCaseError> {
        let digest = Sha256::digest(verifier.as_bytes());
        Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest))
    }
}

struct SessionEntry {
    digest: String,
    record: SessionRecord,
}

/// 会话存储参数。
pub struct SessionStoreConfig {
    /// 空闲过期（秒）。
    pub idle_secs: i64,
    /// 绝对过期（秒）。
    pub absolute_secs: i64,
    /// 容量上限；满时先清理过期，仍满则淘汰最久未活跃会话。
    pub max_entries: usize,
}

impl Default for SessionStoreConfig {
    fn default() -> Self {
        Self {
            idle_secs: 2 * 3600,
            absolute_secs: 7 * 24 * 3600,
            max_entries: 10_000,
        }
    }
}

/// 会话存储的全部可变状态。
///
/// `entries` 与 `by_user` 必须始终一致：放在**同一把锁**下更新，
/// 任意时刻都不会出现「entries 里已没有、索引里还在」或反过来的窗口。
#[derive(Default)]
struct SessionState {
    entries: Vec<SessionEntry>,
    /// user_id → 该用户当前会话摘要（撤销全部用）。
    by_user: HashMap<Uuid, Vec<String>>,
}

pub struct InMemorySessionStore {
    config: SessionStoreConfig,
    clock: Box<dyn Fn() -> OffsetDateTime + Send + Sync>,
    state: Mutex<SessionState>,
}

impl InMemorySessionStore {
    pub fn new(
        config: SessionStoreConfig,
        clock: Box<dyn Fn() -> OffsetDateTime + Send + Sync>,
    ) -> Self {
        Self {
            config,
            clock,
            state: Mutex::new(SessionState::default()),
        }
    }

    /// 生产默认（系统时钟）。
    pub fn with_defaults() -> Self {
        Self::new(
            SessionStoreConfig::default(),
            Box::new(OffsetDateTime::now_utc),
        )
    }

    fn now(&self) -> OffsetDateTime {
        (self.clock)()
    }

    fn is_expired(&self, record: &SessionRecord, now: OffsetDateTime) -> bool {
        let idle = time::Duration::seconds(self.config.idle_secs);
        let absolute = time::Duration::seconds(self.config.absolute_secs);
        now - record.last_seen_at > idle || now - record.created_at > absolute
    }

    /// 清理过期；仍满时淘汰最久未活跃，并同步摘除 `by_user` 索引。
    ///
    /// 调用方必须已持有 `state` 锁：淘汰与索引维护是一个不可分割的更新。
    fn enforce_capacity(&self, state: &mut SessionState, now: OffsetDateTime) {
        let SessionState { entries, by_user } = state;
        let mut evicted = Vec::new();
        let mut kept = Vec::with_capacity(entries.len());
        for entry in entries.drain(..) {
            if self.is_expired(&entry.record, now) {
                evicted.push(entry);
            } else {
                kept.push(entry);
            }
        }
        *entries = kept;
        if entries.len() >= self.config.max_entries {
            entries.sort_by_key(|e| e.record.last_seen_at);
            let overflow = entries.len() + 1 - self.config.max_entries;
            evicted.extend(entries.drain(..overflow));
        }
        for entry in &evicted {
            unindex(by_user, entry.record.user_id, &entry.digest);
        }
    }
}

#[async_trait]
impl SessionStore for InMemorySessionStore {
    async fn create(&self, user_id: Uuid, user_version: i64) -> Result<String, UseCaseError> {
        // 令牌生成需要系统随机；由应用层 SecureRandom 注入更纯粹，
        // 但存储自身也必须保证摘要在同一路径下计算，故内部直接生成。
        let token = opaque_token()?;
        let digest = sha256_hex(token.as_bytes());

        let now = self.now();
        let csrf_token = opaque_token()?;

        // 淘汰、插入与索引更新在同一把锁内完成：不会出现只更新了一半的中间态，
        // 并发的 revoke_all_for_user 也不可能漏掉刚插入的会话。
        let mut state = self.state.lock().unwrap();
        self.enforce_capacity(&mut state, now);
        state.entries.push(SessionEntry {
            digest: digest.clone(),
            record: SessionRecord {
                user_id,
                csrf_token,
                created_at: now,
                last_seen_at: now,
                user_version,
            },
        });
        state.by_user.entry(user_id).or_default().push(digest);
        Ok(token)
    }

    async fn validate(&self, token: &str) -> Result<Option<SessionRecord>, UseCaseError> {
        let digest = sha256_hex(token.as_bytes());
        let now = self.now();
        let mut state = self.state.lock().unwrap();
        if let Some(idx) = state.entries.iter().position(|e| e.digest == digest) {
            if self.is_expired(&state.entries[idx].record, now) {
                let removed = state.entries.remove(idx);
                unindex(&mut state.by_user, removed.record.user_id, &removed.digest);
                return Ok(None);
            }
            state.entries[idx].record.last_seen_at = now;
            return Ok(Some(state.entries[idx].record.clone()));
        }
        Ok(None)
    }

    async fn revoke(&self, token: &str) -> Result<(), UseCaseError> {
        let digest = sha256_hex(token.as_bytes());
        let mut state = self.state.lock().unwrap();
        if let Some(idx) = state.entries.iter().position(|e| e.digest == digest) {
            let removed = state.entries.remove(idx);
            unindex(&mut state.by_user, removed.record.user_id, &removed.digest);
        }
        Ok(())
    }

    async fn revoke_all_for_user(&self, user_id: Uuid) -> Result<(), UseCaseError> {
        let mut state = self.state.lock().unwrap();
        let digests = state.by_user.remove(&user_id).unwrap_or_default();
        if digests.is_empty() {
            return Ok(());
        }
        state.entries.retain(|e| !digests.contains(&e.digest));
        Ok(())
    }
}

/// 会话表的创建/批量撤销互斥锁键（与 persistence::IDENTITY_LOCK 同一手法，取不同键）。
///
/// `create` 的「清理过期 + 按需淘汰 + 插入」和 `revoke_all_for_user` 的整用户删除
/// 必须共用这把锁，否则撤销返回后，一个此前已开始、尚未提交的创建仍可能落库。
/// 事务级锁在提交/回滚时释放；`validate` 与单会话 `revoke` 不需要它。
///
/// 对 crate 外公开（`#[doc(hidden)]`）只为让集成测试能验证两者确实共用同一把锁。
#[doc(hidden)]
pub const SESSION_LOCK: (i32, i32) = (2048002, 1);

/// PostgreSQL 会话存储：状态落库，因此**重启后仍登录**。
///
/// 与 [`InMemorySessionStore`] 使用同一套令牌形态、摘要算法与过期语义，
/// 差别只在状态位置：
/// - 重启不清空；服务与运维进程（或多个实例）看到同一份会话；
/// - 另一个进程的 `revoke_all_for_user` 立刻生效，不再只靠 `users.version` 兜底；
/// - 代价是每次校验都要写一次 `last_seen_at`（空闲续期），并依赖数据库可用性。
///
/// 时钟由构造时注入：生产用系统时钟，测试可在不 sleep 的情况下推进过期。
pub struct PostgresSessionStore {
    pool: PgPool,
    config: SessionStoreConfig,
    clock: Box<dyn Fn() -> OffsetDateTime + Send + Sync>,
}

impl PostgresSessionStore {
    pub fn new(
        pool: PgPool,
        config: SessionStoreConfig,
        clock: Box<dyn Fn() -> OffsetDateTime + Send + Sync>,
    ) -> Self {
        Self {
            pool,
            config,
            clock,
        }
    }

    /// 生产默认（系统时钟、默认 TTL 与容量）。
    pub fn with_defaults(pool: PgPool) -> Self {
        Self::new(
            pool,
            SessionStoreConfig::default(),
            Box::new(OffsetDateTime::now_utc),
        )
    }

    fn now(&self) -> OffsetDateTime {
        (self.clock)()
    }

    /// 空闲过期下界：`last_seen_at >= now - idle` 才算有效。
    fn idle_cutoff(&self, now: OffsetDateTime) -> OffsetDateTime {
        now - time::Duration::seconds(self.config.idle_secs)
    }

    /// 删除全部已过期会话，返回删除行数。
    ///
    /// 创建/校验路径已顺带清理；这里是给运维用的显式入口（例如定时维护，
    /// 或在不触发会话创建的窗口里主动回收）。恢复流程需要的是**清空全部**
    /// 会话而不是只清过期行，走的是 `DELETE FROM sessions`（见 scripts/recovery.py）。
    pub async fn purge_expired(&self) -> Result<u64, UseCaseError> {
        let now = self.now();
        let idle_cutoff = self.idle_cutoff(now);
        // 过期 = 有效条件的取反：expires_at < now 或 last_seen_at < now - idle。
        let result = sqlx::query("DELETE FROM sessions WHERE expires_at < $1 OR last_seen_at < $2")
            .bind(now)
            .bind(idle_cutoff)
            .execute(&self.pool)
            .await
            .map_err(repo_err)?;
        Ok(result.rows_affected())
    }
}

#[async_trait]
impl SessionStore for PostgresSessionStore {
    async fn create(&self, user_id: Uuid, user_version: i64) -> Result<String, UseCaseError> {
        let token = opaque_token()?;
        let digest = sha256_hex(token.as_bytes());
        let csrf_token = opaque_token()?;
        let now = self.now();
        let idle_cutoff = self.idle_cutoff(now);
        let expires_at = now + time::Duration::seconds(self.config.absolute_secs);

        let mut tx = self.pool.begin().await.map_err(repo_err)?;
        // 先拿会话锁再动容量：让「计数—淘汰—插入」对其他创建者原子可见。
        sqlx::query("SELECT pg_advisory_xact_lock($1::int, $2::int)")
            .bind(SESSION_LOCK.0)
            .bind(SESSION_LOCK.1)
            .execute(&mut *tx)
            .await
            .map_err(repo_err)?;

        // 清理已过期（绝对 + 空闲），两个索引都能用上。
        sqlx::query("DELETE FROM sessions WHERE expires_at < $1 OR last_seen_at < $2")
            .bind(now)
            .bind(idle_cutoff)
            .execute(&mut *tx)
            .await
            .map_err(repo_err)?;

        // 仍满则淘汰最久未活跃；本次插入后不得超过 max_entries。
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions")
            .fetch_one(&mut *tx)
            .await
            .map_err(repo_err)?;
        let max_entries = i64::try_from(self.config.max_entries).unwrap_or(i64::MAX);
        let overflow = count.saturating_add(1).saturating_sub(max_entries);
        if overflow > 0 {
            // 并列 last_seen_at 用 token_hash 兜底排序，保证结果稳定。
            sqlx::query(
                "DELETE FROM sessions WHERE token_hash IN ( \
                     SELECT token_hash FROM sessions \
                     ORDER BY last_seen_at ASC, token_hash ASC LIMIT $1 \
                 )",
            )
            .bind(overflow)
            .execute(&mut *tx)
            .await
            .map_err(repo_err)?;
        }

        sqlx::query(
            "INSERT INTO sessions \
                 (token_hash, user_id, csrf_token, user_version, created_at, last_seen_at, expires_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(&digest)
        .bind(user_id)
        .bind(&csrf_token)
        .bind(user_version)
        .bind(now)
        .bind(now)
        .bind(expires_at)
        .execute(&mut *tx)
        .await
        .map_err(repo_err)?;

        tx.commit().await.map_err(repo_err)?;
        Ok(token)
    }

    async fn validate(&self, token: &str) -> Result<Option<SessionRecord>, UseCaseError> {
        let digest = sha256_hex(token.as_bytes());
        let now = self.now();
        let idle_cutoff = self.idle_cutoff(now);

        // 有效边界与内存实现逐字对齐（>=，不是 >）：过期/未知都不返回记录。
        // RETURNING 给出**更新后**的 last_seen_at（= now），与内存返回的续期结果一致。
        let row = sqlx::query(
            "UPDATE sessions SET last_seen_at = $2 \
             WHERE token_hash = $1 AND expires_at >= $2 AND last_seen_at >= $3 \
             RETURNING user_id, csrf_token, created_at, last_seen_at, user_version",
        )
        .bind(&digest)
        .bind(now)
        .bind(idle_cutoff)
        .fetch_optional(&self.pool)
        .await
        .map_err(repo_err)?;

        let Some(row) = row else {
            // 未知或刚过期：按主键精确清掉残留，避免过期行一直占位。
            sqlx::query(
                "DELETE FROM sessions WHERE token_hash = $1 \
                 AND (expires_at < $2 OR last_seen_at < $3)",
            )
            .bind(&digest)
            .bind(now)
            .bind(idle_cutoff)
            .execute(&self.pool)
            .await
            .map_err(repo_err)?;
            return Ok(None);
        };

        Ok(Some(SessionRecord {
            user_id: row.try_get("user_id").map_err(repo_err)?,
            csrf_token: row.try_get("csrf_token").map_err(repo_err)?,
            created_at: row.try_get("created_at").map_err(repo_err)?,
            last_seen_at: row.try_get("last_seen_at").map_err(repo_err)?,
            user_version: row.try_get("user_version").map_err(repo_err)?,
        }))
    }

    async fn revoke(&self, token: &str) -> Result<(), UseCaseError> {
        let digest = sha256_hex(token.as_bytes());
        // 幂等：未知/已撤销令牌同样返回成功，重复撤销不报错。
        sqlx::query("DELETE FROM sessions WHERE token_hash = $1")
            .bind(&digest)
            .execute(&self.pool)
            .await
            .map_err(repo_err)?;
        Ok(())
    }

    async fn revoke_all_for_user(&self, user_id: Uuid) -> Result<(), UseCaseError> {
        // 与 create 共用会话锁，形成同一顺序：撤销要么排在「已开始的创建」之后
        // （于是那个会话也被删掉），要么排在它之前（之后的创建属于撤销后的新登录）。
        // 不共用锁时，上一条路径会漏掉「锁内计数完但尚未插入」的会话。
        let mut tx = self.pool.begin().await.map_err(repo_err)?;
        sqlx::query("SELECT pg_advisory_xact_lock($1::int, $2::int)")
            .bind(SESSION_LOCK.0)
            .bind(SESSION_LOCK.1)
            .execute(&mut *tx)
            .await
            .map_err(repo_err)?;
        sqlx::query("DELETE FROM sessions WHERE user_id = $1")
            .bind(user_id)
            .execute(&mut *tx)
            .await
            .map_err(repo_err)?;
        tx.commit().await.map_err(repo_err)?;
        Ok(())
    }
}

/// OAuth 尝试存储参数。
pub struct AttemptStoreConfig {
    pub ttl_secs: i64,
    pub max_entries: usize,
}

impl Default for AttemptStoreConfig {
    fn default() -> Self {
        Self {
            ttl_secs: 600,
            max_entries: 1_000,
        }
    }
}

pub struct InMemoryOAuthAttemptStore {
    config: AttemptStoreConfig,
    clock: Box<dyn Fn() -> OffsetDateTime + Send + Sync>,
    attempts: Mutex<HashMap<String, OAuthAttempt>>,
}

impl InMemoryOAuthAttemptStore {
    pub fn new(
        config: AttemptStoreConfig,
        clock: Box<dyn Fn() -> OffsetDateTime + Send + Sync>,
    ) -> Self {
        Self {
            config,
            clock,
            attempts: Mutex::new(HashMap::new()),
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(
            AttemptStoreConfig::default(),
            Box::new(OffsetDateTime::now_utc),
        )
    }
}

#[async_trait]
impl OAuthAttemptStore for InMemoryOAuthAttemptStore {
    async fn save(&self, state: String, attempt: OAuthAttempt) -> Result<(), UseCaseError> {
        let now = (self.clock)();
        let mut attempts = self.attempts.lock().unwrap();
        attempts.retain(|_, a| now - a.created_at <= time::Duration::seconds(self.config.ttl_secs));
        if attempts.len() >= self.config.max_entries {
            return Err(UseCaseError::External(
                "登录尝试存储已满，请稍后重试".into(),
            ));
        }
        attempts.insert(state, attempt);
        Ok(())
    }

    async fn consume(&self, state: &str) -> Result<Option<OAuthAttempt>, UseCaseError> {
        let now = (self.clock)();
        let mut attempts = self.attempts.lock().unwrap();
        match attempts.remove(state) {
            Some(attempt) => {
                if now - attempt.created_at > time::Duration::seconds(self.config.ttl_secs) {
                    Ok(None)
                } else {
                    Ok(Some(attempt))
                }
            }
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mutable_now() -> (
        std::sync::Arc<std::sync::Mutex<OffsetDateTime>>,
        impl Fn() -> OffsetDateTime + Send + Sync + 'static,
    ) {
        let now = std::sync::Arc::new(std::sync::Mutex::new(OffsetDateTime::now_utc()));
        let reader = now.clone();
        (now, move || *reader.lock().unwrap())
    }

    /// (entries 条数, by_user 里摘要总数)：不变量是两者始终相等。
    fn index_size(store: &InMemorySessionStore) -> (usize, usize) {
        let state = store.state.lock().unwrap();
        let indexed: usize = state.by_user.values().map(Vec::len).sum();
        (state.entries.len(), indexed)
    }

    #[tokio::test]
    async fn capacity_eviction_and_expiry_keep_user_index_pruned() {
        let (now, clock) = mutable_now();
        let store = InMemorySessionStore::new(
            SessionStoreConfig {
                idle_secs: 10_000,
                absolute_secs: 10_000,
                max_entries: 2,
            },
            Box::new(clock),
        );
        let u1 = Uuid::now_v7();
        let u2 = Uuid::now_v7();
        let u3 = Uuid::now_v7();

        let t1 = store.create(u1, 1).await.unwrap();
        *now.lock().unwrap() += time::Duration::seconds(5);
        let t2 = store.create(u2, 1).await.unwrap();
        *now.lock().unwrap() += time::Duration::seconds(5);
        let _t3 = store.create(u3, 1).await.unwrap();

        // 容量淘汰 t1：u1 的索引项必须一起消失，否则 by_user 单调增长。
        assert!(store.validate(&t1).await.unwrap().is_none());
        let (entries, indexed) = index_size(&store);
        assert_eq!(entries, 2);
        assert_eq!(indexed, 2, "淘汰后索引摘要数必须与 entries 一致");
        assert!(!store.state.lock().unwrap().by_user.contains_key(&u1));

        // 过期删除同样要摘索引。
        *now.lock().unwrap() += time::Duration::seconds(10_001);
        assert!(store.validate(&t2).await.unwrap().is_none());
        let (entries, indexed) = index_size(&store);
        assert_eq!(entries, 1);
        assert_eq!(indexed, 1, "过期删除后索引摘要数必须与 entries 一致");
        assert!(!store.state.lock().unwrap().by_user.contains_key(&u2));
    }

    #[tokio::test]
    async fn revoke_all_updates_both_structures_consistently() {
        let store = InMemorySessionStore::with_defaults();
        let user = Uuid::now_v7();
        let other = Uuid::now_v7();

        let t1 = store.create(user, 1).await.unwrap();
        let t2 = store.create(user, 1).await.unwrap();
        let t3 = store.create(other, 1).await.unwrap();

        store.revoke_all_for_user(user).await.unwrap();

        // 撤销后 entries 与索引必须同时只剩 other 的会话。
        let (entries, indexed) = index_size(&store);
        assert_eq!(entries, 1);
        assert_eq!(indexed, 1);
        assert!(store.validate(&t1).await.unwrap().is_none());
        assert!(store.validate(&t2).await.unwrap().is_none());
        assert!(store.validate(&t3).await.unwrap().is_some());
        assert!(!store.state.lock().unwrap().by_user.contains_key(&user));
    }
}
