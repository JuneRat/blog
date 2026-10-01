use application::ports::{
    PublicCategorySummary, PublicPostDetail, PublicPostSummary, PublicTagSummary, PublicUrlEntry,
    PublishedCategoryQuery, PublishedPostQuery, PublishedTagQuery,
};
use application::{
    UseCaseError,
    audit::AuditContext,
    identity::Actor,
    plugins::*,
    ports::{ContentRenderer, SaveOutcome},
    public_site::{PageView, PostView},
    seo::{PublicBaseUrl, SeoMeta},
    site_info::SiteInfo,
};
use async_trait::async_trait;
use infrastructure::{RenderingRuntime, SystemClock, plugins::*};

struct EmptyPublicData;
#[async_trait]
impl PublishedPostQuery for EmptyPublicData {
    async fn list_public(&self, _: i64, _: i64) -> Result<Vec<PublicPostSummary>, UseCaseError> {
        Ok(vec![])
    }
    async fn find_public_by_slug(&self, _: &str) -> Result<Option<PublicPostDetail>, UseCaseError> {
        Ok(None)
    }
    async fn list_public_for_sitemap(&self, _: i64) -> Result<Vec<PublicUrlEntry>, UseCaseError> {
        Ok(vec![])
    }
}
#[async_trait]
impl PublishedTagQuery for EmptyPublicData {
    async fn list_public_tags(&self, _: i64) -> Result<Vec<PublicTagSummary>, UseCaseError> {
        Ok(vec![])
    }
    async fn find_public_by_slug(&self, _: &str) -> Result<Option<PublicTagSummary>, UseCaseError> {
        Ok(None)
    }
    async fn list_public_posts_by_tag(
        &self,
        _: &str,
        _: i64,
        _: i64,
    ) -> Result<(Vec<PublicPostSummary>, i64), UseCaseError> {
        Ok((vec![], 0))
    }
    async fn list_public_directories(&self, _: i64) -> Result<Vec<PublicUrlEntry>, UseCaseError> {
        Ok(vec![])
    }
}
#[async_trait]
impl PublishedCategoryQuery for EmptyPublicData {
    async fn list_public_categories(
        &self,
        _: i64,
    ) -> Result<Vec<PublicCategorySummary>, UseCaseError> {
        Ok(vec![])
    }
    async fn find_public_by_slug(
        &self,
        _: &str,
    ) -> Result<Option<PublicCategorySummary>, UseCaseError> {
        Ok(None)
    }
    async fn list_public_posts_by_category(
        &self,
        _: &str,
        _: i64,
        _: i64,
    ) -> Result<(Vec<PublicPostSummary>, i64), UseCaseError> {
        Ok((vec![], 0))
    }
    async fn list_public_directories(&self, _: i64) -> Result<Vec<PublicUrlEntry>, UseCaseError> {
        Ok(vec![])
    }
}
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex},
};

#[derive(Default)]
struct MemoryStore(Mutex<PluginSettingsRecord>);
#[async_trait]
impl PluginStore for MemoryStore {
    async fn load(&self) -> Result<PluginSettingsRecord, UseCaseError> {
        Ok(self.0.lock().unwrap().clone())
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
        assert_eq!(expected, state.version);
        *state = PluginSettingsRecord {
            value: value.clone(),
            version: expected + 1,
        };
        Ok(SaveOutcome::Saved {
            new_version: expected + 1,
        })
    }
}
fn runtime() -> (RenderingRuntime, Arc<PluginRuntime>) {
    let plugins = Arc::new(PluginRuntime::new(
        Arc::new(PluginCatalog::builtins()),
        Arc::new(MemoryStore::default()),
        Arc::new(SystemClock),
    ));
    (
        RenderingRuntime::default().with_plugins(plugins.clone()),
        plugins,
    )
}
async fn configure(
    plugins: &PluginRuntime,
    enabled: bool,
    math: bool,
    mermaid: bool,
    expected: i64,
) {
    plugins
        .manager
        .save(
            &Actor::bootstrap_cli(),
            SavePluginCmd {
                id: "markdown-enhance".into(),
                enabled,
                expected_version: expected,
                config: BTreeMap::from([
                    ("math".into(), PluginConfigValue::Boolean(math)),
                    ("mermaid".into(), PluginConfigValue::Boolean(mermaid)),
                ]),
            },
        )
        .await
        .unwrap();
}
const SOURCE: &str = "行内 $x < y$\n\n$$\\frac{a}{b}$$\n\n```mermaid\ngraph LR\n A-->B\n```";

#[cfg(feature = "sqlx-test-support")]
mod common;

#[cfg(feature = "sqlx-test-support")]
#[tokio::test]
async fn existing_posts_and_pages_rebuild_after_enabling_and_disabling_the_builtin() {
    use application::{
        html_rebuild::{HtmlKind, HtmlRebuildStore},
        ports::{PageRepository, PostRepository},
    };
    let pool = common::fresh_database("blog_markdown_enhance_test").await;
    let database = common::database(pool.clone());
    let plugins = Arc::new(PluginRuntime::new(
        Arc::new(PluginCatalog::builtins()),
        Arc::new(PostgresPluginStore::new(database.clone())),
        Arc::new(SystemClock),
    ));
    let runtime = Arc::new(RenderingRuntime::default().with_plugins(plugins.clone()));
    let posts = infrastructure::PostgresPostRepository::new(database.clone(), runtime.clone());
    let pages = infrastructure::PostgresPageRepository::new(database.clone(), runtime.clone());
    let now = time::OffsetDateTime::now_utc();
    let post = domain::content::Post::create_draft(
        domain::identity::UserId(common::seed_user(&pool, "md-writer").await),
        domain::content::Slug::new("extensions").unwrap(),
        "Extensions".into(),
        None,
        SOURCE.into(),
        domain::content::Visibility::Public,
        now,
    )
    .unwrap();
    let page = domain::content::Page::create_draft(
        domain::content::Slug::new("extensions").unwrap(),
        "Extensions".into(),
        SOURCE.into(),
        domain::content::Visibility::Public,
        now,
    )
    .unwrap();
    posts.insert_post(&post, &[], None.into()).await.unwrap();
    pages.insert_page(&page, None.into()).await.unwrap();
    let rebuild =
        infrastructure::PostgresHtmlRebuildStore::new(database, runtime.clone(), runtime.clone());
    assert_eq!(rebuild.pending().await.unwrap().posts, 0);
    for (index, enabled) in [true, false].into_iter().enumerate() {
        configure(&plugins, enabled, true, true, index as i64).await;
        let pending = rebuild.pending().await.unwrap();
        assert_eq!((pending.posts, pending.pages), (1, 1));
        for kind in [HtmlKind::Post, HtmlKind::Page] {
            assert_eq!(
                rebuild.rebuild_batch(kind, None, 10).await.unwrap().rebuilt,
                1
            );
        }
        let rows: Vec<(String, String, i32, i64)> = sqlx::query_as("SELECT content, content_html, content_render_version, version FROM posts UNION ALL SELECT content, content_html, content_render_version, version FROM pages").fetch_all(&pool).await.unwrap();
        let preview = runtime.render_preview(SOURCE).await.unwrap();
        for (source, html, render_version, edit_version) in rows {
            assert_eq!(source, SOURCE);
            assert_eq!(html, preview.content_html);
            assert_eq!(html.contains("math-inline"), enabled);
            assert_eq!(html.contains("language-mermaid"), enabled);
            assert_eq!(
                render_version,
                runtime.current_render_version().await.unwrap()
            );
            assert_eq!(edit_version, 1);
        }
        assert_eq!(rebuild.pending().await.unwrap().posts, 0);
        assert_eq!(rebuild.pending().await.unwrap().pages, 0);
    }
    pool.close().await;
}

#[tokio::test]
async fn defaults_are_off_and_feature_switches_control_parser_and_browser_assets() {
    let (runtime, plugins) = runtime();
    let off = runtime.render_preview(SOURCE).await.unwrap();
    assert!(off.head_html.is_empty());
    assert_eq!(
        off.content_html,
        infrastructure::SanitizingMarkdownRenderer::new().render_markdown(SOURCE)
    );
    for (index, (math, mermaid)) in [(true, false), (false, true), (true, true), (false, false)]
        .into_iter()
        .enumerate()
    {
        configure(&plugins, true, math, mermaid, index as i64).await;
        let saved = runtime.render_content(SOURCE).await.unwrap();
        let preview = runtime.render_preview(SOURCE).await.unwrap();
        assert_eq!(saved.content_html, preview.content_html);
        assert_eq!(preview.content_html.contains("math-inline"), math);
        assert_eq!(preview.content_html.contains("math-display"), math);
        assert_eq!(preview.head_html.contains("math.js"), math);
        assert_eq!(preview.head_html.contains("katex.css"), math);
        assert_eq!(preview.head_html.contains("mermaid.js"), mermaid);
        assert_eq!(preview.head_html.contains("display.css"), math || mermaid);
        assert!(!preview.head_html.contains("https://"));
        assert_eq!(
            preview.head_html.matches(" defer></script>").count(),
            usize::from(math) + usize::from(mermaid)
        );
    }
    configure(&plugins, false, true, true, 4).await;
    assert_eq!(runtime.render_preview(SOURCE).await.unwrap(), off);
}

#[tokio::test]
async fn syntax_respects_code_and_escapes_and_keeps_source_safe_through_sanitization() {
    let (runtime, plugins) = runtime();
    configure(&plugins, true, true, true, 0).await;
    let media = uuid::Uuid::now_v7();
    let source = format!(
        r#"Inline $x < y & z$ and ` $literal$ ` and \$escaped\$.

$$\frac{{a}}{{b}}$$

```text
$not_math$ <script>bad()</script>
```

```mermaid
graph LR
A["<img src='/media/{media}' onerror='bad()'>"] --> B
```

<script>bad()</script><span class="math math-inline" onclick="bad()" style="color:red">x</span>
![actual](/media/{media})
"#
    );
    let rendered = runtime.render_content(&source).await.unwrap();
    let html = &rendered.content_html;
    assert!(html.contains("x &lt; y &amp; z"));
    assert!(html.contains(r"\frac{a}{b}"));
    assert!(html.contains("$literal$"));
    assert!(html.contains("$escaped$"));
    assert!(html.contains("$not_math$ &lt;script&gt;bad()&lt;/script&gt;"));
    assert!(html.contains("<code class=\"language-mermaid\">"));
    assert!(!html.contains("<script>"));
    assert!(!html.contains("onclick="));
    assert!(!html.contains("style="));
    assert_eq!(rendered.media_ids, vec![media]);
    let only_diagram = runtime
        .render_content(&format!(
            "```mermaid\ngraph LR\nA[\"<img src='/media/{media}'>\"]\n```"
        ))
        .await
        .unwrap();
    assert!(only_diagram.media_ids.is_empty());
}

#[tokio::test]
async fn both_themes_include_preview_assets_for_posts_and_pages_but_not_indexes() {
    let (runtime, plugins) = runtime();
    configure(&plugins, true, true, true, 0).await;
    let preview = runtime.render_preview(SOURCE).await.unwrap();
    let site = SiteInfo::default();
    let seo = SeoMeta::page(
        &site,
        &PublicBaseUrl::parse("https://example.test").unwrap(),
        "Example",
        "example",
    );
    let page = PageView {
        title: "Example".into(),
        slug: "example".into(),
        published_at: None,
        updated_at: "now".into(),
        content_html: preview.content_html.clone(),
    };
    let post = PostView {
        title: page.title.clone(),
        slug: page.slug.clone(),
        url: "/posts/example".into(),
        excerpt: None,
        published_at: None,
        updated_at: "now".into(),
        author_display: "Author".into(),
        author_avatar_url: None,
        content_html: preview.content_html.clone(),
        cover_url: None,
        tags: vec![],
        category: None,
        series: vec![],
    };
    for name in ["default", "paper"] {
        let theme = runtime.theme_renderer(
            infrastructure::MiniJinjaThemeRenderer::load(Path::new(&format!(
                "../../themes/{name}"
            )))
            .unwrap()
            .with_data(Arc::new(application::theme_data::ThemeData::new(
                Arc::new(EmptyPublicData),
                Arc::new(EmptyPublicData),
                Arc::new(EmptyPublicData),
            ))),
        );
        for html in [
            theme.render_page(&site, &seo, &page).await.unwrap(),
            theme.render_post(&site, &seo, &post).await.unwrap(),
        ] {
            assert!(
                html.split("</head>")
                    .next()
                    .unwrap()
                    .contains(&preview.head_html)
            );
            assert!(html.contains("data-content-root"));
            assert!(html.contains(&preview.content_html));
        }
        assert!(
            !theme
                .render_index(&site, &seo, &[], &Default::default())
                .await
                .unwrap()
                .contains("/assets/plugins/")
        );
    }
}
