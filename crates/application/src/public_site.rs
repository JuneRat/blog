//! 公开站点读取用例：组装渲染数据并调用主题渲染端口。
//! 匿名可见条件唯一：status=published AND visibility=public AND deleted_at IS NULL。

use std::sync::Arc;

use time::OffsetDateTime;

use crate::error::UseCaseError;
use crate::ports::{ContentRenderer, PublishedPostQuery};

/// 模板展示用的时间格式（应用层渲染契约的一部分）。
fn format_datetime(t: OffsetDateTime) -> String {
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
}

/// 主题渲染端口：入站层不得绕过此端口直接使用模板引擎。
pub trait ThemeRenderer: Send + Sync {
    fn render_index(&self, site: &SiteInfo, posts: &[PostCard]) -> Result<String, UseCaseError>;
    fn render_post(&self, site: &SiteInfo, post: &PostView) -> Result<String, UseCaseError>;
}

pub struct PublicSiteInteractor {
    posts: Arc<dyn PublishedPostQuery>,
    markdown: Arc<dyn ContentRenderer>,
    theme: Arc<dyn ThemeRenderer>,
    site: SiteInfo,
}

impl PublicSiteInteractor {
    pub fn new(
        posts: Arc<dyn PublishedPostQuery>,
        markdown: Arc<dyn ContentRenderer>,
        theme: Arc<dyn ThemeRenderer>,
        site: SiteInfo,
    ) -> Self {
        Self {
            posts,
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
        };
        self.theme.render_post(&self.site, &view)
    }
}
