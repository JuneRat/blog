use uuid::Uuid;
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum CommentModerationMode {
    All,
    Guests,
    FirstComment,
    None,
}
impl From<CommentModerationMode> for application::comments::ModerationMode {
    fn from(value: CommentModerationMode) -> Self {
        match value {
            CommentModerationMode::All => Self::All,
            CommentModerationMode::Guests => Self::Guests,
            CommentModerationMode::FirstComment => Self::FirstComment,
            CommentModerationMode::None => Self::None,
        }
    }
}
impl From<application::comments::ModerationMode> for CommentModerationMode {
    fn from(value: application::comments::ModerationMode) -> Self {
        match value {
            application::comments::ModerationMode::All => Self::All,
            application::comments::ModerationMode::Guests => Self::Guests,
            application::comments::ModerationMode::FirstComment => Self::FirstComment,
            application::comments::ModerationMode::None => Self::None,
        }
    }
}
#[derive(serde::Serialize, ts_rs::TS)]
pub struct CommentSubmissionResult {
    pub message: String,
    pub status: &'static str,
}

#[derive(serde::Serialize, ts_rs::TS, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommentPolicy {
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub moderation: Option<CommentModerationMode>,
    pub version: i64,
}
impl From<application::comments::CommentPolicy> for CommentPolicy {
    fn from(value: application::comments::CommentPolicy) -> Self {
        let application::comments::CommentPolicy {
            enabled,
            moderation,
            version,
        } = value;
        Self {
            enabled,
            moderation: moderation.map(Into::into),
            version,
        }
    }
}
impl From<CommentPolicy> for application::comments::CommentPolicy {
    fn from(value: CommentPolicy) -> Self {
        Self {
            enabled: value.enabled,
            moderation: value.moderation.map(Into::into),
            version: value.version,
        }
    }
}
#[derive(serde::Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields = nullable)]
pub struct SubmitCommentBody {
    pub nickname: Option<String>,
    pub body: String,
    pub parent_id: Option<Uuid>,
    pub email: Option<String>,
}
impl From<SubmitCommentBody> for application::comments::SubmitComment {
    fn from(value: SubmitCommentBody) -> Self {
        Self {
            nickname: value.nickname,
            body: value.body,
            parent_id: value.parent_id,
            email: value.email,
        }
    }
}
