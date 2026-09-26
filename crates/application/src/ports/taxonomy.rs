//! 标签、分类与系列目录端口。

use async_trait::async_trait;
use uuid::Uuid;

use crate::error::UseCaseError;

/// 标签目录条目：含公开文章计数（与公开标签页同一可见性口径）。
///
/// 管理界面的删除保护提示只用「是否被引用」的结论（由删除用例给出），
/// 因此这里不暴露非公开文章的计数，避免草稿/私密的关联规模泄漏给
/// 无 `tag.manage` 的调用者。
#[derive(Debug, Clone, PartialEq)]
pub struct TagWithUsage {
    pub snapshot: domain::content::TagSnapshot,
    /// status=published AND visibility=public AND deleted_at IS NULL 的关联文章数。
    pub public_post_count: i64,
}

/// 标签删除的三态结果（版本冲突与引用保护由用例翻译为不同错误）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagDeleteOutcome {
    Deleted,
    /// expected_version 不匹配：可基于最新版本重试。
    StaleVersion,
    /// 仍被文章引用（含草稿/私密/回收站）：引用保护拒绝删除，携带引用数。
    Referenced {
        count: i64,
    },
    Gone,
}

#[async_trait]
pub trait TagRepository: Send + Sync {
    async fn insert(&self, snapshot: &domain::content::TagSnapshot) -> Result<(), UseCaseError>;
    async fn find_by_slug(
        &self,
        slug: &str,
    ) -> Result<Option<domain::content::TagSnapshot>, UseCaseError>;
    /// 全量目录（数量小，不分页），含公开文章计数，按 slug 排序。
    async fn list(&self) -> Result<Vec<TagWithUsage>, UseCaseError>;

    /// 条件改名（CAS）：命中时 name 更新、version+1 并返回新快照；
    /// 未命中返回 None（调用方区分版本冲突与不存在）。
    async fn rename(
        &self,
        id: Uuid,
        new_name: &str,
        expected_version: i64,
    ) -> Result<Option<domain::content::TagSnapshot>, UseCaseError>;

    /// 条件删除：先在事务内检查引用（post_tags RESTRICT 之外的业务级保护），
    /// 再按版本条件删除。
    async fn delete(
        &self,
        id: Uuid,
        expected_version: i64,
    ) -> Result<TagDeleteOutcome, UseCaseError>;

    /// 返回 `ids` 中确实存在的标签 id（去重、按 id 排序）。
    /// 用例据此把「标签不存在」报为可定位的参数错误，而不是 FK 违规。
    async fn existing_ids(&self, ids: &[Uuid]) -> Result<Vec<Uuid>, UseCaseError>;

    /// 单个标签的公开文章计数（改名响应回填用；与 list 同一口径）。
    async fn public_count(&self, id: Uuid) -> Result<i64, UseCaseError>;
}

/// 分类目录条目：含公开文章计数（直接归属；子树聚合计数需显式查询，首版不提供）。
#[derive(Debug, Clone, PartialEq)]
pub struct CategoryWithUsage {
    pub snapshot: domain::content::CategorySnapshot,
    /// status=published AND visibility=public AND deleted_at IS NULL 且直接归属的文章数。
    pub public_post_count: i64,
}

/// 分类删除的受控结果（引用与子分类保护由用例翻译）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CategoryDeleteOutcome {
    Deleted,
    StaleVersion,
    /// 仍被文章引用或仍有子分类：引用保护拒绝删除。
    Referenced {
        posts: i64,
        children: i64,
    },
    Gone,
}

/// 分类目录写侧端口。
///
/// 防环约定：自引用 CHECK 只排除直接自父；移动（改 parent）的祖先链校验
/// 在 [`CategoryRepository::update`] 的分类树事务锁内完成（docs/content-lifecycle.md §3）。
#[async_trait]
pub trait CategoryRepository: Send + Sync {
    async fn insert(
        &self,
        snapshot: &domain::content::CategorySnapshot,
    ) -> Result<(), UseCaseError>;
    async fn find_by_slug(
        &self,
        slug: &str,
    ) -> Result<Option<domain::content::CategorySnapshot>, UseCaseError>;
    /// 全量目录（含公开文章计数），按 slug 排序。
    async fn list(&self) -> Result<Vec<CategoryWithUsage>, UseCaseError>;

    /// 条件更新（CAS）：name/描述/父节点一次提交；命中返回新快照，未命中 None。
    ///
    /// 父节点变化时在分类树锁内重走祖先链：链上出现自身即 `Err(Invalid)`（成环），
    /// 新父不存在同样 `Err(Invalid)`。树锁保证检查与写入之间无并发移动。
    async fn update(
        &self,
        id: Uuid,
        name: &str,
        description: Option<&str>,
        parent_id: Option<Uuid>,
        expected_version: i64,
    ) -> Result<Option<domain::content::CategorySnapshot>, UseCaseError>;

    /// 条件删除：树锁内先检查文章引用与子分类，再按版本条件删除。
    async fn delete(
        &self,
        id: Uuid,
        expected_version: i64,
    ) -> Result<CategoryDeleteOutcome, UseCaseError>;

    /// 文章设置分类前的存在性校验。
    async fn existing_id(&self, id: Uuid) -> Result<bool, UseCaseError>;

    /// 单个分类的公开文章计数（更新响应回填用）。
    async fn public_count(&self, id: Uuid) -> Result<i64, UseCaseError>;
}

/// 系列目录条目：含公开文章计数与总成员数。
#[derive(Debug, Clone, PartialEq)]
pub struct SeriesWithUsage {
    pub snapshot: domain::content::SeriesSnapshot,
    /// 系列内全部文章数（含草稿/私密/回收站——它们保留位置）。
    pub post_count: i64,
    /// 其中公开可见（published+public+未删除）的文章数。
    pub public_post_count: i64,
}

/// 系列删除的受控结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeriesDeleteOutcome {
    Deleted,
    StaleVersion,
    /// 仍被文章引用（任何可见性）：引用保护拒绝删除。
    Referenced {
        count: i64,
    },
    Gone,
}

/// 重排结果：集合不匹配表示调用方持有的目录已过期。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReorderOutcome {
    /// 成功；返回递增后的 series.version。
    Reordered {
        new_version: i64,
    },
    StaleSeriesVersion,
    /// 提交的文章集合与系列当前成员不一致（须重读目录再排）。
    MembershipMismatch,
    SeriesGone,
}

/// 系列成员视图（重排授权与管理目录共用：含文章概要与作者）。
#[derive(Debug, Clone, PartialEq)]
pub struct SeriesMember {
    pub post_id: Uuid,
    pub author_id: Uuid,
    pub slug: String,
    pub title: String,
    /// draft/published/archived（成员含草稿/私密——它们保留位置）。
    pub status: String,
    pub deleted: bool,
    pub visibility: String,
    pub series_order: i32,
}

#[async_trait]
pub trait SeriesRepository: Send + Sync {
    async fn insert(&self, snapshot: &domain::content::SeriesSnapshot) -> Result<(), UseCaseError>;
    async fn find_by_slug(
        &self,
        slug: &str,
    ) -> Result<Option<domain::content::SeriesSnapshot>, UseCaseError>;
    async fn list(&self) -> Result<Vec<SeriesWithUsage>, UseCaseError>;

    /// 条件更新（CAS）：name/描述/封面一次提交；命中返回新快照，未命中 None。
    async fn update(
        &self,
        id: Uuid,
        name: &str,
        description: Option<&str>,
        cover_media_id: Option<Uuid>,
        expected_version: i64,
    ) -> Result<Option<domain::content::SeriesSnapshot>, UseCaseError>;

    /// 条件删除：仍被任何文章引用（含草稿/私密/回收站）时拒绝。
    async fn delete(
        &self,
        id: Uuid,
        expected_version: i64,
    ) -> Result<SeriesDeleteOutcome, UseCaseError>;

    /// 文章设置系列前的存在性校验。
    async fn existing_id(&self, id: Uuid) -> Result<bool, UseCaseError>;

    /// 系列当前成员（按 series_order 升序；重排授权与管理目录展示共用）。
    async fn members_of(&self, series_id: Uuid) -> Result<Vec<SeriesMember>, UseCaseError>;

    /// 并发安全重排：系列行锁 + series.version 校验 + 成员行锁（按 id 序）
    /// + DEFERRED 位置唯一约束，更新全部成员顺序与 posts.version，
    /// 并递增 series.version。见 docs/database-design.md §4。
    async fn reorder(
        &self,
        series_id: Uuid,
        expected_series_version: i64,
        ordered_post_ids: &[Uuid],
    ) -> Result<ReorderOutcome, UseCaseError>;
}
