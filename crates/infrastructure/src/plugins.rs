//! Trusted, compiled-in plugins. The catalog owns code and immutable assets;
//! application::plugins owns configuration, permissions and lifecycle state.
mod store;
pub use store::PostgresPluginStore;
pub(crate) use store::content_version_on;

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use application::{
    UseCaseError,
    plugins::{
        PluginAssets, PluginConfig, PluginDefinition, PluginHook, PluginPage, PluginRegistry,
        PluginSnapshot, PluginsInteractor,
    },
};
use pulldown_cmark::{Options, Parser, html::push_html};
use sha2::{Digest, Sha256};

/// Runs before the final sanitizer. No plugin may bypass that final pass.
pub trait ContentHook: Send + Sync {
    fn prepare(
        &self,
        source: String,
        _options: &mut Options,
        _config: &PluginConfig,
    ) -> Result<String, UseCaseError> {
        Ok(source)
    }
    fn transform_html(&self, html: String, _config: &PluginConfig) -> Result<String, UseCaseError> {
        Ok(html)
    }
}

/// Opt in only the classes and data attributes needed by extension nodes.
/// Executable markup, event handlers and inline styles remain prohibited.
#[derive(Default)]
pub struct HtmlRule {
    pub tag: &'static str,
    pub classes: Vec<&'static str>,
    pub data_attributes: Vec<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum HeadAssetKind {
    Stylesheet,
    Script,
}

pub struct HeadAsset {
    pub kind: HeadAssetKind,
    /// A relative path from this plugin's registered file snapshot.
    pub path: String,
}

pub trait PageHeadHook: Send + Sync {
    fn assets(
        &self,
        page: PluginPage,
        config: &PluginConfig,
    ) -> Result<Vec<HeadAsset>, UseCaseError>;
}

pub struct PluginRegistration {
    pub definition: PluginDefinition,
    pub content: Option<Arc<dyn ContentHook>>,
    pub html_rules: Vec<HtmlRule>,
    pub page_head: Option<Arc<dyn PageHeadHook>>,
    pub files: BTreeMap<String, Arc<[u8]>>,
}

struct RegisteredPlugin {
    content: Option<Arc<dyn ContentHook>>,
    html_rules: Vec<HtmlRule>,
    page_head: Option<Arc<dyn PageHeadHook>>,
    assets: PluginAssets,
}

pub struct PluginCatalog {
    registry: Arc<PluginRegistry>,
    plugins: BTreeMap<String, RegisteredPlugin>,
}

impl PluginCatalog {
    /// Built-in plugins are opt-in. Test fixtures use separate catalogs.
    pub fn builtins() -> Self {
        Self::new(vec![]).expect("valid built-in plugin catalog")
    }

    pub fn new(registrations: Vec<PluginRegistration>) -> Result<Self, UseCaseError> {
        let mut plugins = BTreeMap::new();
        let mut definitions = Vec::new();
        for mut registration in registrations {
            registration.definition.hooks = [
                registration.content.as_ref().map(|_| PluginHook::Content),
                registration
                    .page_head
                    .as_ref()
                    .map(|_| PluginHook::PageHead),
            ]
            .into_iter()
            .flatten()
            .collect();
            for rule in &registration.html_rules {
                if registration.content.is_none()
                    || !["span", "div", "pre", "code"].contains(&rule.tag)
                    || rule.classes.iter().any(|class| !valid_token(class))
                    || rule
                        .data_attributes
                        .iter()
                        .any(|attr| !attr.starts_with("data-") || !valid_token(attr))
                {
                    return Err(UseCaseError::Invalid("插件正文节点规则无效".into()));
                }
            }
            let mut hash = Sha256::new();
            for (path, bytes) in &registration.files {
                if !valid_asset_path(path) {
                    return Err(UseCaseError::Invalid("插件资源路径无效".into()));
                }
                hash.update((path.len() as u64).to_be_bytes());
                hash.update(path.as_bytes());
                hash.update((bytes.len() as u64).to_be_bytes());
                hash.update(bytes);
            }
            let id = registration.definition.id.clone();
            let assets = PluginAssets {
                id: id.clone(),
                version: format!("{:x}", hash.finalize()),
                files: Arc::new(registration.files),
            };
            definitions.push(registration.definition);
            plugins.insert(
                id,
                RegisteredPlugin {
                    content: registration.content,
                    html_rules: registration.html_rules,
                    page_head: registration.page_head,
                    assets,
                },
            );
        }
        let registry = Arc::new(PluginRegistry::new(definitions)?);
        Ok(Self { registry, plugins })
    }

    pub fn registry(&self) -> Arc<PluginRegistry> {
        self.registry.clone()
    }

    pub fn assets(&self) -> Vec<PluginAssets> {
        self.plugins
            .values()
            .map(|plugin| plugin.assets.clone())
            .collect()
    }

    pub(crate) fn render_markdown(
        &self,
        source: &str,
        snapshot: &PluginSnapshot,
    ) -> Result<String, UseCaseError> {
        let mut options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
        let mut source = source.to_owned();
        for (id, config) in &snapshot.active {
            if let Some(hook) = self.plugins.get(id).and_then(|p| p.content.as_ref()) {
                source = hook.prepare(source, &mut options, config)?;
                domain::content::budget::validate_source(&source)
                    .map_err(|e| UseCaseError::Render(e.to_string()))?;
            }
        }
        let mut html = String::new();
        push_html(&mut html, Parser::new_ext(&source, options));
        let mut sanitizer = ammonia::Builder::default();
        for (id, config) in &snapshot.active {
            if let Some(plugin) = self.plugins.get(id) {
                if let Some(hook) = &plugin.content {
                    html = hook.transform_html(html, config)?;
                    application::rendering_budget::validate_html(&html)
                        .map_err(|e| UseCaseError::Render(e.to_string()))?;
                }
                for rule in &plugin.html_rules {
                    sanitizer.add_allowed_classes(rule.tag, &rule.classes);
                    sanitizer.add_tag_attributes(rule.tag, &rule.data_attributes);
                }
            }
        }
        Ok(sanitizer.clean(&html).to_string())
    }

    pub(crate) fn head_html(
        &self,
        page: PluginPage,
        snapshot: &PluginSnapshot,
    ) -> Result<String, UseCaseError> {
        let mut html = String::new();
        let mut seen = BTreeSet::new();
        for (id, config) in &snapshot.active {
            let Some(plugin) = self.plugins.get(id) else {
                continue;
            };
            let Some(hook) = &plugin.page_head else {
                continue;
            };
            for asset in hook.assets(page, config)? {
                let extension = match asset.kind {
                    HeadAssetKind::Stylesheet => ".css",
                    HeadAssetKind::Script => ".js",
                };
                if !asset.path.ends_with(extension) {
                    return Err(UseCaseError::Render("插件头部资源类型不匹配".into()));
                }
                let url = plugin.assets.url(&asset.path)?;
                if seen.insert((asset.kind, url.clone())) {
                    // Paths contain only safe URL/attribute characters, validated at registration.
                    match asset.kind {
                        HeadAssetKind::Stylesheet => {
                            html.push_str(&format!("<link rel=\"stylesheet\" href=\"{url}\">\n"))
                        }
                        HeadAssetKind::Script => {
                            html.push_str(&format!("<script src=\"{url}\" defer></script>\n"))
                        }
                    }
                }
            }
        }
        Ok(html)
    }
}

fn valid_token(token: &str) -> bool {
    !token.is_empty()
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

fn valid_asset_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 256
        && path.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        })
}

pub struct PluginRuntime {
    pub catalog: Arc<PluginCatalog>,
    pub manager: Arc<PluginsInteractor>,
}

impl PluginRuntime {
    pub fn new(
        catalog: Arc<PluginCatalog>,
        store: Arc<dyn application::plugins::PluginStore>,
        clock: Arc<dyn application::ports::Clock>,
    ) -> Self {
        let manager = Arc::new(PluginsInteractor::new(store, catalog.registry(), clock));
        Self { catalog, manager }
    }
}
