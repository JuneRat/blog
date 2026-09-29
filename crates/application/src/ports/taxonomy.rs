//! 标签、分类与系列目录端口。

use async_trait::async_trait;
use uuid::Uuid;

use crate::error::UseCaseError;

/// 标签目录条目：含公开文章计数（与公开标签页同一可见性口径）。
///
/// 不暴露非公开文章计数，避免草稿/私密的关联规模泄漏给目录读取者。
#[derive(Debug, Clone, PartialEq)]
pub struct TagWithUsage {
    pub snapshot: domain::content::TagSnapshot,
    /// 已发布、公开、未删除且发布时间已到的关联文章数。
    pub public_post_count: i64,
}

/// 标签删除的三态结果（版本冲突与不存在由用例翻译）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagDeleteOutcome {
    Deleted,
    /// expected_version 不匹配：可基于最新版本重试。
    StaleVersion,
    Gone,
}

#[async_trait]
pub trait TagRepository: Send + Sync {
    /// 创建接收已校验的聚合；快照仅用于读取、重建与返回结果。
    async fn insert(
        &self,
        aggregate: &domain::content::Tag,
        actor_id: crate::audit::AuditContext,
    ) -> Result<(), UseCaseError>;
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
        actor_id: crate::audit::AuditContext,
    ) -> Result<Option<domain::content::TagSnapshot>, UseCaseError>;

    /// 条件删除标签并解除关联；保留文章并递增受影响文章版本。
    async fn delete(
        &self,
        id: Uuid,
        expected_version: i64,
        actor_id: crate::audit::AuditContext,
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
    /// 已发布、公开、未删除且发布时间已到的直接归属文章数。
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
    /// 创建接收已校验的聚合；快照仅用于读取、重建与返回结果。
    async fn insert(
        &self,
        aggregate: &domain::content::Category,
        audit_actor: crate::audit::AuditContext,
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
        audit_actor: crate::audit::AuditContext,
    ) -> Result<Option<domain::content::CategorySnapshot>, UseCaseError>;

    /// 条件删除：树锁内先检查文章引用与子分类，再按版本条件删除。
    async fn delete(
        &self,
        id: Uuid,
        expected_version: i64,
        audit_actor: crate::audit::AuditContext,
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
    /// 其中公开可见（published+public+未删除+发布时间已到）的文章数。
    pub public_post_count: i64,
}

/// 系列删除的受控结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeriesDeleteOutcome {
    Deleted,
    StaleVersion,
    Gone,
}

/// 重排结果：集合不匹配表示调用方持有的目录已过期。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReorderOutcome {
    /// 成功；返回 series.version，无实际变化时保持原版本。
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
    /// draft/scheduled/published/archived（成员含草稿/私密/回收站）。
    pub status: String,
    pub deleted: bool,
    pub visibility: String,
    pub position: i32,
}

#[async_trait]
pub trait SeriesRepository: Send + Sync {
    /// 创建接收已校验的聚合；快照仅用于读取、重建与返回结果。
    async fn insert(
        &self,
        aggregate: &domain::content::Series,
        actor_id: crate::audit::AuditContext,
    ) -> Result<(), UseCaseError>;
    async fn find_by_slug(
        &self,
        slug: &str,
    ) -> Result<Option<domain::content::SeriesSnapshot>, UseCaseError>;
    async fn list(&self) -> Result<Vec<SeriesWithUsage>, UseCaseError>;

    /// 条件更新（CAS）：name/描述/封面一次提交；返回本事务的快照与成员计数。
    /// 无实际变化时保持版本与审计不变，仍校验版本；未命中返回 None。
    async fn update(
        &self,
        id: Uuid,
        name: &str,
        description: Option<&str>,
        cover_media_id: Option<Uuid>,
        expected_version: i64,
        actor_id: crate::audit::AuditContext,
    ) -> Result<Option<SeriesWithUsage>, UseCaseError>;

    /// 条件删除系列并解除关联；保留文章并递增受影响文章版本。
    async fn delete(
        &self,
        id: Uuid,
        expected_version: i64,
        actor_id: crate::audit::AuditContext,
    ) -> Result<SeriesDeleteOutcome, UseCaseError>;

    /// 返回所给 id 中存在的系列，去重并按 id 排序；文章关联使用批量校验。
    async fn existing_ids(&self, ids: &[Uuid]) -> Result<Vec<Uuid>, UseCaseError>;

    /// 系列当前成员（按 position、post_id 升序；重排授权与目录展示共用）。
    async fn members_of(&self, series_id: Uuid) -> Result<Vec<SeriesMember>, UseCaseError>;

    /// 原子校验系列版本及完整成员集合后重排；并发成员变更不能造成遗漏或覆盖。
    /// 依次写入 0..n-1 权重；实际变化时递增系列及权重变化的文章版本，连同审计
    /// 一起提交。版本/成员校验失败时不得部分更新。这是事务语义，不指定锁机制。
    async fn reorder(
        &self,
        series_id: Uuid,
        expected_series_version: i64,
        ordered_post_ids: &[Uuid],
        actor_id: crate::audit::AuditContext,
    ) -> Result<ReorderOutcome, UseCaseError>;
}
