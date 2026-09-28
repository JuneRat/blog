//! Connection policy shared by runtime and CLI pools. Retries only establish
//! connections; this module never retries SQL statements or transactions.
use std::{future::Future, time::Duration};

use sqlx::{
    PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};

#[derive(Clone, Debug)]
pub struct DatabasePoolConfig {
    pub max_connections: u32,
    pub min_connections: u32,
    pub acquire_timeout_ms: u64,
    pub idle_timeout_secs: u64,
    pub max_lifetime_secs: u64,
    /// Zero preserves the server/role/DSN setting, including an existing limit.
    pub statement_timeout_ms: u64,
    pub lock_timeout_ms: u64,
    pub idle_in_transaction_timeout_ms: u64,
    pub connect_retries: u32,
    pub connect_retry_backoff_ms: u64,
}

impl Default for DatabasePoolConfig {
    fn default() -> Self {
        Self {
            max_connections: 5,
            min_connections: 0,
            acquire_timeout_ms: 5_000,
            idle_timeout_secs: 600,
            max_lifetime_secs: 1_800,
            statement_timeout_ms: 0,
            lock_timeout_ms: 0,
            idle_in_transaction_timeout_ms: 0,
            connect_retries: 3,
            connect_retry_backoff_ms: 250,
        }
    }
}

impl DatabasePoolConfig {
    pub fn validate(&self) -> Result<(), String> {
        for (name, value, min, max) in [
            ("max_connections", self.max_connections as u64, 1, 1_000),
            (
                "min_connections",
                self.min_connections as u64,
                0,
                self.max_connections as u64,
            ),
            ("acquire_timeout_ms", self.acquire_timeout_ms, 1, 120_000),
            ("idle_timeout_secs", self.idle_timeout_secs, 0, 86_400),
            ("max_lifetime_secs", self.max_lifetime_secs, 0, 86_400),
            (
                "statement_timeout_ms",
                self.statement_timeout_ms,
                0,
                86_400_000,
            ),
            ("lock_timeout_ms", self.lock_timeout_ms, 0, 86_400_000),
            (
                "idle_in_transaction_timeout_ms",
                self.idle_in_transaction_timeout_ms,
                0,
                86_400_000,
            ),
            ("connect_retries", self.connect_retries as u64, 0, 10),
            (
                "connect_retry_backoff_ms",
                self.connect_retry_backoff_ms,
                1,
                5_000,
            ),
        ] {
            if !(min..=max).contains(&value) {
                return Err(format!("database.{name} 必须在 {min}..={max} 范围内"));
            }
        }
        Ok(())
    }
}

pub async fn connect(url: &str) -> Result<crate::Database, crate::DatabaseError> {
    connect_with_config(url, &DatabasePoolConfig::default()).await
}

pub async fn connect_with_config(
    url: &str,
    config: &DatabasePoolConfig,
) -> Result<crate::Database, crate::DatabaseError> {
    connect_pool(url, config)
        .await
        .map(|pool| crate::Database { pool })
        .map_err(crate::DatabaseError)
}

async fn connect_pool(url: &str, config: &DatabasePoolConfig) -> Result<PgPool, sqlx::Error> {
    config
        .validate()
        .map_err(|error| sqlx::Error::Configuration(error.into()))?;
    let mut options: PgConnectOptions = url.parse()?;
    // Startup parameters apply to every connection, including replacements.
    // Unspecified limits do not overwrite role defaults or URL options.
    for (name, value) in [
        ("statement_timeout", config.statement_timeout_ms),
        ("lock_timeout", config.lock_timeout_ms),
        (
            "idle_in_transaction_session_timeout",
            config.idle_in_transaction_timeout_ms,
        ),
    ] {
        if value != 0 {
            options = options.options([(name, value.to_string())]);
        }
    }
    let seconds = |value| (value != 0).then(|| Duration::from_secs(value));
    let pool = PgPoolOptions::new()
        .max_connections(config.max_connections)
        .min_connections(config.min_connections)
        .acquire_timeout(Duration::from_millis(config.acquire_timeout_ms))
        .idle_timeout(seconds(config.idle_timeout_secs))
        .max_lifetime(seconds(config.max_lifetime_secs));
    retry_connect(config, || pool.clone().connect_with(options.clone())).await
}

fn transient(error: &sqlx::Error) -> bool {
    match error {
        sqlx::Error::PoolTimedOut => true,
        sqlx::Error::Io(error) => matches!(
            error.kind(),
            std::io::ErrorKind::ConnectionRefused
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::NotConnected
                | std::io::ErrorKind::TimedOut
                | std::io::ErrorKind::UnexpectedEof
                | std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::AddrNotAvailable
        ),
        sqlx::Error::Database(error) => error.code().is_some_and(|code| {
            code.starts_with("08") || matches!(code.as_ref(), "57P03" | "53300")
        }),
        _ => false,
    }
}

async fn retry_connect<T, F, Fut>(
    config: &DatabasePoolConfig,
    mut connect: F,
) -> Result<T, sqlx::Error>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, sqlx::Error>>,
{
    for attempt in 0..=config.connect_retries {
        match connect().await {
            Ok(value) => return Ok(value),
            Err(error) if attempt < config.connect_retries && transient(&error) => {
                let delay_ms = config
                    .connect_retry_backoff_ms
                    .saturating_mul(1 << attempt)
                    .min(5_000);
                // Never log connection options or error text containing DSN secrets.
                tracing::warn!(
                    retry = attempt + 1,
                    delay_ms,
                    "数据库连接暂不可用，等待重试"
                );
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("bounded retry loop returns its last result")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn retries_are_bounded_and_permanent_errors_are_not_retried() {
        let config = DatabasePoolConfig {
            connect_retries: 2,
            connect_retry_backoff_ms: 1,
            ..Default::default()
        };
        let mut calls = 0;
        let result = retry_connect(&config, || {
            calls += 1;
            std::future::ready(if calls == 3 {
                Ok(42)
            } else {
                Err(sqlx::Error::PoolTimedOut)
            })
        })
        .await;
        assert_eq!(result.unwrap(), 42);
        assert_eq!(calls, 3);
        let mut calls = 0;
        let result: Result<(), _> = retry_connect(&config, || {
            calls += 1;
            std::future::ready(Err(sqlx::Error::PoolTimedOut))
        })
        .await;
        assert!(result.is_err());
        assert_eq!(calls, 3);
        let mut calls = 0;
        let result: Result<(), _> = retry_connect(&config, || {
            calls += 1;
            std::future::ready(Err(sqlx::Error::Configuration(
                "invalid configuration".into(),
            )))
        })
        .await;
        assert!(result.is_err());
        assert_eq!(calls, 1);
    }
}
