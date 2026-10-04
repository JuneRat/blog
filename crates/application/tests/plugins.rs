use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use application::{
    UseCaseError,
    audit::AuditContext,
    identity::{Actor, ActorChannel},
    plugins::*,
    ports::{Clock, SaveOutcome},
};
use async_trait::async_trait;
use domain::identity::{PermissionSet, UserId};

#[derive(Default)]
struct Store(Mutex<(PluginSettingsRecord, usize)>);
#[async_trait]
impl PluginStore for Store {
    async fn load(&self) -> Result<PluginSettingsRecord, UseCaseError> {
        Ok(self.0.lock().unwrap().0.clone())
    }
    async fn save(
        &self,
        value: &PluginSettings,
        expected: i64,
        _: &str,
        _: time::OffsetDateTime,
        _: AuditContext,
    ) -> Result<SaveOutcome, UseCaseError> {
        let mut state = self.0.lock().unwrap();
        if expected != state.0.version {
            return Ok(SaveOutcome::StaleConflict);
        }
        state.0 = PluginSettingsRecord {
            value: value.clone(),
            version: expected + 1,
        };
        state.1 += 1;
        Ok(SaveOutcome::Saved {
            new_version: expected + 1,
        })
    }
}
struct TestClock;
impl Clock for TestClock {
    fn now(&self) -> time::OffsetDateTime {
        time::OffsetDateTime::UNIX_EPOCH
    }
}

fn definition(id: &str, hooks: Vec<PluginHook>) -> PluginDefinition {
    PluginDefinition {
        id: id.into(),
        name: id.into(),
        description: "test".into(),
        version: "1".into(),
        hooks,
        config_fields: vec![PluginConfigField {
            key: "label".into(),
            label: "标题".into(),
            description: String::new(),
            default: PluginConfigValue::Text("default".into()),
        }],
    }
}
fn actor(permission: &str) -> Actor {
    Actor::new(
        UserId(uuid::Uuid::nil()),
        ActorChannel::Session,
        PermissionSet::from_keys([permission]),
    )
}
fn cmd(id: &str, enabled: bool, label: &str, version: i64) -> SavePluginCmd {
    SavePluginCmd {
        id: id.into(),
        enabled,
        config: BTreeMap::from([("label".into(), PluginConfigValue::Text(label.into()))]),
        expected_version: version,
    }
}
fn setup() -> (PluginsInteractor, Arc<Store>) {
    let registry = PluginRegistry::new(vec![
        definition("content", vec![PluginHook::Content]),
        definition("head", vec![PluginHook::PageHead]),
    ])
    .unwrap();
    let store = Arc::new(Store::default());
    (
        PluginsInteractor::new(store.clone(), Arc::new(registry), Arc::new(TestClock)),
        store,
    )
}

#[tokio::test]
async fn defaults_permissions_typed_config_and_missing_plugins() {
    let (manager, store) = setup();
    let admin = actor("plugins.manage");
    assert_eq!(manager.view(&admin).await.unwrap().version, 0);
    assert!(manager.snapshot().await.unwrap().active.is_empty());
    assert!(matches!(
        manager.view(&actor("settings.manage")).await,
        Err(UseCaseError::Forbidden)
    ));
    assert!(matches!(
        manager
            .save(&actor("settings.manage"), cmd("content", true, "x", 0))
            .await,
        Err(UseCaseError::Forbidden)
    ));
    assert!(matches!(
        manager.save(&admin, cmd("missing", true, "x", 0)).await,
        Err(UseCaseError::NotFound(_))
    ));
    let mut invalid = cmd("content", true, "x", 0);
    invalid
        .config
        .insert("label".into(), PluginConfigValue::Boolean(true));
    assert!(matches!(
        manager.save(&admin, invalid).await,
        Err(UseCaseError::Invalid(_))
    ));
    let mut invalid = cmd("content", true, "x", 0);
    invalid
        .config
        .insert("unknown".into(), PluginConfigValue::Text("x".into()));
    assert!(matches!(
        manager.save(&admin, invalid).await,
        Err(UseCaseError::Invalid(_))
    ));
    assert_eq!(store.0.lock().unwrap().1, 0);
}

#[tokio::test]
async fn content_revisions_follow_active_hooks_and_noops_still_check_versions() {
    let (manager, store) = setup();
    let admin = actor("plugins.manage");
    manager
        .save(&admin, cmd("head", true, "a", 0))
        .await
        .unwrap();
    assert_eq!(manager.snapshot().await.unwrap().render_revision, 0);
    manager
        .save(&admin, cmd("content", false, "a", 1))
        .await
        .unwrap();
    assert_eq!(manager.snapshot().await.unwrap().render_revision, 0);
    manager
        .save(&admin, cmd("content", true, "a", 2))
        .await
        .unwrap();
    assert_eq!(manager.snapshot().await.unwrap().render_revision, 1);
    assert!(matches!(
        manager.save(&admin, cmd("content", true, "a", 2)).await,
        Err(UseCaseError::VersionConflict)
    ));
    assert_eq!(
        manager
            .save(&admin, cmd("content", true, "a", 3))
            .await
            .unwrap()
            .version,
        3
    );
    manager
        .save(&admin, cmd("content", true, "b", 3))
        .await
        .unwrap();
    manager
        .save(&admin, cmd("content", false, "b", 4))
        .await
        .unwrap();
    assert_eq!(manager.snapshot().await.unwrap().render_revision, 3);
    assert_eq!(store.0.lock().unwrap().1, 5);
}

#[tokio::test]
async fn concurrent_first_saves_do_not_overwrite_and_removed_plugins_can_be_disabled() {
    let (manager, store) = setup();
    let admin = actor("plugins.manage");
    let (a, b) = tokio::join!(
        manager.save(&admin, cmd("content", true, "a", 0)),
        manager.save(&admin, cmd("head", true, "b", 0))
    );
    assert_ne!(a.is_ok(), b.is_ok());
    {
        let mut state = store.0.lock().unwrap();
        state.0.value.plugins.insert(
            "removed".into(),
            PluginState {
                enabled: true,
                config: BTreeMap::new(),
            },
        );
    }
    let view = manager.view(&admin).await.unwrap();
    assert!(
        !view
            .plugins
            .iter()
            .find(|p| p.definition.id == "removed")
            .unwrap()
            .available
    );
    assert!(
        !manager
            .snapshot()
            .await
            .unwrap()
            .active
            .contains_key("removed")
    );
    manager
        .save(
            &admin,
            SavePluginCmd {
                id: "removed".into(),
                enabled: false,
                config: BTreeMap::new(),
                expected_version: 1,
            },
        )
        .await
        .unwrap();
    assert!(
        store
            .0
            .lock()
            .unwrap()
            .0
            .value
            .plugins
            .contains_key("removed")
    );
}

#[test]
fn registry_and_render_versions_reject_ambiguous_or_overflowing_values() {
    assert!(PluginRegistry::new(vec![definition("x", vec![]), definition("x", vec![])]).is_err());
    assert!(PluginRegistry::new(vec![definition("../x", vec![])]).is_err());
    assert_eq!(content_render_version(2, 0).unwrap(), 2);
    assert_eq!(content_render_version(2, 1).unwrap(), 1026);
    assert!(content_render_version(2, MAX_RENDER_REVISION + 1).is_err());
    assert!(content_render_version(RENDER_REVISION_STRIDE, 0).is_err());
    assert!(
        PluginSettings {
            schema_version: 2,
            ..Default::default()
        }
        .validate()
        .is_err()
    );
}
