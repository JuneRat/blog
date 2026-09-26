//! Comments are plain text, independent of the post editing aggregate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentStatus {
    Pending,
    Approved,
    Rejected,
    Spam,
}
impl CommentStatus {
    pub fn parse(value: &str) -> Result<Self, &'static str> {
        match value {
            "pending" => Ok(Self::Pending),
            "approved" => Ok(Self::Approved),
            "rejected" => Ok(Self::Rejected),
            "spam" => Ok(Self::Spam),
            _ => Err("未知审核状态"),
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Spam => "spam",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModerationAction {
    SetStatus(CommentStatus),
    DeletePermanently,
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
