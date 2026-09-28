//! Deployment-facing database handle. SQLx connections stay inside adapters.
use application::UseCaseError;
use sqlx::PgPool;

#[derive(Clone, Debug)]
pub struct Database {
    pub(crate) pool: PgPool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoolSnapshot {
    pub connections: u32,
    pub idle_connections: usize,
    pub max_connections: u32,
}

impl Database {
    /// Local counters only; collecting metrics never acquires a connection.
    pub fn pool_snapshot(&self) -> PoolSnapshot {
        PoolSnapshot {
            connections: self.pool.size(),
            idle_connections: self.pool.num_idle(),
            max_connections: self.pool.options().get_max_connections(),
        }
    }

    pub async fn close(&self) {
        self.pool.close().await;
    }

    /// Read the PostgreSQL recovery marker; the caller decides startup policy.
    pub async fn is_recovery_isolated(&self) -> Result<bool, UseCaseError> {
        let comment: Option<String> = sqlx::query_scalar(
            "SELECT shobj_description(oid,'pg_database') FROM pg_database WHERE datname=current_database()",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|error| UseCaseError::Repository(format!("读取恢复隔离标记失败：{error}")))?;
        Ok(comment.is_some_and(|comment| comment.starts_with("blog:recovery-isolated:")))
    }
}

/// Connection failures are opaque to the composition root.
#[derive(Debug)]
pub struct DatabaseError(pub(crate) sqlx::Error);

impl std::fmt::Display for DatabaseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, formatter)
    }
}

impl std::error::Error for DatabaseError {}
