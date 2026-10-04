//! 用户、资料和密码命令。

use super::fmt_error;
use application::identity::{Actor, CreateUserCmd, UserInteractor};
use application::password::PasswordInteractor;
use clap::Subcommand;
use std::io::Read;
use std::sync::Arc;

#[derive(Debug, Subcommand)]
pub enum UserAction {
    /// 创建用户
    Create {
        username: String,
        /// 展示名（默认同 username）
        #[arg(long)]
        display_name: Option<String>,
        /// 邮箱（可空）
        #[arg(long)]
        email: Option<String>,
    },
    /// 查看用户
    Show { username: String },
    /// 设置/重置本地密码（Argon2id）；这是密码重置的唯一入口
    Passwd {
        #[arg(long)]
        user: String,
        /// 从 stdin 读取密码；交互终端下缺省提示隐藏输入并二次确认
        #[arg(long)]
        password_stdin: bool,
        /// 清除密码（禁用密码登录）；已是最后一种登录方式时拒绝
        #[arg(long, conflicts_with = "password_stdin")]
        clear: bool,
    },
}

/// 用户命令仅依赖账号和密码用例。
pub struct UserCliDeps {
    pub users: Arc<UserInteractor>,
    pub passwords: Arc<PasswordInteractor>,
}

pub async fn run_user(deps: UserCliDeps, action: UserAction) -> Result<(), String> {
    match action {
        UserAction::Create {
            username,
            display_name,
            email,
        } => {
            let user = deps
                .users
                .create_user(
                    &Actor::bootstrap_cli(),
                    CreateUserCmd {
                        username,
                        email,
                        display_name,
                    },
                )
                .await
                .map_err(fmt_error)?;
            println!("已创建用户 {}（id={}）", user.username, user.id);
            Ok(())
        }
        UserAction::Show { username } => {
            let actor = deps
                .users
                .actor_for_username(&username)
                .await
                .map_err(fmt_error)?;
            let roles = deps
                .users
                .roles_of_user(actor.user_id.0)
                .await
                .map_err(fmt_error)?;
            let password_enabled = deps
                .passwords
                .password_enabled(&username)
                .await
                .map_err(fmt_error)?;
            println!("用户 {}（id={}）", username, actor.user_id.0);
            println!(
                "角色：{}",
                if roles.is_empty() {
                    "（无）".to_string()
                } else {
                    roles.join(", ")
                }
            );
            println!(
                "权限：{}",
                actor.permissions().keys().collect::<Vec<_>>().join(", ")
            );
            println!(
                "密码登录：{}",
                if password_enabled {
                    "已启用"
                } else {
                    "未启用"
                }
            );
            Ok(())
        }
        UserAction::Passwd {
            user,
            password_stdin,
            clear,
        } => {
            // 受控 CLI 以引导身份执行；密码重置是部署权限，不开放为公开入口。
            if clear {
                deps.passwords
                    .clear_password(&Actor::bootstrap_cli(), &user)
                    .await
                    .map_err(fmt_error)?;
                println!("已清除用户 {user} 的本地密码（密码登录已禁用），并撤销其全部会话。");
                return Ok(());
            }
            let password = read_new_password(password_stdin)?;
            deps.passwords
                .set_password(&Actor::bootstrap_cli(), &user, &password)
                .await
                .map_err(fmt_error)?;
            println!("已为用户 {user} 设置本地密码；其全部既有会话已撤销，请用新密码重新登录。");
            Ok(())
        }
    }
}

/// 读取新密码。
///
/// 有意**不提供** `--password` 参数：命令行参数会出现在进程表与 shell 历史里。
/// `--password-stdin`（或 stdin 非终端时）读取整段输入并只去掉一个行尾；
/// 交互终端下隐藏回显并二次确认。
fn read_new_password(password_stdin: bool) -> Result<String, String> {
    use std::io::IsTerminal as _;

    let password = if password_stdin || !std::io::stdin().is_terminal() {
        let mut buffer = String::new();
        std::io::stdin()
            .read_to_string(&mut buffer)
            .map_err(|e| format!("读取 stdin 失败：{e}"))?;
        strip_one_line_ending(&buffer).to_string()
    } else {
        let first =
            rpassword::prompt_password("新密码：").map_err(|e| format!("读取密码失败：{e}"))?;
        let second = rpassword::prompt_password("再次输入新密码：")
            .map_err(|e| format!("读取密码失败：{e}"))?;
        if first != second {
            return Err("两次输入的密码不一致".into());
        }
        first
    };
    if password.is_empty() {
        return Err("密码不能为空".into());
    }
    Ok(password)
}

/// 只去掉一个行尾（`\n` 或 `\r\n`）；其余字符原样保留（空格是合法密码字符）。
fn strip_one_line_ending(input: &str) -> &str {
    let trimmed = input.strip_suffix('\n').unwrap_or(input);
    trimmed.strip_suffix('\r').unwrap_or(trimmed)
}
