//! 渲染适配器：Markdown → 清洗 HTML，以及 MiniJinja 主题渲染。
//! MiniJinja 仅存在于本层；interfaces 通过应用端口间接使用。

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use application::error::UseCaseError;
use application::public_site::{
    CategoryView, PageView, PostCard, PostView, SeriesView, SiteInfo, TagView, ThemeRenderer,
};
use application::seo::SeoMeta;
use application::theme_data::ThemeData;
use minijinja::{Environment, UndefinedBehavior, Value};
use pulldown_cmark::{Options, Parser, html::push_html};

const THEME_API_VERSION: u32 = 1;
const THEME_FUNCTIONS: &[&str] = &[
    "get_posts",
    "get_post",
    "get_categories",
    "get_tags",
    "asset_url",
    "post_url",
];

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ThemeManifest {
    schema_version: u32,
    slug: String,
    name: String,
    theme_api_version: u32,
    required_functions: Vec<String>,
}

fn verify_manifest(theme_dir: &Path) -> Result<(), UseCaseError> {
    let path = theme_dir.join("theme.json");
    let raw =
        std::fs::read(&path).map_err(|e| UseCaseError::Render(format!("读取主题清单失败：{e}")))?;
    let manifest: ThemeManifest = serde_json::from_slice(&raw)
        .map_err(|e| UseCaseError::Render(format!("解析主题清单失败：{e}")))?;
    if manifest.schema_version != 1 || manifest.theme_api_version != THEME_API_VERSION {
        return Err(UseCaseError::Render("主题清单/API 版本不兼容".into()));
    }
    if manifest.slug.is_empty() || manifest.name.trim().is_empty() {
        return Err(UseCaseError::Render("主题清单名称不能为空".into()));
    }
    for required in &manifest.required_functions {
        if !THEME_FUNCTIONS.contains(&required.as_str()) {
            return Err(UseCaseError::Render(format!(
                "主题要求未知函数：{required}"
            )));
        }
    }
    Ok(())
}

/// Markdown 渲染 + ammonia 清洗。
/// 输出进入模板时以 |safe 注入，因此清洗步骤不可省略。
pub struct SanitizingMarkdownRenderer;

impl SanitizingMarkdownRenderer {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SanitizingMarkdownRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl application::ports::ContentRenderer for SanitizingMarkdownRenderer {
    fn render_markdown(&self, source: &str) -> String {
        let mut options = Options::empty();
        options.insert(Options::ENABLE_TABLES);
        options.insert(Options::ENABLE_STRIKETHROUGH);
        let parser = Parser::new_ext(source, options);
        let mut html = String::new();
        push_html(&mut html, parser);
        ammonia::clean(&html)
    }
}

/// 模板上下文（serde 序列化进入 MiniJinja）。
///
/// 每个上下文都带 `seo`：`base.html` 是所有模板的公共骨架，只能引用各上下文
/// 都存在的字段，SEO 元数据因此必须逐页传入而不是从 `site`/`post` 里各取一半。
#[derive(serde::Serialize)]
struct IndexContext<'a> {
    site: &'a SiteInfo,
    seo: &'a SeoMeta,
    posts: &'a [PostCard],
}

#[derive(serde::Serialize)]
struct PostContext<'a> {
    site: &'a SiteInfo,
    seo: &'a SeoMeta,
    post: &'a PostView,
}

#[derive(serde::Serialize)]
struct PageContext<'a> {
    site: &'a SiteInfo,
    seo: &'a SeoMeta,
    page: &'a PageView,
}

#[derive(serde::Serialize)]
struct TagContext<'a> {
    site: &'a SiteInfo,
    seo: &'a SeoMeta,
    tag: &'a TagView,
}

#[derive(serde::Serialize)]
struct CategoryContext<'a> {
    site: &'a SiteInfo,
    seo: &'a SeoMeta,
    category: &'a CategoryView,
}

#[derive(serde::Serialize)]
struct SeriesContext<'a> {
    site: &'a SiteInfo,
    seo: &'a SeoMeta,
    series: &'a SeriesView,
}

/// `url` 过滤器：URL 进入 HTML 属性时只转义属性真正需要的字符，保留 `/`。
///
/// MiniJinja 的 HTML 自动转义把 `/` 也写成 `&#x2f;`（合法 HTML，浏览器与抓取器
/// 都会解码），但 canonical、og:url 与 RSS 自动发现地址会因此变得不可读，外部
/// 工具做字符串比对时也会失配。这里的值由应用层用「装配期校验的基础地址 +
/// 百分号编码的 slug」拼成，不可能含 `<`、`>`、`"`、`'`；仍逐字符兜底转义，
/// 使过滤器本身不依赖调用方是否守规矩。
fn url_attr(value: String) -> Value {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            other => out.push(other),
        }
    }
    Value::from_safe_string(out)
}

/// MiniJinja 主题渲染器：启动时加载并解析主题模板，请求期复用。
pub struct MiniJinjaThemeRenderer {
    env: Environment<'static>,
    data: Option<Arc<ThemeData>>,
    assets: Arc<HashMap<String, String>>,
}

impl MiniJinjaThemeRenderer {
    /// M1 最小模板集：base/index/post/page；M3 增加标签页 tag。
    /// 模板在启动时一次性加载；Environment 复用要求 'static，故按启动期资源泄漏源码。
    pub fn load(theme_dir: &Path) -> Result<Self, UseCaseError> {
        verify_manifest(theme_dir)?;
        let mut env = Environment::new();
        env.set_undefined_behavior(UndefinedBehavior::Strict);
        env.set_fuel(Some(200_000));
        env.set_recursion_limit(100);
        env.add_filter("url", url_attr);
        for name in [
            "base.html",
            "index.html",
            "post.html",
            "page.html",
            "tag.html",
            "category.html",
            "series.html",
        ] {
            let path = theme_dir.join("templates").join(name);
            let source = std::fs::read_to_string(&path)
                .map_err(|e| UseCaseError::Render(format!("读取模板 {name} 失败：{e}")))?;
            let source: &'static str = Box::leak(source.into_boxed_str());
            env.add_template(name, source)
                .map_err(|e| UseCaseError::Render(format!("解析模板 {name} 失败：{e}")))?;
        }
        let assets = load_asset_urls(&theme_dir.join("assets"))?;
        Ok(Self {
            env,
            data: None,
            assets: Arc::new(assets),
        })
    }

    pub fn with_data(mut self, data: Arc<ThemeData>) -> Self {
        self.data = Some(data);
        self
    }

    fn render<T: serde::Serialize>(
        &self,
        template: &str,
        context: T,
    ) -> Result<String, UseCaseError> {
        let mut env = self.env.clone();
        let scope =
            crate::theme_functions::RenderScope::new(self.data.clone(), self.assets.clone())?;
        crate::theme_functions::register(&mut env, scope);
        let html = env
            .get_template(template)
            .and_then(|t| t.render(context))
            .map_err(|e| UseCaseError::Render(e.to_string()))?;
        if html.len() > 1024 * 1024 {
            return Err(UseCaseError::Render("主题输出超过 1 MiB".into()));
        }
        Ok(html)
    }
}

fn load_asset_urls(dir: &Path) -> Result<HashMap<String, String>, UseCaseError> {
    use sha2::{Digest, Sha256};
    let mut urls = HashMap::new();
    if !dir.is_dir() {
        return Ok(urls);
    }
    fn visit(
        root: &Path,
        dir: &Path,
        urls: &mut HashMap<String, String>,
    ) -> Result<(), UseCaseError> {
        for entry in std::fs::read_dir(dir).map_err(|e| UseCaseError::Render(e.to_string()))? {
            let entry = entry.map_err(|e| UseCaseError::Render(e.to_string()))?;
            let kind = entry
                .file_type()
                .map_err(|e| UseCaseError::Render(e.to_string()))?;
            if kind.is_symlink() {
                return Err(UseCaseError::Render("主题资源不允许符号链接".into()));
            }
            if kind.is_dir() {
                visit(root, &entry.path(), urls)?;
            } else if kind.is_file() {
                let path = entry.path();
                let relative = path
                    .strip_prefix(root)
                    .map_err(|e| UseCaseError::Render(e.to_string()))?;
                let name = relative
                    .to_str()
                    .ok_or_else(|| UseCaseError::Render("主题资源路径不是 UTF-8".into()))?
                    .replace('\\', "/");
                let bytes =
                    std::fs::read(&path).map_err(|e| UseCaseError::Render(e.to_string()))?;
                let hash = format!("{:x}", Sha256::digest(bytes));
                let encoded = name
                    .split('/')
                    .map(application::seo::encode_path_segment)
                    .collect::<Vec<_>>()
                    .join("/");
                urls.insert(name, format!("/assets/{encoded}?v={}", &hash[..12]));
            }
        }
        Ok(())
    }
    visit(dir, dir, &mut urls)?;
    Ok(urls)
}

impl ThemeRenderer for MiniJinjaThemeRenderer {
    fn render_index(
        &self,
        site: &SiteInfo,
        seo: &SeoMeta,
        posts: &[PostCard],
    ) -> Result<String, UseCaseError> {
        let ctx = IndexContext { site, seo, posts };
        self.render("index.html", ctx)
    }

    fn render_post(
        &self,
        site: &SiteInfo,
        seo: &SeoMeta,
        post: &PostView,
    ) -> Result<String, UseCaseError> {
        let ctx = PostContext { site, seo, post };
        self.render("post.html", ctx)
    }

    fn render_page(
        &self,
        site: &SiteInfo,
        seo: &SeoMeta,
        page: &PageView,
    ) -> Result<String, UseCaseError> {
        let ctx = PageContext { site, seo, page };
        self.render("page.html", ctx)
    }

    fn render_tag(
        &self,
        site: &SiteInfo,
        seo: &SeoMeta,
        tag: &TagView,
    ) -> Result<String, UseCaseError> {
        let ctx = TagContext { site, seo, tag };
        self.render("tag.html", ctx)
    }

    fn render_category(
        &self,
        site: &SiteInfo,
        seo: &SeoMeta,
        category: &CategoryView,
    ) -> Result<String, UseCaseError> {
        let ctx = CategoryContext {
            site,
            seo,
            category,
        };
        self.render("category.html", ctx)
    }

    fn render_series(
        &self,
        site: &SiteInfo,
        seo: &SeoMeta,
        series: &SeriesView,
    ) -> Result<String, UseCaseError> {
        let ctx = SeriesContext { site, seo, series };
        self.render("series.html", ctx)
    }
}

#[cfg(test)]
mod tests {
    use super::{url_attr, verify_manifest};

    #[test]
    fn url_attr_keeps_slashes_and_escapes_attribute_specials() {
        let out = url_attr("https://example.com/posts/a?x=1&y=2".into());
        assert_eq!(
            out.as_str().unwrap(),
            "https://example.com/posts/a?x=1&amp;y=2",
            "斜杠必须保留，& 必须转义"
        );
        // 标记为安全字符串：模板里跳过自动转义，否则 `/` 又会被写成 &#x2f;。
        assert!(out.is_safe());
    }

    #[test]
    fn url_attr_cannot_break_out_of_a_quoted_attribute() {
        let out = url_attr("\" onmouseover=\"alert(1)".into());
        let rendered = out.as_str().unwrap();
        assert!(!rendered.contains('"'), "{rendered}");
        assert!(rendered.contains("&quot;"), "{rendered}");
    }

    #[test]
    fn theme_manifest_rejects_incompatible_api_and_unknown_function() {
        let dir =
            std::env::temp_dir().join(format!("blog-theme-manifest-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("theme.json");
        std::fs::write(&path, r#"{"schema_version":1,"slug":"x","name":"X","theme_api_version":2,"required_functions":[]}"#).unwrap();
        assert!(verify_manifest(&dir).is_err());
        std::fs::write(&path, r#"{"schema_version":1,"slug":"x","name":"X","theme_api_version":1,"required_functions":["admin_sql"]}"#).unwrap();
        assert!(verify_manifest(&dir).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
