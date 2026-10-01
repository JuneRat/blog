use application::{
    UseCaseError,
    audit::AuditContext,
    plugins::{PluginSettings, PluginSettingsRecord, PluginStore},
    ports::SaveOutcome,
};
use async_trait::async_trait;
use sqlx::{PgConnection, types::Json};

pub struct PostgresPluginStore {
    database: crate::Database,
}

impl PostgresPluginStore {
    pub fn new(database: crate::Database) -> Self {
        Self { database }
    }
}

fn db(error: sqlx::Error) -> UseCaseError {
    UseCaseError::Repository(error.to_string())
}

async fn load_on(connection: &mut PgConnection) -> Result<PluginSettingsRecord, UseCaseError> {
    let row: Option<(Json<PluginSettings>, i64)> =
        sqlx::query_as("SELECT value,version FROM settings WHERE key='plugins'")
            .fetch_optional(connection)
            .await
            .map_err(db)?;
    let record = row.map_or_else(PluginSettingsRecord::default, |(Json(value), version)| {
        PluginSettingsRecord { value, version }
    });
    record.value.validate()?;
    Ok(record)
}

pub(crate) async fn content_version_on(connection: &mut PgConnection) -> Result<i32, UseCaseError> {
    let record = load_on(connection).await?;
    application::plugins::content_render_version(
        crate::CONTENT_RENDER_VERSION,
        record.value.render_revision,
    )
}

#[async_trait]
impl PluginStore for PostgresPluginStore {
    async fn load(&self) -> Result<PluginSettingsRecord, UseCaseError> {
        load_on(&mut *self.database.pool.acquire().await.map_err(db)?).await
    }

    async fn save(
        &self,
        value: &PluginSettings,
        expected_version: i64,
        changed_plugin: &str,
        now: time::OffsetDateTime,
        audit: AuditContext,
    ) -> Result<SaveOutcome, UseCaseError> {
        value.validate()?;
        if self.database.is_recovery_isolated().await? {
            return Err(UseCaseError::Invalid("恢复隔离期间禁止修改插件".into()));
        }
        let mut tx = self.database.pool.begin().await.map_err(db)?;
        // Coordinates revision changes with media purge's reference check.
        crate::locks::acquire(&mut *tx, crate::locks::CONTENT_RELATIONS, false)
            .await
            .map_err(db)?;
        let version: Option<i64> = if expected_version == 0 {
            sqlx::query_scalar("INSERT INTO settings(key,value,version,updated_at) VALUES('plugins',$1,1,$2) ON CONFLICT(key) DO NOTHING RETURNING version")
                .bind(Json(value)).bind(now).fetch_optional(&mut *tx).await.map_err(db)?
        } else {
            sqlx::query_scalar("UPDATE settings SET value=$1,version=version+1,updated_at=$2 WHERE key='plugins' AND version=$3 RETURNING version")
                .bind(Json(value)).bind(now).bind(expected_version).fetch_optional(&mut *tx).await.map_err(db)?
        };
        let Some(new_version) = version else {
            return Ok(SaveOutcome::StaleConflict);
        };
        crate::audit::record_change(&mut tx, audit, "plugin.configure", "plugin", changed_plugin,
            serde_json::json!({"version":new_version, "enabled":value.plugins.get(changed_plugin).is_some_and(|p| p.enabled),
                "render_revision":value.render_revision})).await?;
        tx.commit().await.map_err(db)?;
        Ok(SaveOutcome::Saved { new_version })
    }
}
