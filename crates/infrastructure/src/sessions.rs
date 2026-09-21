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

pub struct InMemorySessionStore {
    config: SessionStoreConfig,
    clock: Box<dyn Fn() -> OffsetDateTime + Send + Sync>,
    entries: Mutex<Vec<SessionEntry>>,
    /// user_id → 该用户当前会话摘要（撤销全部用）。
    by_user: Mutex<HashMap<Uuid, Vec<String>>>,
}

impl InMemorySessionStore {
    pub fn new(
        config: SessionStoreConfig,
        clock: Box<dyn Fn() -> OffsetDateTime + Send + Sync>,
    ) -> Self {
        Self {
            config,
            clock,
            entries: Mutex::new(Vec::new()),
            by_user: Mutex::new(HashMap::new()),
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

    /// 清理过期；仍满时淘汰最久未活跃。
    fn enforce_capacity(&self, entries: &mut Vec<SessionEntry>, now: OffsetDateTime) {
        entries.retain(|e| !self.is_expired(&e.record, now));
        if entries.len() >= self.config.max_entries {
            entries.sort_by_key(|e| e.record.last_seen_at);
            let overflow = entries.len() + 1 - self.config.max_entries;
            entries.drain(..overflow);
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

        {
            let mut entries = self.entries.lock().unwrap();
            self.enforce_capacity(&mut entries, now);
            entries.push(SessionEntry {
                digest: digest.clone(),
                record: SessionRecord {
                    user_id,
                    csrf_token,
                    created_at: now,
                    last_seen_at: now,
                },
            });
        }
        self.by_user
            .lock()
            .unwrap()
            .entry(user_id)
            .or_default()
            .push(digest);
        Ok(token)
    }

    async fn validate(&self, token: &str) -> Result<Option<SessionRecord>, UseCaseError> {
        let digest = sha256_hex(token.as_bytes());
        let now = self.now();
        let mut entries = self.entries.lock().unwrap();
        if let Some(idx) = entries.iter().position(|e| e.digest == digest) {
            if self.is_expired(&entries[idx].record, now) {
                entries.remove(idx);
                return Ok(None);
            }
            entries[idx].record.last_seen_at = now;
            return Ok(Some(entries[idx].record.clone()));
        }
        Ok(None)
    }

    async fn revoke(&self, token: &str) -> Result<(), UseCaseError> {
        let digest = sha256_hex(token.as_bytes());
        let mut entries = self.entries.lock().unwrap();
        if let Some(idx) = entries.iter().position(|e| e.digest == digest) {
            let removed = entries.remove(idx);
            let mut by_user = self.by_user.lock().unwrap();
            if let Some(list) = by_user.get_mut(&removed.record.user_id) {
                list.retain(|d| *d != removed.digest);
                if list.is_empty() {
                    by_user.remove(&removed.record.user_id);
                }
            }
        }
        Ok(())
    }

    async fn revoke_all_for_user(&self, user_id: Uuid) -> Result<(), UseCaseError> {
        let digests: Vec<String> = self
            .by_user
            .lock()
            .unwrap()
            .remove(&user_id)
            .unwrap_or_default();
        if digests.is_empty() {
            return Ok(());
        }
        let mut entries = self.entries.lock().unwrap();
        entries.retain(|e| !digests.contains(&e.digest));
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
