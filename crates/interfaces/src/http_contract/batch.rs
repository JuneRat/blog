use application::batch as app;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const BATCH_BODY_LIMIT: usize = 16 * 1024;

#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct BatchItemInput {
    pub id: Uuid,
    pub expected_version: i64,
}
impl From<BatchItemInput> for app::BatchItem {
    fn from(item: BatchItemInput) -> Self {
        Self {
            id: item.id,
            expected_version: item.expected_version,
        }
    }
}

#[derive(Deserialize, ts_rs::TS)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum BatchPostStatusInput {
    Draft {},
    Published {},
    Archived {},
    Scheduled { published_at: String },
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct BatchPostCategoryInput {
    /// Explicit null clears the category; omission is invalid.
    #[serde(deserialize_with = "required_category")]
    pub category_id: Option<Uuid>,
}
fn required_category<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Uuid>, D::Error> {
    Option::<Uuid>::deserialize(d)
}

#[derive(Deserialize, ts_rs::TS)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum PostBatchInput {
    Trash {
        items: Vec<BatchItemInput>,
    },
    Restore {
        items: Vec<BatchItemInput>,
    },
    Purge {
        items: Vec<BatchItemInput>,
    },
    ChangeStatus {
        items: Vec<BatchItemInput>,
        params: BatchPostStatusInput,
    },
    ChangeCategory {
        items: Vec<BatchItemInput>,
        params: BatchPostCategoryInput,
    },
}
impl PostBatchInput {
    pub fn into_command(
        self,
    ) -> Result<(Vec<app::BatchItem>, app::PostBatchAction), application::UseCaseError> {
        use app::{BatchPostStatus, PostBatchAction};
        let (items, action) = match self {
            Self::Trash { items } => (items, PostBatchAction::Trash),
            Self::Restore { items } => (items, PostBatchAction::Restore),
            Self::Purge { items } => (items, PostBatchAction::Purge),
            Self::ChangeCategory { items, params } => {
                (items, PostBatchAction::ChangeCategory(params.category_id))
            }
            Self::ChangeStatus { items, params } => {
                let status = match params {
                    BatchPostStatusInput::Draft {} => BatchPostStatus::Draft,
                    BatchPostStatusInput::Published {} => BatchPostStatus::Published,
                    BatchPostStatusInput::Archived {} => BatchPostStatus::Archived,
                    BatchPostStatusInput::Scheduled { published_at } => BatchPostStatus::Scheduled(
                        time::OffsetDateTime::parse(
                            &published_at,
                            &time::format_description::well_known::Rfc3339,
                        )
                        .map_err(|_| {
                            application::UseCaseError::Invalid(
                                "published_at 须包含时区的 RFC3339 时间".into(),
                            )
                        })?,
                    ),
                };
                (items, PostBatchAction::ChangeStatus(status))
            }
        };
        Ok((items.into_iter().map(Into::into).collect(), action))
    }
}

#[derive(Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum CommentBatchActionInput {
    Approve,
    Spam,
    Trash,
    Restore,
    Pending,
}
impl From<CommentBatchActionInput> for app::CommentBatchAction {
    fn from(value: CommentBatchActionInput) -> Self {
        match value {
            CommentBatchActionInput::Approve => Self::Approve,
            CommentBatchActionInput::Spam => Self::Spam,
            CommentBatchActionInput::Trash => Self::Trash,
            CommentBatchActionInput::Restore => Self::Restore,
            CommentBatchActionInput::Pending => Self::Pending,
        }
    }
}
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct CommentBatchInput {
    pub items: Vec<BatchItemInput>,
    pub action: CommentBatchActionInput,
}
#[derive(Serialize, ts_rs::TS)]
pub struct BatchItemResult {
    pub id: Uuid,
    pub version: Option<i64>,
    pub changed: bool,
}
#[derive(Serialize, ts_rs::TS)]
pub struct BatchResult {
    pub items: Vec<BatchItemResult>,
    pub affected: i64,
}
impl From<app::BatchResult> for BatchResult {
    fn from(value: app::BatchResult) -> Self {
        Self {
            items: value
                .items
                .into_iter()
                .map(|item| BatchItemResult {
                    id: item.id,
                    version: item.version,
                    changed: item.changed,
                })
                .collect(),
            affected: value.affected,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn category_clear_requires_explicit_null_and_actions_reject_unrelated_parameters() {
        assert!(
            serde_json::from_str::<PostBatchInput>(
                r#"{"action":"change_category","items":[],"params":{}}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<PostBatchInput>(
                r#"{"action":"change_category","items":[],"params":{"category_id":null}}"#
            )
            .is_ok()
        );
        assert!(
            serde_json::from_str::<PostBatchInput>(
                r#"{"action":"trash","items":[],"params":{"category_id":null}}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<PostBatchInput>(
                r#"{"action":"change_status","items":[],"params":{"status":"scheduled"}}"#
            )
            .is_err()
        );
        assert!(serde_json::from_str::<PostBatchInput>(r#"{"action":"change_status","items":[],"params":{"status":"draft","published_at":"2027-01-01T00:00:00Z"}}"#).is_err());
    }
}
