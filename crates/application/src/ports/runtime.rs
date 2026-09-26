//! 时钟、健康检查与通用条件写结果。

use async_trait::async_trait;
use time::OffsetDateTime;

/// 条件保存的三态结果：
/// 区分「版本过期可重试」与「记录已消失/被删（重试无意义）」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveOutcome {
    /// 写入成功，携带数据库返回的递增后版本。
    Saved { new_version: i64 },
    /// expected_version 与当前记录不匹配；调用方应报并发冲突。
    StaleConflict,
    /// 记录不存在或已软删除。
    Gone,
}

pub trait Clock: Send + Sync {
    fn now(&self) -> OffsetDateTime;
}

/// readiness 探针：实现方执行最小健康动作（如 SELECT 1）。
/// None/未装配时调用方按“无依赖可检”处理。
#[async_trait]
pub trait HealthCheck: Send + Sync {
    async fn check(&self) -> bool;
}
