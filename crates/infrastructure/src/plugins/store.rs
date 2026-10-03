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
    // One statement gives the revision and every plugin a single MVCC snapshot.
    let row: Option<(Json<PluginSettings>, i64)> =
        sqlx::query_as("SELECT jsonb_build_object('schema_version',r.schema_version,'render_revision',r.render_revision,'plugins',COALESCE((SELECT jsonb_object_agg(id,jsonb_build_object('enabled',enabled,'config',config)) FROM plugins),'{}'::jsonb)),r.version FROM plugin_runtime r WHERE r.singleton=true")
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
        let mut tx =
            crate::persistence::begin_authorized_write(&self.database.pool, &audit).await?;
        // Coordinates revision changes with media purge's reference check.
        crate::locks::acquire(&mut *tx, crate::locks::CONTENT_RELATIONS, false)
            .await
            .map_err(db)?;
        let version: Option<i64> = if expected_version == 0 {
            sqlx::query_scalar("INSERT INTO plugin_runtime(singleton,schema_version,render_revision,version,updated_at) VALUES(true,$1,$2,1,$3) ON CONFLICT(singleton) DO NOTHING RETURNING version")
                .bind(value.schema_version as i32).bind(value.render_revision as i32).bind(now).fetch_optional(&mut *tx).await.map_err(db)?
        } else {
            sqlx::query_scalar("UPDATE plugin_runtime SET schema_version=$1,render_revision=$2,version=version+1,updated_at=$3 WHERE singleton=true AND version=$4 RETURNING version")
                .bind(value.schema_version as i32).bind(value.render_revision as i32).bind(now).bind(expected_version).fetch_optional(&mut *tx).await.map_err(db)?
        };
        let Some(new_version) = version else {
            return Ok(SaveOutcome::StaleConflict);
        };
        sqlx::query("INSERT INTO plugins(id,enabled,config,created_at,updated_at) SELECT key,(value->>'enabled')::boolean,value->'config',$2,$2 FROM jsonb_each($1) ON CONFLICT(id) DO UPDATE SET enabled=EXCLUDED.enabled,config=EXCLUDED.config,version=plugins.version+1,updated_at=EXCLUDED.updated_at WHERE plugins.enabled IS DISTINCT FROM EXCLUDED.enabled OR plugins.config IS DISTINCT FROM EXCLUDED.config")
            .bind(Json(&value.plugins)).bind(now).execute(&mut *tx).await.map_err(db)?;
        let ids: Vec<&str> = value.plugins.keys().map(String::as_str).collect();
        sqlx::query("DELETE FROM plugins WHERE NOT (id=ANY($1))")
            .bind(ids)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        crate::audit::record_change(&mut tx, audit, "plugin.configure", "plugin", changed_plugin,
            serde_json::json!({"version":new_version, "enabled":value.plugins.get(changed_plugin).is_some_and(|p| p.enabled),
                "render_revision":value.render_revision})).await?;
        tx.commit().await.map_err(db)?;
        Ok(SaveOutcome::Saved { new_version })
    }
}
