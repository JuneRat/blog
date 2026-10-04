//! 写用例共用的版本前提校验。
//!
//! Post（`content.rs`）与 Page（`page.rs`）都要在写入前确认调用方声明的
//! `expected_version`。这段逻辑曾经只写在 Post 里、Page 单独实现时漏掉，
//! 因此抽到一处，避免两套实现再次漂移。

use crate::error::UseCaseError;

/// 校验调用方声明的 `expected_version` 是否与读到的当前版本一致，返回应作为写入前提的版本。
///
/// 幂等操作（无字段变化、重复发布/撤回）不产生写入，但**同样要过这一关**：
/// 否则调用方会拿到「成功」，以为自己的旧版本已经生效，把中间发生的并发修改
/// 掩盖到下一次写入才爆发。`None` 表示「以读到的为准」，读后并发仍由 `save`
/// 的条件更新兜住。
pub fn checked_version(current: i64, requested: Option<i64>) -> Result<i64, UseCaseError> {
    if requested.is_some_and(|expected| expected != current) {
        return Err(UseCaseError::VersionConflict);
    }
    Ok(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_matching_or_absent_declared_version() {
        assert_eq!(checked_version(5, None).unwrap(), 5);
        assert_eq!(checked_version(5, Some(5)).unwrap(), 5);
    }

    #[test]
    fn rejects_stale_or_future_declared_version() {
        for requested in [Some(4), Some(6)] {
            assert!(
                matches!(
                    checked_version(5, requested),
                    Err(UseCaseError::VersionConflict)
                ),
                "{requested:?} 与当前版本 5 不一致时必须冲突"
            );
        }
    }
}
