//! 渲染适配器：Markdown → 清洗 HTML，以及 MiniJinja 主题渲染。
//! MiniJinja 仅存在于本层；interfaces 通过应用端口间接使用。

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;

use application::error::UseCaseError;
use application::public_site::{CategoryView, PageView, PostCard, PostView, SeriesView, TagView};
use application::seo::SeoMeta;
use application::site_info::SiteInfo;
use application::theme_data::ThemeData;
use application::themes::ThemeAssets;
use minijinja::{AutoEscape, Environment, UndefinedBehavior, Value};
use pulldown_cmark::{Options, Parser, html::push_html};
use sha2::{Digest, Sha256};

pub use crate::render_executor::{RenderingLimits, RenderingRuntime};

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

fn verify_manifest(theme_dir: &Path) -> Result<ThemeManifest, UseCaseError> {
    let path = theme_dir.join("theme.json");
    let raw =
        std::fs::read(&path).map_err(|e| UseCaseError::Render(format!("读取主题清单失败：{e}")))?;
    let manifest: ThemeManifest = serde_json::from_slice(&raw)
        .map_err(|e| UseCaseError::Render(format!("解析主题清单失败：{e}")))?;
    if manifest.schema_version != 1 || manifest.theme_api_version != THEME_API_VERSION {
        return Err(UseCaseError::Render("主题清单/API 版本不兼容".into()));
    }
    if manifest.slug.is_empty()
        || !manifest
            .slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        || manifest.name.trim().is_empty()
    {
        return Err(UseCaseError::Render("主题清单名称不能为空".into()));
    }
    for required in &manifest.required_functions {
        if !THEME_FUNCTIONS.contains(&required.as_str()) {
            return Err(UseCaseError::Render(format!(
                "主题要求未知函数：{required}"
            )));
        }
    }
    Ok(manifest)
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

impl SanitizingMarkdownRenderer {
    pub fn render_markdown(&self, source: &str) -> String {
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
    pagination: &'a application::public_site::IndexPagination,
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
#[derive(Clone)]
pub struct MiniJinjaThemeRenderer {
    env: Environment<'static>,
    data: Option<Arc<ThemeData>>,
    assets: Arc<HashMap<String, String>>,
    release_assets: ThemeAssets,
    slug: String,
    name: String,
}

impl MiniJinjaThemeRenderer {
    /// Load a release snapshot. Production registration uses load_checked below.
    pub fn load(theme_dir: &Path) -> Result<Self, UseCaseError> {
        let manifest = verify_manifest(theme_dir)?;
        let mut env = Environment::new();
        // Every theme template emits HTML, including helpers without an .html suffix.
        env.set_auto_escape_callback(|_| AutoEscape::Html);
        env.set_undefined_behavior(UndefinedBehavior::Strict);
        env.set_fuel(Some(200_000));
        env.set_recursion_limit(100);
        env.add_filter("url", url_attr);
        let templates = read_theme_files(&theme_dir.join("templates"))?;
        let asset_files = read_theme_files(&theme_dir.join("assets"))?;
        // Length-prefix each name/body so distinct file trees cannot hash identically
        // due to concatenation ambiguity. BTreeMap makes the release ID deterministic.
        let mut digest = Sha256::new();
        digest.update(THEME_API_VERSION.to_be_bytes());
        digest.update(manifest.slug.as_bytes());
        for (kind, files) in [("templates", &templates), ("assets", &asset_files)] {
            digest.update(kind.as_bytes());
            for (name, bytes) in files {
                digest.update((name.len() as u64).to_be_bytes());
                digest.update(name.as_bytes());
                digest.update((bytes.len() as u64).to_be_bytes());
                digest.update(bytes);
            }
        }
        for (name, bytes) in templates {
            let source = String::from_utf8(bytes.to_vec())
                .map_err(|e| UseCaseError::Render(format!("模板 {name} 不是 UTF-8：{e}")))?;
            env.add_template_owned(name.clone(), source)
                .map_err(|e| UseCaseError::Render(format!("解析模板 {name} 失败：{e}")))?;
        }
        for name in [
            "index.html",
            "post.html",
            "page.html",
            "tag.html",
            "category.html",
            "series.html",
        ] {
            env.get_template(name)
                .map_err(|e| UseCaseError::Render(format!("缺少页面入口 {name}：{e}")))?;
        }
        let release_assets = ThemeAssets {
            slug: manifest.slug.clone(),
            version: format!("{:x}", digest.finalize()),
            files: Arc::new(asset_files),
        };
        let assets = release_assets
            .files
            .keys()
            .map(|name| (name.clone(), release_assets.url(name)))
            .collect();
        Ok(Self {
            env,
            data: None,
            assets: Arc::new(assets),
            release_assets,
            slug: manifest.slug,
            name: manifest.name,
        })
    }

    /// Validate all page contracts with fixed public data before registering a release.
    pub async fn load_checked(
        theme_dir: &Path,
        runtime: &RenderingRuntime,
    ) -> Result<Self, UseCaseError> {
        let renderer = Self::load(theme_dir)?;
        renderer.validate(runtime).await?;
        Ok(renderer)
    }

    pub async fn validate(&self, runtime: &RenderingRuntime) -> Result<(), UseCaseError> {
        crate::theme_validation::validate(self, runtime).await
    }

    pub fn assets(&self) -> ThemeAssets {
        self.release_assets.clone()
    }

    pub fn slug(&self) -> &str {
        &self.slug
    }
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn with_data(mut self, data: Arc<ThemeData>) -> Self {
        self.data = Some(data);
        self
    }

    fn render<T: serde::Serialize>(
        &self,
        template: &str,
        context: T,
        time_zone: &str,
    ) -> Result<String, UseCaseError> {
        let mut env = self.env.clone();
        let dates = Arc::new(crate::SiteTimeZone::parse(time_zone).map_err(UseCaseError::Invalid)?);
        let data = self
            .data
            .as_ref()
            .map(|data| Arc::new(data.as_ref().clone().with_time_zone(dates)));
        let scope = crate::theme_functions::RenderScope::new(data, self.assets.clone())?;
        crate::theme_functions::register(&mut env, scope);
        let html = env
            .get_template(template)
            .and_then(|t| t.render(context))
            .map_err(|e| UseCaseError::Render(e.to_string()))?;
        if html.len() > application::rendering_budget::MAX_PAGE_HTML_BYTES {
            return Err(UseCaseError::Render("主题输出超过 1 MiB".into()));
        }
        Ok(html)
    }
}

/// Snapshot both templates and assets, refusing links at every level including the root.
fn read_theme_files(root: &Path) -> Result<BTreeMap<String, Arc<[u8]>>, UseCaseError> {
    fn visit(
        root: &Path,
        path: &Path,
        files: &mut BTreeMap<String, Arc<[u8]>>,
    ) -> Result<(), UseCaseError> {
        let meta = std::fs::symlink_metadata(path).map_err(|e| {
            UseCaseError::Render(format!("读取主题文件 {} 失败：{e}", path.display()))
        })?;
        if meta.file_type().is_symlink() {
            return Err(UseCaseError::Render("主题文件不允许符号链接".into()));
        }
        if meta.is_dir() {
            for entry in std::fs::read_dir(path).map_err(|e| UseCaseError::Render(e.to_string()))? {
                let entry = entry.map_err(|e| UseCaseError::Render(e.to_string()))?;
                visit(root, &entry.path(), files)?;
            }
        } else if meta.is_file() {
            let name = path
                .strip_prefix(root)
                .ok()
                .and_then(|p| p.to_str())
                .ok_or_else(|| UseCaseError::Render("主题路径不是 UTF-8".into()))?;
            if name.contains('\\') {
                return Err(UseCaseError::Render("主题路径不允许反斜杠".into()));
            }
            let bytes = std::fs::read(path).map_err(|e| UseCaseError::Render(e.to_string()))?;
            files.insert(name.to_string(), bytes.into());
        } else {
            return Err(UseCaseError::Render("主题只允许普通文件和目录".into()));
        }
        Ok(())
    }
    let mut files = BTreeMap::new();
    match std::fs::symlink_metadata(root) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(files),
        _ => visit(root, root, &mut files)?,
    }
    Ok(files)
}

impl MiniJinjaThemeRenderer {
    pub(crate) fn render_index(
        &self,
        site: &SiteInfo,
        seo: &SeoMeta,
        posts: &[PostCard],
        pagination: &application::public_site::IndexPagination,
    ) -> Result<String, UseCaseError> {
        let ctx = IndexContext {
            site,
            seo,
            posts,
            pagination,
        };
        self.render("index.html", ctx, &site.time_zone)
    }

    pub(crate) fn render_post(
        &self,
        site: &SiteInfo,
        seo: &SeoMeta,
        post: &PostView,
    ) -> Result<String, UseCaseError> {
        let ctx = PostContext { site, seo, post };
        self.render("post.html", ctx, &site.time_zone)
    }

    pub(crate) fn render_page(
        &self,
        site: &SiteInfo,
        seo: &SeoMeta,
        page: &PageView,
    ) -> Result<String, UseCaseError> {
        let ctx = PageContext { site, seo, page };
        self.render("page.html", ctx, &site.time_zone)
    }

    pub(crate) fn render_tag(
        &self,
        site: &SiteInfo,
        seo: &SeoMeta,
        tag: &TagView,
    ) -> Result<String, UseCaseError> {
        let ctx = TagContext { site, seo, tag };
        self.render("tag.html", ctx, &site.time_zone)
    }

    pub(crate) fn render_category(
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
        self.render("category.html", ctx, &site.time_zone)
    }

    pub(crate) fn render_series(
        &self,
        site: &SiteInfo,
        seo: &SeoMeta,
        series: &SeriesView,
    ) -> Result<String, UseCaseError> {
        let ctx = SeriesContext { site, seo, series };
        self.render("series.html", ctx, &site.time_zone)
    }
}

#[cfg(test)]
mod tests {
    use super::{url_attr, verify_manifest};

    struct TestTheme(std::path::PathBuf);

    impl TestTheme {
        fn new() -> Self {
            let dir =
                std::env::temp_dir().join(format!("blog-theme-check-{}", uuid::Uuid::now_v7()));
            std::fs::create_dir_all(dir.join("templates")).unwrap();
            std::fs::create_dir_all(dir.join("assets")).unwrap();
            std::fs::copy("../../themes/default/theme.json", dir.join("theme.json")).unwrap();
            for kind in ["templates", "assets"] {
                for entry in std::fs::read_dir(format!("../../themes/default/{kind}")).unwrap() {
                    let entry = entry.unwrap();
                    std::fs::copy(entry.path(), dir.join(kind).join(entry.file_name())).unwrap();
                }
            }
            Self(dir)
        }
        fn write(&self, path: &str, body: &str) {
            let path = self.0.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
    }
    impl Drop for TestTheme {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn builtin_themes_pass_all_page_contracts_including_fifty_posts() {
        let runtime = super::RenderingRuntime::default();
        for name in ["default", "paper"] {
            super::MiniJinjaThemeRenderer::load_checked(
                &std::path::PathBuf::from(format!("../../themes/{name}")),
                &runtime,
            )
            .await
            .unwrap();
        }
    }

    #[tokio::test]
    async fn nested_partials_macros_and_shared_function_cards_are_loaded() {
        let theme = TestTheme::new();
        theme.write("templates/partials/card.html", "{% macro card(post) %}<a href=\"{{ post.url | url }}\">{{ post.title }}</a>{% if post.author_avatar_url %}<img src=\"{{ post.author_avatar_url | url }}\">{% endif %}{% endmacro %}");
        theme.write("templates/partials/list.html", "{% from 'partials/card.html' import card %}{% for post in posts %}{{ card(post) }}{% endfor %}{% for post in get_posts(limit=50).items %}{{ card(post) }}{% endfor %}{% set item = get_post(slug='示例-0') %}{% if item %}{{ card(item) }}{% endif %}");
        theme.write("templates/index.html", "{% include 'partials/list.html' %}");
        // base.html is a helper, not a mandatory entry point.
        for entry in ["post", "page", "tag", "category", "series"] {
            theme.write(&format!("templates/{entry}.html"), "Standalone page");
        }
        std::fs::remove_file(theme.0.join("templates/base.html")).unwrap();
        super::MiniJinjaThemeRenderer::load_checked(&theme.0, &super::RenderingRuntime::default())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn auxiliary_templates_escape_html_regardless_of_extension() {
        let runtime = super::RenderingRuntime::default();
        let site = application::site_info::SiteInfo {
            home_page_size: application::site_info::DEFAULT_HOME_PAGE_SIZE,
            navigation: vec![],
            time_zone: "UTC".into(),
            title: "Site".into(),
            description: String::new(),
            logo_url: None,
        };
        let base = application::seo::PublicBaseUrl::parse("https://example.com").unwrap();
        let title = "<img src=x onerror=\"alert(1)\">";
        let escaped_title = "&lt;img src=x onerror=&quot;alert(1)&quot;&gt;";
        let post = application::public_site::PostCard {
            title: title.into(),
            slug: "example".into(),
            url: "/posts/example".into(),
            excerpt: None,
            published_at: None,
            author_display: "Author".into(),
            author_avatar_url: None,
        };
        let page = application::public_site::PageView {
            title: title.into(),
            slug: "about".into(),
            published_at: None,
            updated_at: "2026-01-01".into(),
            content_html: "<p><strong>已清洗的正文</strong></p>".into(),
        };
        for suffix in [".html", ".jinja", ".j2", ""] {
            let theme = TestTheme::new();
            theme.write(
                &format!("templates/partials/card{suffix}"),
                "<h2>{{ post.title }}</h2>",
            );
            theme.write(
                "templates/index.html",
                &format!(
                    "{{% for post in posts %}}{{% include 'partials/card{suffix}' %}}{{% endfor %}}"
                ),
            );
            theme.write(
                &format!("templates/partials/page{suffix}"),
                "<h1>{{ page.title }}</h1>{{ page.content_html | safe }}",
            );
            theme.write(
                "templates/page.html",
                &format!("{{% include 'partials/page{suffix}' %}}"),
            );
            let renderer = super::MiniJinjaThemeRenderer::load_checked(&theme.0, &runtime)
                .await
                .unwrap();
            let renderer = runtime.theme_renderer(renderer);
            let index_html = renderer
                .render_index(
                    &site,
                    &application::seo::SeoMeta::home(&site, &base),
                    std::slice::from_ref(&post),
                    &Default::default(),
                )
                .await
                .unwrap();
            assert_eq!(index_html, format!("<h2>{escaped_title}</h2>"), "{suffix}");
            let page_html = renderer
                .render_page(
                    &site,
                    &application::seo::SeoMeta::page(&site, &base, &page.title, &page.slug),
                    &page,
                )
                .await
                .unwrap();
            assert_eq!(
                page_html,
                format!("<h1>{escaped_title}</h1>{}", page.content_html),
                "{suffix}"
            );
        }
    }

    #[tokio::test]
    async fn validation_rejects_missing_assets_dependencies_and_conditional_contract_errors() {
        let runtime = super::RenderingRuntime::default();
        for (entry, body, scenario) in [
            (
                "index",
                "{{ asset_url(path='absent.css') }}",
                "empty/index.html",
            ),
            (
                "page",
                "{% include 'partials/missing.html' %}",
                "empty/page.html",
            ),
            (
                "index",
                "{% if pagination.next_url %}{{ missing }}{% endif %}",
                "maximum-first/index.html",
            ),
            ("post", "{{ post.category.name }}", "empty/post.html"),
            (
                "series",
                "{% if series.cover_url %}{{ unknown }}{% endif %}",
                "maximum-first/series.html",
            ),
            (
                "category",
                "{% if category.page == 2 %}{{ unknown }}{% endif %}",
                "maximum-middle/category.html",
            ),
            (
                "tag",
                "{% if not tag.posts %}{{ unknown }}{% endif %}",
                "empty/tag.html",
            ),
        ] {
            let theme = TestTheme::new();
            theme.write(&format!("templates/{entry}.html"), body);
            let error = super::MiniJinjaThemeRenderer::load_checked(&theme.0, &runtime)
                .await
                .err()
                .unwrap()
                .to_string();
            assert!(error.contains(scenario), "{error}");
        }
    }

    #[test]
    fn release_snapshot_and_version_bind_templates_to_asset_bytes() {
        let theme = TestTheme::new();
        let old = super::MiniJinjaThemeRenderer::load(&theme.0).unwrap();
        let same = super::MiniJinjaThemeRenderer::load(&theme.0).unwrap();
        assert_eq!(old.assets().version, same.assets().version);
        let original = old.assets().files["style.css"].clone();
        theme.write("assets/style.css", "new css");
        let changed_asset = super::MiniJinjaThemeRenderer::load(&theme.0).unwrap();
        assert_ne!(old.assets().version, changed_asset.assets().version);
        assert_eq!(old.assets().files["style.css"], original);
        theme.write("templates/index.html", "new template");
        let changed_template = super::MiniJinjaThemeRenderer::load(&theme.0).unwrap();
        assert_ne!(
            changed_asset.assets().version,
            changed_template.assets().version
        );
    }

    #[cfg(unix)]
    #[test]
    fn snapshots_reject_symlinks_including_directory_roots() {
        for path in ["templates/linked.html", "assets/linked.css"] {
            let theme = TestTheme::new();
            std::os::unix::fs::symlink("../theme.json", theme.0.join(path)).unwrap();
            assert!(super::MiniJinjaThemeRenderer::load(&theme.0).is_err());
        }
        for root in ["templates", "assets"] {
            let theme = TestTheme::new();
            std::fs::rename(theme.0.join(root), theme.0.join("original")).unwrap();
            std::os::unix::fs::symlink("original", theme.0.join(root)).unwrap();
            assert!(super::MiniJinjaThemeRenderer::load(&theme.0).is_err());
        }
    }

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

    #[tokio::test]
    async fn theme_registration_rejects_excessive_body_expansion() {
        let theme = TestTheme::new();
        theme.write(
            "templates/post.html",
            "{{ post.content_html | safe }}{{ post.content_html | safe }}",
        );
        let error = super::MiniJinjaThemeRenderer::load_checked(
            &theme.0,
            &super::RenderingRuntime::default(),
        )
        .await
        .err()
        .unwrap()
        .to_string();
        assert!(error.contains("maximum-body/post.html"), "{error}");
    }
}
