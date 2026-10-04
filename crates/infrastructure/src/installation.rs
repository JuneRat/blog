//! Empty-database installation. No existing account, including a deleted or
//! disabled Admin, can make a database eligible for installation again.

use crate::schema_contract::SchemaContract;

use application::{
    UseCaseError,
    audit::AuditContext,
    identity::{BUILTIN_ROLES, PERMISSION_REGISTRY},
    installation::InitialAdmin,
};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

pub async fn connect(url: &str) -> Result<crate::Database, UseCaseError> {
    use sqlx::ConnectOptions;
    let options = url
        .parse::<sqlx::postgres::PgConnectOptions>()
        .map_err(|_| UseCaseError::Invalid("PostgreSQL 连接地址无效".into()))?
        .options([("statement_timeout", "30000"), ("lock_timeout", "10000")])
        .disable_statement_logging();
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .connect_with(options)
        .await
        .map(|pool| crate::Database { pool })
        .map_err(|_| {
            UseCaseError::Invalid(
                "连接 PostgreSQL 失败，请检查地址、数据库名、账号密码和网络".into(),
            )
        })
}

fn database_error(_: sqlx::Error) -> UseCaseError {
    // Connection and PostgreSQL diagnostics may contain credentials or submitted
    // values. Installation exposes only fixed, actionable messages.
    UseCaseError::Invalid("无法检查或初始化数据库，请检查连接账号的建表和读写权限".into())
}

fn occupied() -> UseCaseError {
    UseCaseError::Invalid("安装仅支持空数据库；该库已有数据或不属于本次安装，未作清理".into())
}

pub async fn is_complete(
    database: &crate::Database,
    installation_id: &str,
) -> Result<bool, UseCaseError> {
    let pool = &database.pool;
    let exists: bool = sqlx::query_scalar("SELECT to_regclass('public.settings') IS NOT NULL")
        .fetch_one(pool)
        .await
        .map_err(database_error)?;
    if !exists {
        return Ok(false);
    }
    let id: Option<String> =
        sqlx::query_scalar("SELECT value->>'id' FROM public.settings WHERE key='installation'")
            .fetch_optional(pool)
            .await
            .map_err(database_error)?
            .flatten();
    match id {
        Some(id) if id != installation_id => Err(occupied()),
        Some(_) => Ok(true),
        None => Ok(false),
    }
}

/// A saved local journal permits resuming only our empty baseline. Arbitrary
/// tables, schemas, views and recovery databases are never migrated by setup.
pub async fn check_target(
    database: &crate::Database,
    schema_contract: &SchemaContract,
    resume: bool,
) -> Result<(), UseCaseError> {
    let pool = &database.pool;
    let (schema, comment): (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT current_schema(), shobj_description(oid,'pg_database') FROM pg_database WHERE datname=current_database()",
    ).fetch_one(pool).await.map_err(database_error)?;
    if schema.as_deref() != Some("public") {
        return Err(UseCaseError::Invalid("安装须使用 public schema".into()));
    }
    if comment.is_some_and(|s| s.starts_with("blog:recovery-isolated:")) {
        return Err(UseCaseError::Invalid("恢复隔离数据库不能用于安装".into()));
    }
    // Check privileges without creating any tables or persisting configuration.
    // Installation repeats this preflight; a prior browser check is not authority.
    let allowed: bool = sqlx::query_scalar(
        "SELECT has_schema_privilege('public', 'USAGE') \
         AND has_schema_privilege('public', 'CREATE') \
         AND (EXISTS(SELECT 1 FROM pg_extension WHERE extname='pg_trgm') \
              OR has_database_privilege(current_database(), 'CREATE'))",
    )
    .fetch_one(pool)
    .await
    .map_err(database_error)?;
    if !allowed {
        return Err(UseCaseError::Invalid(
            "连接成功，但账号没有安装所需的建表或扩展权限；请使用该数据库的所有者账号".into(),
        ));
    }
    let extension_available: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM pg_available_extensions WHERE name='pg_trgm')",
    )
    .fetch_one(pool)
    .await
    .map_err(database_error)?;
    if !extension_available {
        return Err(UseCaseError::Invalid(
            "数据库缺少 pg_trgm 扩展，请先在 PostgreSQL 中安装该扩展支持".into(),
        ));
    }
    let objects: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT n.nspname,c.relname,c.relkind::text FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace \
         WHERE n.nspname <> 'information_schema' AND n.nspname !~ '^pg_' AND c.relkind IN ('r','p','v','m','f','S')",
    ).fetch_all(pool).await.map_err(database_error)?;
    if objects.is_empty() {
        return Ok(());
    }
    if !resume
        || objects.iter().any(|(schema, table, kind)| {
            schema != "public"
                || kind != "r"
                || (table != "_sqlx_migrations" && !schema_contract.contains(table))
        })
    {
        return Err(occupied());
    }
    // Also check before migration: a pending journal must never authorize writes
    // to a database populated after an interrupted setup.
    for (_, table, _) in objects
        .iter()
        .filter(|(_, table, _)| table != "_sqlx_migrations")
    {
        let exists: bool =
            sqlx::query_scalar(&format!("SELECT EXISTS(SELECT 1 FROM public.\"{table}\")"))
                .fetch_one(pool)
                .await
                .map_err(database_error)?;
        if exists {
            return Err(occupied());
        }
    }
    Ok(())
}

/// Permission seeds, the credential, ownership, completion marker and audit are
/// one transaction. Failed/competing installs cannot leave a partial account.
pub async fn initialize(
    database: &crate::Database,
    schema_contract: &SchemaContract,
    installation_id: &str,
    admin: &InitialAdmin,
    site: &application::ports::SiteSettingsValue,
    audit: AuditContext,
) -> Result<(), UseCaseError> {
    let pool = &database.pool;
    let mut tx = pool.begin().await.map_err(database_error)?;
    sqlx::query("SET LOCAL lock_timeout = '10s'")
        .execute(&mut *tx)
        .await
        .map_err(database_error)?;
    crate::persistence::acquire_identity_lock(&mut tx)
        .await
        .map_err(database_error)?;
    // Include content tables: even writes that do not take the identity lock
    // must not race the final empty-database check.
    sqlx::query(&format!(
        "LOCK TABLE {} IN SHARE ROW EXCLUSIVE MODE",
        schema_contract.lock_tables_sql()
    ))
    .execute(&mut *tx)
    .await
    .map_err(database_error)?;
    ensure_empty(&mut tx, schema_contract).await?;
    // Initial values become ordinary runtime settings in the same transaction
    // as the first Admin. Subsequent startups never reapply deployment defaults.
    if site.title.is_some() || site.description.is_some() {
        sqlx::query("INSERT INTO settings(key,value) VALUES('site',jsonb_strip_nulls($1))")
            .bind(serde_json::json!({"schema_version":1,"title":site.title,"description":site.description}))
            .execute(&mut *tx).await.map_err(database_error)?;
    }
    for permission in PERMISSION_REGISTRY {
        sqlx::query("INSERT INTO permissions(code,name) VALUES($1,$2)")
            .bind(permission.key)
            .bind(permission.name)
            .execute(&mut *tx)
            .await
            .map_err(database_error)?;
    }
    for role in BUILTIN_ROLES {
        let id = Uuid::now_v7();
        sqlx::query("INSERT INTO roles(id,code,name,description) VALUES($1,$2,$3,$4)")
            .bind(id)
            .bind(role.slug)
            .bind(role.name)
            .bind(role.description)
            .execute(&mut *tx)
            .await
            .map_err(database_error)?;
        sqlx::query(
            "INSERT INTO role_permissions(role_id,permission_code) SELECT $1,unnest($2::text[])",
        )
        .bind(id)
        .bind(role.permissions)
        .execute(&mut *tx)
        .await
        .map_err(database_error)?;
    }
    sqlx::query(
        "INSERT INTO users(id,username,password_hash,created_at,updated_at) VALUES($1,$2,$3,$4,$4)",
    )
    .bind(admin.id)
    .bind(&admin.username)
    .bind(&admin.password_hash)
    .bind(admin.created_at)
    .execute(&mut *tx)
    .await
    .map_err(database_error)?;
    sqlx::query(
        "INSERT INTO user_roles(user_id,role_id) SELECT $1,id FROM roles WHERE code='admin'",
    )
    .bind(admin.id)
    .execute(&mut *tx)
    .await
    .map_err(database_error)?;
    sqlx::query("INSERT INTO settings(key,value) VALUES('installation',$1)")
        .bind(serde_json::json!({"id":installation_id,"admin_id":admin.id}))
        .execute(&mut *tx)
        .await
        .map_err(database_error)?;
    crate::audit::record_change(
        &mut tx,
        audit,
        "installation.complete",
        "user",
        &admin.id.to_string(),
        serde_json::json!({"role":"admin","version":1}),
    )
    .await?;
    tx.commit().await.map_err(database_error)
}

async fn ensure_empty(
    tx: &mut Transaction<'_, Postgres>,
    schema_contract: &SchemaContract,
) -> Result<(), UseCaseError> {
    for table in schema_contract.tables() {
        let exists: bool =
            sqlx::query_scalar(&format!("SELECT EXISTS(SELECT 1 FROM public.\"{table}\")"))
                .fetch_one(&mut **tx)
                .await
                .map_err(database_error)?;
        if exists {
            return Err(occupied());
        }
    }
    Ok(())
}

/// Fresh panel deployments already have a database connection, but no account.
pub async fn needs_installation(database: &crate::Database) -> Result<bool, UseCaseError> {
    let exists: bool = sqlx::query_scalar("SELECT to_regclass('public.users') IS NOT NULL")
        .fetch_one(&database.pool)
        .await
        .map_err(database_error)?;
    if !exists {
        return Ok(true);
    }
    let any: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM public.users)")
        .fetch_one(&database.pool)
        .await
        .map_err(database_error)?;
    Ok(!any)
}
