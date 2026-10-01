//! 公开站点读取用例：组装主题渲染数据与 RSS/sitemap 数据。
//! 匿名可见条件唯一：published + public + 未删除 + 发布时间已到。

use std::sync::Arc;

use time::OffsetDateTime;

use crate::error::UseCaseError;
use crate::ports::{
    DateTimeFormatter, PublishedCategoryQuery, PublishedPageQuery, PublishedPostQuery,
    PublishedSeriesQuery, PublishedTagQuery, SettingsStore, ThemeRenderer, ThemeSettingsStore,
    TimeZoneProvider,
};
use crate::seo::{self, PublicBaseUrl, SeoMeta};
use crate::site_info::{SiteInfo, effective_site};
use crate::syndication::{self, FeedChannel, FeedItem, SitemapEntry};
use crate::themes::ThemeRegistry;
use domain::content::is_reserved_root_slug;

/// 无站点上下文时的默认 UTC 展示格式（CLI 与测试夹具）。
/// HTTP 装配通过 DateTimeFormatter 注入站点时区。
pub fn format_datetime(t: OffsetDateTime) -> String {
    let fmt = time::macros::format_description!("[year]-[month]-[day] [hour]:[minute] UTC");
    t.to_offset(time::UtcOffset::UTC)
        .format(fmt)
        .unwrap_or_else(|_| t.to_string())
}

/// 默认展示策略，供未装配站点时区的工具和测试使用。
pub struct UtcDateTimeFormatter;
impl DateTimeFormatter for UtcDateTimeFormatter {
    fn format(&self, at: OffsetDateTime) -> String {
        format_datetime(at)
    }
}

/// 未装配 IANA 适配器的工具与测试只支持 UTC。
pub struct UtcTimeZones;
impl TimeZoneProvider for UtcTimeZones {
    fn resolve(&self, name: &str) -> Result<Arc<dyn DateTimeFormatter>, UseCaseError> {
        if name == "UTC" {
            Ok(Arc::new(UtcDateTimeFormatter))
        } else {
            Err(UseCaseError::Invalid("时区适配器未装配".into()))
        }
    }
    fn names(&self) -> Vec<String> {
        vec!["UTC".into()]
    }
}

/// API 时间使用可解析的绝对时刻；展示时区由调用端应用。
pub fn api_datetime(at: OffsetDateTime) -> String {
    at.to_offset(time::UtcOffset::UTC)
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| at.to_string())
}

/// 列表页模板数据契约。
/// 首页分页链接由应用层产生，主题不自行拼接地址。
#[derive(Debug, Clone, serde::Serialize)]
pub struct IndexPagination {
    pub page: i64,
    pub previous_url: Option<String>,
    pub next_url: Option<String>,
}
impl Default for IndexPagination {
    fn default() -> Self {
        Self {
            page: 1,
            previous_url: None,
            next_url: None,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PostCard {
    pub title: String,
    pub slug: String,
    pub url: String,
    pub excerpt: Option<String>,
    pub published_at: Option<String>,
    pub author_display: String,
    /// 作者头像站内地址（None = 无头像）；媒体链接独立公开。
    pub author_avatar_url: Option<String>,
}

impl From<crate::ports::PublicPostSummary> for PostCard {
    fn from(post: crate::ports::PublicPostSummary) -> Self {
        Self::in_time_zone(post, &UtcDateTimeFormatter)
    }
}

impl PostCard {
    pub fn in_time_zone(
        post: crate::ports::PublicPostSummary,
        dates: &dyn DateTimeFormatter,
    ) -> Self {
        Self {
            url: seo::post_path(&post.slug),
            title: post.title,
            slug: post.slug,
            excerpt: post.excerpt,
            published_at: post.published_at.map(|at| dates.format(at)),
            author_display: post.author_display,
            author_avatar_url: post.author_avatar_media_id.map(crate::media::media_url),
        }
    }
}

/// 详情页上的标签链接（目录公开；名称取当前值）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct TagCard {
    pub slug: String,
    pub name: String,
}

/// 详情页上的分类链接（至多一个）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct CategoryCard {
    pub slug: String,
    pub name: String,
}

/// 详情页上的系列链接（含排序权重）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct SeriesCard {
    pub slug: String,
    pub name: String,
    pub position: i32,
}

/// 详情页模板数据契约；content_html 已经过清洗。
#[derive(Debug, Clone, serde::Serialize)]
pub struct PostView {
    pub title: String,
    pub slug: String,
    pub url: String,
    pub excerpt: Option<String>,
    pub published_at: Option<String>,
    pub updated_at: String,
    pub author_display: String,
    /// 作者头像站内地址（None = 无头像）。
    pub author_avatar_url: Option<String>,
    pub content_html: String,
    /// 封面站内地址（None = 无封面）。与正文图片同一个 `/media/{id}` 出口，
    /// 媒体链接独立公开。
    pub cover_url: Option<String>,
    /// 当前标签（链接到 /tags/{slug}）。
    pub tags: Vec<TagCard>,
    /// 所属分类（链接到 /categories/{slug}）。
    pub category: Option<CategoryCard>,
    /// 所属系列数组（链接到 /series/{slug}）。
    pub series: Vec<SeriesCard>,
}

/// 页面详情页模板数据契约；content_html 已经过清洗。
#[derive(Debug, Clone, serde::Serialize)]
pub struct PageView {
    pub title: String,
    pub slug: String,
    pub published_at: Option<String>,
    pub updated_at: String,
    pub content_html: String,
}

/// 公开分类页模板数据契约：与标签页同构（分类头 + 分页文章列表）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct CategoryView {
    pub category_slug: String,
    pub category_name: String,
    pub page: i64,
    pub total_pages: i64,
    pub posts: Vec<PostCard>,
}

/// 公开系列页模板数据契约：按阅读顺序的分页文章列表。
#[derive(Debug, Clone, serde::Serialize)]
pub struct SeriesView {
    pub series_slug: String,
    pub series_name: String,
    /// 封面站内地址（None = 无封面）。
    pub cover_url: Option<String>,
    pub page: i64,
    pub total_pages: i64,
    /// 公开成员的连续阅读序号（1 起；不是 post_series.position 排序权重）。
    pub posts: Vec<SeriesPostCard>,
}

/// 系列页条目：文章卡片 + 阅读序号。
#[derive(Debug, Clone, serde::Serialize)]
pub struct SeriesPostCard {
    #[serde(flatten)]
    pub card: PostCard,
    pub index: i64,
}

/// 公开标签页模板数据契约：标签头 + 分页文章列表。
#[derive(Debug, Clone, serde::Serialize)]
pub struct TagView {
    pub tag_slug: String,
    pub tag_name: String,
    /// 当前页码（1 起）。
    pub page: i64,
    /// 总页数（至少 1，空目录也渲染第 1 页）。
    pub total_pages: i64,
    /// 本页公开文章。
    pub posts: Vec<PostCard>,
}

/// 公开列表页（标签页/分类页/系列页）分页大小。
pub const TAG_PAGE_SIZE: i64 = 20;
pub const CATEGORY_PAGE_SIZE: i64 = 20;
pub const SERIES_PAGE_SIZE: i64 = 20;

pub struct PublicSiteInteractor {
    time_zones: Arc<dyn TimeZoneProvider>,
    posts: Arc<dyn PublishedPostQuery>,
    pages: Arc<dyn PublishedPageQuery>,
    tags: Arc<dyn PublishedTagQuery>,
    categories: Arc<dyn PublishedCategoryQuery>,
    series: Arc<dyn PublishedSeriesQuery>,
    theme: Arc<dyn ThemeRenderer>,
    theme_configs: Option<Arc<dyn crate::theme_config::ThemeConfigStore>>,
    themes: Option<(Arc<dyn ThemeSettingsStore>, Arc<ThemeRegistry>)>,
    /// settings 的 site 分组（数据库未配置时整体回退）。
    settings: Arc<dyn SettingsStore>,
    /// 装配回退值：环境变量/内置默认值（进程内不变）。
    fallback: SiteInfo,
    /// 可信站点公开地址：canonical、RSS 链接与 sitemap 一律由它拼绝对 URL。
    base_url: PublicBaseUrl,
}

impl PublicSiteInteractor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        posts: Arc<dyn PublishedPostQuery>,
        pages: Arc<dyn PublishedPageQuery>,
        tags: Arc<dyn PublishedTagQuery>,
        categories: Arc<dyn PublishedCategoryQuery>,
        series: Arc<dyn PublishedSeriesQuery>,
        theme: Arc<dyn ThemeRenderer>,
        settings: Arc<dyn SettingsStore>,
        fallback: SiteInfo,
        base_url: PublicBaseUrl,
    ) -> Self {
        Self {
            time_zones: Arc::new(UtcTimeZones),
            posts,
            pages,
            tags,
            categories,
            series,
            theme,
            themes: None,
            theme_configs: None,
            settings,
            fallback,
            base_url,
        }
    }

    pub fn with_time_zones(mut self, time_zones: Arc<dyn TimeZoneProvider>) -> Self {
        self.time_zones = time_zones;
        self
    }

    pub fn with_themes(
        mut self,
        store: Arc<dyn ThemeSettingsStore>,
        registry: Arc<ThemeRegistry>,
    ) -> Self {
        self.themes = Some((store, registry));
        self
    }

    pub fn with_theme_configs(
        mut self,
        store: Arc<dyn crate::theme_config::ThemeConfigStore>,
    ) -> Self {
        self.theme_configs = Some(store);
        self
    }

    async fn active_theme(&self) -> Result<Arc<dyn ThemeRenderer>, UseCaseError> {
        if let Some((store, registry)) = &self.themes {
            let slug = store
                .find_theme()
                .await?
                .map(|record| record.slug)
                .unwrap_or_else(|| registry.fallback().to_string());
            let snapshot = registry.snapshot(&slug)?;
            let renderer = snapshot.renderer;
            if let Some(configs) = &self.theme_configs {
                let config = match configs.find(&snapshot.slug).await? {
                    Some(record) => record.effective(&snapshot.release, &snapshot.schema)?,
                    None => snapshot.schema.defaults(),
                };
                Ok(renderer.with_config(config).unwrap_or(renderer))
            } else {
                Ok(renderer)
            }
        } else {
            Ok(self.theme.clone())
        }
    }

    /// 本次渲染的生效站点信息：**每次请求解析**，不缓存。
    ///
    /// - 行存在：按字段生效（缺字段回退，见 [`effective_site`]）；
    /// - 行不存在或存储读取失败：整体回退装配值——公开页面不能因为
    ///   配置读取问题对读者 500，后台保存路径的错误仍会如实上报。
    pub async fn site_info(&self) -> SiteInfo {
        match self.settings.find_site().await {
            Ok(Some(record)) => effective_site(&record.value, &self.fallback),
            Ok(None) | Err(_) => self.fallback.clone(),
        }
    }

    async fn render_site_info(&self) -> Result<SiteInfo, UseCaseError> {
        let record = match self.settings.find_site().await {
            Ok(Some(record)) => record,
            Ok(None) | Err(_) => return Ok(self.fallback.clone()),
        };
        let mut site = effective_site(&record.value, &self.fallback);
        let items = crate::navigation::validate_navigation(record.value.navigation)?;
        if !items.is_empty() {
            let slugs: Vec<_> = items.iter().map(|item| item.page_slug.clone()).collect();
            let visible = self.pages.public_navigation_slugs(&slugs).await?;
            site.navigation = items
                .into_iter()
                .filter(|item| visible.contains(&item.page_slug))
                .map(|item| crate::navigation::NavigationLink {
                    label: item.label,
                    url: crate::seo::page_path(&item.page_slug),
                    placement: item.placement,
                })
                .collect();
        }
        Ok(site)
    }

    pub async fn render_index(&self, page: i64) -> Result<String, UseCaseError> {
        let site = self.render_site_info().await?;
        let dates = self.time_zones.resolve(&site.time_zone)?;
        let page_size = crate::site_info::validate_home_page_size(site.home_page_size)?;
        let (page, offset) = public_pagination(page, page_size)?;
        let mut rows = self.posts.list_public(page_size + 1, offset).await?;
        if page > 1 && rows.is_empty() {
            return Err(UseCaseError::NotFound("文章分页".into()));
        }
        let has_next = rows.len() > page_size as usize;
        rows.truncate(page_size as usize);
        let pagination = IndexPagination {
            page,
            previous_url: (page > 1).then(|| crate::seo::index_path(page - 1)),
            next_url: has_next.then(|| crate::seo::index_path(page + 1)),
        };
        let summaries: Vec<PostCard> = rows
            .into_iter()
            .map(|post| PostCard::in_time_zone(post, dates.as_ref()))
            .collect();
        let seo = SeoMeta::home_page(&site, &self.base_url, page);
        self.active_theme()
            .await?
            .render_index(&site, &seo, &summaries, &pagination)
            .await
    }

    /// 渲染公开文章详情；不满足公开条件一律 NotFound（知道 slug 不等于有权读取）。
    pub async fn render_post(&self, slug: &str) -> Result<String, UseCaseError> {
        let site = self.render_site_info().await?;
        let dates = self.time_zones.resolve(&site.time_zone)?;
        let detail = self
            .posts
            .find_public_by_slug(slug)
            .await?
            .ok_or_else(|| UseCaseError::NotFound(format!("文章 {slug}")))?;
        let view = PostView {
            url: seo::post_path(&detail.slug),
            title: detail.title.clone(),
            slug: detail.slug.clone(),
            excerpt: detail.excerpt.clone(),
            published_at: detail.published_at.map(|at| dates.format(at)),
            updated_at: dates.format(detail.updated_at),
            author_display: detail.author_display.clone(),
            author_avatar_url: detail.author_avatar_media_id.map(crate::media::media_url),
            content_html: detail.content_html,
            cover_url: detail.cover_media_id.map(crate::media::media_url),
            tags: detail
                .tags
                .iter()
                .map(|t| TagCard {
                    slug: t.slug.clone(),
                    name: t.name.clone(),
                })
                .collect(),
            category: detail.category.as_ref().map(|c| CategoryCard {
                slug: c.slug.clone(),
                name: c.name.clone(),
            }),
            series: detail
                .series
                .iter()
                .map(|s| SeriesCard {
                    slug: s.slug.clone(),
                    name: s.name.clone(),
                    position: s.position,
                })
                .collect(),
        };
        let seo = SeoMeta::post(
            &site,
            &self.base_url,
            &view.title,
            &view.slug,
            view.excerpt.as_deref(),
        );
        self.active_theme()
            .await?
            .render_post(&site, &seo, &view)
            .await
    }

    /// 渲染公开页面详情（根路径 `/{slug}`）。
    ///
    /// 保留路径在这里再次拒绝：即使历史数据或迁移绕过了创建/发布校验，
    /// 也不能让页面顶掉 `/admin`、`/api` 等系统入口。
    pub async fn render_page(&self, slug: &str) -> Result<String, UseCaseError> {
        let site = self.render_site_info().await?;
        let dates = self.time_zones.resolve(&site.time_zone)?;
        if is_reserved_root_slug(slug) {
            return Err(UseCaseError::NotFound(format!("页面 {slug}")));
        }
        let detail = self
            .pages
            .find_public_by_slug(slug)
            .await?
            .ok_or_else(|| UseCaseError::NotFound(format!("页面 {slug}")))?;
        let view = PageView {
            title: detail.title.clone(),
            slug: detail.slug.clone(),
            published_at: detail.published_at.map(|at| dates.format(at)),
            updated_at: dates.format(detail.updated_at),
            content_html: detail.content_html,
        };
        let seo = SeoMeta::page(&site, &self.base_url, &view.title, &view.slug);
        self.active_theme()
            .await?
            .render_page(&site, &seo, &view)
            .await
    }

    /// 渲染公开标签页 /tags/{slug}?page=N。
    ///
    /// 标签本身没有可见性；未知 slug 一律 NotFound。文章列表复用公开谓词：
    /// 草稿/私密/回收站文章即使挂着该标签也不出现。页码越界渲染空页
    /// （不报错——分页导航按总数链接，越界通常是并发撤文，属正常状态）。
    pub async fn render_tag(&self, slug: &str, page: i64) -> Result<String, UseCaseError> {
        let site = self.render_site_info().await?;
        let dates = self.time_zones.resolve(&site.time_zone)?;
        let tag = self
            .tags
            .find_public_by_slug(slug)
            .await?
            .ok_or_else(|| UseCaseError::NotFound(format!("标签 {slug}")))?;
        let (page, offset) = public_pagination(page, TAG_PAGE_SIZE)?;
        let (posts, total) = self
            .tags
            .list_public_posts_by_tag(slug, TAG_PAGE_SIZE, offset)
            .await?;
        let total_pages = public_total_pages(total, TAG_PAGE_SIZE);
        let view = TagView {
            tag_slug: tag.slug,
            tag_name: tag.name,
            page,
            total_pages,
            posts: posts
                .into_iter()
                .map(|post| PostCard::in_time_zone(post, dates.as_ref()))
                .collect(),
        };
        let seo = SeoMeta::tag(
            &site,
            &self.base_url,
            &view.tag_name,
            &view.tag_slug,
            view.page,
        );
        self.active_theme()
            .await?
            .render_tag(&site, &seo, &view)
            .await
    }

    /// 渲染公开分类页 /categories/{slug}?page=N（直接归属，不含子树）。
    /// 语义与标签页一致：未知 slug 404；只列公开已发布文章；越界页为空页。
    pub async fn render_category(&self, slug: &str, page: i64) -> Result<String, UseCaseError> {
        let site = self.render_site_info().await?;
        let dates = self.time_zones.resolve(&site.time_zone)?;
        let category = self
            .categories
            .find_public_by_slug(slug)
            .await?
            .ok_or_else(|| UseCaseError::NotFound(format!("分类 {slug}")))?;
        let (page, offset) = public_pagination(page, CATEGORY_PAGE_SIZE)?;
        let (posts, total) = self
            .categories
            .list_public_posts_by_category(slug, CATEGORY_PAGE_SIZE, offset)
            .await?;
        let total_pages = public_total_pages(total, CATEGORY_PAGE_SIZE);
        let view = CategoryView {
            category_slug: category.slug,
            category_name: category.name,
            page,
            total_pages,
            posts: posts
                .into_iter()
                .map(|post| PostCard::in_time_zone(post, dates.as_ref()))
                .collect(),
        };
        let seo = SeoMeta::category(
            &site,
            &self.base_url,
            &view.category_name,
            &view.category_slug,
            view.page,
        );
        self.active_theme()
            .await?
            .render_category(&site, &seo, &view)
            .await
    }

    /// 渲染公开系列页 /series/{slug}?page=N：按 position、post_id 稳定排序。
    /// 过滤非公开内容；展示的阅读序号按公开成员连续编号，与排序权重分开。
    pub async fn render_series(&self, slug: &str, page: i64) -> Result<String, UseCaseError> {
        let site = self.render_site_info().await?;
        let dates = self.time_zones.resolve(&site.time_zone)?;
        let series = self
            .series
            .find_public_by_slug(slug)
            .await?
            .ok_or_else(|| UseCaseError::NotFound(format!("系列 {slug}")))?;
        let (page, offset) = public_pagination(page, SERIES_PAGE_SIZE)?;
        let (posts, total) = self
            .series
            .list_public_posts_by_series(slug, SERIES_PAGE_SIZE, offset)
            .await?;
        let total_pages = public_total_pages(total, SERIES_PAGE_SIZE);
        let view = SeriesView {
            series_slug: series.slug,
            series_name: series.name,
            cover_url: series.cover_media_id.map(crate::media::media_url),
            page,
            total_pages,
            posts: posts
                .into_iter()
                .enumerate()
                .map(|(i, s)| SeriesPostCard {
                    index: offset + i as i64 + 1,
                    card: PostCard::in_time_zone(s, dates.as_ref()),
                })
                .collect(),
        };
        let seo = SeoMeta::series(
            &site,
            &self.base_url,
            &view.series_name,
            &view.series_slug,
            view.page,
        );
        self.active_theme()
            .await?
            .render_series(&site, &seo, &view)
            .await
    }

    /// 组装 `/feed.xml` 的频道数据：最新公开已发布文章的 RSS 2.0。
    ///
    /// 复用 `list_public`——草稿、私密、已撤回与软删除在查询谓词里就被排除，
    /// 这里不做第二次可见性判断（两处判断迟早会漂移）。
    pub async fn feed_channel(&self) -> Result<FeedChannel, UseCaseError> {
        let site = self.site_info().await;
        let summaries = self
            .posts
            .list_public(syndication::FEED_ITEM_LIMIT, 0)
            .await?;
        let items: Vec<FeedItem> = summaries
            .into_iter()
            .map(|s| FeedItem {
                url: seo::post_url(&self.base_url, &s.slug),
                title: s.title,
                description: s.excerpt,
                published_at: s.published_at,
            })
            .collect();
        Ok(FeedChannel {
            title: site.title.clone(),
            description: site.description.clone(),
            link: self.base_url.root(),
            self_url: seo::feed_url(&self.base_url),
            items,
        })
    }

    /// 组装 `/sitemap.xml` 的收录条目。
    ///
    /// 收录规则（docs/content-lifecycle.md §4 与 SEO 约定一致）：
    /// - 首页恒定收录；
    /// - 公开已发布文章与公开 Page，各自带 `lastmod`（取 `updated_at`）；
    /// - 标签/分类/系列页**只在至少有一篇公开文章时**收录（空目录是薄内容），
    ///   每条只收录第 1 页地址，分页变体不单独收录；
    /// - 草稿、私密、已撤回、软删除内容一律不出现在任何入口。
    ///
    /// 50,000 条上限是**整个文件**的预算（首页 + 文章 + Page + 目录共享）。
    /// 每个来源都按剩余名额限制查询；预算耗尽后跳过后续来源。
    /// 各来源各自取 50,000 再相加会拼出超限文件，而超限 sitemap 会被抓取器
    /// 整体拒绝。内容超过 50,000 条需要 sitemap index（多文件），属后续范围。
    pub async fn sitemap_entries(&self) -> Result<Vec<SitemapEntry>, UseCaseError> {
        let mut entries = vec![SitemapEntry {
            loc: self.base_url.root(),
            lastmod: None,
        }];

        let slots = syndication::remaining_slots(entries.len());
        if slots > 0 {
            for post in self
                .posts
                .list_public_for_sitemap(slots)
                .await?
                .into_iter()
                .take(slots as usize)
            {
                entries.push(SitemapEntry {
                    loc: seo::post_url(&self.base_url, &post.slug),
                    lastmod: Some(post.updated_at),
                });
            }
        }
        let slots = syndication::remaining_slots(entries.len());
        if slots > 0 {
            for page in self
                .pages
                .list_public_for_sitemap(slots)
                .await?
                .into_iter()
                .take(slots as usize)
            {
                entries.push(SitemapEntry {
                    loc: seo::page_url(&self.base_url, &page.slug),
                    lastmod: Some(page.updated_at),
                });
            }
        }
        let slots = syndication::remaining_slots(entries.len());
        if slots > 0 {
            for tag in self
                .tags
                .list_public_directories(slots)
                .await?
                .into_iter()
                .take(slots as usize)
            {
                entries.push(SitemapEntry {
                    loc: seo::tag_url(&self.base_url, &tag.slug, 1),
                    lastmod: Some(tag.updated_at),
                });
            }
        }
        let slots = syndication::remaining_slots(entries.len());
        if slots > 0 {
            for category in self
                .categories
                .list_public_directories(slots)
                .await?
                .into_iter()
                .take(slots as usize)
            {
                entries.push(SitemapEntry {
                    loc: seo::category_url(&self.base_url, &category.slug, 1),
                    lastmod: Some(category.updated_at),
                });
            }
        }
        let slots = syndication::remaining_slots(entries.len());
        if slots > 0 {
            for series in self
                .series
                .list_public_directories(slots)
                .await?
                .into_iter()
                .take(slots as usize)
            {
                entries.push(SitemapEntry {
                    loc: seo::series_url(&self.base_url, &series.slug, 1),
                    lastmod: Some(series.updated_at),
                });
            }
        }
        Ok(entries)
    }

    /// 可信站点地址生成的 sitemap 地址，供 robots 响应声明。
    pub fn sitemap_url(&self) -> String {
        seo::sitemap_url(&self.base_url)
    }
}

// Reserve room for every one-based series index on the requested page.
fn public_pagination(page: i64, size: i64) -> Result<(i64, i64), UseCaseError> {
    let page = page.max(1);
    let end = page
        .checked_mul(size)
        .ok_or_else(|| UseCaseError::Invalid("页码过大".into()))?;
    Ok((page, end - size))
}

fn public_total_pages(total: i64, size: i64) -> i64 {
    (total / size + i64::from(total % size != 0)).max(1)
}

#[cfg(test)]
mod pagination_tests {
    use super::*;

    #[test]
    fn pagination_bounds_and_totals() {
        for size in [TAG_PAGE_SIZE, CATEGORY_PAGE_SIZE, SERIES_PAGE_SIZE] {
            assert_eq!(public_pagination(i64::MIN, size).unwrap(), (1, 0));
            assert_eq!(public_pagination(3, size).unwrap(), (3, 2 * size));
            let last = i64::MAX / size;
            assert_eq!(public_pagination(last, size).unwrap().1, (last - 1) * size);
            assert!(matches!(
                public_pagination(last + 1, size),
                Err(UseCaseError::Invalid(_))
            ));
            assert!(public_pagination(i64::MAX, size).is_err());
            assert_eq!(public_total_pages(0, size), 1);
            assert_eq!(public_total_pages(size + 1, size), 2);
            assert!(public_total_pages(i64::MAX, size) > 0);
        }
    }
}
