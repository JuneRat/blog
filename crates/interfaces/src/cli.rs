//! 受控 CLI 入站适配器：M1 的唯一写通道（不得暴露为公开管理 HTTP）。
//!
//! 参数解析与输入/输出映射在本层完成；业务规则全部下沉应用层。
//! `migrate` 由 server 装配层拦截执行（依赖 infrastructure），不经过本模块。

use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;

use application::auth::AuthInteractor;
use application::content::PostVisibility;
use application::content::{CreatePostCmd, EditPostCmd, PostInteractor};
use application::error::UseCaseError;
use application::identity::{Actor, CreateUserCmd, RoleInteractor, UserInteractor};
use application::ports::{ProviderConfig, ProviderKind, UserRepository};
use application::public_site::{PublicSiteInteractor, format_datetime};
use clap::{Parser, Subcommand};

use crate::http::{PublicSiteState, public_router};

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
    Show {
        #[arg(long)]
        slug: String,
        /// 查看身份（own 限本人文章，read_any 可看全部）
        #[arg(long = "as")]
        actor: Option<String>,
    },

    /// 列出作者的文章（含非公开状态）
    List {
        #[arg(long)]
        author: String,
        /// 操作身份（缺省为 --author 本人）
        #[arg(long = "as")]
        actor: Option<String>,
    },
}

/// 装配层注入的用例集合。
pub struct CliDeps {
    pub users: Arc<UserInteractor>,
    pub posts: Arc<PostInteractor>,
    pub roles: Arc<RoleInteractor>,
    pub auth: Arc<AuthInteractor>,
    pub secure_cookies: bool,
    pub public_site: Arc<PublicSiteInteractor>,
    pub user_repo: Arc<dyn UserRepository>,
    /// 主题静态资源目录（/assets/）。
    pub assets_dir: Option<PathBuf>,
    /// 后台 SPA 构建产物目录（/admin/）；不存在时不注册该路由。
    pub admin_dist: Option<PathBuf>,
    /// readiness 探针（healthz）。
    pub health: Option<Arc<dyn application::ports::HealthCheck>>,
}

pub async fn run(deps: CliDeps, command: Command) -> Result<(), String> {
    match command {
        Command::Migrate => Err("migrate 由 server 装配层处理".into()),

        Command::User { action } => run_user(deps, action).await,

        Command::Post { action } => run_post(deps, action).await,

        Command::Role { action } => run_role(deps, action).await,

        Command::Oauth { action } => run_oauth(deps, action).await,

        Command::Serve { addr } => {
            let bind = addr
                .or_else(|| std::env::var("BLOG_BIND").ok())
                .unwrap_or_else(|| "127.0.0.1:8080".into());
            let public_state = PublicSiteState {
                site: deps.public_site,
                health: deps.health,
            };
            let auth_state = crate::http_auth::AuthState {
                auth: deps.auth,
                secure_cookies: deps.secure_cookies,
            };
            let admin_state = crate::http_auth::AdminState {
                auth: auth_state.auth.clone(),
                users: deps.users,
                posts: deps.posts,
            };
            let app = public_router(public_state, deps.assets_dir)
                .merge(crate::http_auth::auth_router(auth_state))
                .merge(crate::http_auth::admin_router(admin_state.clone()))
                .merge(crate::http_admin::posts_router(admin_state));
            // 后台 SPA 挂在 /admin 子树；dist 不存在时保持未注册。
            let app = crate::http::mount_admin_spa(app, deps.admin_dist);
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
            Ok(())
        }
    }
}

async fn run_oauth(deps: CliDeps, action: OauthAction) -> Result<(), String> {
    match action {
        OauthAction::AddOidc {
            id,
            name,
            issuer,
            client_id,
            secret_ref,
            scopes,
        } => {
            let mut providers = deps.auth.list_providers().await.map_err(fmt_error)?;
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
            deps.auth
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
            let mut providers = deps.auth.list_providers().await.map_err(fmt_error)?;
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
            deps.auth
                .save_providers(&Actor::bootstrap_cli(), &providers)
                .await
                .map_err(fmt_error)?;
            println!("已保存 GitHub 提供商配置。");
            Ok(())
        }
        OauthAction::List => {
            let providers = deps.auth.list_providers().await.map_err(fmt_error)?;
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
            deps.auth
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
            deps.auth
                .unbind_external_id(&Actor::bootstrap_cli(), &user, &provider, &external_id)
                .await
                .map_err(fmt_error)?;
            println!("已解绑 {external_id}@{provider} 与用户 {user}。");
            Ok(())
        }
        OauthAction::Bindings { user } => {
            let bindings = deps.auth.bindings_of(&user).await.map_err(fmt_error)?;
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

async fn run_role(deps: CliDeps, action: RoleAction) -> Result<(), String> {
    match action {
        RoleAction::Sync => {
            deps.roles.sync_registry().await.map_err(fmt_error)?;
            println!("权限目录与内置角色已同步。");
            Ok(())
        }
        RoleAction::List => {
            let roles = deps.roles.list().await.map_err(fmt_error)?;
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
            deps.roles
                .assign_to_username(&Actor::bootstrap_cli(), &user, &role)
                .await
                .map_err(fmt_error)?;
            println!("已将角色 {role} 分配给 {user}。");
            Ok(())
        }
        RoleAction::Remove { user, role } => {
            deps.roles
                .remove_from_username(&Actor::bootstrap_cli(), &user, &role)
                .await
                .map_err(fmt_error)?;
            println!("已移除 {user} 的角色 {role}。");
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
                    .map(format_datetime)
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

        PostAction::Show { slug, actor } => {
            let actor = resolve_actor(&deps, actor.as_deref(), &slug).await?;
            let dto = deps.posts.find(&actor, &slug).await.map_err(fmt_error)?;
            print_post(&dto);
            Ok(())
        }

        PostAction::List { author, actor } => {
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
                .posts
                .list_by_author(&operator, who.user_id)
                .await
                .map_err(fmt_error)?;
            println!(
                "{:<6} {:<10} {:<8} {:<14} 标题",
                "版本", "状态", "可见", "slug"
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
            // 缺省使用文章作者（写动作随后仍按 own/any 授权）。
            let author = deps.posts.author_of(post_slug).await.map_err(fmt_error)?;
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
