//! 公开站点读取用例：组装渲染数据并调用主题渲染端口。
//! 匿名可见条件唯一：status=published AND visibility=public AND deleted_at IS NULL。

use std::sync::Arc;

use time::OffsetDateTime;

use crate::error::UseCaseError;
use crate::ports::{
    ContentRenderer, PublishedCategoryQuery, PublishedPageQuery, PublishedPostQuery,
    PublishedSeriesQuery, PublishedTagQuery, SettingsStore,
};
use crate::seo::{self, PublicBaseUrl, SeoMeta};
use crate::settings::effective_site;
use crate::syndication::{self, FeedChannel, FeedItem, SitemapEntry};
use domain::content::is_reserved_root_slug;

/// 模板展示用的时间格式（应用层渲染契约的一部分）。
/// CLI 输出复用同一格式，保证各端一致。
pub fn format_datetime(t: OffsetDateTime) -> String {
    let fmt = time::macros::format_description!("[year]-[month]-[day] [hour]:[minute] UTC");
    t.format(fmt).unwrap_or_else(|_| t.to_string())
}

/// 站点基础信息：一次渲染的生效值。
///
/// 生效优先级（M3 起由 settings 驱动）：数据库 site 行 > 装配回退值
/// （环境变量 `BLOG_SITE_TITLE`/`BLOG_SITE_DESCRIPTION` 或内置默认值）。
/// 解析见 [`crate::settings::effective_site`]。
#[derive(Debug, Clone, serde::Serialize)]
pub struct SiteInfo {
    pub title: String,
    pub description: String,
}

/// 列表页模板数据契约。
#[derive(Debug, Clone, serde::Serialize)]
pub struct PostCard {
    pub title: String,
    pub slug: String,
    pub excerpt: Option<String>,
    pub published_at: Option<String>,
    pub author_display: String,
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

/// 详情页上的系列链接（含阅读序号）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct SeriesCard {
    pub slug: String,
    pub name: String,
    pub order: i32,
}

/// 详情页模板数据契约；content_html 已经过清洗。
#[derive(Debug, Clone, serde::Serialize)]
pub struct PostView {
    pub title: String,
    pub slug: String,
    pub excerpt: Option<String>,
    pub published_at: Option<String>,
    pub updated_at: String,
    pub author_display: String,
    pub content_html: String,
    /// 当前标签（链接到 /tags/{slug}）。
    pub tags: Vec<TagCard>,
    /// 所属分类（链接到 /categories/{slug}）。
    pub category: Option<CategoryCard>,
    /// 所属系列（链接到 /series/{slug}；公开序号可能留空档）。
    pub series: Option<SeriesCard>,
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
    pub page: i64,
    pub total_pages: i64,
    /// 公开成员的连续阅读序号（1 起；不是 posts.series_order——草稿占位会造成空档）。
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

/// 主题渲染端口：入站层不得绕过此端口直接使用模板引擎。
///
/// 每个方法都接收本次渲染的 [`SeoMeta`]：canonical/title/description 由应用层
/// 按可信站点地址计算，模板只负责输出，避免同一规则在多个模板里各写一遍。
pub trait ThemeRenderer: Send + Sync {
    fn render_index(
        &self,
        site: &SiteInfo,
        seo: &SeoMeta,
        posts: &[PostCard],
    ) -> Result<String, UseCaseError>;
    fn render_post(
        &self,
        site: &SiteInfo,
        seo: &SeoMeta,
        post: &PostView,
    ) -> Result<String, UseCaseError>;
    fn render_page(
        &self,
        site: &SiteInfo,
        seo: &SeoMeta,
        page: &PageView,
    ) -> Result<String, UseCaseError>;
    fn render_tag(
        &self,
        site: &SiteInfo,
        seo: &SeoMeta,
        tag: &TagView,
    ) -> Result<String, UseCaseError>;
    fn render_category(
        &self,
        site: &SiteInfo,
        seo: &SeoMeta,
        category: &CategoryView,
    ) -> Result<String, UseCaseError>;
    fn render_series(
        &self,
        site: &SiteInfo,
        seo: &SeoMeta,
        series: &SeriesView,
    ) -> Result<String, UseCaseError>;
}

/// 公开列表页（标签页/分类页/系列页）分页大小。
pub const TAG_PAGE_SIZE: i64 = 20;
pub const CATEGORY_PAGE_SIZE: i64 = 20;
pub const SERIES_PAGE_SIZE: i64 = 20;

pub struct PublicSiteInteractor {
    posts: Arc<dyn PublishedPostQuery>,
    pages: Arc<dyn PublishedPageQuery>,
    tags: Arc<dyn PublishedTagQuery>,
    categories: Arc<dyn PublishedCategoryQuery>,
    series: Arc<dyn PublishedSeriesQuery>,
    markdown: Arc<dyn ContentRenderer>,
    theme: Arc<dyn ThemeRenderer>,
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
        markdown: Arc<dyn ContentRenderer>,
        theme: Arc<dyn ThemeRenderer>,
        settings: Arc<dyn SettingsStore>,
        fallback: SiteInfo,
        base_url: PublicBaseUrl,
    ) -> Self {
        Self {
            posts,
            pages,
            tags,
            categories,
            series,
            markdown,
            theme,
            settings,
            fallback,
            base_url,
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

    pub async fn render_index(&self, limit: i64) -> Result<String, UseCaseError> {
        let summaries: Vec<PostCard> = self
            .posts
            .list_public(limit, 0)
            .await?
            .into_iter()
            .map(|s| PostCard {
                title: s.title,
                slug: s.slug,
                excerpt: s.excerpt,
                published_at: s.published_at.map(format_datetime),
                author_display: s.author_display,
            })
            .collect();
        let site = self.site_info().await;
        let seo = SeoMeta::home(&site, &self.base_url);
        self.theme.render_index(&site, &seo, &summaries)
    }

    /// 渲染公开文章详情；不满足公开条件一律 NotFound（知道 slug 不等于有权读取）。
    pub async fn render_post(&self, slug: &str) -> Result<String, UseCaseError> {
        let detail = self
            .posts
            .find_public_by_slug(slug)
            .await?
            .ok_or_else(|| UseCaseError::NotFound(format!("文章 {slug}")))?;
        let view = PostView {
            title: detail.title.clone(),
            slug: detail.slug.clone(),
            excerpt: detail.excerpt.clone(),
            published_at: detail.published_at.map(format_datetime),
            updated_at: format_datetime(detail.updated_at),
            author_display: detail.author_display.clone(),
            content_html: self.markdown.render_markdown(&detail.content),
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
            series: detail.series.as_ref().map(|s| SeriesCard {
                slug: s.slug.clone(),
                name: s.name.clone(),
                order: s.order,
            }),
        };
        let site = self.site_info().await;
        let seo = SeoMeta::post(
            &site,
            &self.base_url,
            &view.title,
            &view.slug,
            view.excerpt.as_deref(),
        );
        self.theme.render_post(&site, &seo, &view)
    }

    /// 渲染公开页面详情（根路径 `/{slug}`）。
    ///
    /// 保留路径在这里再次拒绝：即使历史数据或迁移绕过了创建/发布校验，
    /// 也不能让页面顶掉 `/admin`、`/api` 等系统入口。
    pub async fn render_page(&self, slug: &str) -> Result<String, UseCaseError> {
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
            published_at: detail.published_at.map(format_datetime),
            updated_at: format_datetime(detail.updated_at),
            content_html: self.markdown.render_markdown(&detail.content),
        };
        let site = self.site_info().await;
        let seo = SeoMeta::page(&site, &self.base_url, &view.title, &view.slug);
        self.theme.render_page(&site, &seo, &view)
    }

    /// 渲染公开标签页 /tags/{slug}?page=N。
    ///
    /// 标签本身没有可见性；未知 slug 一律 NotFound。文章列表复用公开谓词：
    /// 草稿/私密/回收站文章即使挂着该标签也不出现。页码越界渲染空页
    /// （不报错——分页导航按总数链接，越界通常是并发撤文，属正常状态）。
    pub async fn render_tag(&self, slug: &str, page: i64) -> Result<String, UseCaseError> {
        let tag = self
            .tags
            .find_public_by_slug(slug)
            .await?
            .ok_or_else(|| UseCaseError::NotFound(format!("标签 {slug}")))?;
        let page = page.max(1);
        let offset = (page - 1) * TAG_PAGE_SIZE;
        let (posts, total) = self
            .tags
            .list_public_posts_by_tag(slug, TAG_PAGE_SIZE, offset)
            .await?;
        let total_pages = ((total + TAG_PAGE_SIZE - 1) / TAG_PAGE_SIZE).max(1);
        let view = TagView {
            tag_slug: tag.slug,
            tag_name: tag.name,
            page,
            total_pages,
            posts: posts
                .into_iter()
                .map(|s| PostCard {
                    title: s.title,
                    slug: s.slug,
                    excerpt: s.excerpt,
                    published_at: s.published_at.map(format_datetime),
                    author_display: s.author_display,
                })
                .collect(),
        };
        let site = self.site_info().await;
        let seo = SeoMeta::tag(
            &site,
            &self.base_url,
            &view.tag_name,
            &view.tag_slug,
            view.page,
        );
        self.theme.render_tag(&site, &seo, &view)
    }

    /// 渲染公开分类页 /categories/{slug}?page=N（直接归属，不含子树）。
    /// 语义与标签页一致：未知 slug 404；只列公开已发布文章；越界页为空页。
    pub async fn render_category(&self, slug: &str, page: i64) -> Result<String, UseCaseError> {
        let category = self
            .categories
            .find_public_by_slug(slug)
            .await?
            .ok_or_else(|| UseCaseError::NotFound(format!("分类 {slug}")))?;
        let page = page.max(1);
        let offset = (page - 1) * CATEGORY_PAGE_SIZE;
        let (posts, total) = self
            .categories
            .list_public_posts_by_category(slug, CATEGORY_PAGE_SIZE, offset)
            .await?;
        let total_pages = ((total + CATEGORY_PAGE_SIZE - 1) / CATEGORY_PAGE_SIZE).max(1);
        let view = CategoryView {
            category_slug: category.slug,
            category_name: category.name,
            page,
            total_pages,
            posts: posts
                .into_iter()
                .map(|s| PostCard {
                    title: s.title,
                    slug: s.slug,
                    excerpt: s.excerpt,
                    published_at: s.published_at.map(format_datetime),
                    author_display: s.author_display,
                })
                .collect(),
        };
        let site = self.site_info().await;
        let seo = SeoMeta::category(
            &site,
            &self.base_url,
            &view.category_name,
            &view.category_slug,
            view.page,
        );
        self.theme.render_category(&site, &seo, &view)
    }

    /// 渲染公开系列页 /series/{slug}?page=N：按阅读顺序（series_order 升序）。
    /// 草稿/私密/回收站保留位置但不出现；页内展示的阅读序号按公开成员连续编号，
    /// 不透出 posts.series_order 的空档。
    pub async fn render_series(&self, slug: &str, page: i64) -> Result<String, UseCaseError> {
        let series = self
            .series
            .find_public_by_slug(slug)
            .await?
            .ok_or_else(|| UseCaseError::NotFound(format!("系列 {slug}")))?;
        let page = page.max(1);
        let offset = (page - 1) * SERIES_PAGE_SIZE;
        let (posts, total) = self
            .series
            .list_public_posts_by_series(slug, SERIES_PAGE_SIZE, offset)
            .await?;
        let total_pages = ((total + SERIES_PAGE_SIZE - 1) / SERIES_PAGE_SIZE).max(1);
        let view = SeriesView {
            series_slug: series.slug,
            series_name: series.name,
            page,
            total_pages,
            posts: posts
                .into_iter()
                .enumerate()
                .map(|(i, s)| SeriesPostCard {
                    index: offset + i as i64 + 1,
                    card: PostCard {
                        title: s.title,
                        slug: s.slug,
                        excerpt: s.excerpt,
                        published_at: s.published_at.map(format_datetime),
                        author_display: s.author_display,
                    },
                })
                .collect(),
        };
        let site = self.site_info().await;
        let seo = SeoMeta::series(
            &site,
            &self.base_url,
            &view.series_name,
            &view.series_slug,
            view.page,
        );
        self.theme.render_series(&site, &seo, &view)
    }

    /// 渲染 `/feed.xml`：最新公开已发布文章的 RSS 2.0。
    ///
    /// 复用 `list_public`——草稿、私密、已撤回与软删除在查询谓词里就被排除，
    /// 这里不做第二次可见性判断（两处判断迟早会漂移）。
    pub async fn render_feed(&self) -> Result<String, UseCaseError> {
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
        Ok(syndication::render_feed(&FeedChannel {
            title: site.title.clone(),
            description: site.description.clone(),
            link: self.base_url.root(),
            self_url: seo::feed_url(&self.base_url),
            items,
        }))
    }

    /// 渲染 `/sitemap.xml`。
    ///
    /// 收录规则（docs/content-lifecycle.md §4 与 SEO 约定一致）：
    /// - 首页恒定收录；
    /// - 公开已发布文章与公开 Page，各自带 `lastmod`（取 `updated_at`）；
    /// - 标签/分类/系列页**只在至少有一篇公开文章时**收录（空目录是薄内容），
    ///   每条只收录第 1 页地址，分页变体不单独收录；
    /// - 草稿、私密、已撤回、软删除内容一律不出现在任何入口。
    ///
    /// 50,000 条上限是**整个文件**的预算（首页 + 文章 + Page + 目录共享）。
    /// 文章与 Page 按剩余名额限制查询；标签、分类、系列在还有名额时仍全量读取，
    /// 预算耗尽后跳过后续来源，最终由渲染层截断到总上限。
    /// 各来源各自取 50,000 再相加会拼出超限文件，而超限 sitemap 会被抓取器
    /// 整体拒绝。内容超过 50,000 条需要 sitemap index（多文件），属后续范围。
    pub async fn render_sitemap(&self) -> Result<String, UseCaseError> {
        let mut entries = vec![SitemapEntry {
            loc: self.base_url.root(),
            lastmod: None,
        }];

        let slots = syndication::remaining_slots(entries.len());
        if slots > 0 {
            for post in self.posts.list_public_for_sitemap(slots).await? {
                entries.push(SitemapEntry {
                    loc: seo::post_url(&self.base_url, &post.slug),
                    lastmod: Some(post.updated_at),
                });
            }
        }
        let slots = syndication::remaining_slots(entries.len());
        if slots > 0 {
            for page in self.pages.list_public_for_sitemap(slots).await? {
                entries.push(SitemapEntry {
                    loc: seo::page_url(&self.base_url, &page.slug),
                    lastmod: Some(page.updated_at),
                });
            }
        }
        // 目录枚举端口不接受 limit，因此目录可能把 entries 推过上限；
        // render_sitemap 的兜底截断保证输出仍然合法（目录排在最后，先被截掉）。
        let slots = syndication::remaining_slots(entries.len());
        if slots > 0 {
            for tag in self.tags.list_public_directories().await? {
                entries.push(SitemapEntry {
                    loc: seo::tag_url(&self.base_url, &tag.slug, 1),
                    lastmod: Some(tag.updated_at),
                });
            }
        }
        let slots = syndication::remaining_slots(entries.len());
        if slots > 0 {
            for category in self.categories.list_public_directories().await? {
                entries.push(SitemapEntry {
                    loc: seo::category_url(&self.base_url, &category.slug, 1),
                    lastmod: Some(category.updated_at),
                });
            }
        }
        let slots = syndication::remaining_slots(entries.len());
        if slots > 0 {
            for series in self.series.list_public_directories().await? {
                entries.push(SitemapEntry {
                    loc: seo::series_url(&self.base_url, &series.slug, 1),
                    lastmod: Some(series.updated_at),
                });
            }
        }
        Ok(syndication::render_sitemap(&entries))
    }

    /// 渲染 `/robots.txt`：允许抓取公开内容，屏蔽后台/接口/认证前缀，
    /// 并声明 sitemap 地址（否则抓取器无从发现 sitemap）。
    ///
    /// 纯字符串，不读数据库——robots 是站点级约定，不随内容变化。
    pub fn render_robots(&self) -> String {
        format!(
            "User-agent: *\n\
             Allow: /\n\
             Disallow: /admin\n\
             Disallow: /api\n\
             Disallow: /auth\n\
             \n\
             Sitemap: {}\n",
            seo::sitemap_url(&self.base_url)
        )
    }
}
