//! 单条评论聚合：源文、作者与回复关系创建后不可修改，审核只改变本节点状态。
//! 文章和父/根评论通过 ID 关联；提交端口负责在一致的事实下调用领域行为。
//! HTML、来源 IP、审计和持久化时间戳由适配器维护，不进入写聚合。
use crate::identity::{Email, UserId};
use uuid::Uuid;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CommentError {
    #[error("新评论已关闭")]
    Closed,
    #[error("父评论不可回复或回复关系无效")]
    InvalidReply,
    #[error("请先恢复到待审核，再通过审核")]
    RestoreBeforeApproval,
    #[error("评论版本冲突")]
    VersionConflict,
    #[error("评论快照无效：{0}")]
    InvalidSnapshot(&'static str),
}

/// 创建时已解析的作者。账号昵称来自当前账号，不能使用访客昵称或邮箱覆盖。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommentAuthor {
    Guest {
        nickname: CommentNickname,
        email: Option<Email>,
    },
    Account {
        user_id: UserId,
        nickname: CommentNickname,
    },
}

/// 提交端口读取的最小关系事实；不加载整棵评论树或父级正文。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommentReference {
    pub id: Uuid,
    pub post_id: Uuid,
    pub parent_id: Option<Uuid>,
    pub root_id: Option<Uuid>,
    pub status: CommentStatus,
}

#[derive(Debug, Clone, Copy)]
pub struct ReplyContext {
    pub parent: CommentReference,
    pub root: CommentReference,
}

/// 文章可访问性由提交端口保证；开关和关联事实须保持有效直到提交结束。
#[derive(Debug, Clone, Copy)]
pub struct CommentSubmission {
    pub post_id: Uuid,
    pub global_enabled: bool,
    pub post_enabled: bool,
    pub reply: Option<ReplyContext>,
}

impl CommentSubmission {
    pub fn ensure_open(&self) -> Result<(), CommentError> {
        if !self.global_enabled || !self.post_enabled {
            return Err(CommentError::Closed);
        }
        Ok(())
    }

    fn relation(&self) -> Result<(Option<Uuid>, Option<Uuid>), CommentError> {
        let Some(ReplyContext { parent, root }) = self.reply else {
            return Ok((None, None));
        };
        if parent.post_id != self.post_id
            || root.post_id != self.post_id
            || parent.status != CommentStatus::Approved
            || !valid_relation(parent.id, parent.parent_id, parent.root_id)
            || root.parent_id.is_some()
            || root.root_id.is_some()
            || parent.root_id.unwrap_or(parent.id) != root.id
        {
            return Err(CommentError::InvalidReply);
        }
        // 根节点可处于隐藏状态：已通过的后代仍可被回复。
        Ok((Some(parent.id), Some(root.id)))
    }
}

/// 持久化重建入口；源文和昵称按原样校验，审核不会隐式规范化旧数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentSnapshot {
    pub id: Uuid,
    pub post_id: Uuid,
    pub parent_id: Option<Uuid>,
    pub root_id: Option<Uuid>,
    pub user_id: Option<Uuid>,
    pub nickname: String,
    pub email: Option<String>,
    pub body: String,
    pub status: CommentStatus,
    pub version: i64,
}

#[derive(Debug, Clone)]
pub struct Comment {
    snapshot: CommentSnapshot,
}

impl Comment {
    pub fn submit(
        context: CommentSubmission,
        author: CommentAuthor,
        body: CommentBody,
    ) -> Result<Self, CommentError> {
        context.ensure_open()?;
        let (parent_id, root_id) = context.relation()?;
        let (user_id, nickname, email) = match author {
            CommentAuthor::Guest { nickname, email } => {
                (None, nickname, email.map(|email| email.as_str().to_owned()))
            }
            CommentAuthor::Account { user_id, nickname } => (Some(user_id.0), nickname, None),
        };
        Self::reconstitute(CommentSnapshot {
            id: Uuid::now_v7(),
            post_id: context.post_id,
            parent_id,
            root_id,
            user_id,
            nickname: nickname.as_str().to_owned(),
            email,
            body: body.as_str().to_owned(),
            status: CommentStatus::Pending,
            version: 1,
        })
    }

    pub fn reconstitute(snapshot: CommentSnapshot) -> Result<Self, CommentError> {
        validate_text(&snapshot.nickname, &snapshot.body).map_err(CommentError::InvalidSnapshot)?;
        if snapshot.version < 1 {
            return Err(CommentError::InvalidSnapshot("版本必须为正整数"));
        }
        if !valid_relation(snapshot.id, snapshot.parent_id, snapshot.root_id) {
            return Err(CommentError::InvalidSnapshot("回复关系结构无效"));
        }
        if snapshot.user_id.is_some() && snapshot.email.is_some() {
            return Err(CommentError::InvalidSnapshot("账号评论不能附带访客邮箱"));
        }
        if let Some(email) = &snapshot.email {
            Email::new(email).map_err(|_| CommentError::InvalidSnapshot("邮箱无效"))?;
        }
        Ok(Self { snapshot })
    }

    pub fn snapshot(&self) -> CommentSnapshot {
        self.snapshot.clone()
    }

    pub fn reference(&self) -> CommentReference {
        CommentReference {
            id: self.snapshot.id,
            post_id: self.snapshot.post_id,
            parent_id: self.snapshot.parent_id,
            root_id: self.snapshot.root_id,
            status: self.snapshot.status,
        }
    }

    pub fn status(&self) -> CommentStatus {
        self.snapshot.status
    }

    /// 先检查版本，即使目标状态相同也不能接受过期请求。
    /// 返回是否变化；持久化端口仅在实际变化时递增版本并更新审计/时间戳。
    pub fn moderate(
        &mut self,
        expected_version: i64,
        action: ModerationAction,
    ) -> Result<bool, CommentError> {
        if expected_version != self.snapshot.version {
            return Err(CommentError::VersionConflict);
        }
        let ModerationAction::SetStatus(next) = action;
        if !self.snapshot.status.may_transition_to(next) {
            return Err(CommentError::RestoreBeforeApproval);
        }
        let changed = self.snapshot.status != next;
        self.snapshot.status = next;
        Ok(changed)
    }
}

fn valid_relation(id: Uuid, parent: Option<Uuid>, root: Option<Uuid>) -> bool {
    parent.is_some() == root.is_some() && parent != Some(id) && root != Some(id)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentStatus {
    Pending,
    Approved,
    Trash,
    Spam,
}
impl CommentStatus {
    pub fn parse(value: &str) -> Result<Self, &'static str> {
        match value {
            "pending" => Ok(Self::Pending),
            "approved" => Ok(Self::Approved),
            "trash" => Ok(Self::Trash),
            "spam" => Ok(Self::Spam),
            _ => Err("未知审核状态"),
        }
    }
    fn may_transition_to(self, next: Self) -> bool {
        !matches!(self, Self::Spam | Self::Trash) || next != Self::Approved
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Trash => "trash",
            Self::Spam => "spam",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModerationAction {
    SetStatus(CommentStatus),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentBody(String);
impl CommentBody {
    pub fn new(raw: &str) -> Result<Self, &'static str> {
        let body = raw.replace("\r\n", "\n").trim().to_owned();
        validate_body(&body)?;
        Ok(Self(body))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentNickname(String);
impl CommentNickname {
    pub fn new(raw: &str) -> Result<Self, &'static str> {
        let nickname = raw.trim();
        validate_nickname(nickname)?;
        Ok(Self(nickname.to_owned()))
    }
    /// Account labels are display text: strip controls, trim and truncate before
    /// passing through the same constructor as guest nicknames. Blank labels
    /// fall back to the validated username.
    pub fn from_account(display_name: Option<&str>, username: &str) -> Result<Self, &'static str> {
        let label: String = display_name
            .unwrap_or(username)
            .chars()
            .filter(|c| !c.is_control())
            .collect();
        let label = label.trim();
        let label = if label.is_empty() { username } else { label };
        Self::new(&label.chars().take(64).collect::<String>())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub fn validate_text(nickname: &str, body: &str) -> Result<(), &'static str> {
    validate_nickname(nickname)?;
    validate_body(body)
}
pub fn validate_nickname(nickname: &str) -> Result<(), &'static str> {
    if nickname.trim().is_empty() || nickname.chars().count() > 64 {
        return Err("昵称须为 1–64 字");
    }
    if nickname.chars().any(char::is_control) {
        return Err("评论含不支持的控制字符");
    }
    Ok(())
}
pub fn validate_body(body: &str) -> Result<(), &'static str> {
    if body.trim().is_empty() || body.chars().count() > 2000 {
        return Err("评论须为 1–2,000 字");
    }
    if body
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err("评论含不支持的控制字符");
    }
    Ok(())
}
pub fn valid_status(status: &str) -> bool {
    CommentStatus::parse(status).is_ok()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn text_limits_count_unicode_and_preserve_plain_text() {
        assert!(validate_text("游客", &"字".repeat(2000)).is_ok());
        assert!(validate_text("游客", &"字".repeat(2001)).is_err());
        assert!(validate_text(" ", "正文").is_err());
        assert!(validate_text("游客", "\n ").is_err());
        assert!(validate_text("<script>", "<img onerror=x>\n下一行").is_ok());
        assert!(validate_text("游客\n管理员", "正文").is_err());
    }

    #[test]
    fn validated_text_and_account_labels_share_nickname_rules() {
        assert_eq!(CommentBody::new(" a\r\nb ").unwrap().as_str(), "a\nb");
        assert!(CommentBody::new("a\u{0}b").is_err());
        assert!(CommentNickname::new("a\nb").is_err());
        let name = CommentNickname::from_account(Some("  Alice\nAdmin\u{7}  "), "alice").unwrap();
        assert_eq!(name.as_str(), "AliceAdmin");
        assert_eq!(
            CommentNickname::from_account(Some("\n\u{7}"), "alice")
                .unwrap()
                .as_str(),
            "alice"
        );
        assert_eq!(
            CommentNickname::from_account(Some(&"字".repeat(65)), "alice")
                .unwrap()
                .as_str()
                .chars()
                .count(),
            64
        );
        assert!(CommentStatus::parse("deleted").is_err());
    }
}
