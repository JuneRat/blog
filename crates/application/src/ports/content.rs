//! 内容提交与公开阅读查询。

use async_trait::async_trait;
use time::OffsetDateTime;
use uuid::Uuid;

use super::runtime::SaveOutcome;
use crate::error::UseCaseError;
use domain::content::page::{Page, PageSnapshot};
use domain::content::post::{Post, PostSnapshot};

/// 同一数据库快照中的文章及其标签；仅作为读取/提交结果，不作为写入命令。
#[derive(Debug, Clone, PartialEq)]
pub struct PostRecord {
    pub snapshot: PostSnapshot,
    /// 去重并按 id 排序。
    pub tag_ids: Vec<Uuid>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PostCommitOutcome {
    Saved(Box<PostRecord>),
    StaleConflict,
    Gone,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PageCommitOutcome {
    Saved(PageSnapshot),
    StaleConflict,
    Gone,
}

#[async_trait]
pub trait PostRepository: Send + Sync {
    /// 一次读取正文、版本和标签，禁止以独立查询拼装不同快照。
    async fn find_record_by_id(&self, id: Uuid) -> Result<Option<PostRecord>, UseCaseError>;

    /// 保存已经通过领域行为构造的草稿；正文、标签、系列版本、媒体引用同事务。
    async fn insert_post(&self, post: &Post, tag_ids: &[Uuid]) -> Result<PostRecord, UseCaseError>;

    /// 提交领域变更与 CAS：正文/标签/媒体引用及受影响系列版本必须原子更新。
    /// `tag_ids=None` 保留当前标签；返回的完整记录必须在本事务内取得。
    async fn commit_post(
        &self,
        post: &Post,
        expected_version: i64,
        now: OffsetDateTime,
        tag_ids: Option<&[Uuid]>,
    ) -> Result<PostCommitOutcome, UseCaseError>;

    /// 提交领域已决定的回收站转换（status/deleted_at），不修改内容与系列关系。
    /// CAS 同时检查原回收站状态：移入要求当前未删除，恢复要求当前已删除。
    async fn commit_lifecycle(
        &self,
        post: &Post,
        expected_version: i64,
        now: OffsetDateTime,
    ) -> Result<PostCommitOutcome, UseCaseError>;

    async fn find_by_id(&self, id: Uuid) -> Result<Option<PostSnapshot>, UseCaseError>;
    async fn list_by_author(&self, author_id: Uuid) -> Result<Vec<PostSnapshot>, UseCaseError>;
    async fn list_trash_by_author(
        &self,
        author_id: Uuid,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<PostSnapshot>, i64), UseCaseError>;
    async fn purge(&self, id: Uuid, expected_version: i64) -> Result<SaveOutcome, UseCaseError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageDeleteOutcome {
    Deleted,
    StaleVersion,
    Gone,
}

/// Page 写侧端口。Page 无软删除：不存在「已消失但仍是版本冲突」之外的第三态，
/// 但沿用同一 `SaveOutcome` 以便与 Post 的并发语义保持一致。
#[async_trait]
pub trait PageRepository: Send + Sync {
    async fn insert_page(&self, page: &Page) -> Result<PageSnapshot, UseCaseError>;

    /// Page 没有独立标签集合，结果属于同次 CAS，不做提交后回读。
    async fn commit_page(
        &self,
        page: &Page,
        expected_version: i64,
        now: OffsetDateTime,
    ) -> Result<PageCommitOutcome, UseCaseError>;

    async fn find_by_id(&self, id: Uuid) -> Result<Option<PageSnapshot>, UseCaseError>;
    /// 站点级列表：无作者过滤，按更新时间倒序。
    async fn list(&self) -> Result<Vec<PageSnapshot>, UseCaseError>;
    /// 只删除指定 id 与版本；slug 可重用，不能仅凭 slug 删除新占位者。
    async fn delete(
        &self,
        id: Uuid,
        expected_version: i64,
    ) -> Result<PageDeleteOutcome, UseCaseError>;
}

/// 公开列表条目。
#[derive(Debug, Clone, serde::Serialize)]
pub struct PublicPostSummary {
    pub title: String,
    pub slug: String,
    pub excerpt: Option<String>,
    pub published_at: Option<OffsetDateTime>,
    pub author_display: String,
    /// 作者头像媒体 id（None = 无头像）；公开页 URL 由应用层生成。
    pub author_avatar_media_id: Option<Uuid>,
}

/// sitemap 用条目：路径片段 + 最近修改时间。
///
/// 与页面渲染用的列表条目分开：sitemap 需要**全部**公开 URL 且只关心
/// `slug`/`updated_at`，不必为它把正文摘要素材拉进内存。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicUrlEntry {
    pub slug: String,
    pub updated_at: OffsetDateTime,
}

/// 公开文章上挂的标签引用（详情页展示；名称取当前值）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PublicTagRef {
    pub slug: String,
    pub name: String,
}

/// 公开文章上的分类引用（详情页展示；至多一个）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PublicCategoryRef {
    pub slug: String,
    pub name: String,
}

/// 公开文章上的系列引用（含阅读序号）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PublicSeriesRef {
    pub slug: String,
    pub name: String,
    pub order: i32,
}

/// 公开详情；正文是写入时已清洗的 HTML 派生内容。
#[derive(Debug, Clone)]
pub struct PublicPostDetail {
    pub title: String,
    pub slug: String,
    pub excerpt: Option<String>,
    pub published_at: Option<OffsetDateTime>,
    pub updated_at: OffsetDateTime,
    pub author_display: String,
    pub author_username: String,
    /// 作者头像媒体 id（None = 无头像）；公开页 URL 由应用层生成。
    pub author_avatar_media_id: Option<Uuid>,
    pub content_html: String,
    /// 封面媒体资产 id（None = 无封面）；公开页 URL 由应用层生成。
    pub cover_media_id: Option<Uuid>,
    /// 当前关联标签（仅取存在于 tags 表的行；无可见性过滤——标签目录本身公开）。
    pub tags: Vec<PublicTagRef>,
    /// 所属分类（至多一个；分类目录本身公开）。
    pub category: Option<PublicCategoryRef>,
    /// 所属系列与阅读序号（公开页序号可能因草稿占位而留空档）。
    pub series: Option<PublicSeriesRef>,
}

#[async_trait]
pub trait PublishedPostQuery: Send + Sync {
    /// 只返回 status=published AND visibility=public AND deleted_at IS NULL 的文章。
    async fn list_public(
        &self,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<PublicPostSummary>, UseCaseError>;
    async fn find_public_by_slug(
        &self,
        slug: &str,
    ) -> Result<Option<PublicPostDetail>, UseCaseError>;
    /// sitemap 用：同一公开谓词下按最近更新倒序枚举，`limit` 由调用方给上限。
    async fn list_public_for_sitemap(
        &self,
        limit: i64,
    ) -> Result<Vec<PublicUrlEntry>, UseCaseError>;
}

/// 公开标签页数据源。标签目录本身没有可见性；可见性过滤作用在文章列表上。
#[derive(Debug, Clone, serde::Serialize)]
pub struct PublicTagSummary {
    pub slug: String,
    pub name: String,
}

/// 公开分类页数据源（与标签页同构：目录无可见性，过滤作用在文章列表）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct PublicCategorySummary {
    pub slug: String,
    pub name: String,
}

/// 公开系列页数据源。
#[derive(Debug, Clone, serde::Serialize)]
pub struct PublicSeriesSummary {
    pub slug: String,
    pub name: String,
    /// 封面媒体资产 id（None = 无封面）；公开页 URL 由应用层生成。
    pub cover_media_id: Option<Uuid>,
}

#[async_trait]
pub trait PublishedSeriesQuery: Send + Sync {
    async fn find_public_by_slug(
        &self,
        slug: &str,
    ) -> Result<Option<PublicSeriesSummary>, UseCaseError>;

    /// 系列内公开文章按 series_order 升序分页（含总数）。
    async fn list_public_posts_by_series(
        &self,
        series_slug: &str,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<PublicPostSummary>, i64), UseCaseError>;

    /// sitemap 用：**至少有一篇公开文章**的系列，含该系列公开文章的最近更新时间。
    ///
    /// 空目录天然被过滤掉——列表页没有公开内容，收录它只会制造薄内容。
    async fn list_public_directories(&self) -> Result<Vec<PublicUrlEntry>, UseCaseError>;
}

#[async_trait]
pub trait PublishedCategoryQuery: Send + Sync {
    async fn list_public_categories(
        &self,
        limit: i64,
    ) -> Result<Vec<PublicCategorySummary>, UseCaseError>;
    async fn find_public_by_slug(
        &self,
        slug: &str,
    ) -> Result<Option<PublicCategorySummary>, UseCaseError>;

    /// 该分类**直接归属**的公开文章分页（不含子树；docs/content-lifecycle.md §3）。
    async fn list_public_posts_by_category(
        &self,
        category_slug: &str,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<PublicPostSummary>, i64), UseCaseError>;

    /// sitemap 用：至少有一篇直接归属公开文章的分类（规则同系列）。
    async fn list_public_directories(&self) -> Result<Vec<PublicUrlEntry>, UseCaseError>;
}

#[async_trait]
pub trait PublishedTagQuery: Send + Sync {
    async fn list_public_tags(&self, limit: i64) -> Result<Vec<PublicTagSummary>, UseCaseError>;
    /// 标签是否存在（未知 slug 一律 None，与文章详情同样不泄漏差异）。
    async fn find_public_by_slug(
        &self,
        slug: &str,
    ) -> Result<Option<PublicTagSummary>, UseCaseError>;

    /// 该标签下的公开文章分页（复用公开文章谓词，含总数供分页导航）。
    async fn list_public_posts_by_tag(
        &self,
        tag_slug: &str,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<PublicPostSummary>, i64), UseCaseError>;

    /// sitemap 用：至少挂有一篇公开文章的标签（规则同系列）。
    async fn list_public_directories(&self) -> Result<Vec<PublicUrlEntry>, UseCaseError>;
}

/// 公开页面详情（Page 无作者、无软删除）。
#[derive(Debug, Clone)]
pub struct PublicPageDetail {
    pub title: String,
    pub slug: String,
    pub published_at: Option<OffsetDateTime>,
    pub updated_at: OffsetDateTime,
    pub content_html: String,
}

#[async_trait]
pub trait PublishedPageQuery: Send + Sync {
    /// 只返回 status=published AND visibility=public 的页面。
    async fn find_public_by_slug(
        &self,
        slug: &str,
    ) -> Result<Option<PublicPageDetail>, UseCaseError>;

    /// sitemap 用：枚举全部公开页面（Page 无软删除，谓词只有状态与可见性）。
    async fn list_public_for_sitemap(
        &self,
        limit: i64,
    ) -> Result<Vec<PublicUrlEntry>, UseCaseError>;
}
