//! Site-wide plugin management. Executable code is registered at assembly time;
//! only enabled flags and typed configuration live in settings, never in articles.
use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{
    UseCaseError,
    audit::AuditContext,
    identity::Actor,
    ports::{Clock, SaveOutcome},
    version::checked_version,
};

/// Lower ten bits retain the existing renderer version. Higher bits identify
/// changes to the enabled content hooks, without adding article columns.
pub const RENDER_REVISION_STRIDE: i32 = 1024;
pub const MAX_RENDER_REVISION: u32 = (i32::MAX / RENDER_REVISION_STRIDE) as u32;

pub fn content_render_version(base: i32, revision: u32) -> Result<i32, UseCaseError> {
    if !(1..RENDER_REVISION_STRIDE).contains(&base) || revision > MAX_RENDER_REVISION {
        return Err(UseCaseError::DataCorrupt("插件正文渲染版本无效".into()));
    }
    Ok(revision as i32 * RENDER_REVISION_STRIDE + base)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginHook {
    Content,
    PageHead,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PluginConfigValue {
    Boolean(bool),
    Integer(i32),
    Text(String),
}

pub type PluginConfig = BTreeMap<String, PluginConfigValue>;

#[derive(Debug, Clone, Serialize)]
pub struct PluginConfigField {
    pub key: String,
    pub label: String,
    pub description: String,
    /// The type of the default also declares the type accepted by this field.
    pub default: PluginConfigValue,
}

#[derive(Debug, Clone, Serialize)]
pub struct PluginDefinition {
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub hooks: Vec<PluginHook>,
    pub config_fields: Vec<PluginConfigField>,
}

/// Provider-specific checks run before configuration is persisted. Implementations
/// receive normalized, typed values; disabled plugins may allow incomplete setup.
pub trait PluginConfigValidator: Send + Sync {
    fn validate(&self, config: &PluginConfig, enabled: bool) -> Result<(), UseCaseError>;
}

#[derive(Default)]
pub struct PluginRegistry {
    definitions: BTreeMap<String, PluginDefinition>,
    validators: BTreeMap<String, Arc<dyn PluginConfigValidator>>,
}

pub fn valid_plugin_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 48
        && id.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

impl PluginRegistry {
    pub fn new(definitions: Vec<PluginDefinition>) -> Result<Self, UseCaseError> {
        let mut entries = BTreeMap::new();
        for definition in definitions {
            if !valid_plugin_id(&definition.id)
                || definition.name.trim().is_empty()
                || definition.version.trim().is_empty()
                || definition.config_fields.len() > 16
            {
                return Err(UseCaseError::Invalid("插件清单无效".into()));
            }
            let mut fields = BTreeMap::new();
            for field in &definition.config_fields {
                if !valid_plugin_id(&field.key)
                    || field.label.trim().is_empty()
                    || fields.insert(&field.key, ()).is_some()
                {
                    return Err(UseCaseError::Invalid("插件配置字段无效或重复".into()));
                }
            }
            normalize_config(&definition, &PluginConfig::new())?;
            if entries.insert(definition.id.clone(), definition).is_some() {
                return Err(UseCaseError::Invalid("插件标识重复".into()));
            }
        }
        Ok(Self {
            definitions: entries,
            validators: BTreeMap::new(),
        })
    }

    pub fn definitions(&self) -> impl Iterator<Item = &PluginDefinition> {
        self.definitions.values()
    }

    pub fn with_config_validator(
        mut self,
        id: String,
        validator: Arc<dyn PluginConfigValidator>,
    ) -> Result<Self, UseCaseError> {
        let definition = self
            .definitions
            .get(&id)
            .ok_or_else(|| UseCaseError::Invalid("配置校验器对应的插件未注册".into()))?;
        validator.validate(&normalize_config(definition, &PluginConfig::new())?, false)?;
        if self.validators.insert(id, validator).is_some() {
            return Err(UseCaseError::Invalid("插件配置校验器重复".into()));
        }
        Ok(self)
    }

    fn validate_config(
        &self,
        id: &str,
        config: &PluginConfig,
        enabled: bool,
    ) -> Result<(), UseCaseError> {
        if let Some(validator) = self.validators.get(id) {
            validator.validate(config, enabled)?;
        }
        Ok(())
    }
}

fn normalize_config(
    definition: &PluginDefinition,
    config: &PluginConfig,
) -> Result<PluginConfig, UseCaseError> {
    if config
        .keys()
        .any(|key| !definition.config_fields.iter().any(|f| &f.key == key))
    {
        return Err(UseCaseError::Invalid("存在未声明的插件配置项".into()));
    }
    definition
        .config_fields
        .iter()
        .map(|field| {
            let value = config.get(&field.key).unwrap_or(&field.default);
            if std::mem::discriminant(value) != std::mem::discriminant(&field.default)
                || matches!(value, PluginConfigValue::Text(text) if text.len() > 2048)
            {
                return Err(UseCaseError::Invalid(format!(
                    "插件配置 {} 类型或长度无效",
                    field.label
                )));
            }
            Ok((field.key.clone(), value.clone()))
        })
        .collect()
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginState {
    pub enabled: bool,
    pub config: PluginConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginSettings {
    pub schema_version: u32,
    pub render_revision: u32,
    pub plugins: BTreeMap<String, PluginState>,
}

impl Default for PluginSettings {
    fn default() -> Self {
        Self {
            schema_version: 1,
            render_revision: 0,
            plugins: BTreeMap::new(),
        }
    }
}

impl PluginSettings {
    pub fn validate(&self) -> Result<(), UseCaseError> {
        if self.schema_version != 1
            || self.render_revision > MAX_RENDER_REVISION
            || self.plugins.len() > 128
            || self.plugins.iter().any(|(id, state)| {
                !valid_plugin_id(id)
                    || state.config.len() > 16
                    || state.config.iter().any(|(key, value)| {
                        !valid_plugin_id(key)
                            || matches!(value, PluginConfigValue::Text(text) if text.len() > 2048)
                    })
            })
        {
            return Err(UseCaseError::DataCorrupt("插件设置无效或版本不兼容".into()));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
pub struct PluginSettingsRecord {
    pub value: PluginSettings,
    /// Zero means no settings row has been written yet.
    pub version: i64,
}

#[async_trait]
pub trait PluginStore: Send + Sync {
    async fn load(&self) -> Result<PluginSettingsRecord, UseCaseError>;
    /// CAS and audit must commit in the same transaction.
    async fn save(
        &self,
        value: &PluginSettings,
        expected_version: i64,
        changed_plugin: &str,
        now: time::OffsetDateTime,
        audit: AuditContext,
    ) -> Result<SaveOutcome, UseCaseError>;
}

#[derive(Debug, Clone)]
pub struct PluginView {
    pub definition: PluginDefinition,
    pub available: bool,
    pub enabled: bool,
    pub config: PluginConfig,
}

#[derive(Debug, Clone)]
pub struct PluginsView {
    pub version: i64,
    pub plugins: Vec<PluginView>,
}

pub struct SavePluginCmd {
    pub id: String,
    pub enabled: bool,
    pub config: PluginConfig,
    pub expected_version: i64,
}

/// A per-operation snapshot: hooks and configuration cannot change halfway
/// through rendering. Entries always run in stable plugin-id order.
#[derive(Debug, Clone, Default)]
pub struct PluginSnapshot {
    pub render_revision: u32,
    pub active: BTreeMap<String, PluginConfig>,
}

pub struct PluginsInteractor {
    store: Arc<dyn PluginStore>,
    registry: Arc<PluginRegistry>,
    clock: Arc<dyn Clock>,
}

impl PluginsInteractor {
    pub fn new(
        store: Arc<dyn PluginStore>,
        registry: Arc<PluginRegistry>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            store,
            registry,
            clock,
        }
    }

    pub async fn snapshot(&self) -> Result<PluginSnapshot, UseCaseError> {
        let record = self.store.load().await?;
        record.value.validate()?;
        let mut active = BTreeMap::new();
        for definition in self.registry.definitions() {
            if let Some(state) = record
                .value
                .plugins
                .get(&definition.id)
                .filter(|s| s.enabled)
            {
                let config = normalize_config(definition, &state.config)
                    .and_then(|config| {
                        self.registry
                            .validate_config(&definition.id, &config, true)?;
                        Ok(config)
                    })
                    .map_err(|_| {
                        UseCaseError::DataCorrupt(format!(
                            "插件 {} 配置与清单不兼容",
                            definition.id
                        ))
                    })?;
                active.insert(definition.id.clone(), config);
            }
        }
        Ok(PluginSnapshot {
            render_revision: record.value.render_revision,
            active,
        })
    }

    pub async fn view(&self, actor: &Actor) -> Result<PluginsView, UseCaseError> {
        require_permission(actor)?;
        let record = self.store.load().await?;
        record.value.validate()?;
        self.view_of(record)
    }

    fn view_of(&self, record: PluginSettingsRecord) -> Result<PluginsView, UseCaseError> {
        let mut entries = record.value.plugins;
        let mut plugins = Vec::new();
        for definition in self.registry.definitions() {
            let state = entries.remove(&definition.id).unwrap_or_default();
            plugins.push(PluginView {
                definition: definition.clone(),
                available: true,
                enabled: state.enabled,
                config: normalize_config(definition, &state.config)?,
            });
        }
        // Removed code does not erase configuration; it can still be disabled.
        plugins.extend(entries.into_iter().map(|(id, state)| PluginView {
            definition: PluginDefinition {
                name: id.clone(),
                id,
                description: "当前程序未注册此插件".into(),
                version: String::new(),
                hooks: vec![],
                config_fields: vec![],
            },
            available: false,
            enabled: state.enabled,
            config: state.config,
        }));
        Ok(PluginsView {
            version: record.version,
            plugins,
        })
    }

    pub async fn save(
        &self,
        actor: &Actor,
        cmd: SavePluginCmd,
    ) -> Result<PluginsView, UseCaseError> {
        actor.ensure_write_channel()?;
        require_permission(actor)?;
        let mut current = self.store.load().await?;
        current.value.validate()?;
        let expected = checked_version(current.version, Some(cmd.expected_version))?;
        let definition = self.registry.definitions.get(&cmd.id);
        let old = current
            .value
            .plugins
            .get(&cmd.id)
            .cloned()
            .unwrap_or_default();
        let config = match definition {
            Some(definition) => normalize_config(definition, &cmd.config)?,
            None if current.value.plugins.contains_key(&cmd.id)
                && !cmd.enabled
                && cmd.config == old.config =>
            {
                cmd.config
            }
            None => return Err(UseCaseError::NotFound("插件未注册".into())),
        };
        let next = PluginState {
            enabled: cmd.enabled,
            config,
        };
        self.registry
            .validate_config(&cmd.id, &next.config, next.enabled)?;
        let old_normalized = PluginState {
            enabled: old.enabled,
            config: match definition {
                Some(definition) => normalize_config(definition, &old.config)?,
                None => old.config,
            },
        };
        if next == old_normalized {
            return self.view_of(current);
        }
        // An unavailable plugin may have previously owned content hooks.
        let affects_content = definition.is_none_or(|d| d.hooks.contains(&PluginHook::Content));
        if affects_content && (old.enabled || next.enabled) {
            current.value.render_revision = current
                .value
                .render_revision
                .checked_add(1)
                .filter(|revision| *revision <= MAX_RENDER_REVISION)
                .ok_or_else(|| UseCaseError::Invalid("插件渲染版本已达上限".into()))?;
        }
        current.value.plugins.insert(cmd.id.clone(), next);
        current.value.validate()?;
        match self
            .store
            .save(
                &current.value,
                expected,
                &cmd.id,
                self.clock.now(),
                actor.audit_context(),
            )
            .await?
        {
            SaveOutcome::Saved { new_version } => {
                current.version = new_version;
                self.view_of(current)
            }
            SaveOutcome::StaleConflict | SaveOutcome::Gone => Err(UseCaseError::VersionConflict),
        }
    }
}

fn require_permission(actor: &Actor) -> Result<(), UseCaseError> {
    if !actor.has_permission("plugins.manage") {
        return Err(UseCaseError::Forbidden);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginPage {
    Index,
    Post,
    Page,
    Tag,
    Category,
    Series,
    /// Isolated, non-persistent article/page body preview in the admin.
    Preview,
}

/// Immutable public files, supplied by registered plugin code. Configuration
/// cannot introduce URLs or files outside this snapshot.
#[derive(Clone)]
pub struct PluginAssets {
    pub id: String,
    pub version: String,
    pub files: Arc<BTreeMap<String, Arc<[u8]>>>,
}

impl PluginAssets {
    pub fn url(&self, path: &str) -> Result<String, UseCaseError> {
        if !self.files.contains_key(path) {
            return Err(UseCaseError::Render(format!(
                "插件 {} 资源未注册：{path}",
                self.id
            )));
        }
        Ok(format!(
            "/assets/plugins/{}/{}/{}",
            self.id, self.version, path
        ))
    }
}
