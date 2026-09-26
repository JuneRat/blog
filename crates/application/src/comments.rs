use crate::{error::UseCaseError, identity::Actor};
use async_trait::async_trait;
pub use domain::comment::{CommentBody, CommentNickname, CommentStatus, ModerationAction};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Comment {
    pub id: Uuid,
    pub post_id: Uuid,
    pub post_slug: String,
    pub post_title: String,
    pub parent_id: Option<Uuid>,
    pub nickname: String,
    pub body: String,
    pub is_author: bool,
    pub status: String,
    pub version: i64,
    pub created_at: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommentPolicy {
    pub enabled: bool,
    pub version: i64,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct CommentPage {
    pub items: Vec<Comment>,
    pub total: i64,
    pub enabled: bool,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubmitComment {
    pub nickname: String,
    pub body: String,
    pub parent_id: Option<Uuid>,
    pub request_id: Uuid,
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
    pub request_id: Uuid,
}

#[derive(Clone, Copy)]
pub struct CommentScope {
    pub user_id: Uuid,
    pub all: bool,
}
#[async_trait]
pub trait CommentRepository: Send + Sync {
    async fn public_list(
        &self,
        slug: &str,
        parent: Option<Uuid>,
        page: i64,
    ) -> Result<CommentPage, UseCaseError>;
    async fn submit(&self, slug: &str, client: &str, cmd: NewComment) -> Result<(), UseCaseError>;
    async fn list(
        &self,
        scope: CommentScope,
        status: Option<CommentStatus>,
        post: Option<Uuid>,
        page: i64,
    ) -> Result<CommentPage, UseCaseError>;
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
}
impl CommentInteractor {
    pub fn new(repo: Arc<dyn CommentRepository>) -> Self {
        Self { repo }
    }
    pub async fn public_list(
        &self,
        slug: &str,
        parent: Option<Uuid>,
        page: i64,
    ) -> Result<CommentPage, UseCaseError> {
        self.repo
            .public_list(slug, parent, checked_page(page)?)
            .await
    }
    pub async fn submit(
        &self,
        slug: &str,
        actor: Option<&Actor>,
        client: &str,
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
        self.repo
            .submit(
                slug,
                client,
                NewComment {
                    author,
                    body,
                    parent_id: cmd.parent_id,
                    request_id: cmd.request_id,
                },
            )
            .await
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
    ) -> Result<(), UseCaseError> {
        actor.ensure_write_channel()?;
        if version < 1 {
            return Err(UseCaseError::Invalid("缺少评论版本".into()));
        }
        self.repo.moderate(scope(actor)?, id, version, action).await
    }
    pub async fn policy(
        &self,
        actor: &Actor,
        post: Option<Uuid>,
        update: Option<CommentPolicy>,
    ) -> Result<CommentPolicy, UseCaseError> {
        if update.is_some() {
            actor.ensure_write_channel()?;
        }
        let scope = if post.is_none() {
            if !actor.has_permission("settings.manage") {
                return Err(UseCaseError::Forbidden);
            }
            CommentScope {
                user_id: actor.user_id.0,
                all: true,
            }
        } else {
            scope(actor)?
        };
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
    })
}
fn checked_page(page: i64) -> Result<i64, UseCaseError> {
    if !(1..=100_000).contains(&page) {
        return Err(UseCaseError::Invalid("页码超出范围".into()));
    }
    Ok(page)
}
