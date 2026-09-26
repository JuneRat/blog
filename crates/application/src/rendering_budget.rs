//! 渲染输出策略，由基础设施渲染器与持久化适配器执行。
pub const MAX_CONTENT_HTML_BYTES: usize = 768 * 1024;
pub const THEME_CHROME_BYTES: usize = 256 * 1024;
pub const MAX_PAGE_HTML_BYTES: usize = MAX_CONTENT_HTML_BYTES + THEME_CHROME_BYTES;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RenderingBudgetError {
    #[error("正文渲染后的 HTML 超过 {MAX_CONTENT_HTML_BYTES} 字节上限，请缩短正文或简化格式")]
    HtmlTooLarge,
}

pub fn validate_html(html: &str) -> Result<(), RenderingBudgetError> {
    if html.len() > MAX_CONTENT_HTML_BYTES {
        return Err(RenderingBudgetError::HtmlTooLarge);
    }
    Ok(())
}
