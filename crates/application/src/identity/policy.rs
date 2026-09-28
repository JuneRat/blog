//! 身份写入的纯规则。适配器须在身份排他锁内取得当前事实，再调用这些规则。
//! 返回计划不授权未来的写入；锁、CAS、会话撤销与审计仍由同一事务负责。

use crate::error::UseCaseError;
use domain::identity::{PermissionSet, UserSnapshot, UserStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusChangePlan {
    Unchanged,
    Update { requires_owner_check: bool },
}

pub fn require_account_management(permissions: &PermissionSet) -> Result<(), UseCaseError> {
    if !permissions.has("user.manage") {
        return Err(UseCaseError::Forbidden);
    }
    Ok(())
}

/// 授权先于版本判断，版本判断先于幂等；无变化也不能绕过 Owner 权限或旧版本。
pub fn plan_status_change(
    permissions: &PermissionSet,
    current: &UserSnapshot,
    is_owner: bool,
    desired: UserStatus,
    expected_version: i64,
) -> Result<StatusChangePlan, UseCaseError> {
    require_account_management(permissions)?;
    if current.deleted_at.is_some() {
        return Err(UseCaseError::NotFound("用户".into()));
    }
    if is_owner && !permissions.has("ownership.manage") {
        return Err(UseCaseError::Forbidden);
    }
    if current.version != expected_version {
        return Err(UseCaseError::VersionConflict);
    }
    if current.status == desired {
        return Ok(StatusChangePlan::Unchanged);
    }
    Ok(StatusChangePlan::Update {
        requires_owner_check: is_owner && desired == UserStatus::Disabled,
    })
}

/// 总数包含本次将失去访问能力的 Owner；账号列表的提示也使用同一阈值。
pub fn has_other_loginable_owner(loginable_owners: i64) -> bool {
    loginable_owners > 1
}

/// 仅对确实持有 Owner 且可登录的目标调用；不能用分页中的 Owner 数量代替全局总数。
pub fn ensure_owner_removal_allowed(loginable_owners: i64) -> Result<(), UseCaseError> {
    if !has_other_loginable_owner(loginable_owners) {
        return Err(UseCaseError::LastOwnerProtected);
    }
    Ok(())
}
