//! Theme configuration, references and audit changes share a transaction.
use crate::{audit::record_change, persistence::sync_media_refs};
use application::{UseCaseError, audit::AuditContext, theme_config::*};
use async_trait::async_trait;
use sqlx::{PgConnection, PgPool, Row};

#[derive(Clone)]
pub struct PostgresThemesStore {
    pub(crate) pool: PgPool,
}
impl PostgresThemesStore {
    pub fn new(database: crate::Database) -> Self {
        Self {
            pool: database.pool,
        }
    }
    pub(crate) async fn find_on(
        conn: &mut PgConnection,
        slug: &str,
    ) -> Result<Option<ThemeRecord>, UseCaseError> {
        sqlx::query("SELECT id,slug,config,config_schema_version,version,release,media_fields FROM themes WHERE slug=$1 FOR UPDATE")
            .bind(slug).fetch_optional(conn).await.map_err(db)?.map(record).transpose()
    }
    pub(crate) async fn insert_on(
        conn: &mut PgConnection,
        id: uuid::Uuid,
        slug: &str,
        release: &str,
        schema: &ThemeSchema,
    ) -> Result<(), UseCaseError> {
        sqlx::query("INSERT INTO themes (id,slug,config_schema_version,release,media_fields,created_at,updated_at) VALUES ($1,$2,$3,$4,$5,now(),now())")
            .bind(id).bind(slug).bind(schema.config_schema_version as i32).bind(release).bind(schema.media_fields()).execute(conn).await.map_err(db)?;
        Ok(())
    }
    /// Normal startup or confirmed installation, after package preflight.
    pub async fn initialize(
        &self,
        slug: &str,
        release: &str,
        schema: &ThemeSchema,
    ) -> Result<(), UseCaseError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        lock(&mut tx).await?;
        if let Some(current) = Self::find_on(&mut tx, slug).await? {
            schema.validate_config(&current.config).map_err(|e| {
                UseCaseError::Invalid(format!("主题 {slug} 配置与新声明不兼容，原数据已保留：{e}"))
            })?;
            if current.release != release
                || current.config_schema_version != schema.config_schema_version
                || current.media_fields != schema.media_fields()
            {
                sqlx::query("UPDATE themes SET release=$2,config_schema_version=$3,media_fields=$4,version=version+1,updated_at=now() WHERE id=$1")
                    .bind(current.id).bind(release).bind(schema.config_schema_version as i32).bind(schema.media_fields()).execute(&mut *tx).await.map_err(db)?;
                sync_media_refs(
                    &mut tx,
                    application::ports::MediaContentKind::Theme,
                    current.id,
                    &schema.media_ids(&current.config),
                )
                .await?;
                record_change(
                    &mut tx,
                    AuditContext::system(),
                    "theme.reconcile",
                    "theme",
                    &current.id.to_string(),
                    serde_json::json!({"slug":slug,"release":release}),
                )
                .await?;
            }
        } else {
            Self::insert_on(&mut tx, uuid::Uuid::now_v7(), slug, release, schema).await?;
        }
        tx.commit().await.map_err(db)
    }
}
pub(crate) async fn lock(conn: &mut PgConnection) -> Result<(), UseCaseError> {
    crate::locks::acquire(conn, crate::locks::THEMES, false)
        .await
        .map_err(db)
}
pub(crate) fn db(e: impl std::fmt::Display) -> UseCaseError {
    UseCaseError::Repository(e.to_string())
}
fn record(row: sqlx::postgres::PgRow) -> Result<ThemeRecord, UseCaseError> {
    Ok(ThemeRecord {
        id: row.try_get("id").map_err(db)?,
        slug: row.try_get("slug").map_err(db)?,
        config: row
            .try_get::<sqlx::types::Json<ThemeConfig>, _>("config")
            .map_err(db)?
            .0,
        config_schema_version: row.try_get::<i32, _>("config_schema_version").map_err(db)? as u32,
        version: row.try_get("version").map_err(db)?,
        release: row.try_get("release").map_err(db)?,
        media_fields: row.try_get("media_fields").map_err(db)?,
    })
}
#[async_trait]
impl ThemeConfigStore for PostgresThemesStore {
    async fn find(&self, slug: &str) -> Result<Option<ThemeRecord>, UseCaseError> {
        sqlx::query("SELECT id,slug,config,config_schema_version,version,release,media_fields FROM themes WHERE slug=$1").bind(slug).fetch_optional(&self.pool).await.map_err(db)?.map(record).transpose()
    }
    async fn save(
        &self,
        current: &ThemeRecord,
        config: &ThemeConfig,
        schema: &ThemeSchema,
        actor: AuditContext,
    ) -> Result<ThemeRecord, UseCaseError> {
        schema.validate_config(config)?;
        let mut tx = crate::persistence::begin_authorized_write(&self.pool, &actor).await?;
        lock(&mut tx).await?;
        let mut actual = Self::find_on(&mut tx, &current.slug)
            .await?
            .ok_or(UseCaseError::VersionConflict)?;
        if actual.id != current.id
            || actual.version != current.version
            || actual.release != current.release
            || actual.config_schema_version != current.config_schema_version
        {
            return Err(UseCaseError::VersionConflict);
        }
        actual.effective(&current.release, schema)?;
        if actual.config == *config {
            tx.commit().await.map_err(db)?;
            return Ok(actual);
        }
        sync_media_refs(
            &mut tx,
            application::ports::MediaContentKind::Theme,
            actual.id,
            &schema.media_ids(config),
        )
        .await?;
        actual.version = sqlx::query_scalar("UPDATE themes SET config=$2,version=version+1,updated_at=now() WHERE id=$1 RETURNING version").bind(actual.id).bind(sqlx::types::Json(config)).fetch_one(&mut *tx).await.map_err(db)?;
        record_change(&mut tx, actor, "theme.settings", "theme", &actual.id.to_string(), serde_json::json!({"slug":actual.slug,"release":actual.release,"version":actual.version})).await?;
        tx.commit().await.map_err(db)?;
        actual.config = config.clone();
        Ok(actual)
    }
}
