//! Argon2id 口令哈希适配器。
//!
//! 参数固定为 OWASP 推荐的 Argon2id 最小配置（m=19456 KiB、t=2、p=1、32 字节输出，
//! 见 `argon2::Params::DEFAULT`）。参数写入 PHC 字符串自描述，将来提高后
//! [`PasswordHasher::needs_rehash`] 会在成功登录时透明升级旧哈希。
//!
//! 每次哈希约占用 19 MiB 内存：用信号量限制并发，避免并发登录把进程内存打满
//! （默认 4 路并发 ≈ 76 MiB 峰值）。KDF 在阻塞线程池执行，不占异步执行器。

use std::sync::Arc;

use application::error::UseCaseError;
use application::ports::PasswordHasher;
use argon2::password_hash::{PasswordHash, PasswordHasher as _, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use async_trait::async_trait;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// 默认并发上限；每次哈希约 19 MiB，4 路 ≈ 76 MiB。
pub const DEFAULT_MAX_CONCURRENCY: usize = 4;

/// 盐长度（字节）；16 字节 = 128 位，远超唯一性所需的量级。
const SALT_LEN: usize = 16;

pub struct Argon2PasswordHasher {
    params: Params,
    permits: Arc<Semaphore>,
}

impl Argon2PasswordHasher {
    pub fn new(params: Params, max_concurrency: usize) -> Self {
        Self {
            params,
            permits: Arc::new(Semaphore::new(max_concurrency.max(1))),
        }
    }

    /// 生产默认：Argon2id + OWASP 最小参数。
    pub fn with_defaults() -> Self {
        Self::new(Params::DEFAULT, DEFAULT_MAX_CONCURRENCY)
    }

    fn context(&self) -> Argon2<'static> {
        Argon2::new(Algorithm::Argon2id, Version::V0x13, self.params.clone())
    }

    async fn acquire(&self) -> Result<OwnedSemaphorePermit, UseCaseError> {
        self.permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| UseCaseError::Repository("口令哈希并发信号量已关闭".into()))
    }
}

#[async_trait]
impl PasswordHasher for Argon2PasswordHasher {
    async fn hash(&self, password: &str) -> Result<String, UseCaseError> {
        let permit = self.acquire().await?;
        let mut salt_bytes = [0u8; SALT_LEN];
        getrandom::fill(&mut salt_bytes)
            .map_err(|e| UseCaseError::Repository(format!("系统随机源失败：{e}")))?;
        let context = self.context();
        let password = password.to_string();
        tokio::task::spawn_blocking(move || {
            // permit 移入闭包：并发额度覆盖整个阻塞计算，函数返回才释放。
            let _permit = permit;
            let salt = SaltString::encode_b64(&salt_bytes)
                .map_err(|e| UseCaseError::Repository(format!("盐编码失败：{e}")))?;
            context
                .hash_password(password.as_bytes(), &salt)
                .map(|hash| hash.to_string())
                .map_err(|e| UseCaseError::Repository(format!("口令哈希失败：{e}")))
        })
        .await
        .map_err(|e| UseCaseError::Repository(format!("口令哈希任务失败：{e}")))?
    }

    async fn verify(&self, password: &str, phc_hash: &str) -> Result<bool, UseCaseError> {
        let permit = self.acquire().await?;
        let context = self.context();
        let password = password.to_string();
        let phc_hash = phc_hash.to_string();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let parsed = PasswordHash::new(&phc_hash)
                .map_err(|e| UseCaseError::Repository(format!("口令哈希格式非法：{e}")))?;
            match context.verify_password(password.as_bytes(), &parsed) {
                Ok(()) => Ok(true),
                // 唯一预期的不匹配错误；其余（参数非法等）按存储损坏处理。
                Err(argon2::password_hash::Error::Password) => Ok(false),
                Err(e) => Err(UseCaseError::Repository(format!("口令校验失败：{e}"))),
            }
        })
        .await
        .map_err(|e| UseCaseError::Repository(format!("口令校验任务失败：{e}")))?
    }

    fn needs_rehash(&self, phc_hash: &str) -> bool {
        let Ok(parsed) = PasswordHash::new(phc_hash) else {
            // 无法解析的存储值一律视为需要重设（重新登录成功时会用当前策略覆盖）。
            return true;
        };
        if parsed.algorithm.as_str() != "argon2id" {
            return true;
        }
        if parsed.version != Some(Version::V0x13.into()) {
            return true;
        }
        let expected = &self.params;
        let memory = parsed.params.get("m").and_then(|v| v.decimal().ok());
        let iterations = parsed.params.get("t").and_then(|v| v.decimal().ok());
        let parallelism = parsed.params.get("p").and_then(|v| v.decimal().ok());
        memory != Some(expected.m_cost())
            || iterations != Some(expected.t_cost())
            || parallelism != Some(expected.p_cost())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hasher() -> Argon2PasswordHasher {
        // 测试用小参数（m=8 是库允许的最小值），避免单元测试引入真实内存开销。
        Argon2PasswordHasher::new(Params::new(8, 1, 1, None).unwrap(), 2)
    }

    #[tokio::test]
    async fn hash_and_verify_round_trip() {
        let hasher = hasher();
        let phc = hasher.hash("correct horse battery staple").await.unwrap();
        assert!(phc.starts_with("$argon2id$"), "{phc}");
        assert!(
            hasher
                .verify("correct horse battery staple", &phc)
                .await
                .unwrap()
        );
        assert!(
            !hasher
                .verify("wrong password entirely", &phc)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn each_hash_uses_a_fresh_salt() {
        let hasher = hasher();
        let first = hasher.hash("same password value").await.unwrap();
        let second = hasher.hash("same password value").await.unwrap();
        assert_ne!(first, second, "相同明文必须得到不同 PHC（随机盐）");
    }

    /// 把生产参数钉进测试：crate 升级或有人改常量时必须在这里显式确认。
    #[tokio::test]
    async fn production_defaults_pin_owasp_argon2id_parameters() {
        let hasher = Argon2PasswordHasher::with_defaults();
        let phc = hasher.hash("some password value").await.unwrap();
        let parsed = PasswordHash::new(&phc).unwrap();

        assert_eq!(parsed.algorithm.as_str(), "argon2id");
        assert_eq!(parsed.version, Some(Version::V0x13.into()));
        assert_eq!(
            parsed.params.get("m").and_then(|v| v.decimal().ok()),
            Some(19 * 1024),
            "内存成本必须是 19456 KiB"
        );
        assert_eq!(
            parsed.params.get("t").and_then(|v| v.decimal().ok()),
            Some(2),
            "迭代次数必须是 2"
        );
        assert_eq!(
            parsed.params.get("p").and_then(|v| v.decimal().ok()),
            Some(1),
            "并行度必须是 1"
        );
        assert_eq!(
            parsed.hash.as_ref().map(|output| output.len()),
            Some(32),
            "输出必须是 32 字节"
        );
        assert!(parsed.salt.is_some(), "必须带盐");
        assert_eq!(DEFAULT_MAX_CONCURRENCY, 4, "并发上限变化需重新评估内存峰值");
    }

    #[test]
    fn needs_rehash_detects_weaker_and_foreign_parameters() {
        let strict = Argon2PasswordHasher::with_defaults();
        // 默认参数生成的哈希不应触发升级。
        let current = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(strict.hash("some password here"))
            .unwrap();
        assert!(!strict.needs_rehash(&current));

        for weak in [
            // m 低于策略
            "$argon2id$v=19$m=8,t=2,p=1$c2FsdHNhbHRzYWx0c2E$3n0lURsRZ0Y1pExsVQmT5r4qK6yQ0m8v9n4p1r7t2w8",
            // 次数低于策略
            "$argon2id$v=19$m=19456,t=1,p=1$c2FsdHNhbHRzYWx0c2E$3n0lURsRZ0Y1pExsVQmT5r4qK6yQ0m8v9n4p1r7t2w8",
            // 非 Argon2id
            "$argon2i$v=19$m=19456,t=2,p=1$c2FsdHNhbHRzYWx0c2E$3n0lURsRZ0Y1pExsVQmT5r4qK6yQ0m8v9n4p1r7t2w8",
            // 完全无法解析
            "not-a-phc-string",
        ] {
            assert!(strict.needs_rehash(weak), "应判定需要升级：{weak}");
        }
    }
}
