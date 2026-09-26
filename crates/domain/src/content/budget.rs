//! 正文源文大小规则；渲染输出预算由 application 定义。
pub const MAX_SOURCE_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContentBudgetError {
    #[error("正文源文超过 {MAX_SOURCE_BYTES} 字节上限，请缩短正文")]
    SourceTooLarge,
}

pub fn validate_source(source: &str) -> Result<(), ContentBudgetError> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(ContentBudgetError::SourceTooLarge);
    }
    Ok(())
}
