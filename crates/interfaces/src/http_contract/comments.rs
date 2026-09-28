use uuid::Uuid;
#[derive(serde::Serialize, ts_rs::TS, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommentPolicy {
    pub enabled: bool,
    pub version: i64,
}
impl From<application::comments::CommentPolicy> for CommentPolicy {
    fn from(value: application::comments::CommentPolicy) -> Self {
        let application::comments::CommentPolicy { enabled, version } = value;
        Self { enabled, version }
    }
}
impl From<CommentPolicy> for application::comments::CommentPolicy {
    fn from(value: CommentPolicy) -> Self {
        Self {
            enabled: value.enabled,
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
