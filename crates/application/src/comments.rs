use crate::{error::UseCaseError, identity::Actor, ports::CommentRenderer};
use async_trait::async_trait;
pub use domain::comment::{CommentBody, CommentNickname, CommentStatus, ModerationAction};
use serde::{Deserialize, Serialize};
use std::{net::IpAddr, sync::Arc};
use uuid::Uuid;

/// 后台查询投影；写入规则由 domain::comment::Comment 聚合维护。
#[derive(Debug, Clone)]
pub struct CommentDto {
    pub id: Uuid,
    pub post_id: Uuid,
    pub post_slug: String,
    pub post_title: String,
    pub parent_id: Option<Uuid>,
    pub root_id: Option<Uuid>,
    pub parent_nickname: Option<String>,
    pub author_email: Option<String>,
    pub ip_address: Option<String>,
    pub content_html: String,
    pub nickname: String,
    pub body: String,
    pub is_author: bool,
    pub status: CommentStatus,
    pub version: i64,
    pub created_at: String,
}
/// Public responses cannot contain source text, contact details or moderation metadata.
#[derive(Debug, Serialize, Deserialize)]
pub struct PublicComment {
    pub id: Uuid,
    pub parent_id: Option<Uuid>,
    pub root_id: Option<Uuid>,
    pub parent_nickname: Option<String>,
    pub nickname: String,
    pub content_html: String,
    pub is_author: bool,
    pub placeholder: bool,
    pub deleted: bool,
    pub created_at: String,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct PublicCommentPage {
    pub items: Vec<PublicComment>,
    pub total: i64,
    pub enabled: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommentPolicy {
    pub enabled: bool,
    pub version: i64,
}
#[derive(Debug)]
pub struct CommentPage {
    pub items: Vec<CommentDto>,
    pub total: i64,
    pub enabled: bool,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubmitComment {
    pub nickname: String,
    pub body: String,
    pub parent_id: Option<Uuid>,
    pub email: Option<String>,
}
/// Validated write command; raw HTTP input cannot reach a repository.
#[derive(Debug, Clone)]
pub enum CommentAuthor {
    Guest(CommentNickname),
    Account(Uuid),
}
#[derive(Debug, Clone)]
pub struct NewComment {
    pub author: CommentAuthor,
    pub body: CommentBody,
    pub parent_id: Option<Uuid>,
    pub email: Option<domain::identity::Email>,
}

#[derive(Clone, Copy)]
pub struct CommentScope {
    pub user_id: Uuid,
    pub all: bool,
    pub ip_address: Option<IpAddr>,
}
#[async_trait]
pub trait CommentRepository: Send + Sync {
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
    ) -> Result<(), UseCaseError>;
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
pub struct CommentInteractor {
    repo: Arc<dyn CommentRepository>,
    renderer: Arc<dyn CommentRenderer>,
}
impl CommentInteractor {
    pub fn new(repo: Arc<dyn CommentRepository>, renderer: Arc<dyn CommentRenderer>) -> Self {
        Self { repo, renderer }
    }
    pub async fn public_list(
        &self,
        slug: &str,
        root: Option<Uuid>,
        page: i64,
    ) -> Result<PublicCommentPage, UseCaseError> {
        self.repo.public_list(slug, root, checked_page(page)?).await
    }
    pub async fn submit(
        &self,
        slug: &str,
        actor: Option<&Actor>,
        client: Option<IpAddr>,
        cmd: SubmitComment,
    ) -> Result<(), UseCaseError> {
        if let Some(actor) = actor {
            actor.ensure_write_channel()?;
        }
        let author = match actor {
            Some(actor) => CommentAuthor::Account(actor.user_id.0),
            None => CommentAuthor::Guest(
                CommentNickname::new(&cmd.nickname).map_err(|e| UseCaseError::Invalid(e.into()))?,
            ),
        };
        let body = CommentBody::new(&cmd.body).map_err(|e| UseCaseError::Invalid(e.into()))?;
        let email = if actor.is_none() {
            cmd.email
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .map(domain::identity::Email::new)
                .transpose()
                .map_err(|e| UseCaseError::Invalid(e.to_string()))?
        } else {
            None
        };
        self.repo
            .submit(
                slug,
                client,
                NewComment {
                    author,
                    body,
                    parent_id: cmd.parent_id,
                    email,
                },
            )
            .await
    }

    pub async fn preview(&self, source: &str) -> Result<String, UseCaseError> {
        let body = CommentBody::new(source).map_err(|e| UseCaseError::Invalid(e.into()))?;
        self.renderer.render_comment(body.as_str()).await
    }

    pub async fn list(
        &self,
        actor: &Actor,
        status: Option<&str>,
        post: Option<Uuid>,
        page: i64,
    ) -> Result<CommentPage, UseCaseError> {
        let status = status
            .map(CommentStatus::parse)
            .transpose()
            .map_err(|e| UseCaseError::Invalid(e.into()))?;
        self.repo
            .list(scope(actor)?, status, post, checked_page(page)?)
            .await
    }
    pub async fn moderate(
        &self,
        actor: &Actor,
        id: Uuid,
        version: i64,
        action: ModerationAction,
        ip_address: Option<IpAddr>,
    ) -> Result<(), UseCaseError> {
        actor.ensure_write_channel()?;
        if version < 1 {
            return Err(UseCaseError::Invalid("缺少评论版本".into()));
        }
        let mut scope = scope(actor)?;
        scope.ip_address = ip_address;
        self.repo.moderate(scope, id, version, action).await
    }
    pub async fn policy(
        &self,
        actor: &Actor,
        post: Option<Uuid>,
        update: Option<CommentPolicy>,
        ip_address: Option<IpAddr>,
    ) -> Result<CommentPolicy, UseCaseError> {
        if update.is_some() {
            actor.ensure_write_channel()?;
        }
        let mut scope = if post.is_none() {
            if !actor.has_permission("settings.manage") {
                return Err(UseCaseError::Forbidden);
            }
            CommentScope {
                user_id: actor.user_id.0,
                all: true,
                ip_address,
            }
        } else {
            scope(actor)?
        };
        scope.ip_address = ip_address;
        self.repo.policy(scope, post, update).await
    }
}
fn scope(actor: &Actor) -> Result<CommentScope, UseCaseError> {
    let all = actor.has_permission("post.update_any");
    if !all && !actor.has_permission("post.update") {
        return Err(UseCaseError::Forbidden);
    }
    Ok(CommentScope {
        user_id: actor.user_id.0,
        all,
        ip_address: None,
    })
}
fn checked_page(page: i64) -> Result<i64, UseCaseError> {
    if !(1..=100_000).contains(&page) {
        return Err(UseCaseError::Invalid("页码超出范围".into()));
    }
    Ok(page)
}
