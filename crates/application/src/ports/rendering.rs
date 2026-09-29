//! 内容与主题渲染端口；执行并发、阻塞隔离与超时由适配器负责。

use async_trait::async_trait;
use uuid::Uuid;

use crate::error::UseCaseError;
use crate::public_site::{CategoryView, PageView, PostCard, PostView, SeriesView, TagView};
use crate::seo::SeoMeta;
use crate::site_info::SiteInfo;

/// 将绝对时刻转换为站点时区的展示文本；不改变存储、排序和到期判断。
/// IANA 时区规则由适配器提供，应用层不依赖时区数据库。
pub trait DateTimeFormatter: Send + Sync {
    fn format(&self, at: time::OffsetDateTime) -> String;
}

/// IANA 名称校验与展示策略解析；数据库时区规则由适配器提供。
pub trait TimeZoneProvider: Send + Sync {
    fn resolve(&self, name: &str) -> Result<std::sync::Arc<dyn DateTimeFormatter>, UseCaseError>;
    fn names(&self) -> Vec<String>;
}

/// 同次渲染的清洗后 HTML 与正文图片引用，必须一起持久化。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedContent {
    pub content_html: String,
    /// 仅包含 HTML 中的站内图片引用；按 UUID 排序并去重，不含独立封面字段。
    pub media_ids: Vec<Uuid>,
}

/// Markdown → 清洗后 HTML 和媒体引用；CPU 工作与执行策略由适配器负责。
#[async_trait]
pub trait ContentRenderer: Send + Sync {
    async fn render_content(&self, source: &str) -> Result<RenderedContent, UseCaseError>;
}

/// 受限评论 Markdown；预览与持久化使用相同规则，不生成媒体引用。
#[async_trait]
pub trait CommentRenderer: Send + Sync {
    async fn render_comment(&self, source: &str) -> Result<String, UseCaseError>;
}

/// 主题渲染端口：入站层不得绕过此端口直接使用模板引擎。
///
/// 每个方法都接收本次渲染的 [`SeoMeta`]：canonical/title/description 由应用层
/// 按可信站点地址计算，模板只负责输出，避免同一规则在多个模板里各写一遍。
#[async_trait]
pub trait ThemeRenderer: Send + Sync {
    async fn render_index(
        &self,
        site: &SiteInfo,
        seo: &SeoMeta,
        posts: &[PostCard],
        pagination: &crate::public_site::IndexPagination,
    ) -> Result<String, UseCaseError>;
    async fn render_post(
        &self,
        site: &SiteInfo,
        seo: &SeoMeta,
        post: &PostView,
    ) -> Result<String, UseCaseError>;
    async fn render_page(
        &self,
        site: &SiteInfo,
        seo: &SeoMeta,
        page: &PageView,
    ) -> Result<String, UseCaseError>;
    async fn render_tag(
        &self,
        site: &SiteInfo,
        seo: &SeoMeta,
        tag: &TagView,
    ) -> Result<String, UseCaseError>;
    async fn render_category(
        &self,
        site: &SiteInfo,
        seo: &SeoMeta,
        category: &CategoryView,
    ) -> Result<String, UseCaseError>;
    async fn render_series(
        &self,
        site: &SiteInfo,
        seo: &SeoMeta,
        series: &SeriesView,
    ) -> Result<String, UseCaseError>;
}
