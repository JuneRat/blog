//! Trusted, compiled-in plugins. The catalog owns code and immutable assets;
//! application::plugins owns configuration, permissions and lifecycle state.
mod analytics_umami;
mod markdown_enhance;
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
        PluginAssets, PluginConfig, PluginConfigValidator, PluginDefinition, PluginHook,
        PluginPage, PluginRegistry, PluginSnapshot, PluginsInteractor,
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

/// External scripts are declared by trusted providers, never supplied as HTML.
#[derive(Clone)]
pub struct ExternalScript {
    pub src: String,
    pub data_attributes: BTreeMap<String, String>,
}

pub trait PageHeadHook: Send + Sync {
    fn assets(
        &self,
        page: PluginPage,
        config: &PluginConfig,
    ) -> Result<Vec<HeadAsset>, UseCaseError>;

    fn external_scripts(
        &self,
        _page: PluginPage,
        _config: &PluginConfig,
    ) -> Result<Vec<ExternalScript>, UseCaseError> {
        Ok(vec![])
    }
}

pub struct PluginRegistration {
    pub definition: PluginDefinition,
    pub content: Option<Arc<dyn ContentHook>>,
    pub html_rules: Vec<HtmlRule>,
    pub page_head: Option<Arc<dyn PageHeadHook>>,
    pub config_validator: Option<Arc<dyn PluginConfigValidator>>,
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
        Self::new(vec![
            analytics_umami::registration(),
            markdown_enhance::registration(),
        ])
        .expect("valid built-in plugin catalog")
    }

    pub fn new(registrations: Vec<PluginRegistration>) -> Result<Self, UseCaseError> {
        let mut plugins = BTreeMap::new();
        let mut definitions = Vec::new();
        let mut validators = Vec::new();
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
            if let Some(validator) = registration.config_validator {
                validators.push((id.clone(), validator));
            }
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
        let mut registry = PluginRegistry::new(definitions)?;
        for (id, validator) in validators {
            registry = registry.with_config_validator(id, validator)?;
        }
        let registry = Arc::new(registry);
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
        let mut external_seen = BTreeSet::new();
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
            // External providers must never receive data from an editor preview.
            if page != PluginPage::Preview {
                for script in hook.external_scripts(page, config)? {
                    let url = browser_url(&script.src, "插件脚本地址")?;
                    if url.fragment().is_some()
                        || script.data_attributes.len() > 16
                        || script.data_attributes.iter().any(|(key, value)| {
                            !key.starts_with("data-")
                                || key.len() <= 5
                                || !valid_token(key)
                                || value.len() > 2048
                        })
                    {
                        return Err(UseCaseError::Render("插件外部脚本声明无效".into()));
                    }
                    if external_seen.insert((url.to_string(), script.data_attributes.clone())) {
                        html.push_str(&format!(
                            "<script src=\"{}\" defer",
                            escape_attribute(url.as_str())
                        ));
                        for (key, value) in script.data_attributes {
                            html.push_str(&format!(" {key}=\"{}\"", escape_attribute(&value)));
                        }
                        html.push_str("></script>\n");
                    }
                }
            }
        }
        Ok(html)
    }
}

/// Browser-only targets: HTTPS, or loopback HTTP for local development. The
/// server never fetches these URLs; credentials and ambiguous whitespace fail.
fn browser_url(raw: &str, label: &str) -> Result<url::Url, UseCaseError> {
    let raw = raw.trim();
    let invalid = || {
        UseCaseError::Invalid(format!(
            "{label}须为无账号密码的 HTTPS 地址，本机开发可使用 HTTP"
        ))
    };
    let url = url::Url::parse(raw).map_err(|_| invalid())?;
    let loopback = match url.host() {
        Some(url::Host::Domain(host)) => host == "localhost",
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    };
    if raw.len() > 2048
        || raw.chars().any(|c| c.is_whitespace() || c.is_control())
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || !(url.scheme() == "https" || (url.scheme() == "http" && loopback))
    {
        return Err(invalid());
    }
    Ok(url)
}

fn escape_attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\'', "&#39;")
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

#[cfg(test)]
mod external_script_tests {
    use super::*;

    struct Hook(Vec<ExternalScript>);
    impl PageHeadHook for Hook {
        fn assets(&self, _: PluginPage, _: &PluginConfig) -> Result<Vec<HeadAsset>, UseCaseError> {
            Ok(vec![])
        }
        fn external_scripts(
            &self,
            _: PluginPage,
            _: &PluginConfig,
        ) -> Result<Vec<ExternalScript>, UseCaseError> {
            Ok(self.0.clone())
        }
    }

    fn render(scripts: Vec<ExternalScript>, page: PluginPage) -> Result<String, UseCaseError> {
        let catalog = PluginCatalog::new(vec![PluginRegistration {
            definition: PluginDefinition {
                id: "external-fixture".into(),
                name: "External fixture".into(),
                description: String::new(),
                version: "1".into(),
                hooks: vec![],
                config_fields: vec![],
            },
            content: None,
            html_rules: vec![],
            page_head: Some(Arc::new(Hook(scripts))),
            config_validator: None,
            files: BTreeMap::new(),
        }])?;
        catalog.head_html(
            page,
            &PluginSnapshot {
                active: BTreeMap::from([("external-fixture".into(), PluginConfig::new())]),
                ..Default::default()
            },
        )
    }

    #[test]
    fn external_declarations_escape_values_deduplicate_and_cannot_add_event_handlers() {
        let script = ExternalScript {
            src: "https://stats.example.test/script.js?a=1&b=2".into(),
            data_attributes: BTreeMap::from([(
                "data-value".into(),
                "\"><script>bad()</script>&'".into(),
            )]),
        };
        let html = render(vec![script.clone(), script.clone()], PluginPage::Index).unwrap();
        assert_eq!(html.matches("<script ").count(), 1);
        assert!(!html.contains("<script>bad()"));
        assert!(
            html.contains("data-value=\"&quot;&gt;&lt;script&gt;bad()&lt;/script&gt;&amp;&#39;\"")
        );
        assert!(html.contains("?a=1&amp;b=2"));
        // Even an external hook that ignores page scope cannot enter a preview.
        assert!(
            render(vec![script.clone()], PluginPage::Preview)
                .unwrap()
                .is_empty()
        );
        for name in ["onload", "src", "data-", "data-x\" onload"] {
            let mut invalid = script.clone();
            invalid.data_attributes = BTreeMap::from([(name.into(), "bad()".into())]);
            assert!(render(vec![invalid], PluginPage::Index).is_err());
        }
    }
}
