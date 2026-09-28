//! 受控 CLI 入站适配器：M1 的唯一写通道（不得暴露为公开管理 HTTP）。
//!
//! 参数解析与输入/输出映射在本层完成；业务规则全部下沉应用层。
//! `migrate` 与 `rebuild-html` 是基础设施维护，由 server 装配层拦截执行。

use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;

use application::auth::OAuthManagementInteractor;
use application::content::PostVisibility;
use application::content::{CreatePostCmd, EditPostCmd, PostInteractor};
use application::error::UseCaseError;
use application::identity::{Actor, CreateUserCmd, RoleInteractor, UserInteractor};
use application::password::PasswordInteractor;
use application::ports::{ProviderConfig, ProviderKind};
use application::public_site::format_datetime;
use clap::{Parser, Subcommand};

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
    RebuildHtml,

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
    /// 移除用户的角色（最后一个有效 Owner 会被拒绝）
    Remove {
        #[arg(long)]
        user: String,
        #[arg(long)]
        role: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum OauthAction {
    /// 新增或更新通用 OIDC 提供商（绑定精确 issuer）
    AddOidc {
        /// 提供商 id（用于 URL 与回调路径，如 keycloak）
        #[arg(long)]
        id: String,
        /// 登录页展示名（缺省用 id）
        #[arg(long)]
        name: Option<String>,
        /// 精确 issuer URL（https）
        #[arg(long)]
        issuer: String,
        #[arg(long)]
        client_id: String,
        /// 保存 client secret 的环境变量名
        #[arg(long)]
        secret_ref: String,
        /// 空格分隔；默认 "openid profile email"
        #[arg(long)]
        scopes: Option<String>,
    },
    /// 新增或更新 GitHub 提供商
    AddGithub {
        #[arg(long, default_value = "github")]
        id: String,
        /// 登录页展示名（缺省用 id）
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        client_id: String,
        #[arg(long)]
        secret_ref: String,
        #[arg(long)]
        scopes: Option<String>,
    },
    /// 列出已配置的提供商（不含秘密）
    List,
    /// 为用户绑定外部身份（需核对稳定的 sub / 数值用户 ID）
    Bind {
        #[arg(long)]
        user: String,
        /// 提供商 id
        #[arg(long)]
        provider: String,
        /// 外部稳定用户 ID（OIDC sub / GitHub 数值 id）
        #[arg(long)]
        external_id: String,
        /// 资料快照邮箱（可空）
        #[arg(long)]
        email: Option<String>,
    },
    /// 解绑外部身份（最后一种登录方式会被拒绝）
    Unbind {
        #[arg(long)]
        user: String,
        #[arg(long)]
        provider: String,
        #[arg(long)]
        external_id: String,
    },
    /// 列出用户的外部身份绑定
    Bindings {
        #[arg(long)]
        user: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum MediaAction {
    /// 清理超期上传暂存文件，不删除正式图片或软删除记录
    CleanupStaging,
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

/// 用户命令仅依赖账号和密码用例。
pub struct UserCliDeps {
    pub users: Arc<UserInteractor>,
    pub passwords: Arc<PasswordInteractor>,
}

/// 文章命令只需要文章用例和操作身份解析。
pub struct PostCliDeps {
    pub content_queries: Arc<application::content_queries::ContentQueries>,
    pub users: Arc<UserInteractor>,
    pub posts: Arc<PostInteractor>,
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

pub async fn run_oauth(
    oauth: &OAuthManagementInteractor,
    action: OauthAction,
) -> Result<(), String> {
    match action {
        OauthAction::AddOidc {
            id,
            name,
            issuer,
            client_id,
            secret_ref,
            scopes,
        } => {
            let mut providers = oauth.list_providers().await.map_err(fmt_error)?;
            upsert_provider(
                &mut providers,
                ProviderConfig {
                    id,
                    name: normalize_optional(name),
                    kind: ProviderKind::Oidc,
                    issuer: Some(issuer),
                    client_id,
                    secret_ref,
                    scopes: split_scopes(scopes),
                },
            );
            oauth
                .save_providers(&Actor::bootstrap_cli(), &providers)
                .await
                .map_err(fmt_error)?;
            println!("已保存 OIDC 提供商配置（秘密经 secret_ref 从环境读取，不落库）。");
            Ok(())
        }
        OauthAction::AddGithub {
            id,
            name,
            client_id,
            secret_ref,
            scopes,
        } => {
            let mut providers = oauth.list_providers().await.map_err(fmt_error)?;
            upsert_provider(
                &mut providers,
                ProviderConfig {
                    id,
                    name: normalize_optional(name),
                    kind: ProviderKind::GitHub,
                    issuer: None,
                    client_id,
                    secret_ref,
                    scopes: split_scopes(scopes),
                },
            );
            oauth
                .save_providers(&Actor::bootstrap_cli(), &providers)
                .await
                .map_err(fmt_error)?;
            println!("已保存 GitHub 提供商配置。");
            Ok(())
        }
        OauthAction::List => {
            let providers = oauth.list_providers().await.map_err(fmt_error)?;
            if providers.is_empty() {
                println!("（未配置提供商；用 oauth add-oidc / add-github 添加）");
            }
            for p in providers {
                let name = p.name.clone().unwrap_or_else(|| p.id.clone());
                println!(
                    "{:<14} {:<6} name={:<16} client_id={} issuer={}",
                    p.id,
                    match p.kind {
                        ProviderKind::Oidc => "oidc",
                        ProviderKind::GitHub => "github",
                    },
                    name,
                    p.client_id,
                    p.issuer.unwrap_or_else(|| "-".into())
                );
            }
            Ok(())
        }
        OauthAction::Bind {
            user,
            provider,
            external_id,
            email,
        } => {
            oauth
                .bind_external_id(
                    &Actor::bootstrap_cli(),
                    &user,
                    &provider,
                    &external_id,
                    email,
                )
                .await
                .map_err(fmt_error)?;
            println!("已将 {external_id}@{provider} 绑定到用户 {user}。");
            Ok(())
        }
        OauthAction::Unbind {
            user,
            provider,
            external_id,
        } => {
            oauth
                .unbind_external_id(&Actor::bootstrap_cli(), &user, &provider, &external_id)
                .await
                .map_err(fmt_error)?;
            println!("已解绑 {external_id}@{provider} 与用户 {user}。");
            Ok(())
        }
        OauthAction::Bindings { user } => {
            let bindings = oauth.bindings_of(&user).await.map_err(fmt_error)?;
            if bindings.is_empty() {
                println!("用户 {user} 没有外部身份绑定。");
            }
            for b in bindings {
                println!("{b}");
            }
            Ok(())
        }
    }
}

fn upsert_provider(providers: &mut Vec<ProviderConfig>, config: ProviderConfig) {
    providers.retain(|p| p.id != config.id);
    providers.push(config);
}

fn split_scopes(scopes: Option<String>) -> Vec<String> {
    scopes
        .map(|s| s.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default()
}

/// 空/空白展示名视为未提供（回退到 id）。
fn normalize_optional(value: Option<String>) -> Option<String> {
    value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// 独立维护入口，只清理暂存区；正常及软删除媒体均保留。
pub async fn run_media(
    media: &application::media::MediaInteractor,
    action: MediaAction,
) -> Result<(), String> {
    match action {
        MediaAction::CleanupStaging => {
            let removed = media
                .cleanup_staging(&Actor::bootstrap_cli())
                .await
                .map_err(fmt_error)?;
            println!("已清理超期暂存文件：{removed}；正式图片保持不变。");
            Ok(())
        }
    }
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
                    dto.id, dto.version, dto.status, dto.visibility, dto.slug, dto.title
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

fn fmt_error(e: UseCaseError) -> String {
    e.to_string()
}

/// CLI 只负责把维护输入交给用例并呈现结果。
pub async fn run_maintenance(
    maintenance: &application::retention::RetentionMaintenance,
    batch_size: i64,
    max_batches: u32,
    dry_run: bool,
) -> Result<(), String> {
    let result = maintenance
        .run(batch_size, max_batches, dry_run)
        .await
        .map_err(|e| e.to_string())?;
    println!(
        "{}",
        serde_json::to_string(&result).map_err(|e| e.to_string())?
    );
    Ok(())
}

pub async fn run_publish_due(
    publisher: &application::publishing::PublishDueInteractor,
) -> Result<(), String> {
    let total = publisher.run().await.map_err(|e| e.to_string())?;
    println!("已发布 {total} 条到期内容。");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_post_commands_require_uuid_identity() {
        let id = uuid::Uuid::now_v7();
        for name in ["edit", "publish", "withdraw", "show"] {
            let cli = Cli::try_parse_from(["blog", "post", name, "--id", &id.to_string()])
                .expect("UUID command should parse");
            let Command::Post { action } = cli.command else {
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
            Command::Post {
                action: PostAction::Create { slug: Some(slug), .. },
            } if slug == "hello"
        ));
    }
}
