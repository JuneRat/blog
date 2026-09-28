//! Configuration is read by the command that owns it.

use std::io::Write;
use std::path::{Path, PathBuf};

use application::seo::PublicBaseUrl;
use application::site_info::SiteInfo;

pub struct DatabaseConfig {
    pub url: String,
    pub migrations_dir: PathBuf,
}

impl DatabaseConfig {
    pub fn from_env(saved: Option<&SavedConfig>) -> Self {
        Self {
            url: std::env::var("DATABASE_URL")
                .ok()
                .or_else(|| saved.map(|c| c.database_url.clone()))
                .unwrap_or_else(|| "postgres://blog:blog@127.0.0.1:5432/blog".into()),
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
    pub fn from_env(addr: Option<String>, saved: Option<&SavedConfig>) -> Result<Self, String> {
        let raw_url = std::env::var("BLOG_PUBLIC_BASE_URL")
            .ok()
            .or_else(|| saved.map(|c| c.public_base_url.clone()))
            .unwrap_or_else(|| "http://127.0.0.1:8080".into());
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
            bind: bind_address(addr),
        })
    }
}

pub fn bind_address(addr: Option<String>) -> String {
    addr.or_else(|| std::env::var("BLOG_BIND").ok())
        .unwrap_or_else(|| "127.0.0.1:8080".into())
}

pub fn config_path() -> PathBuf {
    env_path("BLOG_CONFIG_FILE", "data/config.json")
}

/// Immutable installation journal. The matching database marker determines
/// completion, so a crash between the file write and DB commit is resumable.
/// Passwords and the ephemeral installation token are never written here.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedConfig {
    pub database_url: String,
    pub public_base_url: String,
    pub installation_id: String,
}

pub fn read_saved(path: &Path) -> Result<Option<SavedConfig>, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("无法读取安装配置文件，请检查 BLOG_CONFIG_FILE 和访问权限".into()),
    };
    if !metadata.is_file() || metadata.len() > 16 * 1024 {
        return Err("安装配置必须是小于 16 KiB 的普通文件，不能是符号链接".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err("安装配置含数据库凭据，请将文件权限设为 600".into());
        }
    }
    let bytes = std::fs::read(path).map_err(|_| "读取安装配置失败")?;
    let saved: SavedConfig =
        serde_json::from_slice(&bytes).map_err(|_| "安装配置格式无效，请检查 BLOG_CONFIG_FILE")?;
    application::installation::validate_database_url(&saved.database_url)
        .map_err(|e| e.to_string())?;
    // Website URL validation stays in serve, keeping maintenance CLI available.
    if saved.installation_id.len() != 64
        || !saved.installation_id.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err("安装配置中的 installation_id 无效".into());
    }
    Ok(Some(saved))
}

pub fn save_new(path: &Path, saved: &SavedConfig) -> Result<(), String> {
    use application::ports::SecureRandom;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(parent)
        .map_err(|_| "无法创建配置目录，请检查 BLOG_CONFIG_FILE 的写入权限")?;
    let suffix = infrastructure::SystemSecureRandom
        .token_hex()
        .map_err(|_| "无法生成配置临时文件名")?;
    let temporary = parent.join(format!(".blog-config-{suffix}.tmp"));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        let bytes = serde_json::to_vec_pretty(saved)?;
        if bytes.len() > 16 * 1024 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "configuration too large",
            ));
        }
        file.write_all(&bytes)?;
        file.sync_all()?;
        // Hard-link publication is atomic and never overwrites a concurrent
        // installation's config (rename would silently replace it on Unix).
        std::fs::hard_link(&temporary, path)?;
        #[cfg(unix)]
        std::fs::File::open(parent)?.sync_all()?;
        Ok::<_, std::io::Error>(())
    })();
    let _ = std::fs::remove_file(&temporary);
    result.map_err(|_| {
        "无法安全保存安装配置；请检查目录权限、已有文件及磁盘空间，然后重启继续".into()
    })
}

pub fn media_dir() -> PathBuf {
    env_path("BLOG_MEDIA_DIR", "data/media")
}

fn env_path(name: &str, fallback: &str) -> PathBuf {
    std::env::var(name)
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(fallback))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publication_is_private_complete_and_never_overwrites_existing_config() {
        let dir = std::env::temp_dir().join(format!("blog-config-test-{}", uuid::Uuid::now_v7()));
        let path = dir.join("config.json");
        let mut saved = SavedConfig {
            database_url: "postgres://user:secret@localhost/blog".into(),
            public_base_url: "https://example.com".into(),
            installation_id: "a".repeat(64),
        };
        save_new(&path, &saved).unwrap();
        saved.database_url = "postgres://other:secret@localhost/other".into();
        assert!(save_new(&path, &saved).is_err());
        assert_eq!(
            read_saved(&path).unwrap().unwrap().database_url,
            "postgres://user:secret@localhost/blog"
        );
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            1,
            "temporary credentials must be removed"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_and_readable_by_others_configs_are_rejected() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = std::env::temp_dir().join(format!("blog-config-links-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let link = dir.join("config.json");
        symlink(dir.join("missing.json"), &link).unwrap();
        assert!(read_saved(&link).is_err());
        std::fs::remove_file(&link).unwrap();
        std::fs::write(&link, "{}").unwrap();
        std::fs::set_permissions(&link, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_saved(&link).err().unwrap().contains("600"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
