//! settings 的 site 分组存取。
//!
//! oauth 分组（受保护配置）在 `oauth.rs` 的 `PostgresOAuthConfigStore`，
//! 与这里物理上就是不同的 key、不同的端口与授权路径，互不影响。

use application::error::UseCaseError;
use application::ports::{SaveOutcome, SettingsStore, SiteSettingsRecord, SiteSettingsValue};
use async_trait::async_trait;
use sqlx::PgPool;
use time::OffsetDateTime;

/// site 配置存 settings（key='site'，value 为 JSONB 对象，含 schema_version）。
pub struct PostgresSettingsStore {
    pool: PgPool,
}

impl PostgresSettingsStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl SettingsStore for PostgresSettingsStore {
    async fn find_site(&self) -> Result<Option<SiteSettingsRecord>, UseCaseError> {
        let row: Option<(sqlx::types::Json<SiteSettingsValue>, i64)> =
            sqlx::query_as("SELECT value, version FROM settings WHERE key = 'site'")
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| UseCaseError::Repository(e.to_string()))?;
        Ok(row.map(|(value, version)| SiteSettingsRecord {
            value: value.0,
            version,
        }))
    }

    /// UPSERT + 版本 CAS：插入路径带出版本 1；更新路径要求版本匹配，
    /// 否则 0 行受影响（StaleConflict），不覆盖并发写入。
    async fn save_site(
        &self,
        value: &SiteSettingsValue,
        expected_version: i64,
        now: OffsetDateTime,
    ) -> Result<SaveOutcome, UseCaseError> {
        let stored = serde_json::json!({
            "schema_version": 1,
            "title": value.title,
            "description": value.description,
        });
        let new_version: Option<i64> = sqlx::query_scalar(
            r#"
            INSERT INTO settings (key, value, version, updated_at)
            VALUES ('site', $1, 1, $2)
            ON CONFLICT (key) DO UPDATE SET
                value = EXCLUDED.value,
                version = settings.version + 1,
                updated_at = EXCLUDED.updated_at
            WHERE settings.version = $3
            RETURNING version
            "#,
        )
        .bind(stored)
        .bind(now)
        .bind(expected_version)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| UseCaseError::Repository(e.to_string()))?;

        match new_version {
            Some(version) => Ok(SaveOutcome::Saved {
                new_version: version,
            }),
            None => Ok(SaveOutcome::StaleConflict),
        }
    }
}
