//! 受控 CLI 入站适配器；与管理 HTTP 共用应用用例及权限规则。
//!
//! 参数解析与输入/输出映射在本层完成；业务规则全部下沉应用层。
//! `migrate` 由 server 处理；HTML 维护由本层调用应用用例并呈现结果。

mod maintenance;
mod media;
mod oauth;
mod posts;
mod roles;
mod users;

pub use maintenance::{run_html_rebuild, run_maintenance, run_publish_due};
pub use media::{MediaAction, MediaPurgeAction, run_media, run_media_purge};
pub use oauth::{OauthAction, run_oauth};
pub use posts::{PostAction, PostCliDeps, run_post};
pub use roles::{RoleAction, run_role};
pub use users::{UserAction, UserCliDeps, run_user};

use application::error::UseCaseError;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "blog",
    version,
    about = "博客受控 CLI 与公开 SSR 服务入口",
    after_help = "不指定子命令时默认启动 serve，监听地址读取 BLOG_BIND 或部署配置。"
)]
pub struct Cli {
    /// 部署配置文件（优先于 BLOG_CONFIG_FILE，默认 config.toml）
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// 供装配入口使用的参数解析（server 不直接依赖 clap）。
pub fn parse_args() -> Cli {
    use clap::Parser as _;
    Cli::parse()
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// 检查部署配置或查看脱敏值与来源
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// 使用 BLOG_MAINTENANCE_DATABASE_URL 清理过期评论 IP 和审计，不运行迁移/发布任务
    Maintenance {
        #[arg(long, default_value_t = 1000)]
        batch_size: i64,
        #[arg(long, default_value_t = 100)]
        max_batches: u32,
        /// 只统计过期数据，不执行清理
        #[arg(long)]
        dry_run: bool,
    },
    /// 发布到期的预约文章与页面。
    PublishDue,
    /// 执行数据库结构迁移，不重建 HTML（由 server 装配层直接处理）
    Migrate,

    /// 显式重建旧渲染版本的文章、页面与评论 HTML；不改变编辑版本或业务时间
    RebuildHtml {
        #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(i64).range(1..=1000))]
        batch_size: i64,
        /// 三类内容共用的单次批次数上限
        #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u32).range(1..=1000))]
        max_batches: u32,
        /// 只读校验结构并统计待重建数量，不迁移、不渲染、不写入
        #[arg(long)]
        dry_run: bool,
    },

    /// 用户管理（受控操作，不开放自助注册）
    User {
        #[command(subcommand)]
        action: UserAction,
    },

    /// 文章管理（写通道需相应权限）
    Post {
        #[command(subcommand)]
        action: PostAction,
    },

    /// 角色与授权管理（受控 CLI；结构性保护始终生效）
    Role {
        #[command(subcommand)]
        action: RoleAction,
    },

    /// OAuth 提供商与外部身份绑定（受控 CLI）
    Oauth {
        #[command(subcommand)]
        action: OauthAction,
    },

    /// 媒体库维护（受控 CLI；回收流程需要 media.delete_any）
    Media {
        #[command(subcommand)]
        action: MediaAction,
    },

    /// 启动公开 SSR 服务
    Serve {
        /// 监听地址，如 127.0.0.1:8080
        #[arg(long)]
        addr: Option<String>,
    },
}

impl Default for Command {
    fn default() -> Self {
        Self::Serve { addr: None }
    }
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum ConfigScope {
    Serve,
    Database,
    Maintenance,
    Media,
    Resources,
    All,
}

#[derive(Debug, Subcommand)]
pub enum ConfigAction {
    /// 只校验配置，不连接数据库、不写入文件
    Check {
        #[arg(long = "for", value_enum, default_value = "serve")]
        scope: ConfigScope,
    },
    /// 输出脱敏 JSON；运行期设置仍以数据库为准
    Show {
        #[arg(long)]
        sources: bool,
        #[arg(long = "for", value_enum, default_value = "serve")]
        scope: ConfigScope,
    },
}
fn fmt_error(e: UseCaseError) -> String {
    e.to_string()
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn omitted_command_starts_serve_with_configured_address() {
        for args in [vec!["blog"], vec!["blog", "--config", "local.toml"]] {
            let cli = Cli::try_parse_from(&args).unwrap();
            assert_eq!(
                cli.config,
                (args.len() > 1).then(|| PathBuf::from("local.toml"))
            );
            assert!(matches!(
                cli.command.unwrap_or_default(),
                Command::Serve { addr: None }
            ));
        }
    }

    #[test]
    fn explicit_commands_and_address_keep_their_meaning() {
        let cli = Cli::try_parse_from(["blog", "serve", "--addr", "127.0.0.1:3000"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Serve { addr: Some(addr) }) if addr == "127.0.0.1:3000"
        ));
        let cli = Cli::try_parse_from(["blog", "migrate"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Migrate)));
    }

    #[test]
    fn help_and_invalid_commands_do_not_fall_back_to_serve() {
        assert_eq!(
            Cli::try_parse_from(["blog", "--help"]).unwrap_err().kind(),
            clap::error::ErrorKind::DisplayHelp
        );
        for args in [vec!["blog", "serv"], vec!["blog", "user"]] {
            assert!(Cli::try_parse_from(args).is_err());
        }
    }

    #[test]
    fn existing_post_commands_require_uuid_identity() {
        let id = uuid::Uuid::now_v7();
        for name in ["edit", "publish", "withdraw", "show"] {
            let cli = Cli::try_parse_from(["blog", "post", name, "--id", &id.to_string()])
                .expect("UUID command should parse");
            let Some(Command::Post { action }) = cli.command else {
                panic!("expected post command");
            };
            let parsed_id = match action {
                PostAction::Edit { id, .. }
                | PostAction::Publish { id, .. }
                | PostAction::Withdraw { id, .. }
                | PostAction::Show { id, .. } => id,
                _ => panic!("expected existing post command"),
            };
            assert_eq!(parsed_id, id);
            assert!(Cli::try_parse_from(["blog", "post", name, "--id", "a-slug"]).is_err());
            assert!(Cli::try_parse_from(["blog", "post", name, "--slug", "a-slug"]).is_err());
        }
    }

    #[test]
    fn create_can_set_the_public_slug() {
        let cli = Cli::try_parse_from([
            "blog", "post", "create", "--author", "sun", "--slug", "hello",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Post {
                action: PostAction::Create { slug: Some(slug), .. },
            }) if slug == "hello"
        ));
    }
}
