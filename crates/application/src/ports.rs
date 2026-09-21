//! 出站端口：由使用方（应用层）定义，由基础设施实现。
//!
//! M1 写用例均为单条件语句原子更新，事务即语句本身；
//! 多写用例出现时再引入工作单元抽象（见 docs/architecture.md §5）。

use async_trait::async_trait;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::UseCaseError;
use domain::content::post::PostSnapshot;
use domain::identity::UserSnapshot;

// ---------------------------------------------------------------------------
// 写侧端口
// ---------------------------------------------------------------------------

/// 条件保存的三态结果：
/// 区分「版本过期可重试」与「记录已消失/被删（重试无意义）」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveOutcome {
    /// 写入成功，携带数据库返回的递增后版本。
    Saved { new_version: i64 },
    /// expected_version 与当前记录不匹配；调用方应报并发冲突。
    StaleConflict,
    /// 记录不存在或已软删除。
    Gone,
}

#[async_trait]
pub trait PostRepository: Send + Sync {
    async fn find_by_slug(&self, slug: &str) -> Result<Option<PostSnapshot>, UseCaseError>;
    async fn find_by_id(&self, id: Uuid) -> Result<Option<PostSnapshot>, UseCaseError>;
    async fn list_by_author(&self, author_id: Uuid) -> Result<Vec<PostSnapshot>, UseCaseError>;
    async fn insert(&self, snapshot: &PostSnapshot) -> Result<(), UseCaseError>;

    /// 条件保存：`expected_version` 匹配当前记录时写入并 version+1。
    async fn save(
        &self,
        snapshot: &PostSnapshot,
        expected_version: i64,
        now: OffsetDateTime,
    ) -> Result<SaveOutcome, UseCaseError>;
}

#[async_trait]
pub trait UserRepository: Send + Sync {
    async fn insert(&self, snapshot: &UserSnapshot) -> Result<(), UseCaseError>;
    async fn find_by_id(&self, id: Uuid) -> Result<Option<UserSnapshot>, UseCaseError>;
    async fn find_by_username(&self, username: &str) -> Result<Option<UserSnapshot>, UseCaseError>;
}

// ---------------------------------------------------------------------------
// 读侧端口（轻量 CQRS：面向页面的公开只读查询）
// ---------------------------------------------------------------------------

/// 公开列表条目。
#[derive(Debug, Clone, serde::Serialize)]
pub struct PublicPostSummary {
    pub title: String,
    pub slug: String,
    pub excerpt: Option<String>,
    pub published_at: Option<OffsetDateTime>,
    pub author_display: String,
}

/// 公开详情（正文为 Markdown 源文，渲染交给出站端口）。
#[derive(Debug, Clone)]
pub struct PublicPostDetail {
    pub title: String,
    pub slug: String,
    pub excerpt: Option<String>,
    pub published_at: Option<OffsetDateTime>,
    pub updated_at: OffsetDateTime,
    pub author_display: String,
    pub author_username: String,
    pub content: String,
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
}

// ---------------------------------------------------------------------------
// 基础能力端口
// ---------------------------------------------------------------------------

pub trait Clock: Send + Sync {
    fn now(&self) -> OffsetDateTime;
}

/// readiness 探针：实现方执行最小健康动作（如 SELECT 1）。
/// None/未装配时调用方按“无依赖可检”处理。
#[async_trait]
pub trait HealthCheck: Send + Sync {
    async fn check(&self) -> bool;
}

/// Markdown → 清洗后 HTML。清洗规则由实现方（基础设施）负责。
pub trait ContentRenderer: Send + Sync {
    fn render_markdown(&self, source: &str) -> String;
}
