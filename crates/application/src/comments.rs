pub use crate::ports::CommentRepository;
use crate::{error::UseCaseError, identity::Actor, ports::CommentRenderer};
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
    pub nickname: Option<String>,
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
impl CommentScope {
    /// Authorize against the current post owner. Write adapters must read this
    /// fact and keep it stable through commit; a pre-transaction lookup is not enough.
    pub fn authorize_post(self, author: Uuid) -> Result<(), UseCaseError> {
        if self.all || self.user_id == author {
            Ok(())
        } else {
            Err(UseCaseError::Forbidden)
        }
    }
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
                CommentNickname::new(cmd.nickname.as_deref().unwrap_or_default())
                    .map_err(|e| UseCaseError::Invalid(e.into()))?,
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn moderation_scope_checks_the_locked_post_owner() {
        let user = Uuid::now_v7();
        let other = Uuid::now_v7();
        let own = CommentScope {
            user_id: user,
            all: false,
            ip_address: None,
        };
        assert!(own.authorize_post(user).is_ok());
        assert!(matches!(
            own.authorize_post(other),
            Err(UseCaseError::Forbidden)
        ));
        assert!(
            CommentScope { all: true, ..own }
                .authorize_post(other)
                .is_ok()
        );
    }
}
