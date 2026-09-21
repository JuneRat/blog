//! 用例层统一错误。领域错误表达业务规则；适配器把技术错误映射为端口约定的错误。

#[derive(Debug, thiserror::Error)]
pub enum UseCaseError {
    #[error("未找到：{0}")]
    NotFound(String),

    /// 乐观并发冲突：expected_version 与当前记录不一致，不自动覆盖。
    #[error("版本冲突：内容已被并发修改，请基于最新版本重试")]
    VersionConflict,

    /// 数据库唯一约束兜底命中的冲突（slug、username 等）。
    #[error("{0} 已被占用")]
    Conflict(String),

    #[error("无权执行该操作")]
    Forbidden,

    #[error("{0}")]
    Invalid(String),

    #[error("存储错误：{0}")]
    Repository(String),

    #[error("渲染错误：{0}")]
    Render(String),
}
