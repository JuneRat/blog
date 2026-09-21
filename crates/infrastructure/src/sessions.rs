//! 单实例内存会话与 OAuth 尝试存储 + 安全随机源。
//!
//! - 有 TTL 与容量上限；重启全部失效（明确行为，不承诺跨重启保持登录）。
//! - 服务端只保存令牌的 SHA-256 摘要；明文令牌仅在签发时返回一次。
//! - state 一次性消费：consume 即删除。

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use base64::Engine as _;
use sha2::{Digest, Sha256};
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
    async fn create(&self, user_id: Uuid) -> Result<String, UseCaseError> {
        // 令牌生成需要系统随机；由应用层 SecureRandom 注入更纯粹，
        // 但存储自身也必须保证摘要在同一路径下计算，故内部直接生成。
        let mut buf = [0u8; 32];
        getrandom::fill(&mut buf)
            .map_err(|e| UseCaseError::Repository(format!("系统随机源失败：{e}")))?;
        let token = buf.iter().map(|b| format!("{b:02x}")).collect::<String>();
        let digest = sha256_hex(token.as_bytes());

        let now = self.now();
        let csrf_token = {
            let mut c = [0u8; 32];
            getrandom::fill(&mut c)
                .map_err(|e| UseCaseError::Repository(format!("系统随机源失败：{e}")))?;
            c.iter().map(|b| format!("{b:02x}")).collect::<String>()
        };

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

        let t1 = store.create(u1).await.unwrap();
        *now.lock().unwrap() += time::Duration::seconds(5);
        let t2 = store.create(u2).await.unwrap();
        *now.lock().unwrap() += time::Duration::seconds(5);
        let _t3 = store.create(u3).await.unwrap();

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

        let t1 = store.create(user).await.unwrap();
        let t2 = store.create(user).await.unwrap();
        let t3 = store.create(other).await.unwrap();

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
