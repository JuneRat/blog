//! 渲染执行策略：分离正文、评论与主题并发预算、有限等待与阻塞任务隔离。
//!
//! 超时或调用者取消不会终止已经开始的阻塞任务；许可随任务持有到真正完成，
//! 防止客户端反复取消请求后绕过并发上限。只缓存纯 Markdown 结果，不缓存主题数据。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use application::error::UseCaseError;
use application::plugins::{PluginPage, PluginSnapshot, content_render_version};
use application::ports::{
    CommentRenderer, ContentRenderer, RenderedContent, RenderedPreview, ThemeRenderer,
};
use application::public_site::{CategoryView, PageView, PostCard, PostView, SeriesView, TagView};
use application::rendering_observer::{
    QueueOutcome, RenderKind as RenderPool, RenderingEvent, RenderingObserver,
};
use application::seo::SeoMeta;
use application::site_info::SiteInfo;
use async_trait::async_trait;
use tokio::sync::Semaphore;

use crate::media_refs::extract_media_ids_from_html;
use crate::rendering::{MiniJinjaThemeRenderer, SanitizingMarkdownRenderer};

#[derive(Debug, Clone)]
pub struct RenderingLimits {
    /// Public theme renders (including time spent waiting for data).
    pub concurrency: usize,
    /// Reserved for content writes; public traffic cannot acquire these slots.
    pub content_concurrency: usize,
    /// Anonymous comment writes and previews cannot consume article write slots.
    pub comment_concurrency: usize,
    pub queue_timeout: Duration,
    pub execution_timeout: Duration,
    pub markdown_cache_entries: usize,
    /// 包含源文本、结果 HTML 和媒体 UUID 的总字节上限；大于上限的单次结果不进入缓存。
    pub markdown_cache_bytes: usize,
}

impl Default for RenderingLimits {
    fn default() -> Self {
        Self {
            concurrency: 16,
            content_concurrency: 4,
            comment_concurrency: 4,
            queue_timeout: Duration::from_millis(250),
            execution_timeout: Duration::from_secs(2),
            markdown_cache_entries: 64,
            markdown_cache_bytes: 8 * 1024 * 1024,
        }
    }
}

#[derive(Default)]
struct MarkdownCache {
    entries: VecDeque<(String, RenderedContent)>,
    bytes: usize,
}

impl MarkdownCache {
    fn entry_bytes(source: &str, rendered: &RenderedContent) -> usize {
        source
            .len()
            .saturating_add(rendered.content_html.len())
            .saturating_add(
                rendered
                    .media_ids
                    .len()
                    .saturating_mul(std::mem::size_of::<uuid::Uuid>()),
            )
    }

    fn get(&mut self, source: &str) -> Option<RenderedContent> {
        let index = self.entries.iter().position(|(key, _)| key == source)?;
        let entry = self.entries.remove(index)?;
        let rendered = entry.1.clone();
        self.entries.push_back(entry);
        Some(rendered)
    }

    fn insert(&mut self, source: String, rendered: RenderedContent, limits: &RenderingLimits) {
        let bytes = Self::entry_bytes(&source, &rendered);
        if limits.markdown_cache_entries == 0 || bytes > limits.markdown_cache_bytes {
            return;
        }
        if let Some(index) = self.entries.iter().position(|(key, _)| key == &source) {
            let (key, value) = self.entries.remove(index).expect("existing cache entry");
            self.bytes -= Self::entry_bytes(&key, &value);
        }
        while self.entries.len() >= limits.markdown_cache_entries
            || self.bytes.saturating_add(bytes) > limits.markdown_cache_bytes
        {
            let Some((key, value)) = self.entries.pop_front() else {
                break;
            };
            self.bytes -= Self::entry_bytes(&key, &value);
        }
        self.bytes += bytes;
        self.entries.push_back((source, rendered));
    }
}

struct RuntimeState {
    limits: RenderingLimits,
    theme_slots: Arc<Semaphore>,
    content_slots: Arc<Semaphore>,
    comment_slots: Arc<Semaphore>,
    markdown: Mutex<MarkdownCache>,
}

/// 进程内共享实例：正文写入、评论预览/提交与公开主题各有独立并发许可。
#[derive(Clone)]
pub struct RenderingRuntime {
    state: Arc<RuntimeState>,
    observer: Option<Arc<dyn RenderingObserver>>,
    plugins: Option<Arc<crate::plugins::PluginRuntime>>,
}

struct QueueMeasurement {
    observer: Option<Arc<dyn RenderingObserver>>,
    pool: RenderPool,
    started: Instant,
    outcome: QueueOutcome,
}

impl Drop for QueueMeasurement {
    fn drop(&mut self) {
        if let Some(observer) = &self.observer {
            observer.observe(
                self.pool,
                RenderingEvent::QueueFinished {
                    elapsed: self.started.elapsed(),
                    outcome: self.outcome,
                },
            );
        }
    }
}

struct WorkerMeasurement {
    observer: Option<Arc<dyn RenderingObserver>>,
    pool: RenderPool,
    started: Instant,
    success: bool,
}

impl Drop for WorkerMeasurement {
    fn drop(&mut self) {
        if let Some(observer) = &self.observer {
            observer.observe(
                self.pool,
                RenderingEvent::Finished {
                    elapsed: self.started.elapsed(),
                    success: self.success,
                },
            );
        }
    }
}

impl Default for RenderingRuntime {
    fn default() -> Self {
        Self::with_limits(RenderingLimits::default()).expect("valid default rendering limits")
    }
}

impl RenderingRuntime {
    pub fn with_plugins(mut self, plugins: Arc<crate::plugins::PluginRuntime>) -> Self {
        self.plugins = Some(plugins);
        self
    }

    pub(crate) fn for_theme_validation(&self) -> Self {
        let mut runtime = self.clone();
        runtime.plugins = None;
        runtime
    }

    async fn plugin_snapshot(&self) -> Result<PluginSnapshot, UseCaseError> {
        match &self.plugins {
            Some(plugins) => plugins.manager.snapshot().await,
            None => Ok(PluginSnapshot::default()),
        }
    }

    pub fn with_observer(mut self, observer: Arc<dyn RenderingObserver>) -> Self {
        self.observer = Some(observer);
        self
    }

    pub fn with_limits(limits: RenderingLimits) -> Result<Self, UseCaseError> {
        if limits.concurrency == 0
            || limits.concurrency > Semaphore::MAX_PERMITS
            || limits.content_concurrency == 0
            || limits.content_concurrency > Semaphore::MAX_PERMITS
            || limits.comment_concurrency == 0
            || limits.comment_concurrency > Semaphore::MAX_PERMITS
            || limits.queue_timeout.is_zero()
            || limits.execution_timeout.is_zero()
        {
            return Err(UseCaseError::Render(
                "渲染并发数和超时必须为有效正值".into(),
            ));
        }
        Ok(Self {
            state: Arc::new(RuntimeState {
                theme_slots: Arc::new(Semaphore::new(limits.concurrency)),
                content_slots: Arc::new(Semaphore::new(limits.content_concurrency)),
                comment_slots: Arc::new(Semaphore::new(limits.comment_concurrency)),
                limits,
                markdown: Mutex::new(MarkdownCache::default()),
            }),
            observer: None,
            plugins: None,
        })
    }

    pub fn theme_renderer(&self, renderer: MiniJinjaThemeRenderer) -> Arc<dyn ThemeRenderer> {
        Arc::new(ThemeExecutor {
            runtime: self.clone(),
            renderer: Arc::new(renderer),
        })
    }

    async fn execute<T, F>(
        &self,
        pool: RenderPool,
        kind: &'static str,
        task: F,
    ) -> Result<T, UseCaseError>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, UseCaseError> + Send + 'static,
    {
        let slots = match pool {
            RenderPool::Content => &self.state.content_slots,
            RenderPool::Comment => &self.state.comment_slots,
            RenderPool::Theme => &self.state.theme_slots,
        };
        let queued = Instant::now();
        if let Some(observer) = &self.observer {
            observer.observe(pool, RenderingEvent::Queued);
        }
        let mut measurement = QueueMeasurement {
            observer: self.observer.clone(),
            pool,
            started: queued,
            outcome: QueueOutcome::Cancelled,
        };
        let permit = tokio::time::timeout(
            self.state.limits.queue_timeout,
            slots.clone().acquire_owned(),
        )
        .await
        .map_err(|_| {
            measurement.outcome = QueueOutcome::Timeout;
            tracing::warn!(
                kind,
                queue_ms = queued.elapsed().as_millis() as u64,
                "渲染排队超时"
            );
            UseCaseError::Render("渲染排队超时".into())
        })?
        .map_err(|_| {
            measurement.outcome = QueueOutcome::Closed;
            UseCaseError::Render("渲染执行器已关闭".into())
        })?;
        measurement.outcome = QueueOutcome::Admitted;
        drop(measurement);
        let queue_ms = queued.elapsed().as_millis() as u64;
        let span = tracing::Span::current();
        let observer = self.observer.clone();
        let mut worker = tokio::task::spawn_blocking(move || {
            // 此许可必须在阻塞闭包内，不能由等待它的 async future 持有。
            let _permit = permit;
            span.in_scope(|| {
                let started = Instant::now();
                if let Some(observer) = &observer {
                    observer.observe(pool, RenderingEvent::Started);
                }
                let mut measurement = WorkerMeasurement {
                    observer,
                    pool,
                    started,
                    success: false,
                };
                let result = task();
                measurement.success = result.is_ok();
                tracing::debug!(
                    kind,
                    queue_ms,
                    execution_ms = started.elapsed().as_millis() as u64,
                    success = result.is_ok(),
                    "渲染任务完成"
                );
                result
            })
        });
        match tokio::time::timeout(self.state.limits.execution_timeout, &mut worker).await {
            Ok(result) => {
                result.map_err(|error| UseCaseError::Render(format!("渲染任务失败：{error}")))?
            }
            Err(_) => {
                if let Some(observer) = &self.observer {
                    observer.observe(pool, RenderingEvent::ExecutionTimeout);
                }
                // 尚未开始的 blocking job 可取消；已开始的继续持有其许可直到完成。
                worker.abort();
                tracing::warn!(kind, queue_ms, "渲染执行超时");
                Err(UseCaseError::Render("渲染执行超时".into()))
            }
        }
    }
}

#[async_trait]
impl CommentRenderer for RenderingRuntime {
    async fn render_comment(&self, source: &str) -> Result<String, UseCaseError> {
        domain::comment::validate_body(source).map_err(|e| UseCaseError::Invalid(e.into()))?;
        let source = source.to_owned();
        self.execute(RenderPool::Comment, "comment", move || {
            let html = crate::comment_rendering::render(&source);
            application::rendering_budget::validate_html(&html)
                .map_err(|e| UseCaseError::Invalid(e.to_string()))?;
            Ok(html)
        })
        .await
    }
}

#[async_trait]
impl ContentRenderer for RenderingRuntime {
    async fn current_render_version(&self) -> Result<i32, UseCaseError> {
        content_render_version(
            crate::CONTENT_RENDER_VERSION,
            self.plugin_snapshot().await?.render_revision,
        )
    }

    async fn render_content(&self, source: &str) -> Result<RenderedContent, UseCaseError> {
        domain::content::budget::validate_source(source)
            .map_err(|e| UseCaseError::Invalid(e.to_string()))?;
        let snapshot = self.plugin_snapshot().await?;
        self.render_content_with_snapshot(source, snapshot).await
    }

    async fn render_preview(&self, source: &str) -> Result<RenderedPreview, UseCaseError> {
        domain::content::budget::validate_source(source)
            .map_err(|e| UseCaseError::Invalid(e.to_string()))?;
        let snapshot = self.plugin_snapshot().await?;
        let rendered = self
            .render_content_with_snapshot(source, snapshot.clone())
            .await?;
        let plugins = self.plugins.clone();
        let head_html = self
            .execute(RenderPool::Content, "preview.head", move || match plugins {
                Some(plugins) => plugins.catalog.head_html(PluginPage::Preview, &snapshot),
                None => Ok(String::new()),
            })
            .await?;
        Ok(RenderedPreview {
            content_html: rendered.content_html,
            head_html,
        })
    }
}

impl RenderingRuntime {
    async fn render_content_with_snapshot(
        &self,
        source: &str,
        snapshot: PluginSnapshot,
    ) -> Result<RenderedContent, UseCaseError> {
        let render_version =
            content_render_version(crate::CONTENT_RENDER_VERSION, snapshot.render_revision)?;
        // The version distinguishes disabled/configured hooks even for identical Markdown.
        let cache_key = format!("{render_version}\0{source}");
        let plugins = self.plugins.clone();
        let started = Instant::now();
        let cached = self
            .state
            .markdown
            .lock()
            .map_err(|_| UseCaseError::Render("Markdown 缓存锁失效".into()))?
            .get(&cache_key);
        tracing::debug!(
            kind = "markdown",
            cache_hit = cached.is_some(),
            lookup_us = started.elapsed().as_micros() as u64,
            input_bytes = source.len(),
            "Markdown 缓存查询"
        );
        if let Some(rendered) = cached {
            return Ok(rendered);
        }
        let source = source.to_owned();
        let state = self.state.clone();
        self.execute(RenderPool::Content, "markdown", move || {
            let content_html = match plugins {
                Some(plugins) => plugins.catalog.render_markdown(&source, &snapshot)?,
                None => SanitizingMarkdownRenderer::new().render_markdown(&source),
            };
            application::rendering_budget::validate_html(&content_html)
                .map_err(|e| UseCaseError::Invalid(e.to_string()))?;
            let media_ids = extract_media_ids_from_html(&content_html);
            let rendered = RenderedContent {
                content_html,
                render_version,
                media_ids,
            };
            tracing::debug!(
                output_bytes = rendered.content_html.len(),
                media_count = rendered.media_ids.len(),
                "正文 HTML 与媒体引用生成完成"
            );
            state
                .markdown
                .lock()
                .map_err(|_| UseCaseError::Render("Markdown 缓存锁失效".into()))?
                .insert(cache_key, rendered.clone(), &state.limits);
            Ok(rendered)
        })
        .await
    }
}

struct ThemeExecutor {
    runtime: RenderingRuntime,
    renderer: Arc<MiniJinjaThemeRenderer>,
}

impl ThemeExecutor {
    async fn render<F>(
        &self,
        page: PluginPage,
        kind: &'static str,
        task: F,
    ) -> Result<String, UseCaseError>
    where
        F: FnOnce(&MiniJinjaThemeRenderer) -> Result<String, UseCaseError> + Send + 'static,
    {
        let snapshot = self.runtime.plugin_snapshot().await?;
        let plugins = self.runtime.plugins.clone();
        let renderer = self.renderer.as_ref().clone();
        self.runtime
            .execute(RenderPool::Theme, kind, move || {
                let head = match plugins {
                    Some(plugins) => plugins.catalog.head_html(page, &snapshot)?,
                    None => String::new(),
                };
                task(&renderer.with_plugin_head(head))
            })
            .await
    }
}

#[async_trait]
impl ThemeRenderer for ThemeExecutor {
    async fn render_index(
        &self,
        site: &SiteInfo,
        seo: &SeoMeta,
        posts: &[PostCard],
        pagination: &application::public_site::IndexPagination,
    ) -> Result<String, UseCaseError> {
        let (site, seo, posts, pagination) = (
            site.clone(),
            seo.clone(),
            posts.to_vec(),
            pagination.clone(),
        );
        self.render(PluginPage::Index, "theme.index", move |renderer| {
            renderer.render_index(&site, &seo, &posts, &pagination)
        })
        .await
    }

    async fn render_post(
        &self,
        site: &SiteInfo,
        seo: &SeoMeta,
        view: &PostView,
    ) -> Result<String, UseCaseError> {
        let (site, seo, view) = (site.clone(), seo.clone(), view.clone());
        self.render(PluginPage::Post, "theme.post", move |renderer| {
            renderer.render_post(&site, &seo, &view)
        })
        .await
    }
    async fn render_page(
        &self,
        site: &SiteInfo,
        seo: &SeoMeta,
        view: &PageView,
    ) -> Result<String, UseCaseError> {
        let (site, seo, view) = (site.clone(), seo.clone(), view.clone());
        self.render(PluginPage::Page, "theme.page", move |renderer| {
            renderer.render_page(&site, &seo, &view)
        })
        .await
    }
    async fn render_tag(
        &self,
        site: &SiteInfo,
        seo: &SeoMeta,
        view: &TagView,
    ) -> Result<String, UseCaseError> {
        let (site, seo, view) = (site.clone(), seo.clone(), view.clone());
        self.render(PluginPage::Tag, "theme.tag", move |renderer| {
            renderer.render_tag(&site, &seo, &view)
        })
        .await
    }
    async fn render_category(
        &self,
        site: &SiteInfo,
        seo: &SeoMeta,
        view: &CategoryView,
    ) -> Result<String, UseCaseError> {
        let (site, seo, view) = (site.clone(), seo.clone(), view.clone());
        self.render(PluginPage::Category, "theme.category", move |renderer| {
            renderer.render_category(&site, &seo, &view)
        })
        .await
    }
    async fn render_series(
        &self,
        site: &SiteInfo,
        seo: &SeoMeta,
        view: &SeriesView,
    ) -> Result<String, UseCaseError> {
        let (site, seo, view) = (site.clone(), seo.clone(), view.clone());
        self.render(PluginPage::Series, "theme.series", move |renderer| {
            renderer.render_series(&site, &seo, &view)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Observer(Mutex<Vec<RenderingEvent>>);

    impl RenderingObserver for Observer {
        fn observe(&self, _kind: RenderPool, event: RenderingEvent) {
            self.0.lock().unwrap().push(event);
        }
    }

    #[tokio::test]
    async fn telemetry_tracks_cancellation_timeout_and_actual_worker_completion() {
        let observer = Arc::new(Observer::default());
        let runtime = limited().with_observer(observer.clone());
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let worker_runtime = runtime.clone();
        let caller = tokio::spawn(async move {
            worker_runtime
                .execute(RenderPool::Content, "held", move || {
                    let _ = started.send(());
                    let _ = blocked.recv();
                    Ok(())
                })
                .await
        });
        ready.await.unwrap();
        // A queued future dropped by its caller must release only its waiting
        // gauge; the active worker remains active after execution timeout.
        let waiting = runtime.execute(RenderPool::Content, "cancelled", || Ok(()));
        assert!(
            tokio::time::timeout(Duration::from_millis(1), waiting)
                .await
                .is_err()
        );
        assert!(caller.await.unwrap().is_err());
        assert!(
            runtime
                .execute(RenderPool::Content, "overload", || Ok(()))
                .await
                .is_err()
        );
        {
            let events = observer.0.lock().unwrap();
            assert!(events.iter().any(|event| matches!(
                event,
                RenderingEvent::QueueFinished {
                    outcome: QueueOutcome::Cancelled,
                    ..
                }
            )));
            assert!(events.iter().any(|event| matches!(
                event,
                RenderingEvent::QueueFinished {
                    outcome: QueueOutcome::Timeout,
                    ..
                }
            )));
            assert!(
                events
                    .iter()
                    .any(|event| matches!(event, RenderingEvent::ExecutionTimeout))
            );
            assert!(
                !events
                    .iter()
                    .any(|event| matches!(event, RenderingEvent::Finished { .. }))
            );
        }
        release.send(()).unwrap();
        // Receiving the permit again proves the held worker and its metric
        // guard have both completed (without relying on a scheduling sleep).
        let _permit = runtime.state.content_slots.acquire().await.unwrap();
        let events = observer.0.lock().unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, RenderingEvent::Queued))
                .count(),
            3
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, RenderingEvent::QueueFinished { .. }))
                .count(),
            3
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, RenderingEvent::Finished { success: true, .. }))
                .count(),
            1
        );
    }

    fn limited() -> RenderingRuntime {
        RenderingRuntime::with_limits(RenderingLimits {
            concurrency: 1,
            content_concurrency: 1,
            comment_concurrency: 1,
            queue_timeout: Duration::from_millis(25),
            execution_timeout: Duration::from_millis(100),
            markdown_cache_entries: 2,
            markdown_cache_bytes: 40,
        })
        .unwrap()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn saturated_public_workers_leave_article_write_capacity_available() {
        let runtime = RenderingRuntime::default();
        for (pool, count) in [(RenderPool::Theme, 16), (RenderPool::Comment, 4)] {
            let mut workers = Vec::new();
            let mut releases = Vec::new();
            for _ in 0..count {
                let (started, ready) = tokio::sync::oneshot::channel();
                let (release, blocked) = std::sync::mpsc::channel();
                releases.push(release);
                let worker_runtime = runtime.clone();
                workers.push(tokio::spawn(async move {
                    worker_runtime
                        .execute(pool, "public.waiting", move || {
                            let _ = started.send(());
                            let _ = blocked.recv();
                            Ok(())
                        })
                        .await
                }));
                ready.await.unwrap();
            }
            let content = runtime
                .render_content(&format!("正文写入仍可运行 {count}"))
                .await;
            for release in releases {
                let _ = release.send(());
            }
            assert!(content.unwrap().content_html.contains("正文写入仍可运行"));
            for worker in workers {
                worker.await.unwrap().unwrap();
            }
        }
    }

    #[tokio::test]
    async fn cancelled_caller_keeps_running_worker_permit_and_rejects_overload() {
        let runtime = limited();
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let worker_runtime = runtime.clone();
        let caller = tokio::spawn(async move {
            worker_runtime
                .execute(RenderPool::Content, "test", move || {
                    let _ = started.send(());
                    let _ = blocked.recv();
                    Ok("complete".to_string())
                })
                .await
        });
        ready.await.unwrap();
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        assert_eq!(runtime.state.content_slots.available_permits(), 0);
        let error = runtime
            .execute(RenderPool::Content, "overload", || {
                Ok("must not run".to_string())
            })
            .await;
        assert!(matches!(error, Err(UseCaseError::Render(message)) if message == "渲染排队超时"));
        release.send(()).unwrap();
        assert_eq!(
            runtime
                .execute(RenderPool::Content, "after", || Ok("available".to_string()))
                .await
                .unwrap(),
            "available"
        );
    }

    #[tokio::test]
    async fn execution_timeout_does_not_release_a_running_worker_permit() {
        let runtime = limited();
        let (release, blocked) = std::sync::mpsc::channel();
        let result = runtime
            .execute(RenderPool::Content, "slow", move || {
                let _ = blocked.recv();
                Ok("late".to_string())
            })
            .await;
        assert!(matches!(result, Err(UseCaseError::Render(message)) if message == "渲染执行超时"));
        assert_eq!(runtime.state.content_slots.available_permits(), 0);
        let queued = runtime
            .execute(RenderPool::Content, "queued", || {
                Ok("must not run".to_string())
            })
            .await;
        assert!(matches!(queued, Err(UseCaseError::Render(message)) if message == "渲染排队超时"));
        release.send(()).unwrap();
        assert!(
            runtime
                .execute(RenderPool::Content, "after", || Ok(String::new()))
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn markdown_cache_hits_without_a_worker_and_is_bounded_by_entries_and_bytes() {
        let runtime = limited();
        assert_eq!(
            runtime.render_content("a").await.unwrap().content_html,
            "<p>a</p>\n"
        );
        let permit = runtime
            .state
            .content_slots
            .clone()
            .acquire_owned()
            .await
            .unwrap();
        // 唯一执行槽正被占用：命中仍立即返回，未命中会排队超时。
        assert_eq!(
            runtime.render_content("a").await.unwrap().content_html,
            "<p>a</p>\n"
        );
        assert!(runtime.render_content("missing").await.is_err());
        drop(permit);
        runtime.render_content("b").await.unwrap();
        runtime.render_content("c").await.unwrap();
        {
            let mut cache = runtime.state.markdown.lock().unwrap();
            assert_eq!(cache.entries.len(), 2);
            assert!(cache.get("a").is_none());
            assert!(cache.bytes <= 40);
        }
        // 单个超大结果仍可渲染，但不挤入缓存、也不清空已有热点。
        runtime.render_content(&"x".repeat(100)).await.unwrap();
        {
            let cache = runtime.state.markdown.lock().unwrap();
            assert_eq!(cache.entries.len(), 2);
            assert!(cache.bytes <= 40);
        }
        runtime.render_content("12345678901234").await.unwrap();
        let cache = runtime.state.markdown.lock().unwrap();
        assert_eq!(cache.entries.len(), 1, "字节上限应先于条目数上限淘汰");
        assert!(cache.bytes <= 40);
    }

    #[tokio::test]
    async fn async_markdown_renderer_sanitizes_the_cached_result() {
        let runtime = RenderingRuntime::default();
        let source = "# 标题\n\n<script>alert('x')</script>\n\n[链接](javascript:alert(1))";
        let first = runtime.render_content(source).await.unwrap();
        assert!(first.content_html.contains("<h1>标题</h1>"));
        assert!(!first.content_html.contains("<script"));
        assert!(!first.content_html.contains("javascript:"));
        assert_eq!(runtime.render_content(source).await.unwrap(), first);
    }

    #[tokio::test]
    async fn cached_content_includes_sorted_unique_media_and_counts_their_bytes() {
        let first = uuid::Uuid::from_u128(1);
        let second = uuid::Uuid::from_u128(2);
        let ignored = uuid::Uuid::from_u128(3);
        let source = format!(
            "![二](/media/{second})\n![一](/media/{first})\n![重复](/media/{second})\n\n<!-- <img src='/media/{ignored}'> -->"
        );
        let runtime = RenderingRuntime::default();
        let rendered = runtime.render_content(&source).await.unwrap();
        assert_eq!(rendered.media_ids, vec![first, second]);
        let permits = runtime
            .state
            .content_slots
            .clone()
            .acquire_many_owned(4)
            .await
            .unwrap();
        assert_eq!(runtime.render_content(&source).await.unwrap(), rendered);
        drop(permits);

        // 文本本身能放下、计入 UUID 后超限的结果不能进入缓存。
        let runtime = RenderingRuntime::with_limits(RenderingLimits {
            markdown_cache_bytes: source.len() + rendered.content_html.len(),
            ..RenderingLimits::default()
        })
        .unwrap();
        assert_eq!(runtime.render_content(&source).await.unwrap(), rendered);
        assert!(runtime.state.markdown.lock().unwrap().entries.is_empty());
    }

    #[tokio::test]
    async fn theme_executor_uses_its_own_budget_and_preserves_stored_html() {
        let runtime = limited();
        let theme = runtime.theme_renderer(
            MiniJinjaThemeRenderer::load(std::path::Path::new("../../themes/default")).unwrap(),
        );
        let site = SiteInfo {
            home_page_size: application::site_info::DEFAULT_HOME_PAGE_SIZE,
            navigation: vec![],
            time_zone: "UTC".into(),
            title: "测试站点".into(),
            description: "渲染执行器测试".into(),
            logo_url: None,
        };
        let base = application::seo::PublicBaseUrl::parse("https://blog.test").unwrap();
        let seo = SeoMeta::page(&site, &base, "页面", "page");
        let page = PageView {
            title: "页面".into(),
            slug: "page".into(),
            published_at: None,
            updated_at: "2026-01-01".into(),
            content_html: "<p><strong>持久化 HTML</strong></p>".into(),
        };
        let permit = runtime
            .state
            .theme_slots
            .clone()
            .acquire_owned()
            .await
            .unwrap();
        let error = theme.render_page(&site, &seo, &page).await;
        assert!(matches!(error, Err(UseCaseError::Render(message)) if message == "渲染排队超时"));
        drop(permit);
        let html = theme.render_page(&site, &seo, &page).await.unwrap();
        assert!(html.contains(&page.content_html));
    }

    #[tokio::test]
    async fn source_and_html_budgets_cover_boundaries_and_expansion() {
        use application::rendering_budget::MAX_CONTENT_HTML_BYTES;
        use domain::content::budget::MAX_SOURCE_BYTES;
        let runtime = RenderingRuntime::default();
        let content = "x".repeat(MAX_CONTENT_HTML_BYTES - 8);
        let rendered = runtime.render_content(&content).await.unwrap();
        assert_eq!(rendered.content_html.len(), MAX_CONTENT_HTML_BYTES);
        assert!(matches!(
            runtime.render_content(&(content + "x")).await,
            Err(UseCaseError::Invalid(_))
        ));
        assert!(matches!(
            runtime
                .render_content(&format!("{}& ", "a".repeat(1024)).repeat(765))
                .await,
            Err(UseCaseError::Invalid(_))
        ));
        assert!(matches!(
            runtime
                .render_content(&"x".repeat(MAX_SOURCE_BYTES + 1))
                .await,
            Err(UseCaseError::Invalid(_))
        ));
    }
}
