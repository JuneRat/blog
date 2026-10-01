//! Framework tests use a small notation fixture independently of built-in plugins.
use application::{
    UseCaseError,
    audit::AuditContext,
    html_rebuild::{HtmlKind, HtmlRebuildStore},
    identity::Actor,
    plugins::*,
    ports::{CommentRenderer, ContentRenderer, PageRepository, PostRepository, SaveOutcome},
    public_site::PageView,
    seo::{PublicBaseUrl, SeoMeta},
    site_info::SiteInfo,
};
use async_trait::async_trait;
use infrastructure::{RenderingRuntime, SystemClock, plugins::*};
use pulldown_cmark::Options;
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

mod common;

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
        if expected != state.version {
            return Ok(SaveOutcome::StaleConflict);
        }
        *state = PluginSettingsRecord {
            value: value.clone(),
            version: expected + 1,
        };
        Ok(SaveOutcome::Saved {
            new_version: expected + 1,
        })
    }
}

struct Notation(Arc<AtomicUsize>);
impl ContentHook for Notation {
    fn prepare(
        &self,
        source: String,
        options: &mut Options,
        _: &PluginConfig,
    ) -> Result<String, UseCaseError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        if source == "fail-hook" {
            return Err(UseCaseError::Render("fixture failure".into()));
        }
        options.insert(Options::ENABLE_MATH);
        Ok(source)
    }
    fn transform_html(&self, html: String, config: &PluginConfig) -> Result<String, UseCaseError> {
        let PluginConfigValue::Text(label) = &config["label"] else {
            unreachable!()
        };
        Ok(format!(
            "{html}<div class=\"fixture\" data-fixture=\"ok\" onclick=\"bad()\">{label}</div><script>bad()</script>"
        ))
    }
}
impl PageHeadHook for Notation {
    fn assets(&self, page: PluginPage, _: &PluginConfig) -> Result<Vec<HeadAsset>, UseCaseError> {
        if !matches!(
            page,
            PluginPage::Page | PluginPage::Post | PluginPage::Preview
        ) {
            return Ok(vec![]);
        }
        Ok(vec![
            HeadAsset {
                kind: HeadAssetKind::Stylesheet,
                path: "notation.css".into(),
            },
            HeadAsset {
                kind: HeadAssetKind::Script,
                path: "notation.js".into(),
            },
            HeadAsset {
                kind: HeadAssetKind::Script,
                path: "notation.js".into(),
            },
        ])
    }
}

fn registration(calls: Arc<AtomicUsize>) -> PluginRegistration {
    let hook = Arc::new(Notation(calls));
    PluginRegistration {
        definition: PluginDefinition {
            id: "notation".into(),
            name: "Notation fixture".into(),
            description: String::new(),
            version: "1".into(),
            hooks: vec![],
            config_fields: vec![PluginConfigField {
                key: "label".into(),
                label: "Label".into(),
                description: String::new(),
                default: PluginConfigValue::Text("first".into()),
            }],
        },
        content: Some(hook.clone()),
        page_head: Some(hook),
        html_rules: vec![
            HtmlRule {
                tag: "span",
                classes: vec!["math", "math-inline", "math-display"],
                data_attributes: vec![],
            },
            HtmlRule {
                tag: "div",
                classes: vec!["fixture"],
                data_attributes: vec!["data-fixture"],
            },
        ],
        files: BTreeMap::from([
            ("notation.js".into(), Arc::from(b"/* fixture */".as_slice())),
            (
                "notation.css".into(),
                Arc::from(b".fixture { color: blue }".as_slice()),
            ),
        ]),
    }
}
fn cmd(enabled: bool, label: &str, expected_version: i64) -> SavePluginCmd {
    SavePluginCmd {
        id: "notation".into(),
        enabled,
        expected_version,
        config: BTreeMap::from([("label".into(), PluginConfigValue::Text(label.into()))]),
    }
}

#[tokio::test]
async fn enabled_hooks_keep_nodes_sanitize_html_invalidate_cache_and_leave_comments_alone() {
    let calls = Arc::new(AtomicUsize::new(0));
    let catalog = Arc::new(PluginCatalog::new(vec![registration(calls.clone())]).unwrap());
    let plugins = Arc::new(PluginRuntime::new(
        catalog,
        Arc::new(MemoryStore::default()),
        Arc::new(SystemClock),
    ));
    let runtime = RenderingRuntime::default().with_plugins(plugins.clone());
    let actor = Actor::bootstrap_cli();
    let source = "$x < y$\n\n**normal** <img src=x onerror=bad()>";
    let disabled = runtime.render_content(source).await.unwrap();
    assert_eq!(
        disabled.content_html,
        infrastructure::SanitizingMarkdownRenderer::new().render_markdown(source)
    );
    assert!(!disabled.content_html.contains("math-inline"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    plugins
        .manager
        .save(&actor, cmd(true, "first", 0))
        .await
        .unwrap();
    let enabled = runtime.render_content(source).await.unwrap();
    assert!(enabled.content_html.contains("math-inline"));
    assert!(enabled.content_html.contains("x &lt; y"));
    assert!(enabled.content_html.contains("data-fixture=\"ok\""));
    assert!(enabled.content_html.contains("<strong>normal</strong>"));
    assert!(!enabled.content_html.contains("<script"));
    assert!(!enabled.content_html.contains("onclick"));
    assert!(!enabled.content_html.contains("onerror"));
    assert_ne!(enabled.render_version, disabled.render_version);
    assert_eq!(runtime.render_content(source).await.unwrap(), enabled);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    plugins
        .manager
        .save(&actor, cmd(true, "second", 1))
        .await
        .unwrap();
    let changed = runtime.render_content(source).await.unwrap();
    assert!(changed.content_html.contains("second"));
    assert_ne!(changed.render_version, enabled.render_version);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(runtime.render_content("fail-hook").await.is_err());
    let before_comment = calls.load(Ordering::SeqCst);
    assert!(
        !runtime
            .render_comment("$x$ **comment**")
            .await
            .unwrap()
            .contains("math-inline")
    );
    assert_eq!(calls.load(Ordering::SeqCst), before_comment);
    plugins
        .manager
        .save(&actor, cmd(false, "second", 2))
        .await
        .unwrap();
    assert_eq!(
        runtime.render_content(source).await.unwrap().content_html,
        disabled.content_html
    );
}

#[tokio::test]
async fn public_theme_head_is_scoped_deduplicated_and_uses_deferred_immutable_assets() {
    let catalog = Arc::new(PluginCatalog::new(vec![registration(Arc::default())]).unwrap());
    let plugins = Arc::new(PluginRuntime::new(
        catalog.clone(),
        Arc::new(MemoryStore::default()),
        Arc::new(SystemClock),
    ));
    let runtime = RenderingRuntime::default().with_plugins(plugins.clone());
    let theme = runtime.theme_renderer(
        infrastructure::MiniJinjaThemeRenderer::load(Path::new("../../themes/default")).unwrap(),
    );
    let site = SiteInfo::default();
    let seo = SeoMeta::page(
        &site,
        &PublicBaseUrl::parse("https://example.test").unwrap(),
        "Page",
        "page",
    );
    let page = PageView {
        title: "Page".into(),
        slug: "page".into(),
        published_at: None,
        updated_at: "now".into(),
        content_html: "<p>body</p>".into(),
    };
    assert!(
        !theme
            .render_page(&site, &seo, &page)
            .await
            .unwrap()
            .contains("notation.js")
    );
    plugins
        .manager
        .save(&Actor::bootstrap_cli(), cmd(true, "label", 0))
        .await
        .unwrap();
    let html = theme.render_page(&site, &seo, &page).await.unwrap();
    let head = html.split("</head>").next().unwrap();
    let assets = catalog.assets();
    assert!(head.contains(&format!(
        "<script src=\"{}\" defer></script>",
        assets[0].url("notation.js").unwrap()
    )));
    assert_eq!(html.matches("notation.js").count(), 1);
    assert!(head.contains("rel=\"stylesheet\""));
    assert!(head.contains("notation.css"));
    assert!(
        !theme
            .render_index(&site, &seo, &[], &Default::default())
            .await
            .unwrap()
            .contains("notation.js")
    );
    plugins
        .manager
        .save(&Actor::bootstrap_cli(), cmd(false, "label", 1))
        .await
        .unwrap();
    assert!(
        !theme
            .render_page(&site, &seo, &page)
            .await
            .unwrap()
            .contains("notation.js")
    );
}

struct UnavailableStore;
#[async_trait]
impl PluginStore for UnavailableStore {
    async fn load(&self) -> Result<PluginSettingsRecord, UseCaseError> {
        Err(UseCaseError::Repository("fixture unavailable".into()))
    }
    async fn save(
        &self,
        _: &PluginSettings,
        _: i64,
        _: &str,
        _: time::OffsetDateTime,
        _: AuditContext,
    ) -> Result<SaveOutcome, UseCaseError> {
        unreachable!()
    }
}

#[tokio::test]
async fn theme_preflight_is_database_free_even_when_live_plugins_are_unavailable() {
    let plugins = Arc::new(PluginRuntime::new(
        Arc::new(PluginCatalog::builtins()),
        Arc::new(UnavailableStore),
        Arc::new(SystemClock),
    ));
    let runtime = RenderingRuntime::default().with_plugins(plugins);
    for theme in ["../../themes/default", "../../theme-packages/paper"] {
        infrastructure::MiniJinjaThemeRenderer::load_checked(Path::new(theme), &runtime)
            .await
            .unwrap();
    }
    assert!(runtime.render_content("live request").await.is_err());
}

struct HeldHook {
    started: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    released: Mutex<std::sync::mpsc::Receiver<()>>,
}
impl ContentHook for HeldHook {
    fn prepare(
        &self,
        source: String,
        _: &mut Options,
        _: &PluginConfig,
    ) -> Result<String, UseCaseError> {
        self.started
            .lock()
            .unwrap()
            .take()
            .unwrap()
            .send(())
            .unwrap();
        self.released.lock().unwrap().recv().unwrap();
        Ok(format!("{source}\n\nfrom hook"))
    }
}

#[tokio::test]
async fn settings_changed_during_render_do_not_mislabel_or_poison_the_cached_html() {
    let (started, ready) = tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    let mut plugin = registration(Arc::default());
    plugin.content = Some(Arc::new(HeldHook {
        started: Mutex::new(Some(started)),
        released: Mutex::new(released),
    }));
    let plugins = Arc::new(PluginRuntime::new(
        Arc::new(PluginCatalog::new(vec![plugin]).unwrap()),
        Arc::new(MemoryStore::default()),
        Arc::new(SystemClock),
    ));
    let runtime = RenderingRuntime::default().with_plugins(plugins.clone());
    let actor = Actor::bootstrap_cli();
    plugins
        .manager
        .save(&actor, cmd(true, "old", 0))
        .await
        .unwrap();
    let old_version = runtime.current_render_version().await.unwrap();
    let rendering = runtime.clone();
    let in_flight = tokio::spawn(async move { rendering.render_content("source").await.unwrap() });
    ready.await.unwrap();
    plugins
        .manager
        .save(&actor, cmd(false, "old", 1))
        .await
        .unwrap();
    release.send(()).unwrap();
    let old = in_flight.await.unwrap();
    assert_eq!(old.render_version, old_version);
    assert!(old.content_html.contains("from hook"));
    let new = runtime.render_content("source").await.unwrap();
    assert_eq!(
        new.render_version,
        runtime.current_render_version().await.unwrap()
    );
    assert_ne!(new.render_version, old.render_version);
    assert!(!new.content_html.contains("from hook"));
}

#[tokio::test]
async fn preview_body_and_head_use_the_same_snapshot_during_a_settings_change() {
    let (started, ready) = tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    let mut plugin = registration(Arc::default());
    plugin.content = Some(Arc::new(HeldHook {
        started: Mutex::new(Some(started)),
        released: Mutex::new(released),
    }));
    let plugins = Arc::new(PluginRuntime::new(
        Arc::new(PluginCatalog::new(vec![plugin]).unwrap()),
        Arc::new(MemoryStore::default()),
        Arc::new(SystemClock),
    ));
    let runtime = RenderingRuntime::default().with_plugins(plugins.clone());
    let actor = Actor::bootstrap_cli();
    plugins
        .manager
        .save(&actor, cmd(true, "old", 0))
        .await
        .unwrap();
    let rendering = runtime.clone();
    let in_flight = tokio::spawn(async move { rendering.render_preview("source").await.unwrap() });
    ready.await.unwrap();
    plugins
        .manager
        .save(&actor, cmd(false, "old", 1))
        .await
        .unwrap();
    release.send(()).unwrap();
    let old = in_flight.await.unwrap();
    assert!(old.content_html.contains("from hook"));
    assert!(old.head_html.contains("notation.js"));
    let new = runtime.render_preview("source").await.unwrap();
    assert!(!new.content_html.contains("from hook"));
    assert!(new.head_html.is_empty());
}

#[test]
fn registration_rejects_unsafe_markup_and_resource_paths_and_versions_follow_bytes() {
    for path in [
        "../evil.js",
        "/evil.js",
        "x/../evil.js",
        "evil\".js",
        "https://evil.test/a.js",
    ] {
        let mut plugin = registration(Arc::default());
        plugin
            .files
            .insert(path.into(), Arc::from(b"bad".as_slice()));
        assert!(PluginCatalog::new(vec![plugin]).is_err(), "{path}");
    }
    let mut plugin = registration(Arc::default());
    plugin.html_rules[0].data_attributes = vec!["onclick"];
    assert!(PluginCatalog::new(vec![plugin]).is_err());
    let old = PluginCatalog::new(vec![registration(Arc::default())])
        .unwrap()
        .assets()[0]
        .version
        .clone();
    let mut plugin = registration(Arc::default());
    plugin
        .files
        .insert("notation.js".into(), Arc::from(b"updated".as_slice()));
    let new = PluginCatalog::new(vec![plugin]).unwrap().assets()[0]
        .version
        .clone();
    assert_ne!(old, new);
}

#[tokio::test]
async fn postgres_state_audit_restart_concurrency_and_existing_article_rebuild() {
    let pool = common::fresh_database("blog_plugins_test").await;
    let database = common::database(pool.clone());
    let store = Arc::new(PostgresPluginStore::new(database.clone()));
    let catalog = Arc::new(PluginCatalog::new(vec![registration(Arc::default())]).unwrap());
    let plugins = Arc::new(PluginRuntime::new(
        catalog.clone(),
        store.clone(),
        Arc::new(SystemClock),
    ));
    let runtime = Arc::new(RenderingRuntime::default().with_plugins(plugins.clone()));
    let posts = infrastructure::PostgresPostRepository::new(database.clone(), runtime.clone());
    let pages = infrastructure::PostgresPageRepository::new(database.clone(), runtime.clone());
    let author = common::seed_user(&pool, "plugin-writer").await;
    let now = time::OffsetDateTime::now_utc();
    let post = domain::content::Post::create_draft(
        domain::identity::UserId(author),
        domain::content::Slug::new("plugin-post").unwrap(),
        "Post".into(),
        None,
        "$x$".into(),
        domain::content::Visibility::Public,
        now,
    )
    .unwrap();
    let page = domain::content::Page::create_draft(
        domain::content::Slug::new("plugin-page").unwrap(),
        "Page".into(),
        "$x$".into(),
        domain::content::Visibility::Public,
        now,
    )
    .unwrap();
    posts.insert_post(&post, &[], None.into()).await.unwrap();
    pages.insert_page(&page, None.into()).await.unwrap();
    let rebuilder = infrastructure::PostgresHtmlRebuildStore::new(
        database.clone(),
        runtime.clone(),
        runtime.clone(),
    );
    assert_eq!(rebuilder.pending().await.unwrap().posts, 0);
    let actor = Actor::bootstrap_cli();
    let (a, b) = tokio::join!(
        plugins.manager.save(&actor, cmd(true, "a", 0)),
        plugins.manager.save(&actor, cmd(true, "b", 0))
    );
    assert_ne!(a.is_ok(), b.is_ok());
    assert_eq!(rebuilder.pending().await.unwrap().posts, 1);
    assert_eq!(rebuilder.pending().await.unwrap().pages, 1);
    for kind in [HtmlKind::Post, HtmlKind::Page] {
        assert_eq!(
            rebuilder
                .rebuild_batch(kind, None, 10)
                .await
                .unwrap()
                .rebuilt,
            1
        );
    }
    assert_eq!(rebuilder.pending().await.unwrap().posts, 0);
    let version = runtime.current_render_version().await.unwrap();
    let rows: Vec<(String, i32, i64)> = sqlx::query_as("SELECT content_html,content_render_version,version FROM posts UNION ALL SELECT content_html,content_render_version,version FROM pages").fetch_all(&pool).await.unwrap();
    for (html, actual, version_source) in rows {
        assert!(html.contains("math-inline"));
        assert_eq!(actual, version);
        assert_eq!(version_source, 1);
    }
    let restarted = PluginRuntime::new(
        catalog,
        Arc::new(PostgresPluginStore::new(database)),
        Arc::new(SystemClock),
    );
    assert_eq!(restarted.manager.view(&actor).await.unwrap().version, 1);
    assert!(
        restarted
            .manager
            .snapshot()
            .await
            .unwrap()
            .active
            .contains_key("notation")
    );
    let audits: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_logs WHERE action='plugin.configure'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(audits, 1);
    assert!(matches!(
        store
            .save(&PluginSettings::default(), 0, "notation", now, None.into())
            .await
            .unwrap(),
        SaveOutcome::StaleConflict
    ));
    plugins
        .manager
        .save(&actor, cmd(false, "after", 1))
        .await
        .unwrap();
    assert_eq!(rebuilder.pending().await.unwrap().posts, 1);
    rebuilder
        .rebuild_batch(HtmlKind::Post, None, 10)
        .await
        .unwrap();
    let html: String = sqlx::query_scalar("SELECT content_html FROM posts")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(!html.contains("math-inline"));
    assert!(html.contains("$x$"));
    pool.close().await;
}
