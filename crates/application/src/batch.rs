//! Bounded, versioned batch commands. Storage adapters commit the whole command atomically.
use crate::{
    error::UseCaseError,
    identity::{Actor, authorize_own_or_any},
};
use domain::{
    content::{Post, PostPatch},
    identity::UserId,
};
use std::collections::HashSet;
use time::OffsetDateTime;
use uuid::Uuid;

pub const MAX_BATCH_ITEMS: usize = 100;

#[derive(Debug, Clone, Copy)]
pub struct BatchItem {
    pub id: Uuid,
    pub expected_version: i64,
}

/// Only validated, nonempty, unique, versioned targets may reach a write port.
#[derive(Debug, Clone)]
pub struct BatchItems(Vec<BatchItem>);
impl BatchItems {
    pub fn new(items: Vec<BatchItem>) -> Result<Self, UseCaseError> {
        if items.is_empty() || items.len() > MAX_BATCH_ITEMS {
            return Err(UseCaseError::Invalid(format!(
                "批量操作必须包含 1–{MAX_BATCH_ITEMS} 项"
            )));
        }
        let mut ids = HashSet::new();
        for item in &items {
            if item.expected_version < 1 {
                return Err(UseCaseError::Invalid(
                    "每项 expected_version 必须为正整数".into(),
                ));
            }
            if !ids.insert(item.id) {
                return Err(UseCaseError::Invalid("批量操作不能包含重复 id".into()));
            }
        }
        Ok(Self(items))
    }
    pub fn items(&self) -> &[BatchItem] {
        &self.0
    }
    pub fn sorted_ids(&self) -> Vec<Uuid> {
        let mut ids: Vec<_> = self.0.iter().map(|item| item.id).collect();
        ids.sort_unstable();
        ids
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct BatchItemResult {
    pub id: Uuid,
    /// None only after permanent deletion.
    pub version: Option<i64>,
    pub changed: bool,
}
#[derive(Debug, Clone, serde::Serialize)]
pub struct BatchResult {
    /// Same order as the request, including unchanged targets.
    pub items: Vec<BatchItemResult>,
    pub affected: i64,
}

#[derive(Debug, Clone, Copy)]
pub enum BatchPostStatus {
    Draft,
    Published,
    Archived,
    Scheduled(OffsetDateTime),
}
#[derive(Debug, Clone, Copy)]
pub enum PostBatchAction {
    Trash,
    Restore,
    Purge,
    ChangeStatus(BatchPostStatus),
    ChangeCategory(Option<Uuid>),
}
impl PostBatchAction {
    pub fn name(self) -> &'static str {
        match self {
            Self::Trash => "trash",
            Self::Restore => "restore",
            Self::Purge => "purge",
            Self::ChangeStatus(_) => "change_status",
            Self::ChangeCategory(_) => "change_category",
        }
    }
    fn permissions(self) -> (&'static str, &'static str) {
        match self {
            Self::Trash | Self::Restore => ("post.delete", "post.delete_any"),
            Self::Purge => ("post.purge", "post.purge"),
            Self::ChangeCategory(_) => ("post.update", "post.update_any"),
            Self::ChangeStatus(BatchPostStatus::Published | BatchPostStatus::Scheduled(_)) => {
                ("post.publish", "post.publish_any")
            }
            Self::ChangeStatus(BatchPostStatus::Draft | BatchPostStatus::Archived) => {
                ("post.unpublish", "post.unpublish_any")
            }
        }
    }
    pub fn ensure_permission(self, actor: &Actor) -> Result<(), UseCaseError> {
        let (own, any) = self.permissions();
        if actor.has_permission(own) || actor.has_permission(any) {
            Ok(())
        } else {
            Err(UseCaseError::Forbidden)
        }
    }
    /// Called with the current, locked author fact by the adapter.
    pub fn authorize(self, actor: &Actor, author: Uuid) -> Result<(), UseCaseError> {
        let (own, any) = self.permissions();
        authorize_own_or_any(actor, own, any, UserId(author))
    }
    /// Apply existing aggregate behaviors; raw status/category writes cannot bypass them.
    pub fn apply(self, post: &mut Post, now: OffsetDateTime) -> Result<bool, UseCaseError> {
        let snapshot = post.snapshot();
        let invalid = |error: domain::content::PostError| UseCaseError::Invalid(error.to_string());
        match self {
            Self::Restore => {
                if snapshot.deleted_at.is_none() {
                    return Err(UseCaseError::NotFound(format!(
                        "回收站文章 {}",
                        snapshot.id
                    )));
                }
                Ok(post.restore())
            }
            Self::Purge => {
                if snapshot.deleted_at.is_none() {
                    return Err(UseCaseError::Invalid("只能永久删除回收站文章".into()));
                }
                Ok(true)
            }
            action => {
                if snapshot.deleted_at.is_some() {
                    return Err(UseCaseError::NotFound(format!("文章 {}", snapshot.id)));
                }
                match action {
                    Self::Trash => Ok(post.trash(now)),
                    Self::ChangeCategory(id) => post
                        .edit(PostPatch {
                            category_id: Some(id),
                            ..Default::default()
                        })
                        .map_err(invalid),
                    Self::ChangeStatus(BatchPostStatus::Draft) => Ok(post.withdraw()),
                    Self::ChangeStatus(BatchPostStatus::Published) => {
                        post.publish(now).map_err(invalid)
                    }
                    Self::ChangeStatus(BatchPostStatus::Archived) => {
                        post.archive().map_err(invalid)
                    }
                    Self::ChangeStatus(BatchPostStatus::Scheduled(at)) => {
                        post.schedule(at, now).map_err(invalid)
                    }
                    Self::Restore | Self::Purge => unreachable!(),
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum CommentBatchAction {
    Approve,
    Spam,
    Trash,
    Restore,
    Pending,
}
impl CommentBatchAction {
    pub fn name(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::Spam => "spam",
            Self::Trash => "trash",
            Self::Restore => "restore",
            Self::Pending => "pending",
        }
    }
    pub fn moderation(self) -> domain::comment::ModerationAction {
        use domain::comment::{CommentStatus, ModerationAction};
        ModerationAction::SetStatus(match self {
            Self::Approve => CommentStatus::Approved,
            Self::Spam => CommentStatus::Spam,
            Self::Trash => CommentStatus::Trash,
            Self::Restore | Self::Pending => CommentStatus::Pending,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn targets_require_unique_ids_positive_versions_and_a_bounded_nonempty_list() {
        let item = BatchItem {
            id: Uuid::now_v7(),
            expected_version: 1,
        };
        assert!(BatchItems::new(vec![]).is_err());
        assert!(BatchItems::new(vec![item, item]).is_err());
        assert!(
            BatchItems::new(vec![BatchItem {
                expected_version: 0,
                ..item
            }])
            .is_err()
        );
        let items: Vec<_> = (0..MAX_BATCH_ITEMS)
            .map(|_| BatchItem {
                id: Uuid::now_v7(),
                ..item
            })
            .collect();
        assert!(BatchItems::new(items.clone()).is_ok());
        assert!(BatchItems::new(items.into_iter().chain([item]).collect()).is_err());
    }
}
