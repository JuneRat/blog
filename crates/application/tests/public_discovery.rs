//! 数据用例独立于 XML/主题；验证查询预算、失败短路和站点信息回退。
use application::UseCaseError;
use application::ports::*;
use application::public_site::*;
use application::seo::{PublicBaseUrl, SeoMeta};
use application::site_info::SiteInfo;
use application::syndication::SITEMAP_URL_LIMIT;
use async_trait::async_trait;
use std::sync::{Arc, Mutex};
use time::OffsetDateTime;

struct Sources {
    counts: [usize; 5],
    fail: Option<usize>,
    calls: Mutex<Vec<(usize, i64)>>,
}
impl Sources {
    fn rows(&self, source: usize, limit: i64) -> Result<Vec<PublicUrlEntry>, UseCaseError> {
        self.calls.lock().unwrap().push((source, limit));
        if self.fail == Some(source) {
            return Err(UseCaseError::Repository("source unavailable".into()));
        }
        Ok((0..self.counts[source].min(limit as usize))
            .map(|i| PublicUrlEntry {
                slug: format!("source-{source}-{i}"),
                updated_at: OffsetDateTime::UNIX_EPOCH,
            })
            .collect())
    }
}
#[async_trait]
impl PublishedPostQuery for Sources {
    async fn list_public(
        &self,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<PublicPostSummary>, UseCaseError> {
        assert_eq!((limit, offset), (20, 0));
        Ok(vec![PublicPostSummary {
            title: "A & <B>".into(),
            slug: "关于".into(),
            excerpt: Some("raw & text".into()),
            published_at: Some(OffsetDateTime::UNIX_EPOCH),
            author_display: "author".into(),
            author_avatar_media_id: None,
        }])
    }
    async fn find_public_by_slug(&self, _: &str) -> Result<Option<PublicPostDetail>, UseCaseError> {
        panic!("unexpected detail lookup")
    }
    async fn list_public_for_sitemap(
        &self,
        limit: i64,
    ) -> Result<Vec<PublicUrlEntry>, UseCaseError> {
        self.rows(0, limit)
    }
}
#[async_trait]
impl PublishedPageQuery for Sources {
    async fn find_public_by_slug(&self, _: &str) -> Result<Option<PublicPageDetail>, UseCaseError> {
        panic!("unexpected detail lookup")
    }
    async fn list_public_for_sitemap(
        &self,
        limit: i64,
    ) -> Result<Vec<PublicUrlEntry>, UseCaseError> {
        self.rows(1, limit)
    }
}
#[async_trait]
impl PublishedTagQuery for Sources {
    async fn list_public_tags(&self, _: i64) -> Result<Vec<PublicTagSummary>, UseCaseError> {
        panic!("unexpected directory page")
    }
    async fn find_public_by_slug(&self, _: &str) -> Result<Option<PublicTagSummary>, UseCaseError> {
        panic!("unexpected detail lookup")
    }
    async fn list_public_posts_by_tag(
        &self,
        _: &str,
        _: i64,
        _: i64,
    ) -> Result<(Vec<PublicPostSummary>, i64), UseCaseError> {
        panic!("unexpected member lookup")
    }
    async fn list_public_directories(
        &self,
        limit: i64,
    ) -> Result<Vec<PublicUrlEntry>, UseCaseError> {
        self.rows(2, limit)
    }
}
#[async_trait]
impl PublishedCategoryQuery for Sources {
    async fn list_public_categories(
        &self,
        _: i64,
    ) -> Result<Vec<PublicCategorySummary>, UseCaseError> {
        panic!("unexpected directory page")
    }
    async fn find_public_by_slug(
        &self,
        _: &str,
    ) -> Result<Option<PublicCategorySummary>, UseCaseError> {
        panic!("unexpected detail lookup")
    }
    async fn list_public_posts_by_category(
        &self,
        _: &str,
        _: i64,
        _: i64,
    ) -> Result<(Vec<PublicPostSummary>, i64), UseCaseError> {
        panic!("unexpected member lookup")
    }
    async fn list_public_directories(
        &self,
        limit: i64,
    ) -> Result<Vec<PublicUrlEntry>, UseCaseError> {
        self.rows(3, limit)
    }
}
#[async_trait]
impl PublishedSeriesQuery for Sources {
    async fn find_public_by_slug(
        &self,
        _: &str,
    ) -> Result<Option<PublicSeriesSummary>, UseCaseError> {
        panic!("unexpected detail lookup")
    }
    async fn list_public_posts_by_series(
        &self,
        _: &str,
        _: i64,
        _: i64,
    ) -> Result<(Vec<PublicPostSummary>, i64), UseCaseError> {
        panic!("unexpected member lookup")
    }
    async fn list_public_directories(
        &self,
        limit: i64,
    ) -> Result<Vec<PublicUrlEntry>, UseCaseError> {
        self.rows(4, limit)
    }
}

struct Settings {
    fail: bool,
}
#[async_trait]
impl SettingsStore for Settings {
    async fn find_site(&self) -> Result<Option<SiteSettingsRecord>, UseCaseError> {
        if self.fail {
            return Err(UseCaseError::Repository("settings unavailable".into()));
        }
        Ok(Some(SiteSettingsRecord {
            version: 1,
            value: SiteSettingsValue {
                home_page_size: None,
                navigation: vec![],
                time_zone: None,
                title: Some("  DB & title  ".into()),
                description: Some("".into()),
                logo_media_id: None,
            },
        }))
    }
    async fn save_site(
        &self,
        _: &SiteSettingsValue,
        _: i64,
        _: OffsetDateTime,
        _: application::audit::AuditContext,
    ) -> Result<SaveOutcome, UseCaseError> {
        panic!("read must not write")
    }
}
// Machine-readable data must not invoke a theme renderer.
struct NoTheme;
#[async_trait]
impl ThemeRenderer for NoTheme {
    async fn render_index(
        &self,
        _: &SiteInfo,
        _: &SeoMeta,
        _: &[PostCard],
        _: &IndexPagination,
    ) -> Result<String, UseCaseError> {
        panic!("syndication must not use theme rendering")
    }
    async fn render_post(
        &self,
        _: &SiteInfo,
        _: &SeoMeta,
        _: &PostView,
    ) -> Result<String, UseCaseError> {
        panic!("syndication must not use theme rendering")
    }
    async fn render_page(
        &self,
        _: &SiteInfo,
        _: &SeoMeta,
        _: &PageView,
    ) -> Result<String, UseCaseError> {
        panic!("syndication must not use theme rendering")
    }
    async fn render_tag(
        &self,
        _: &SiteInfo,
        _: &SeoMeta,
        _: &TagView,
    ) -> Result<String, UseCaseError> {
        panic!("syndication must not use theme rendering")
    }
    async fn render_category(
        &self,
        _: &SiteInfo,
        _: &SeoMeta,
        _: &CategoryView,
    ) -> Result<String, UseCaseError> {
        panic!("syndication must not use theme rendering")
    }
    async fn render_series(
        &self,
        _: &SiteInfo,
        _: &SeoMeta,
        _: &SeriesView,
    ) -> Result<String, UseCaseError> {
        panic!("syndication must not use theme rendering")
    }
}
fn site(sources: Arc<Sources>, settings_fail: bool) -> PublicSiteInteractor {
    PublicSiteInteractor::new(
        sources.clone(),
        sources.clone(),
        sources.clone(),
        sources.clone(),
        sources,
        Arc::new(NoTheme),
        Arc::new(Settings {
            fail: settings_fail,
        }),
        SiteInfo {
            home_page_size: application::site_info::DEFAULT_HOME_PAGE_SIZE,
            navigation: vec![],
            time_zone: "UTC".into(),
            title: "fallback".into(),
            description: "fallback description".into(),
            logo_url: None,
        },
        PublicBaseUrl::parse("https://blog.test").unwrap(),
    )
}
fn sources(counts: [usize; 5], fail: Option<usize>) -> Arc<Sources> {
    Arc::new(Sources {
        counts,
        fail,
        calls: Mutex::new(vec![]),
    })
}
#[tokio::test]
async fn each_source_receives_only_the_remaining_whole_file_budget() {
    let sources = sources([2, 3, 4, 5, 6], None);
    let entries = site(sources.clone(), false)
        .sitemap_entries()
        .await
        .unwrap();
    assert_eq!(entries.len(), 21);
    assert_eq!(entries[0].loc, "https://blog.test/");
    assert_eq!(
        *sources.calls.lock().unwrap(),
        vec![(0, 49999), (1, 49997), (2, 49994), (3, 49990), (4, 49985)]
    );
    assert_eq!(
        entries.last().unwrap().loc,
        "https://blog.test/series/source-4-5"
    );
}
#[tokio::test]
async fn any_source_can_exhaust_the_budget_and_skip_all_later_queries() {
    for full_source in 0..5 {
        let mut counts = [1; 5];
        counts[full_source] = SITEMAP_URL_LIMIT as usize;
        let sources = sources(counts, None);
        let entries = site(sources.clone(), false)
            .sitemap_entries()
            .await
            .unwrap();
        assert_eq!(entries.len(), SITEMAP_URL_LIMIT as usize);
        let calls = sources.calls.lock().unwrap();
        assert_eq!(calls.len(), full_source + 1);
        for &(source, limit) in calls.iter() {
            assert_eq!(limit, SITEMAP_URL_LIMIT - 1 - source as i64);
        }
    }
}
#[tokio::test]
async fn query_failure_propagates_without_reading_later_sources() {
    let sources = sources([1; 5], Some(2));
    assert!(matches!(
        site(sources.clone(), false).sitemap_entries().await,
        Err(UseCaseError::Repository(_))
    ));
    assert_eq!(sources.calls.lock().unwrap().len(), 3);
}
#[tokio::test]
async fn feed_returns_raw_data_and_resolves_site_settings_without_rendering() {
    for fail in [false, true] {
        let channel = site(sources([0; 5], None), fail)
            .feed_channel()
            .await
            .unwrap();
        assert_eq!(channel.title, if fail { "fallback" } else { "DB & title" });
        assert_eq!(
            channel.description,
            if fail { "fallback description" } else { "" }
        );
        assert_eq!(channel.items[0].title, "A & <B>");
        assert_eq!(channel.items[0].description.as_deref(), Some("raw & text"));
        assert_eq!(
            channel.items[0].url,
            "https://blog.test/posts/%E5%85%B3%E4%BA%8E"
        );
        assert_eq!(channel.self_url, "https://blog.test/feed.xml");
    }
}
