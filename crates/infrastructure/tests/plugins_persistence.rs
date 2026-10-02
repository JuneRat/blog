//! Independent plugin records, legacy migration and transactional invariants.
mod common;

use application::{
    UseCaseError,
    identity::Actor,
    plugins::{PluginConfigValue, PluginSettings, PluginState, PluginStore, SavePluginCmd},
};
use infrastructure::{SystemClock, plugins::*};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};
use time::OffsetDateTime;

struct Baseline(PathBuf);
impl Baseline {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("blog-plugin-migrations-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&path).unwrap();
        for entry in std::fs::read_dir("../../migrations/postgres").unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".sql")
                && name
                    .split('_')
                    .next()
                    .and_then(|n| n.parse::<u32>().ok())
                    .is_some_and(|v| v < 8)
            {
                std::fs::copy(entry.path(), path.join(name)).unwrap();
            }
        }
        Self(path)
    }
}
impl Drop for Baseline {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

async fn previous_database(name: &str) -> sqlx::PgPool {
    let admin_url = common::admin_url();
    common::assert_loopback(&admin_url);
    let admin = common::connect(&admin_url).await.unwrap();
    sqlx::raw_sql(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"))
        .execute(&admin)
        .await
        .unwrap();
    sqlx::raw_sql(&format!("CREATE DATABASE {name}"))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    let pool = common::connect(&common::test_db_url(&admin_url, name))
        .await
        .unwrap();
    let baseline = Baseline::new();
    sqlx::migrate::Migrator::new(baseline.0.clone())
        .await
        .unwrap()
        .run(&pool)
        .await
        .unwrap();
    pool
}

#[tokio::test]
async fn upgrade_preserves_configs_versions_timestamps_and_unavailable_plugins() {
    let pool = previous_database("blog_plugins_upgrade_test").await;
    let mut value = json!({
        "schema_version": 1, "render_revision": 7,
        "plugins": {
            "markdown-enhance": {"enabled": true, "config": {"math": true, "mermaid": false}},
            "removed-plugin": {"enabled": true, "config": {"label": "保留配置", "count": -12}},
            "disabled-plugin": {"enabled": false, "config": {"label": "disabled"}}
        }
    });
    // Valid legacy strings can expand sixfold when serialized as JSON escapes.
    value["plugins"]["disabled-plugin"]["config"] = Value::Object(
        (0..16)
            .map(|index| (format!("field-{index}"), json!("\u{0001}".repeat(2048))))
            .collect(),
    );
    let timestamp = OffsetDateTime::UNIX_EPOCH;
    sqlx::query("INSERT INTO settings(key,value,version,updated_at) VALUES('plugins',$1,19,$2),('site','{\"title\":\"keep\"}',3,$2)")
        .bind(&value).bind(timestamp).execute(&pool).await.unwrap();
    let database = common::database(pool.clone());
    infrastructure::migrate_schema(&database, "../../migrations/postgres")
        .await
        .unwrap();
    // Repeated startup verifies history and never reapplies the conversion.
    infrastructure::migrate_schema(&database, "../../migrations/postgres")
        .await
        .unwrap();
    let store = Arc::new(PostgresPluginStore::new(database));
    let migrated = store.load().await.unwrap();
    assert_eq!(serde_json::to_value(&migrated.value).unwrap(), value);
    assert_eq!(migrated.version, 19);
    let rows: Vec<(String, i64, OffsetDateTime, OffsetDateTime)> =
        sqlx::query_as("SELECT id,version,created_at,updated_at FROM plugins ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(rows.len(), 3);
    assert!(
        rows.iter()
            .all(|(_, version, created, updated)| *version == 19
                && *created == timestamp
                && *updated == timestamp)
    );
    let site: (Value, i64) = sqlx::query_as("SELECT value,version FROM settings WHERE key='site'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(site, (json!({"title":"keep"}), 3));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM settings WHERE key='plugins'")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM audit_logs")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );

    let runtime = PluginRuntime::new(
        Arc::new(PluginCatalog::builtins()),
        store.clone(),
        Arc::new(SystemClock),
    );
    let actor = Actor::bootstrap_cli();
    let removed = runtime
        .manager
        .view(&actor)
        .await
        .unwrap()
        .plugins
        .into_iter()
        .find(|plugin| plugin.definition.id == "removed-plugin")
        .unwrap();
    assert!(!removed.available);
    runtime
        .manager
        .save(
            &actor,
            SavePluginCmd {
                id: "removed-plugin".into(),
                enabled: false,
                config: removed.config,
                expected_version: 19,
            },
        )
        .await
        .unwrap();
    let after = store.load().await.unwrap();
    assert_eq!(after.version, 20);
    assert_eq!(after.value.render_revision, 8);
    assert!(!after.value.plugins["removed-plugin"].enabled);
    assert_eq!(
        after.value.plugins["markdown-enhance"],
        migrated.value.plugins["markdown-enhance"]
    );
    pool.close().await;
}

#[tokio::test]
async fn invalid_legacy_data_rolls_back_without_removing_settings_or_advancing_history() {
    let pool = previous_database("blog_plugins_invalid_upgrade_test").await;
    let database = common::database(pool.clone());
    let good = json!({"schema_version":1,"render_revision":1,"plugins":{"sample":{"enabled":true,"config":{"label":"keep"}}}});
    sqlx::query("INSERT INTO settings(key,value,version) VALUES('plugins',$1,4)")
        .bind(&good)
        .execute(&pool)
        .await
        .unwrap();
    let mut incompatible = good.clone();
    incompatible["schema_version"] = json!(2);
    let mut bad_revision = good.clone();
    bad_revision["render_revision"] = json!(2097152);
    let mut unknown_field = good.clone();
    unknown_field["plugins"]["sample"]["extra"] = json!(true);
    let mut bad_config = good.clone();
    bad_config["plugins"]["sample"]["config"]["label"] = json!({"nested":"unsupported"});
    let mut bad_integer = good.clone();
    bad_integer["plugins"]["sample"]["config"]["count"] = json!(2147483648_i64);
    let mut bad_registry = good.clone();
    bad_registry["plugins"] = json!([]);
    for invalid in [
        incompatible,
        bad_revision,
        unknown_field,
        bad_config,
        bad_integer,
        bad_registry,
    ] {
        sqlx::query("UPDATE settings SET value=$1 WHERE key='plugins'")
            .bind(&invalid)
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                infrastructure::migrate_schema(&database, "../../migrations/postgres")
            )
            .await
            .expect("failed migration must release its session lock before retry")
            .is_err()
        );
        let retained: (Value, i64) =
            sqlx::query_as("SELECT value,version FROM settings WHERE key='plugins'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(retained, (invalid, 4));
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT max(version) FROM _sqlx_migrations")
                .fetch_one(&pool)
                .await
                .unwrap(),
            7
        );
        assert!(sqlx::query_scalar::<_, bool>("SELECT to_regclass('public.plugins') IS NULL AND to_regclass('public.plugin_runtime') IS NULL").fetch_one(&pool).await.unwrap());
    }
    sqlx::query("UPDATE settings SET value=$1 WHERE key='plugins'")
        .bind(good)
        .execute(&pool)
        .await
        .unwrap();
    infrastructure::migrate_schema(&database, "../../migrations/postgres")
        .await
        .unwrap();
    pool.close().await;
}

#[tokio::test]
async fn independent_rows_cas_noop_and_audit_rollback_keep_runtime_consistent() {
    let pool = common::fresh_database("blog_plugins_records_test").await;
    let database = common::database(pool.clone());
    let store = Arc::new(PostgresPluginStore::new(database));
    assert_eq!(store.load().await.unwrap().version, 0);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM plugins UNION ALL SELECT count(*) FROM plugin_runtime"
        )
        .fetch_all(&pool)
        .await
        .unwrap(),
        vec![0, 0]
    );
    let runtime = PluginRuntime::new(
        Arc::new(PluginCatalog::builtins()),
        store.clone(),
        Arc::new(SystemClock),
    );
    let actor = Actor::bootstrap_cli();
    let command = |enabled, math, version| SavePluginCmd {
        id: "markdown-enhance".into(),
        enabled,
        config: BTreeMap::from([
            ("math".into(), PluginConfigValue::Boolean(math)),
            ("mermaid".into(), PluginConfigValue::Boolean(false)),
        ]),
        expected_version: version,
    };
    runtime
        .manager
        .save(&actor, command(true, true, 0))
        .await
        .unwrap();
    let untouched: (i64, OffsetDateTime, OffsetDateTime) = sqlx::query_as(
        "SELECT version,created_at,updated_at FROM plugins WHERE id='markdown-enhance'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let mut state = store.load().await.unwrap().value;
    state
        .plugins
        .insert("other-plugin".into(), PluginState::default());
    store
        .save(
            &state,
            1,
            "other-plugin",
            OffsetDateTime::now_utc(),
            None.into(),
        )
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_as::<_, (i64, OffsetDateTime, OffsetDateTime)>(
            "SELECT version,created_at,updated_at FROM plugins WHERE id='markdown-enhance'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        untouched
    );
    let before = store.load().await.unwrap();
    runtime
        .manager
        .save(&actor, command(true, true, 2))
        .await
        .unwrap();
    assert_eq!(store.load().await.unwrap().version, before.version);
    assert!(matches!(
        runtime.manager.save(&actor, command(true, false, 1)).await,
        Err(UseCaseError::VersionConflict)
    ));

    // An audit failure must roll back both per-plugin data and global revisions.
    sqlx::raw_sql("CREATE FUNCTION reject_plugin_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'audit unavailable'; END $$; CREATE TRIGGER reject_plugin_audit BEFORE INSERT ON audit_logs FOR EACH ROW EXECUTE FUNCTION reject_plugin_audit();")
        .execute(&pool).await.unwrap();
    assert!(
        runtime
            .manager
            .save(&actor, command(true, false, 2))
            .await
            .is_err()
    );
    let rolled_back = store.load().await.unwrap();
    assert_eq!(rolled_back.version, before.version);
    assert_eq!(rolled_back.value, before.value);
    assert_eq!(
        sqlx::query_as::<_, (i64, OffsetDateTime, OffsetDateTime)>(
            "SELECT version,created_at,updated_at FROM plugins WHERE id='markdown-enhance'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        untouched
    );
    sqlx::raw_sql(
        "DROP TRIGGER reject_plugin_audit ON audit_logs; DROP FUNCTION reject_plugin_audit();",
    )
    .execute(&pool)
    .await
    .unwrap();
    let (first, second) = tokio::join!(
        runtime.manager.save(&actor, command(true, false, 2)),
        runtime.manager.save(&actor, command(false, true, 2))
    );
    assert_ne!(first.is_ok(), second.is_ok());
    let committed = store.load().await.unwrap();
    assert_eq!(committed.version, 3);
    assert_eq!(committed.value.render_revision, 2);
    assert_eq!(
        committed.value.plugins["other-plugin"],
        PluginState::default()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT version FROM plugins WHERE id='markdown-enhance'")
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM settings WHERE key='plugins'")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM audit_logs WHERE action='plugin.configure'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        3
    );
    pool.close().await;
}

#[tokio::test]
async fn empty_legacy_registry_keeps_its_version_and_render_revision() {
    let pool = previous_database("blog_plugins_empty_upgrade_test").await;
    sqlx::query("INSERT INTO settings(key,value,version) VALUES('plugins','{\"schema_version\":1,\"render_revision\":5,\"plugins\":{}}',8)")
        .execute(&pool).await.unwrap();
    let database = common::database(pool.clone());
    infrastructure::migrate_schema(&database, "../../migrations/postgres")
        .await
        .unwrap();
    let state = PostgresPluginStore::new(database).load().await.unwrap();
    assert_eq!(state.version, 8);
    assert_eq!(
        state.value,
        PluginSettings {
            render_revision: 5,
            ..Default::default()
        }
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM plugins")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    pool.close().await;
}
