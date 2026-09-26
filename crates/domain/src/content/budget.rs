//! Source and sanitized body budgets. Theme chrome has a separate allowance.
pub const MAX_SOURCE_BYTES: usize = 1024 * 1024;
pub const MAX_CONTENT_HTML_BYTES: usize = 768 * 1024;
pub const THEME_CHROME_BYTES: usize = 256 * 1024;
pub const MAX_PAGE_HTML_BYTES: usize = MAX_CONTENT_HTML_BYTES + THEME_CHROME_BYTES;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContentBudgetError {
    #[error("正文源文超过 {MAX_SOURCE_BYTES} 字节上限，请缩短正文")]
    SourceTooLarge,
    #[error("正文渲染后的 HTML 超过 {MAX_CONTENT_HTML_BYTES} 字节上限，请缩短正文或简化格式")]
    HtmlTooLarge,
}

pub fn validate_source(source: &str) -> Result<(), ContentBudgetError> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(ContentBudgetError::SourceTooLarge);
    }
    Ok(())
}

pub fn validate_html(html: &str) -> Result<(), ContentBudgetError> {
    if html.len() > MAX_CONTENT_HTML_BYTES {
        return Err(ContentBudgetError::HtmlTooLarge);
    }
    Ok(())
}
