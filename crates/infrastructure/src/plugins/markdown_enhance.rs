//! Markdown notation stays as escaped text in stored HTML. Browser bundles do
//! the final layout; the same bundles run in public themes and admin previews.
use super::*;
use application::plugins::{PluginConfigField, PluginConfigValue};

struct MarkdownEnhance;

fn enabled(config: &PluginConfig, key: &str) -> bool {
    matches!(config.get(key), Some(PluginConfigValue::Boolean(true)))
}

impl ContentHook for MarkdownEnhance {
    fn prepare(
        &self,
        source: String,
        options: &mut Options,
        config: &PluginConfig,
    ) -> Result<String, UseCaseError> {
        if enabled(config, "math") {
            // Parser events handle code spans, escaped dollars and TeX escaping.
            options.insert(Options::ENABLE_MATH);
        }
        // Fenced Mermaid already renders to <pre><code class="language-mermaid">.
        // Preserve that class below; no HTML/string reparsing is necessary.
        Ok(source)
    }
}

impl PageHeadHook for MarkdownEnhance {
    fn assets(
        &self,
        page: PluginPage,
        config: &PluginConfig,
    ) -> Result<Vec<HeadAsset>, UseCaseError> {
        if !matches!(
            page,
            PluginPage::Post | PluginPage::Page | PluginPage::Preview
        ) {
            return Ok(vec![]);
        }
        let math = enabled(config, "math");
        let mermaid = enabled(config, "mermaid");
        let mut assets = Vec::new();
        if math || mermaid {
            assets.push(HeadAsset {
                kind: HeadAssetKind::Stylesheet,
                path: "display.css".into(),
            });
        }
        if math {
            assets.push(HeadAsset {
                kind: HeadAssetKind::Stylesheet,
                path: "katex.css".into(),
            });
            assets.push(HeadAsset {
                kind: HeadAssetKind::Script,
                path: "math.js".into(),
            });
        }
        if mermaid {
            assets.push(HeadAsset {
                kind: HeadAssetKind::Script,
                path: "mermaid.js".into(),
            });
        }
        Ok(assets)
    }
}

pub(super) fn registration() -> PluginRegistration {
    let hook = Arc::new(MarkdownEnhance);
    PluginRegistration {
        definition: PluginDefinition {
            id: "markdown-enhance".into(),
            name: "Markdown 增强".into(),
            description: "支持 KaTeX 公式与 Mermaid 图表，文章、独立页面及正文预览共用渲染规则。开启后请通过内容重建更新历史正文。".into(),
            version: "1.0.0".into(),
            hooks: vec![],
            config_fields: vec![
                PluginConfigField {
                    key: "math".into(), label: "公式".into(),
                    description: "使用 $...$ 编写行内公式，$$...$$ 编写独立公式。".into(),
                    default: PluginConfigValue::Boolean(true),
                },
                PluginConfigField {
                    key: "mermaid".into(), label: "Mermaid 图表".into(),
                    description: "使用 mermaid 代码块编写流程图、时序图等。启用后文章和独立页面会加载图表脚本。".into(),
                    default: PluginConfigValue::Boolean(true),
                },
            ],
        },
        content: Some(hook.clone()),
        page_head: Some(hook),
        html_rules: vec![
            HtmlRule { tag: "span", classes: vec!["math", "math-inline", "math-display"], data_attributes: vec![] },
            HtmlRule { tag: "code", classes: vec!["language-mermaid"], data_attributes: vec![] },
        ],
        files: include!("../../assets/markdown-enhance/files.rs"),
    }
}
