//! 受控 CLI 入站适配器：M1 的唯一写通道（不得暴露为公开管理 HTTP）。
//!
//! 参数解析与输入/输出映射在本层完成；业务规则全部下沉应用层。
//! `migrate` 由 server 装配层拦截执行（依赖 infrastructure），不经过本模块。

use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;

use application::content::{CreatePostCmd, EditPostCmd, PostInteractor};
use application::content::PostVisibility;
use application::error::UseCaseError;
use application::identity::{Actor, CreateUserCmd, UserInteractor};
use application::ports::UserRepository;
use application::public_site::PublicSiteInteractor;
use clap::{Parser, Subcommand};

use crate::http::{public_router, PublicSiteState};

#[derive(Debug, Parser)]
#[command(name = "blog", version, about = "博客受控 CLI 与公开 SSR 服务入口")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

/// 供装配入口使用的参数解析（server 不直接依赖 clap）。
pub fn parse_args() -> Cli {
    use clap::Parser as _;
    Cli::parse()
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// 执行数据库迁移（由 server 装配层直接处理）
    Migrate,

    /// 用户管理（受控操作，不开放自助注册）
    User {
        #[command(subcommand)]
        action: UserAction,
    },

    /// 文章管理（M1 写通道）
    Post {
        #[command(subcommand)]
        action: PostAction,
    },

    /// 启动公开 SSR 服务
    Serve {
        /// 监听地址，如 127.0.0.1:8080
        #[arg(long)]
        addr: Option<String>,
    },
}

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
}

#[derive(Debug, Subcommand)]
pub enum PostAction {
    /// 创建文章草稿
    Create {
        /// 作者用户名
        #[arg(long)]
        author: String,
        /// slug；缺省时生成临时唯一值（草稿即占用）
        #[arg(long)]
        slug: Option<String>,
        #[arg(long, default_value = "")]
        title: String,
        #[arg(long)]
        excerpt: Option<String>,
        /// Markdown 文件路径；`-` 表示从 stdin 读取
        #[arg(long)]
        content_file: Option<PathBuf>,
        /// public（默认）或 private
        #[arg(long)]
        visibility: Option<String>,
    },

    /// 编辑文章（保存已发布内容直接更新线上）
    Edit {
        #[arg(long)]
        slug: String,
        /// 发布前可改；首次发布后锁定
        #[arg(long)]
        new_slug: Option<String>,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        excerpt: Option<String>,
        /// Markdown 文件路径；`-` 表示从 stdin 读取
        #[arg(long)]
        content_file: Option<PathBuf>,
        /// public 或 private
        #[arg(long)]
        visibility: Option<String>,
        /// 期望版本；缺省用当前版本（仍可检测读后并发修改）
        #[arg(long)]
        if_version: Option<i64>,
        /// 以指定用户身份执行（缺省为文章作者）
        #[arg(long = "as")]
        actor: Option<String>,
    },

    /// 发布文章（draft → published）
    Publish {
        #[arg(long)]
        slug: String,
        #[arg(long)]
        if_version: Option<i64>,
        #[arg(long = "as")]
        actor: Option<String>,
    },

    /// 撤回文章（published → draft，slug 保持锁定）
    Withdraw {
        #[arg(long)]
        slug: String,
        #[arg(long)]
        if_version: Option<i64>,
        #[arg(long = "as")]
        actor: Option<String>,
    },

    /// 查看文章当前状态（CLI/后台视图，含草稿）
    Show { #[arg(long)] slug: String },

    /// 列出作者的文章（含非公开状态）
    List {
        #[arg(long)]
        author: String,
    },
}

/// 装配层注入的用例集合。
pub struct CliDeps {
    pub users: Arc<UserInteractor>,
    pub posts: Arc<PostInteractor>,
    pub public_site: Arc<PublicSiteInteractor>,
    pub user_repo: Arc<dyn UserRepository>,
    /// 主题静态资源目录（/assets/）。
    pub assets_dir: Option<PathBuf>,
}

pub async fn run(deps: CliDeps, command: Command) -> Result<(), String> {
    match command {
        Command::Migrate => Err("migrate 由 server 装配层处理".into()),

        Command::User { action } => run_user(deps, action).await,

        Command::Post { action } => run_post(deps, action).await,

        Command::Serve { addr } => {
            let bind = addr
                .or_else(|| std::env::var("BLOG_BIND").ok())
                .unwrap_or_else(|| "127.0.0.1:8080".into());
            let state = PublicSiteState {
                site: deps.public_site,
            };
            let app = public_router(state, deps.assets_dir);
            let listener = tokio::net::TcpListener::bind(&bind)
                .await
                .map_err(|e| format!("绑定 {bind} 失败：{e}"))?;
            println!("公开站点已启动：http://{bind}");
            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown_signal())
                .await
                .map_err(|e| format!("服务退出：{e}"))?;
            Ok(())
        }
    }
}

async fn run_user(deps: CliDeps, action: UserAction) -> Result<(), String> {
    match action {
        UserAction::Create {
            username,
            display_name,
            email,
        } => {
            let user = deps
                .users
                .create_user(CreateUserCmd {
                    username,
                    email,
                    display_name,
                })
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
            println!("用户 {}（id={}）", username, actor.user_id.0);
            Ok(())
        }
    }
}

async fn run_post(deps: CliDeps, action: PostAction) -> Result<(), String> {
    match action {
        PostAction::Create {
            author,
            slug,
            title,
            excerpt,
            content_file,
            visibility,
        } => {
            let content = read_content(content_file.as_deref())?;
            let actor = deps
                .users
                .actor_for_username(&author)
                .await
                .map_err(fmt_error)?;
            let dto = deps
                .posts
                .create(
                    &actor,
                    CreatePostCmd {
                        slug,
                        title,
                        excerpt,
                        content,
                        visibility: parse_visibility(visibility.as_deref())?,
                    },
                )
                .await
                .map_err(fmt_error)?;
            println!(
                "已创建草稿 slug={} version={} status={}",
                dto.slug, dto.version, dto.status
            );
            Ok(())
        }

        PostAction::Edit {
            slug,
            new_slug,
            title,
            excerpt,
            content_file,
            visibility,
            if_version,
            actor,
        } => {
            let actor = resolve_actor(&deps, actor.as_deref(), &slug).await?;
            let content = match content_file {
                Some(path) => Some(read_content(Some(&path))?),
                None => None,
            };
            let visibility = match visibility.as_deref() {
                None => None,
                Some(v) => Some(parse_visibility(Some(v))?),
            };
            let dto = deps
                .posts
                .edit(
                    &actor,
                    EditPostCmd {
                        target_slug: slug,
                        new_slug,
                        title,
                        excerpt,
                        content,
                        visibility,
                        expected_version: if_version,
                    },
                )
                .await
                .map_err(fmt_error)?;
            println!(
                "已保存 slug={} version={} status={}",
                dto.slug, dto.version, dto.status
            );
            Ok(())
        }

        PostAction::Publish {
            slug,
            if_version,
            actor,
        } => {
            let actor = resolve_actor(&deps, actor.as_deref(), &slug).await?;
            let dto = deps
                .posts
                .publish(&actor, &slug, if_version)
                .await
                .map_err(fmt_error)?;
            println!(
                "已发布 slug={} published_at={}",
                dto.slug,
                dto.published_at
                    .map(|t| t.to_string())
                    .unwrap_or_else(|| "-".into())
            );
            Ok(())
        }

        PostAction::Withdraw {
            slug,
            if_version,
            actor,
        } => {
            let actor = resolve_actor(&deps, actor.as_deref(), &slug).await?;
            let dto = deps
                .posts
                .withdraw(&actor, &slug, if_version)
                .await
                .map_err(fmt_error)?;
            println!("已撤回 slug={} status={}", dto.slug, dto.status);
            Ok(())
        }

        PostAction::Show { slug } => {
            let dto = deps.posts.find(&slug).await.map_err(fmt_error)?;
            print_post(&dto);
            Ok(())
        }

        PostAction::List { author } => {
            let who = deps
                .users
                .actor_for_username(&author)
                .await
                .map_err(fmt_error)?;
            let list = deps
                .posts
                .list_by_author(who.user_id)
                .await
                .map_err(fmt_error)?;
            println!(
                "{:<6} {:<10} {:<8} {:<14} {}",
                "版本", "状态", "可见", "slug", "标题"
            );
            for dto in list {
                println!(
                    "v{:<5} {:<10} {:<8} {:<14} {}",
                    dto.version, dto.status, dto.visibility, dto.slug, dto.title
                );
            }
            Ok(())
        }
    }
}

async fn resolve_actor(
    deps: &CliDeps,
    actor_username: Option<&str>,
    post_slug: &str,
) -> Result<Actor, String> {
    match actor_username {
        Some(username) => deps
            .users
            .actor_for_username(username)
            .await
            .map_err(fmt_error),
        None => {
            // 缺省使用文章作者。
            let snapshot = deps.posts.find(post_slug).await.map_err(fmt_error)?;
            deps.users
                .actor_for_user_id(snapshot.author_id)
                .await
                .map_err(fmt_error)
        }
    }
}

fn parse_visibility(value: Option<&str>) -> Result<PostVisibility, String> {
    match value {
        None => Ok(PostVisibility::Public),
        Some("public") => Ok(PostVisibility::Public),
        Some("private") => Ok(PostVisibility::Private),
        Some(other) => Err(format!("visibility 只支持 public/private，收到 {other}")),
    }
}

fn read_content(path: Option<&std::path::Path>) -> Result<String, String> {
    match path {
        None => Ok(String::new()),
        Some(p) if p == std::path::Path::new("-") => {
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .map_err(|e| format!("读取 stdin 失败：{e}"))?;
            Ok(buf)
        }
        Some(p) => {
            std::fs::read_to_string(p).map_err(|e| format!("读取 {} 失败：{e}", p.display()))
        }
    }
}

fn print_post(dto: &application::content::PostDto) {
    println!("slug:        {}", dto.slug);
    println!("title:       {}", dto.title);
    println!("status:      {}", dto.status);
    println!("visibility:  {}", dto.visibility);
    println!("version:     {}", dto.version);
    println!(
        "published_at: {}",
        dto.published_at
            .map(|t| t.to_string())
            .unwrap_or_else(|| "-".into())
    );
    println!("updated_at:  {}", dto.updated_at);
    if dto.deleted {
        println!("（已在回收站）");
    }
}

fn fmt_error(e: UseCaseError) -> String {
    e.to_string()
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    println!("收到退出信号，正在关闭…");
}
