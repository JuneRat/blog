//! Configuration is read by the command that owns it.

use std::path::PathBuf;

use application::public_site::SiteInfo;
use application::seo::PublicBaseUrl;

pub struct DatabaseConfig {
    pub url: String,
    pub migrations_dir: PathBuf,
}

impl DatabaseConfig {
    pub fn from_env() -> Self {
        Self {
            url: std::env::var("DATABASE_URL")
                .unwrap_or_else(|_| "postgres://blog:blog@127.0.0.1:5432/blog".into()),
            migrations_dir: env_path("BLOG_MIGRATIONS_DIR", "migrations/postgres"),
        }
    }
}

pub struct SiteConfig {
    pub theme_dir: PathBuf,
    pub site: SiteInfo,
    pub admin_dist: PathBuf,
    pub media_dir: PathBuf,
    pub public_base_url: PublicBaseUrl,
    pub secure_cookies: bool,
    pub bind: String,
    pub trusted_proxies: Vec<std::net::IpAddr>,
}

impl SiteConfig {
    /// Only `serve` reads website configuration. A broken URL or theme must not
    /// prevent an operator from repairing accounts, roles or OAuth bindings.
    pub fn from_env(addr: Option<String>) -> Result<Self, String> {
        let raw_url = std::env::var("BLOG_PUBLIC_BASE_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:8080".into());
        let public_base_url = PublicBaseUrl::parse(&raw_url)
            .map_err(|error| format!("BLOG_PUBLIC_BASE_URL 无效：{error}"))?;
        let secure_cookies = match std::env::var("BLOG_SECURE_COOKIES") {
            Ok(value) => value == "1" || value.eq_ignore_ascii_case("true"),
            Err(_) => public_base_url.as_str().starts_with("https://"),
        };
        let trusted_proxies = std::env::var("BLOG_TRUSTED_PROXIES")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(|v| {
                v.parse()
                    .map_err(|_| format!("BLOG_TRUSTED_PROXIES 包含无效 IP：{v}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            trusted_proxies,
            theme_dir: env_path("BLOG_THEME_DIR", "themes/default"),
            site: SiteInfo {
                title: std::env::var("BLOG_SITE_TITLE").unwrap_or_else(|_| "Sun's Blog".into()),
                description: std::env::var("BLOG_SITE_DESCRIPTION")
                    .unwrap_or_else(|_| "一个 Rust 博客".into()),
                logo_url: None,
            },
            admin_dist: env_path("BLOG_ADMIN_DIST", "apps/admin/dist"),
            media_dir: media_dir(),
            public_base_url,
            secure_cookies,
            bind: addr
                .or_else(|| std::env::var("BLOG_BIND").ok())
                .unwrap_or_else(|| "127.0.0.1:8080".into()),
        })
    }
}

pub fn media_dir() -> PathBuf {
    env_path("BLOG_MEDIA_DIR", "data/media")
}

fn env_path(name: &str, fallback: &str) -> PathBuf {
    std::env::var(name)
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(fallback))
}
