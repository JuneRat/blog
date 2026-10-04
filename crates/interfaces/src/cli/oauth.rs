//! OAuth 配置和外部身份维护命令。

use super::fmt_error;
use application::auth::OAuthManagementInteractor;
use application::identity::Actor;
use application::ports::{ProviderConfig, ProviderKind};
use clap::Subcommand;

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
            let settings = oauth.provider_settings().await.map_err(fmt_error)?;
            let mut providers = settings.providers;
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
                .save_providers(&Actor::bootstrap_cli(), &providers, settings.version)
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
            let settings = oauth.provider_settings().await.map_err(fmt_error)?;
            let mut providers = settings.providers;
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
                .save_providers(&Actor::bootstrap_cli(), &providers, settings.version)
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
