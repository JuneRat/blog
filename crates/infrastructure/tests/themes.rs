//! Real PostgreSQL configuration/lifecycle tests, including abrupt process exits.
mod common;
use application::{
    audit::AuditContext,
    theme_config::*,
    themes::{ThemePackages, ThemeUninstallIdentity},
};
use infrastructure::{
    RenderingRuntime, theme_packages::LocalThemePackages, themes::PostgresThemesStore,
};
use std::{
    io::{Cursor, Write},
    path::{Path, PathBuf},
    sync::Arc,
};
use uuid::Uuid;

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("blog-themes-db-{}", Uuid::now_v7()));
        for (name, body) in files("default", false) {
            let dest = path.join("default").join(name);
            std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
            std::fs::write(dest, body).unwrap();
        }
        Self(path)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn files(slug: &str, configured: bool) -> Vec<(String, String)> {
    let mut files=vec![("theme.json".into(),serde_json::json!({"schema_version":1,"slug":slug,"name":slug,"theme_api_version":1,"required_functions":[]}).to_string())];
    for entry in ["index", "post", "page", "tag", "category", "series"] {
        files.push((
            format!("templates/{entry}.html"),
            if configured {
                "{{ theme.config.title }}"
            } else {
                "legacy"
            }
            .into(),
        ));
    }
    if configured {
        files.push(("settings.schema.json".into(),serde_json::json!({"config_schema_version":1,"fields":[{"key":"title","type":"text","label":"Title","default":"default","max_length":100},{"key":"image","type":"media","label":"Image","default":null}]}).to_string()));
    }
    files
}
fn package() -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, body) in files("custom", true) {
        zip.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(body.as_bytes()).unwrap();
    }
    zip.finish().unwrap().into_inner()
}
async fn load(root: &Path, pool: &sqlx::PgPool, enabled: bool) -> LocalThemePackages {
    let db = common::database(pool.clone());
    let data = Arc::new(application::theme_data::ThemeData::new(
        Arc::new(infrastructure::PostgresPublishedPostQuery::new(db.clone())),
        Arc::new(infrastructure::PostgresPublishedTagQuery::new(db.clone())),
        Arc::new(infrastructure::PostgresPublishedCategoryQuery::new(
            db.clone(),
        )),
    ));
    LocalThemePackages::load_persistent(
        &root.join("default"),
        data,
        Arc::new(RenderingRuntime::default()),
        PostgresThemesStore::new(db),
        enabled,
    )
    .await
    .unwrap()
}
fn identity(record: &ThemeRecord) -> ThemeUninstallIdentity {
    ThemeUninstallIdentity {
        id: record.id,
        version: record.version,
        config_schema_version: record.config_schema_version,
        release: record.release.clone(),
        selection_version: 0,
    }
}
fn journals(root: &Path) -> usize {
    std::fs::read_dir(root)
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".theme-op-")
        })
        .count()
}
async fn media(pool: &sqlx::PgPool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO media(id,path,filename,mime_type,size,width,height,checksum_sha256) VALUES($1,$2,'theme.png','image/png',1,1,1,$3)").bind(id).bind(format!("objects/{id}.png")).bind("a".repeat(64)).execute(pool).await.unwrap();
    id
}

#[tokio::test]
async fn persistence_refs_conflicts_compatible_upgrades_and_reinstall_identity() {
    let pool = common::fresh_database("blog_test_theme_config").await;
    let root = Root::new();
    let service = load(&root.0, &pool, true).await;
    let store = PostgresThemesStore::new(common::database(pool.clone()));
    assert!(
        store
            .find("default")
            .await
            .unwrap()
            .unwrap()
            .config
            .is_empty()
    );
    service
        .install(package(), AuditContext::system(), service.lock().await)
        .await
        .unwrap();
    let original = store.find("custom").await.unwrap().unwrap();
    let schema = service.registry().schema("custom");
    assert_eq!(
        original.effective(&original.release, &schema).unwrap()["title"],
        ThemeValue::Text("default".into())
    );
    let a = media(&pool).await;
    let b = media(&pool).await;
    let config = std::collections::BTreeMap::from([
        ("title".into(), ThemeValue::Text("saved".into())),
        ("image".into(), ThemeValue::Text(a.to_string())),
    ]);
    let saved = store
        .save(&original, &config, &schema, AuditContext::system())
        .await
        .unwrap();
    assert_eq!(saved.version, 2);
    assert_eq!(
        store
            .save(&saved, &config, &schema, AuditContext::system())
            .await
            .unwrap()
            .version,
        2
    );
    assert!(matches!(
        store
            .save(&original, &config, &schema, AuditContext::system())
            .await,
        Err(application::UseCaseError::VersionConflict)
    ));
    let usage = application::ports::MediaRepository::usage_of(
        &infrastructure::PostgresMediaRepository::new(common::database(pool.clone())),
        a,
    )
    .await
    .unwrap();
    assert_eq!(usage[0].source, application::ports::MediaUsageSource::Theme);
    assert!(!usage[0].public);
    assert_eq!(usage[0].content_id, saved.id);
    let mut replaced = config.clone();
    replaced.insert("image".into(), ThemeValue::Text(b.to_string()));
    let replaced = store
        .save(&saved, &replaced, &schema, AuditContext::system())
        .await
        .unwrap();
    let refs: Vec<Uuid> =
        sqlx::query_scalar("SELECT media_id FROM media_refs WHERE source_type='theme'")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(refs, [b]);
    let id = replaced.id;
    drop(service);
    let service = load(&root.0, &pool, true).await;
    assert_eq!(store.find("custom").await.unwrap().unwrap(), replaced);
    service
        .uninstall(
            "custom",
            AuditContext::system(),
            identity(&replaced),
            service.lock().await,
        )
        .await
        .unwrap();
    assert!(store.find("custom").await.unwrap().is_none());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM media_refs WHERE source_type='theme'")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM media")
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
    service
        .install(package(), AuditContext::system(), service.lock().await)
        .await
        .unwrap();
    let fresh = store.find("custom").await.unwrap().unwrap();
    assert_ne!(id, fresh.id);
    assert!(fresh.config.is_empty());
    assert!(matches!(
        service
            .uninstall(
                "custom",
                AuditContext::system(),
                identity(&replaced),
                service.lock().await
            )
            .await,
        Err(application::UseCaseError::VersionConflict)
    ));
    // A missing directory never turns into a successful uninstall or clears data.
    std::fs::rename(root.0.join("custom"), root.0.join(".missing-custom")).unwrap();
    assert!(
        service
            .uninstall(
                "custom",
                AuditContext::system(),
                identity(&fresh),
                service.lock().await
            )
            .await
            .is_err()
    );
    assert_eq!(store.find("custom").await.unwrap().unwrap().id, fresh.id);
    std::fs::rename(root.0.join(".missing-custom"), root.0.join("custom")).unwrap();
    drop(service);
    let path = root.0.join("custom/settings.schema.json");
    let mut declaration: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    declaration["config_schema_version"] = 2.into();
    declaration["fields"][0]["default"] = "new-default".into();
    std::fs::write(&path, serde_json::to_vec(&declaration).unwrap()).unwrap();
    let service = load(&root.0, &pool, true).await;
    let upgraded = store.find("custom").await.unwrap().unwrap();
    assert_eq!(upgraded.config_schema_version, 2);
    assert_eq!(upgraded.version, 2);
    assert_ne!(upgraded.release, fresh.release);
    assert!(upgraded.config.is_empty());
    let saved = store
        .save(
            &upgraded,
            &std::collections::BTreeMap::from([("title".into(), ThemeValue::Text("keep".into()))]),
            &service.registry().schema("custom"),
            AuditContext::system(),
        )
        .await
        .unwrap();
    drop(service);
    declaration["fields"][0]["type"] = "integer".into();
    declaration["fields"][0]["default"] = 1.into();
    declaration["fields"][0]
        .as_object_mut()
        .unwrap()
        .remove("max_length");
    std::fs::write(&path, serde_json::to_vec(&declaration).unwrap()).unwrap();
    let service = load(&root.0, &pool, true).await;
    assert!(!service.registry().contains("custom"));
    assert_eq!(store.find("custom").await.unwrap().unwrap(), saved);
    assert_eq!(journals(&root.0), 0);
}

#[tokio::test]
async fn database_and_commit_failures_compensate_filesystem_and_registry() {
    let pool = common::fresh_database("blog_test_theme_failure").await;
    let root = Root::new();
    let service = load(&root.0, &pool, true).await;
    let store = PostgresThemesStore::new(common::database(pool.clone()));
    sqlx::raw_sql("CREATE FUNCTION reject_theme_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='theme.install' THEN RAISE EXCEPTION 'injected install failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_theme_audit BEFORE INSERT ON audit_logs FOR EACH ROW EXECUTE FUNCTION reject_theme_audit()").execute(&pool).await.unwrap();
    assert!(
        service
            .install(package(), AuditContext::system(), service.lock().await)
            .await
            .is_err()
    );
    assert!(!root.0.join("custom").exists());
    assert!(!service.registry().contains("custom"));
    assert!(store.find("custom").await.unwrap().is_none());
    assert_eq!(journals(&root.0), 0);
    sqlx::raw_sql("DROP TRIGGER reject_theme_audit ON audit_logs")
        .execute(&pool)
        .await
        .unwrap();
    service
        .install(package(), AuditContext::system(), service.lock().await)
        .await
        .unwrap();
    let record = store.find("custom").await.unwrap().unwrap();
    let image = media(&pool).await;
    let config =
        std::collections::BTreeMap::from([("image".into(), ThemeValue::Text(image.to_string()))]);
    sqlx::raw_sql("CREATE OR REPLACE FUNCTION reject_theme_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='theme.settings' THEN RAISE EXCEPTION 'injected settings audit failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_theme_audit BEFORE INSERT ON audit_logs FOR EACH ROW EXECUTE FUNCTION reject_theme_audit()").execute(&pool).await.unwrap();
    assert!(
        store
            .save(
                &record,
                &config,
                &service.registry().schema("custom"),
                AuditContext::system()
            )
            .await
            .is_err()
    );
    assert_eq!(store.find("custom").await.unwrap().unwrap(), record);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM media_refs WHERE source_type='theme'")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    sqlx::raw_sql("DROP TRIGGER reject_theme_audit ON audit_logs")
        .execute(&pool)
        .await
        .unwrap();
    let record = store
        .save(
            &record,
            &config,
            &service.registry().schema("custom"),
            AuditContext::system(),
        )
        .await
        .unwrap();
    sqlx::raw_sql("CREATE FUNCTION reject_theme_commit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected commit failure'; END $$; CREATE CONSTRAINT TRIGGER reject_theme_commit AFTER DELETE ON themes DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION reject_theme_commit()").execute(&pool).await.unwrap();
    assert!(
        service
            .uninstall(
                "custom",
                AuditContext::system(),
                identity(&record),
                service.lock().await
            )
            .await
            .is_err()
    );
    assert!(root.0.join("custom").exists());
    assert!(service.registry().contains("custom"));
    assert_eq!(store.find("custom").await.unwrap().unwrap(), record);
    assert_eq!(journals(&root.0), 0);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM media_refs WHERE source_type='theme'")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    // Recovery assembly is read-only, even when records do not yet exist.
    drop(service);
    sqlx::raw_sql("DROP TRIGGER reject_theme_commit ON themes")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM themes WHERE slug='default'")
        .execute(&pool)
        .await
        .unwrap();
    let service = load(&root.0, &pool, false).await;
    assert!(store.find("default").await.unwrap().is_none());
    assert!(
        service
            .install(package(), AuditContext::system(), service.lock().await)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn recovery_waits_for_the_original_database_decision_before_compensating() {
    let pool = common::fresh_database("blog_test_theme_pending_commit").await;
    let root = Root::new();
    let service = load(&root.0, &pool, true).await;
    service
        .install(package(), AuditContext::system(), service.lock().await)
        .await
        .unwrap();
    let store = PostgresThemesStore::new(common::database(pool.clone()));
    let record = store.find("custom").await.unwrap().unwrap();
    drop(service);
    let journal = root.0.join(format!(".theme-op-{}", Uuid::now_v7()));
    std::fs::create_dir(&journal).unwrap();
    std::fs::write(journal.join("operation.json"), serde_json::to_vec(&serde_json::json!({"kind":"uninstall","slug":"custom","id":record.id,"release":record.release})).unwrap()).unwrap();
    std::fs::rename(root.0.join("custom"), journal.join("payload")).unwrap();
    // The previous backend is still finishing its transaction after the
    // application's connection disappeared. Recovery must await its decision.
    let mut original = pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(2048004,1)")
        .execute(&mut *original)
        .await
        .unwrap();
    sqlx::query("DELETE FROM themes WHERE id=$1")
        .bind(record.id)
        .execute(&mut *original)
        .await
        .unwrap();
    let recovery_root = root.0.clone();
    let recovery_pool = pool.clone();
    let recovering = tokio::spawn(async move { load(&recovery_root, &recovery_pool, true).await });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let waiting: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND wait_event='advisory')").fetch_one(&pool).await.unwrap();
            if waiting { break; }
            assert!(!recovering.is_finished(), "recovery must await the database decision");
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    original.commit().await.unwrap();
    let service = recovering.await.unwrap();
    assert!(!service.registry().contains("custom"));
    assert!(!root.0.join("custom").exists());
    assert!(store.find("custom").await.unwrap().is_none());
    assert_eq!(journals(&root.0), 0);
}

#[tokio::test]
async fn theme_crash_child() {
    let Ok(stage) = std::env::var("BLOG_THEME_CRASH_STAGE") else {
        return;
    };
    let root = PathBuf::from(std::env::var("BLOG_THEME_CRASH_ROOT").unwrap());
    let pool = common::connect(&std::env::var("BLOG_THEME_CRASH_DSN").unwrap())
        .await
        .unwrap();
    let service = load(&root, &pool, true)
        .await
        .with_failure_hook(Arc::new(move |checkpoint| {
            if checkpoint == stage {
                std::process::exit(86)
            }
        }));
    if std::env::var("BLOG_THEME_CRASH_STAGE")
        .unwrap()
        .starts_with("install")
    {
        service
            .install(package(), AuditContext::system(), service.lock().await)
            .await
            .unwrap();
    } else {
        let store = PostgresThemesStore::new(common::database(pool.clone()));
        let record = store.find("custom").await.unwrap().unwrap();
        service
            .uninstall(
                "custom",
                AuditContext::system(),
                identity(&record),
                service.lock().await,
            )
            .await
            .unwrap();
    }
    panic!("checkpoint did not exit");
}

#[tokio::test]
async fn abrupt_process_exit_recovers_each_install_and_uninstall_boundary() {
    let name = "blog_test_theme_crash";
    let pool = common::fresh_database(name).await;
    for stage in [
        "install.prepared",
        "install.published",
        "install.committed",
        "uninstall.prepared",
        "uninstall.quarantined",
        "uninstall.committed",
    ] {
        sqlx::query("DELETE FROM themes")
            .execute(&pool)
            .await
            .unwrap();
        let root = Root::new();
        if stage.starts_with("uninstall") {
            let service = load(&root.0, &pool, true).await;
            service
                .install(package(), AuditContext::system(), service.lock().await)
                .await
                .unwrap();
            let store = PostgresThemesStore::new(common::database(pool.clone()));
            let record = store.find("custom").await.unwrap().unwrap();
            store
                .save(
                    &record,
                    &std::collections::BTreeMap::from([(
                        "title".into(),
                        ThemeValue::Text("retained".into()),
                    )]),
                    &service.registry().schema("custom"),
                    AuditContext::system(),
                )
                .await
                .unwrap();
        }
        let dsn = common::test_db_url(&common::admin_url(), name);
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "theme_crash_child", "--nocapture"])
            .env("BLOG_THEME_CRASH_STAGE", stage)
            .env("BLOG_THEME_CRASH_ROOT", &root.0)
            .env("BLOG_THEME_CRASH_DSN", dsn)
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(86), "{stage}");
        assert_eq!(journals(&root.0), 1, "{stage}");
        let service = load(&root.0, &pool, true).await;
        let store = PostgresThemesStore::new(common::database(pool.clone()));
        let record = store.find("custom").await.unwrap();
        let expected = stage == "install.committed"
            || stage.starts_with("uninstall") && stage != "uninstall.committed";
        assert_eq!(record.is_some(), expected, "{stage}");
        assert_eq!(service.registry().contains("custom"), expected, "{stage}");
        assert_eq!(root.0.join("custom").exists(), expected, "{stage}");
        assert_eq!(journals(&root.0), 0, "{stage}");
        assert!(
            std::fs::read_dir(&root.0).unwrap().all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".staging-")),
            "{stage}"
        );
        if stage.starts_with("uninstall") && expected {
            assert_eq!(
                record.unwrap().config["title"],
                ThemeValue::Text("retained".into())
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_does_not_release_the_mutation_lock_mid_publication() {
    let pool = common::fresh_database("blog_test_theme_cancel").await;
    let root = Root::new();
    let published = Arc::new(tokio::sync::Notify::new());
    let signal = published.clone();
    let service = load(&root.0, &pool, true)
        .await
        .with_failure_hook(Arc::new(move |stage| {
            if stage == "install.published" {
                signal.notify_one();
                std::thread::sleep(std::time::Duration::from_millis(150));
            }
        }));
    let caller = service.clone();
    let request = tokio::spawn(async move {
        caller
            .install(package(), AuditContext::system(), caller.lock().await)
            .await
    });
    published.notified().await;
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    let _guard = service.lock().await;
    assert!(service.registry().contains("custom"));
    assert!(
        PostgresThemesStore::new(common::database(pool))
            .find("custom")
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(journals(&root.0), 0);
}

#[tokio::test]
async fn forward_upgrade_from_previous_chain_keeps_selection_and_plugin_settings() {
    let name = "blog_test_themes_upgrade";
    let admin_dsn = common::admin_url();
    common::assert_loopback(&admin_dsn);
    let admin = common::connect(&admin_dsn).await.unwrap();
    sqlx::raw_sql(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"))
        .execute(&admin)
        .await
        .unwrap();
    sqlx::raw_sql(&format!("CREATE DATABASE {name}"))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    let pool = common::connect(&common::test_db_url(&admin_dsn, name))
        .await
        .unwrap();
    let root = Root::new();
    let migrations = root.0.join(".baseline");
    std::fs::create_dir(&migrations).unwrap();
    for entry in std::fs::read_dir("../../migrations/postgres").unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".sql")
            && name
                .split('_')
                .next()
                .and_then(|n| n.parse::<u32>().ok())
                .is_some_and(|v| v < 7)
        {
            std::fs::copy(entry.path(), migrations.join(name)).unwrap();
        }
    }
    sqlx::migrate::Migrator::new(migrations)
        .await
        .unwrap()
        .run(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO settings(key,value,version) VALUES('theme',$1,9),('plugins',$2,4)")
        .bind(serde_json::json!({"schema_version":1,"slug":"paper"})).bind(serde_json::json!({"schema_version":1,"plugins":{"sample":{"enabled":true,"config":{"label":"unchanged"}}}})).execute(&pool).await.unwrap();
    let before: Vec<(String, serde_json::Value, i64)> =
        sqlx::query_as("SELECT key,value,version FROM settings ORDER BY key")
            .fetch_all(&pool)
            .await
            .unwrap();
    infrastructure::migrate_schema(&common::database(pool.clone()), "../../migrations/postgres")
        .await
        .unwrap();
    let after: Vec<(String, serde_json::Value, i64)> =
        sqlx::query_as("SELECT key,value,version FROM settings ORDER BY key")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(before, after);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM themes")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}
