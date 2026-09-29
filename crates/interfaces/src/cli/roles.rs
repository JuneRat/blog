//! 角色与授权命令。

use super::fmt_error;
use application::identity::{Actor, RoleInteractor};
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum RoleAction {
    /// 同步权限目录与内置角色（幂等；启动时自动执行）
    Sync,
    /// 列出全部角色
    List,
    /// 为用户分配角色
    Assign {
        #[arg(long)]
        user: String,
        #[arg(long)]
        role: String,
    },
    /// 移除用户的角色（最后一个有效 Admin 会被拒绝）
    Remove {
        #[arg(long)]
        user: String,
        #[arg(long)]
        role: String,
    },
}

pub async fn run_role(roles: &RoleInteractor, action: RoleAction) -> Result<(), String> {
    match action {
        RoleAction::Sync => {
            roles.sync_registry().await.map_err(fmt_error)?;
            println!("权限目录与内置角色已同步。");
            Ok(())
        }
        RoleAction::List => {
            let roles = roles
                .list(&Actor::bootstrap_cli())
                .await
                .map_err(fmt_error)?;
            println!("{:<10} {:<16} {:<8} 权限数", "slug", "名称", "内置");
            for role in roles {
                println!(
                    "{:<10} {:<16} {:<8} {}",
                    role.slug,
                    role.name,
                    if role.builtin { "是" } else { "否" },
                    role.permission_count
                );
            }
            Ok(())
        }
        RoleAction::Assign { user, role } => {
            roles
                .assign_to_username(&Actor::bootstrap_cli(), &user, &role)
                .await
                .map_err(fmt_error)?;
            println!("已将角色 {role} 分配给 {user}。");
            Ok(())
        }
        RoleAction::Remove { user, role } => {
            roles
                .remove_from_username(&Actor::bootstrap_cli(), &user, &role)
                .await
                .map_err(fmt_error)?;
            println!("已移除 {user} 的角色 {role}。");
            Ok(())
        }
    }
}
