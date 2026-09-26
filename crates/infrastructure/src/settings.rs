//! settings 的 site 分组存取。
//!
//! oauth 分组（受保护配置）在 `oauth.rs` 的 `PostgresOAuthConfigStore`，
//! 与这里物理上就是不同的 key、不同的端口与授权路径，互不影响。

use application::error::UseCaseError;
use application::ports::{
    MediaContentKind, SITE_MEDIA_CONTENT_ID, SaveOutcome, SettingsStore, SiteSettingsRecord,
    SiteSettingsValue, ThemeSettingsRecord, ThemeSettingsStore,
};
use async_trait::async_trait;
use sqlx::PgPool;
use time::OffsetDateTime;

use crate::persistence::{media_ids_for, sync_media_refs};

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
    ///
    /// 站点 logo 的 id 存在 `site` 值的 JSONB 里（用户确认的取舍），但引用关系
    /// 仍写进 `content_media_refs`，且与配置行**同一事务**：删除保护与公开来源
    /// 继续以引用表为唯一判据。JSON 里的 id 没有 FK 兜底，`ready` 校验由
    /// `sync_media_refs` 在锁内完成——指向不可用资产的保存整次回滚。
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
            "logo_media_id": value.logo_media_id,
        });
        let mut tx = self.pool.begin().await.map_err(map_repo_error)?;
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
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_repo_error)?;

        let Some(version) = new_version else {
            tx.commit().await.map_err(map_repo_error)?;
            return Ok(SaveOutcome::StaleConflict);
        };
        // 站点 logo 的引用行与配置行同事务整体替换（None 时清空引用）。
        sync_media_refs(
            &mut tx,
            MediaContentKind::Site,
            SITE_MEDIA_CONTENT_ID,
            &media_ids_for(&[], value.logo_media_id),
        )
        .await?;
        tx.commit().await.map_err(map_repo_error)?;
        Ok(SaveOutcome::Saved {
            new_version: version,
        })
    }
}

fn map_repo_error(e: sqlx::Error) -> UseCaseError {
    UseCaseError::Repository(e.to_string())
}

#[async_trait]
impl ThemeSettingsStore for PostgresSettingsStore {
    async fn find_theme(&self) -> Result<Option<ThemeSettingsRecord>, UseCaseError> {
        let row: Option<(String, i64)> =
            sqlx::query_as("SELECT value->>'slug', version FROM settings WHERE key = 'theme'")
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| UseCaseError::Repository(e.to_string()))?;
        Ok(row.map(|(slug, version)| ThemeSettingsRecord { slug, version }))
    }

    async fn save_theme(
        &self,
        slug: &str,
        expected_version: i64,
        now: OffsetDateTime,
    ) -> Result<SaveOutcome, UseCaseError> {
        let value = serde_json::json!({ "schema_version": 1, "slug": slug });
        let new_version: Option<i64> = sqlx::query_scalar(
            r#"
            INSERT INTO settings (key, value, version, updated_at)
            VALUES ('theme', $1, 1, $2)
            ON CONFLICT (key) DO UPDATE SET
                value = EXCLUDED.value,
                version = settings.version + 1,
                updated_at = EXCLUDED.updated_at
            WHERE settings.version = $3
            RETURNING version
            "#,
        )
        .bind(value)
        .bind(now)
        .bind(expected_version)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| UseCaseError::Repository(e.to_string()))?;
        Ok(match new_version {
            Some(new_version) => SaveOutcome::Saved { new_version },
            None => SaveOutcome::StaleConflict,
        })
    }
}
