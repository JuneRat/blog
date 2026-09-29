//! 文章命令及其输入输出映射。

use super::fmt_error;
use application::content::{CreatePostCmd, EditPostCmd, PostInteractor, PostVisibility};
use application::identity::{Actor, UserInteractor};
use application::public_site::format_datetime;
use clap::Subcommand;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;

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
        id: uuid::Uuid,
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
        id: uuid::Uuid,
        #[arg(long)]
        if_version: Option<i64>,
        #[arg(long = "as")]
        actor: Option<String>,
    },

    /// 撤回文章（published → draft，slug 保持锁定）
    Withdraw {
        #[arg(long)]
        id: uuid::Uuid,
        #[arg(long)]
        if_version: Option<i64>,
        #[arg(long = "as")]
        actor: Option<String>,
    },

    /// 查看文章当前状态（CLI/后台视图，含草稿）
    Show {
        #[arg(long)]
        id: uuid::Uuid,
        /// 查看身份（own 限本人文章，read_any 可看全部）
        #[arg(long = "as")]
        actor: Option<String>,
    },

    /// 列出作者的文章（含非公开状态）
    List {
        #[arg(long)]
        author: String,
        /// 页码，每页 20 条
        #[arg(long, default_value_t = 1)]
        page: i64,
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        visibility: Option<String>,
        /// 操作身份（缺省为 --author 本人）
        #[arg(long = "as")]
        actor: Option<String>,
    },
}

/// 文章命令只需要文章用例和操作身份解析。
pub struct PostCliDeps {
    pub content_queries: Arc<application::content_queries::ContentQueries>,
    pub users: Arc<UserInteractor>,
    pub posts: Arc<PostInteractor>,
}

pub async fn run_post(deps: PostCliDeps, action: PostAction) -> Result<(), String> {
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
                        tag_ids: Vec::new(),
                        category_id: None,
                        series: Vec::new(),
                        cover_media_id: None,
                    },
                )
                .await
                .map_err(fmt_error)?;
            println!(
                "已创建草稿 id={} slug={} version={} status={}",
                dto.id, dto.slug, dto.version, dto.status
            );
            Ok(())
        }

        PostAction::Edit {
            id,
            new_slug,
            title,
            excerpt,
            content_file,
            visibility,
            if_version,
            actor,
        } => {
            let actor = resolve_actor(&deps, actor.as_deref(), id).await?;
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
                        id,
                        new_slug,
                        title,
                        excerpt,
                        content,
                        visibility,
                        tag_ids: None,
                        category_id: None,
                        series: None,
                        cover_media_id: None,
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
            id,
            if_version,
            actor,
        } => {
            let actor = resolve_actor(&deps, actor.as_deref(), id).await?;
            let dto = deps
                .posts
                .publish(&actor, id, if_version)
                .await
                .map_err(fmt_error)?;
            println!(
                "已发布 slug={} published_at={}",
                dto.slug,
                dto.published_at
                    .map(format_datetime)
                    .unwrap_or_else(|| "-".into())
            );
            Ok(())
        }

        PostAction::Withdraw {
            id,
            if_version,
            actor,
        } => {
            let actor = resolve_actor(&deps, actor.as_deref(), id).await?;
            let dto = deps
                .posts
                .withdraw(&actor, id, if_version)
                .await
                .map_err(fmt_error)?;
            println!("已撤回 slug={} status={}", dto.slug, dto.status);
            Ok(())
        }

        PostAction::Show { id, actor } => {
            let actor = resolve_actor(&deps, actor.as_deref(), id).await?;
            let dto = deps.posts.find(&actor, id).await.map_err(fmt_error)?;
            print_post(&dto);
            Ok(())
        }

        PostAction::List {
            author,
            actor,
            page,
            status,
            visibility,
        } => {
            // 操作身份缺省为目标作者本人（actor_for_username 内部走同一规范化）。
            let operator_name = actor.as_deref().unwrap_or(&author);
            let operator = deps
                .users
                .actor_for_username(operator_name)
                .await
                .map_err(fmt_error)?;
            let who = deps
                .users
                .actor_for_username(&author)
                .await
                .map_err(fmt_error)?;
            let list = deps
                .content_queries
                .posts(
                    &operator,
                    who.user_id,
                    application::content_queries::ContentListRequest {
                        page,
                        status,
                        visibility,
                        trash: false,
                        ..Default::default()
                    },
                )
                .await
                .map_err(fmt_error)?;
            println!(
                "{:<36} {:<6} {:<10} {:<8} {:<14} 标题",
                "ID", "版本", "状态", "可见", "slug"
            );
            println!(
                "第 {} 页，每页 {} 条，共 {} 条",
                list.page, list.per_page, list.total
            );
            for dto in list.items {
                println!(
                    "{} v{:<5} {:<10} {:<8} {:<14} {}",
                    dto.id,
                    dto.version,
                    dto.status.as_str(),
                    dto.visibility.as_str(),
                    dto.slug,
                    dto.title
                );
            }
            Ok(())
        }
    }
}

async fn resolve_actor(
    deps: &PostCliDeps,
    actor_username: Option<&str>,
    post_id: uuid::Uuid,
) -> Result<Actor, String> {
    match actor_username {
        Some(username) => deps
            .users
            .actor_for_username(username)
            .await
            .map_err(fmt_error),
        None => {
            // 缺省使用文章作者（写动作随后仍按 own/any 授权）。
            let author = deps.posts.author_of(post_id).await.map_err(fmt_error)?;
            deps.users
                .actor_for_user_id(author.0)
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
    println!("id:          {}", dto.id);
    println!("slug:        {}", dto.slug);
    println!("title:       {}", dto.title);
    println!("status:      {}", dto.status);
    println!("visibility:  {}", dto.visibility);
    println!("version:     {}", dto.version);
    println!(
        "published_at: {}",
        dto.published_at
            .map(format_datetime)
            .unwrap_or_else(|| "-".into())
    );
    println!("updated_at:  {}", format_datetime(dto.updated_at));
    if dto.deleted {
        println!("（已在回收站）");
    }
}
