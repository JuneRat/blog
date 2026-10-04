use crate::content_queries::{AdminPageSummary, AdminPostSummary, PageListFilter, PostListFilter};
use crate::error::UseCaseError;
use async_trait::async_trait;
use uuid::Uuid;

/// 普通列表按 updated_at DESC, id DESC；回收站按 deleted_at DESC, id DESC。
/// 筛选后的总数与当前页必须属于同一数据库快照，禁止读取正文或逐条补查询。
#[async_trait]
pub trait AdminPostQuery: Send + Sync {
    async fn list(
        &self,
        author_id: Option<Uuid>,
        filter: &PostListFilter,
    ) -> Result<(Vec<AdminPostSummary>, i64), UseCaseError>;
}

/// 页面是站点级资源，没有作者维度；其余分页约定与 AdminPostQuery 一致。
#[async_trait]
pub trait AdminPageQuery: Send + Sync {
    async fn list(
        &self,
        filter: &PageListFilter,
    ) -> Result<(Vec<AdminPageSummary>, i64), UseCaseError>;
}
