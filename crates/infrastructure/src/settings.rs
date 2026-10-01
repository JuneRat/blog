//! settings 的 site 分组存取。
//!
//! oauth 分组（受保护配置）在 `oauth.rs` 的 `PostgresOAuthConfigStore`，
//! 与这里物理上就是不同的 key、不同的端口与授权路径，互不影响。

use application::error::UseCaseError;
use application::ports::{
    MediaContentKind, SITE_MEDIA_CONTENT_ID, SaveOutcome, SettingsReadObserver, SettingsStore,
    SiteSettingsReadOutcome, SiteSettingsRecord, SiteSettingsValue, ThemeSettingsRecord,
    ThemeSettingsStore,
};
use async_trait::async_trait;
use sqlx::PgPool;
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use time::OffsetDateTime;

use crate::audit::record_change;
use crate::persistence::{media_ids_for, sync_media_refs};

/// site 配置存 settings（key='site'，value 为 JSONB 对象，含 schema_version）。
pub struct PostgresSettingsStore {
    pool: PgPool,
    read_health: Mutex<ReadHealth>,
    observer: Option<Arc<dyn SettingsReadObserver>>,
}

#[derive(Default)]
struct ReadHealth {
    degraded: bool,
    last_warning: Option<Instant>,
}

impl PostgresSettingsStore {
    pub fn new(database: crate::Database) -> Self {
        let pool = database.pool;
        Self {
            pool,
            read_health: Mutex::default(),
            observer: None,
        }
    }

    pub fn with_read_observer(mut self, observer: Arc<dyn SettingsReadObserver>) -> Self {
        self.observer = Some(observer);
        self
    }

    fn observe_read(&self, outcome: SiteSettingsReadOutcome) {
        let mut health = self
            .read_health
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let failed = outcome == SiteSettingsReadOutcome::Failed;
        let recovered = health.degraded && !failed;
        if failed {
            let now = Instant::now();
            if health
                .last_warning
                .is_none_or(|last| now.duration_since(last) >= Duration::from_secs(60))
            {
                tracing::warn!("读取站点设置失败，公开页面可能使用回退值");
                health.last_warning = Some(now);
            }
        } else if recovered {
            tracing::info!("站点设置读取已恢复");
            health.last_warning = None;
        }
        health.degraded = failed;
        if let Some(observer) = &self.observer {
            observer.observe_site_read(outcome, recovered);
        }
    }
}

#[async_trait]
impl SettingsStore for PostgresSettingsStore {
    async fn find_site(&self) -> Result<Option<SiteSettingsRecord>, UseCaseError> {
        let result = sqlx::query_as("SELECT value, version FROM settings WHERE key = 'site'")
            .fetch_optional(&self.pool)
            .await;
        let outcome = match &result {
            Ok(Some(_)) => SiteSettingsReadOutcome::Configured,
            Ok(None) => SiteSettingsReadOutcome::Missing,
            Err(_) => SiteSettingsReadOutcome::Failed,
        };
        self.observe_read(outcome);
        let row: Option<(sqlx::types::Json<SiteSettingsValue>, i64)> =
            result.map_err(|e| UseCaseError::Repository(e.to_string()))?;
        Ok(row.map(|(value, version)| SiteSettingsRecord {
            value: value.0,
            version,
        }))
    }

    /// UPSERT + 版本 CAS：插入路径带出版本 1；更新路径要求版本匹配，
    /// 否则 0 行受影响（StaleConflict），不覆盖并发写入。
    ///
    /// logo ID 存在 site JSONB 中，media_refs 与配置行在同一事务同步。
    /// 新引用要求媒体存在且未软删除，原值可以保留历史引用；校验失败整体回滚。
    async fn save_site(
        &self,
        value: &SiteSettingsValue,
        expected_version: i64,
        now: OffsetDateTime,
        audit_actor: application::audit::AuditContext,
    ) -> Result<SaveOutcome, UseCaseError> {
        let stored = serde_json::json!({
            "schema_version": 1,
            "navigation": value.navigation,
            "home_page_size": value.home_page_size,
            "time_zone": value.time_zone,
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
        record_change(
            &mut tx,
            audit_actor,
            "settings.site",
            "settings",
            "site",
            serde_json::json!({"version":version}),
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
        audit_actor: application::audit::AuditContext,
    ) -> Result<SaveOutcome, UseCaseError> {
        let value = serde_json::json!({ "schema_version": 1, "slug": slug });
        let mut tx = self.pool.begin().await.map_err(map_repo_error)?;
        crate::themes::lock(&mut tx).await?;
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
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| UseCaseError::Repository(e.to_string()))?;
        if let Some(version) = new_version {
            record_change(
                &mut tx,
                audit_actor,
                "settings.theme",
                "settings",
                "theme",
                serde_json::json!({"version":version,"slug":slug}),
            )
            .await?;
        }
        tx.commit().await.map_err(map_repo_error)?;
        Ok(match new_version {
            Some(new_version) => SaveOutcome::Saved { new_version },
            None => SaveOutcome::StaleConflict,
        })
    }
}
