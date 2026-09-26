//! Comments are plain text, independent of the post editing aggregate.
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
    matches!(status, "pending" | "approved" | "rejected" | "spam")
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
}
