//! 公开站点读取用例：组装渲染数据并调用主题渲染端口。
//! 匿名可见条件唯一：status=published AND visibility=public AND deleted_at IS NULL。

use std::sync::Arc;

use time::OffsetDateTime;

use crate::error::UseCaseError;
use crate::ports::{
    ContentRenderer, PublishedCategoryQuery, PublishedPageQuery, PublishedPostQuery,
    PublishedTagQuery,
};
use domain::content::is_reserved_root_slug;

/// 模板展示用的时间格式（应用层渲染契约的一部分）。
/// CLI 输出复用同一格式，保证各端一致。
pub fn format_datetime(t: OffsetDateTime) -> String {
    let fmt = time::macros::format_description!("[year]-[month]-[day] [hour]:[minute] UTC");
    t.format(fmt).unwrap_or_else(|_| t.to_string())
}

/// 站点基础信息（M1 来自装配配置；M3 迁移到 settings 分组）。
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
pub trait ThemeRenderer: Send + Sync {
    fn render_index(&self, site: &SiteInfo, posts: &[PostCard]) -> Result<String, UseCaseError>;
    fn render_post(&self, site: &SiteInfo, post: &PostView) -> Result<String, UseCaseError>;
    fn render_page(&self, site: &SiteInfo, page: &PageView) -> Result<String, UseCaseError>;
    fn render_tag(&self, site: &SiteInfo, tag: &TagView) -> Result<String, UseCaseError>;
    fn render_category(
        &self,
        site: &SiteInfo,
        category: &CategoryView,
    ) -> Result<String, UseCaseError>;
}

/// 公开列表页（标签页/分类页）分页大小。
pub const TAG_PAGE_SIZE: i64 = 20;
pub const CATEGORY_PAGE_SIZE: i64 = 20;

pub struct PublicSiteInteractor {
    posts: Arc<dyn PublishedPostQuery>,
    pages: Arc<dyn PublishedPageQuery>,
    tags: Arc<dyn PublishedTagQuery>,
    categories: Arc<dyn PublishedCategoryQuery>,
    markdown: Arc<dyn ContentRenderer>,
    theme: Arc<dyn ThemeRenderer>,
    site: SiteInfo,
}

impl PublicSiteInteractor {
    pub fn new(
        posts: Arc<dyn PublishedPostQuery>,
        pages: Arc<dyn PublishedPageQuery>,
        tags: Arc<dyn PublishedTagQuery>,
        categories: Arc<dyn PublishedCategoryQuery>,
        markdown: Arc<dyn ContentRenderer>,
        theme: Arc<dyn ThemeRenderer>,
        site: SiteInfo,
    ) -> Self {
        Self {
            posts,
            pages,
            tags,
            categories,
            markdown,
            theme,
            site,
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
        self.theme.render_index(&self.site, &summaries)
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
        };
        self.theme.render_post(&self.site, &view)
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
        self.theme.render_page(&self.site, &view)
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
        self.theme.render_tag(&self.site, &view)
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
        self.theme.render_category(&self.site, &view)
    }
}
