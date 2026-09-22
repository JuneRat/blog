//! 渲染适配器：Markdown → 清洗 HTML，以及 MiniJinja 主题渲染。
//! MiniJinja 仅存在于本层；interfaces 通过应用端口间接使用。

use std::path::Path;

use application::error::UseCaseError;
use application::public_site::{PageView, PostCard, PostView, SiteInfo, TagView, ThemeRenderer};
use minijinja::Environment;
use pulldown_cmark::{Options, Parser, html::push_html};

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
#[derive(serde::Serialize)]
struct IndexContext<'a> {
    site: &'a SiteInfo,
    posts: &'a [PostCard],
}

#[derive(serde::Serialize)]
struct PostContext<'a> {
    site: &'a SiteInfo,
    post: &'a PostView,
}

#[derive(serde::Serialize)]
struct PageContext<'a> {
    site: &'a SiteInfo,
    page: &'a PageView,
}

#[derive(serde::Serialize)]
struct TagContext<'a> {
    site: &'a SiteInfo,
    tag: &'a TagView,
}

/// MiniJinja 主题渲染器：启动时加载并解析主题模板，请求期复用。
pub struct MiniJinjaThemeRenderer {
    env: Environment<'static>,
}

impl MiniJinjaThemeRenderer {
    /// M1 最小模板集：base/index/post/page；M3 增加标签页 tag。
    /// 模板在启动时一次性加载；Environment 复用要求 'static，故按启动期资源泄漏源码。
    pub fn load(theme_dir: &Path) -> Result<Self, UseCaseError> {
        let mut env = Environment::new();
        for name in [
            "base.html",
            "index.html",
            "post.html",
            "page.html",
            "tag.html",
        ] {
            let path = theme_dir.join("templates").join(name);
            let source = std::fs::read_to_string(&path)
                .map_err(|e| UseCaseError::Render(format!("读取模板 {name} 失败：{e}")))?;
            let source: &'static str = Box::leak(source.into_boxed_str());
            env.add_template(name, source)
                .map_err(|e| UseCaseError::Render(format!("解析模板 {name} 失败：{e}")))?;
        }
        Ok(Self { env })
    }
}

impl ThemeRenderer for MiniJinjaThemeRenderer {
    fn render_index(&self, site: &SiteInfo, posts: &[PostCard]) -> Result<String, UseCaseError> {
        let ctx = IndexContext { site, posts };
        self.env
            .get_template("index.html")
            .and_then(|t| t.render(ctx))
            .map_err(|e| UseCaseError::Render(e.to_string()))
    }

    fn render_post(&self, site: &SiteInfo, post: &PostView) -> Result<String, UseCaseError> {
        let ctx = PostContext { site, post };
        self.env
            .get_template("post.html")
            .and_then(|t| t.render(ctx))
            .map_err(|e| UseCaseError::Render(e.to_string()))
    }

    fn render_page(&self, site: &SiteInfo, page: &PageView) -> Result<String, UseCaseError> {
        let ctx = PageContext { site, page };
        self.env
            .get_template("page.html")
            .and_then(|t| t.render(ctx))
            .map_err(|e| UseCaseError::Render(e.to_string()))
    }

    fn render_tag(&self, site: &SiteInfo, tag: &TagView) -> Result<String, UseCaseError> {
        let ctx = TagContext { site, tag };
        self.env
            .get_template("tag.html")
            .and_then(|t| t.render(ctx))
            .map_err(|e| UseCaseError::Render(e.to_string()))
    }
}
