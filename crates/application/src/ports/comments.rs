use crate::{
    comments::{
        CommentPage, CommentPolicy, CommentScope, CommentStatus, ModerationAction, NewComment,
        PublicCommentPage,
    },
    error::UseCaseError,
};
use async_trait::async_trait;
use std::net::IpAddr;
use uuid::Uuid;

#[async_trait]
pub trait CommentRepository: Send + Sync {
    /// Keep current post ownership and all comment versions stable through one atomic commit.
    /// Validate transitions even for noops; write one aggregate audit only if anything changes.
    async fn batch_moderate(
        &self,
        scope: CommentScope,
        items: &crate::batch::BatchItems,
        action: crate::batch::CommentBatchAction,
    ) -> Result<crate::batch::BatchResult, UseCaseError>;

    async fn public_list(
        &self,
        slug: &str,
        root: Option<Uuid>,
        page: i64,
    ) -> Result<PublicCommentPage, UseCaseError>;
    /// 业务提交端口：读取当前文章、开关、回复关系和账号事实，调用领域聚合创建，
    /// 并将源文、派生 HTML 和审计原子提交。事实须在提交前持续有效：关闭评论与
    /// 提交、隐藏父评论与回复必须串行生效。这是事务语义要求，不指定锁或数据库。
    /// 评论提交不改变文章版本。
    async fn submit(
        &self,
        slug: &str,
        client: Option<IpAddr>,
        cmd: NewComment,
    ) -> Result<CommentStatus, UseCaseError>;
    async fn list(
        &self,
        scope: CommentScope,
        status: Option<CommentStatus>,
        post: Option<Uuid>,
        page: i64,
    ) -> Result<CommentPage, UseCaseError>;
    /// 校验资源归属后重建领域聚合并审核；版本前提在无变化请求中也必须验证。
    /// 实际状态变化、评论版本递增、时间戳与审计须原子提交；无变化不写版本或审计。
    /// 源文、作者及回复关系保持不变。这些是事务语义要求，适配器可采用等价实现。
    async fn moderate(
        &self,
        scope: CommentScope,
        id: Uuid,
        version: i64,
        action: ModerationAction,
    ) -> Result<(), UseCaseError>;
    async fn policy(
        &self,
        scope: CommentScope,
        post: Option<Uuid>,
        update: Option<CommentPolicy>,
    ) -> Result<CommentPolicy, UseCaseError>;
}
