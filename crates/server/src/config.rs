//! Deployment configuration belongs to the composition root. Merge sources once,
//! then validate only the fields owned by the selected command.
mod commands;
mod files;
mod schema;
#[cfg(test)]
mod tests;

pub use commands::run_command;
pub use files::InstallJournal;

use application::{ports::SiteSettingsValue, seo::PublicBaseUrl, site_info::SiteInfo};
use interfaces::cli::ConfigScope;
use schema::FIELDS;
use std::{collections::BTreeMap, net::IpAddr, path::PathBuf};

pub const DEFAULT_PATH: &str = "config.toml";

pub struct DatabaseConfig {
    pub url: String,
    pub migrations_dir: PathBuf,
    pub pool: infrastructure::DatabasePoolConfig,
}

pub struct SiteConfig {
    pub http: crate::transport::HttpLimits,
    pub theme_dir: PathBuf,
    pub site: SiteInfo,
    pub admin_dist: PathBuf,
    pub media_dir: PathBuf,
    pub public_base_url: PublicBaseUrl,
    pub secure_cookies: bool,
    pub bind: String,
    pub trusted_proxies: Vec<IpAddr>,
}

#[derive(Clone)]
pub struct DeploymentConfig {
    pub path: PathBuf,
    original: Option<String>,
    values: toml::Table,
    env: BTreeMap<String, String>,
}

impl DeploymentConfig {
    pub fn load(path: Option<PathBuf>) -> Result<Self, String> {
        let mut env = BTreeMap::new();
        for name in FIELDS
            .iter()
            .filter_map(|f| f.env)
            .chain(["BLOG_CONFIG_FILE"])
        {
            match std::env::var(name) {
                Ok(value) => {
                    env.insert(name.to_owned(), value);
                }
                Err(std::env::VarError::NotPresent) => {}
                Err(_) => return Err(format!("{name} 必须是有效 UTF-8")),
            }
        }
        let path = path
            .or_else(|| env.get("BLOG_CONFIG_FILE").map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from(DEFAULT_PATH));
        if path.as_os_str().is_empty() {
            return Err("配置文件路径不能为空".into());
        }
        let original = files::read_private(&path)?;
        Self::parse(path, original, env)
    }

    fn parse(
        path: PathBuf,
        original: Option<String>,
        env: BTreeMap<String, String>,
    ) -> Result<Self, String> {
        let values = original
            .as_deref()
            .unwrap_or("")
            .parse::<toml::Table>()
            .map_err(|e| {
                let line = e.span().map(|span| {
                    original.as_deref().unwrap_or("")[..span.start]
                        .bytes()
                        .filter(|b| *b == b'\n')
                        .count()
                        + 1
                });
                // TOML diagnostics include source snippets, which may contain credentials.
                format!(
                    "TOML 语法无效{}",
                    line.map(|n| format!("（第 {n} 行）")).unwrap_or_default()
                )
            })?;
        schema::check_keys(&values)?;
        Ok(Self {
            path,
            original,
            values,
            env,
        })
    }

    pub fn reload(&self) -> Result<Self, String> {
        Self::parse(
            self.path.clone(),
            files::read_private(&self.path)?,
            self.env.clone(),
        )
    }

    fn value(&self, key: &str) -> Result<(Option<toml::Value>, String), String> {
        let field = FIELDS
            .iter()
            .find(|f| f.key == key)
            .expect("registered field");
        if let Some(name) = field.env
            && let Some(value) = self.env.get(name)
        {
            return Ok((Some(field.parse_env(value)?), format!("env:{name}")));
        }
        let (section, name) = key.split_once('.').expect("sectioned field");
        if let Some(value) = self.values.get(section).and_then(|s| s.get(name)) {
            if !field.kind.accepts(value) {
                return Err(format!("{key} 类型无效，应为 {}", field.kind.label()));
            }
            return Ok((Some(value.clone()), format!("toml:{}", self.path.display())));
        }
        Ok((
            field
                .default
                .map(|value| field.parse_env(value))
                .transpose()?,
            "default".into(),
        ))
    }

    fn optional_string(&self, key: &str) -> Result<Option<String>, String> {
        let (value, _) = self.value(key)?;
        let value = value.map(|v| v.as_str().expect("string field").to_owned());
        if key != "bootstrap.description" && value.as_ref().is_some_and(|v| v.trim().is_empty()) {
            return Err(format!("{key} 不能为空"));
        }
        Ok(value)
    }

    fn string(&self, key: &str) -> Result<String, String> {
        self.optional_string(key)?
            .ok_or_else(|| format!("缺少 {key}"))
    }

    fn boolean(&self, key: &str) -> Result<Option<bool>, String> {
        Ok(self
            .value(key)?
            .0
            .map(|v| v.as_bool().expect("boolean field")))
    }

    fn path_value(&self, key: &str) -> Result<PathBuf, String> {
        // All relative resource paths resolve from the process working directory,
        // regardless of whether they come from TOML, env or defaults.
        Ok(PathBuf::from(self.string(key)?))
    }

    pub fn configured_database_url(&self) -> Result<Option<String>, String> {
        let url = self.optional_string("database.url")?;
        if let Some(url) = &url {
            application::installation::validate_database_url(url).map_err(|_| {
                "database.url / DATABASE_URL 必须是包含库名的 PostgreSQL 地址".to_owned()
            })?;
        }
        Ok(url)
    }

    pub fn database(&self) -> Result<DatabaseConfig, String> {
        Ok(DatabaseConfig {
            url: self
                .configured_database_url()?
                .ok_or("请配置 database.url 或 DATABASE_URL")?,
            migrations_dir: self.path_value("database.migrations_dir")?,
            pool: self.database_pool()?,
        })
    }

    pub fn database_pool(&self) -> Result<infrastructure::DatabasePoolConfig, String> {
        let number = |key: &str| -> Result<u64, String> {
            let value = self.value(key)?.0.unwrap().as_integer().unwrap();
            u64::try_from(value).map_err(|_| format!("{key} 不能为负数"))
        };
        let small = |key: &str| -> Result<u32, String> {
            u32::try_from(number(key)?).map_err(|_| format!("{key} 超出范围"))
        };
        let pool = infrastructure::DatabasePoolConfig {
            max_connections: small("database.max_connections")?,
            min_connections: small("database.min_connections")?,
            acquire_timeout_ms: number("database.acquire_timeout_ms")?,
            idle_timeout_secs: number("database.idle_timeout_secs")?,
            max_lifetime_secs: number("database.max_lifetime_secs")?,
            statement_timeout_ms: number("database.statement_timeout_ms")?,
            lock_timeout_ms: number("database.lock_timeout_ms")?,
            idle_in_transaction_timeout_ms: number("database.idle_in_transaction_timeout_ms")?,
            connect_retries: small("database.connect_retries")?,
            connect_retry_backoff_ms: number("database.connect_retry_backoff_ms")?,
        };
        pool.validate()?;
        Ok(pool)
    }

    pub fn maintenance_url(&self) -> Result<String, String> {
        if let Some(url) = self.optional_string("maintenance.database_url")? {
            application::installation::validate_database_url(&url).map_err(|_| {
                "maintenance.database_url / BLOG_MAINTENANCE_DATABASE_URL 无效".to_owned()
            })?;
            return Ok(url);
        }
        self.configured_database_url()?
            .ok_or_else(|| "请配置 database.url 或 DATABASE_URL；也可指定独立维护连接".into())
    }

    pub fn recovery_mode(&self) -> Result<bool, String> {
        Ok(self.boolean("recovery.enabled")?.unwrap_or(false))
    }

    pub fn log_json(&self) -> Result<bool, String> {
        match self.string("logging.format")?.as_str() {
            "text" => Ok(false),
            "json" => Ok(true),
            _ => Err("logging.format / BLOG_LOG_FORMAT 只能是 text 或 json".into()),
        }
    }

    pub fn legacy_site_time_zone(&self) -> Result<infrastructure::SiteTimeZone, String> {
        infrastructure::SiteTimeZone::parse(&self.string("server.time_zone")?)
            .map_err(|error| format!("server.time_zone / BLOG_TIME_ZONE {error}"))
    }

    pub fn metrics_bind(&self) -> Result<Option<std::net::SocketAddr>, String> {
        self.optional_string("metrics.bind")?
            .map(|value| {
                value
                    .parse()
                    .map_err(|_| "metrics.bind / BLOG_METRICS_BIND 必须是 IP:端口".into())
            })
            .transpose()
    }

    pub fn log_filter(&self) -> Result<tracing_subscriber::EnvFilter, String> {
        tracing_subscriber::EnvFilter::try_new(self.string("logging.filter")?)
            .map_err(|_| "logging.filter / RUST_LOG 无效".into())
    }

    pub fn media_dir(&self) -> Result<PathBuf, String> {
        self.path_value("paths.media_dir")
    }

    pub fn configured_public_url(&self) -> Result<Option<String>, String> {
        let (value, source) = self.value("server.public_base_url")?;
        Ok((source != "default").then(|| value.unwrap().as_str().unwrap().to_owned()))
    }

    fn http_limits(&self) -> Result<crate::transport::HttpLimits, String> {
        let number = |key: &str, max: i64| -> Result<u64, String> {
            let value = self.value(key)?.0.unwrap().as_integer().unwrap();
            if !(1..=max).contains(&value) {
                return Err(format!("{key} 必须为 1–{max}"));
            }
            Ok(value as u64)
        };
        let seconds = |key: &str| number(key, 3600).map(std::time::Duration::from_secs);
        let limits = crate::transport::HttpLimits {
            requests: interfaces::http_limits::RequestTimeouts {
                request: seconds("server.request_timeout_secs")?,
                upload: seconds("server.upload_timeout_secs")?,
            },
            headers: seconds("server.header_timeout_secs")?,
            io_idle: seconds("server.io_idle_timeout_secs")?,
            connection_age: seconds("server.connection_max_age_secs")?,
            shutdown: seconds("server.shutdown_timeout_secs")?,
            max_connections: number("server.max_http_connections", 65536)? as usize,
        };
        if limits.requests.upload < limits.requests.request
            || limits.connection_age <= limits.requests.upload.max(limits.headers)
        {
            return Err("upload_timeout_secs 须不小于 request_timeout_secs；connection_max_age_secs 须大于上传及请求头期限".into());
        }
        Ok(limits)
    }

    pub fn site(&self, addr: Option<String>) -> Result<SiteConfig, String> {
        self.metrics_bind()?;
        let public_base_url = PublicBaseUrl::parse(&self.string("server.public_base_url")?)
            .map_err(|_| "server.public_base_url / BLOG_PUBLIC_BASE_URL 无效，须为不含路径前缀的 http/https 地址".to_owned())?;
        let https = public_base_url.as_str().starts_with("https://");
        let secure_cookies = self.boolean("server.secure_cookies")?.unwrap_or(https);
        if https && !secure_cookies {
            return Err("HTTPS 公开地址不允许关闭 Secure Cookie；移除 server.secure_cookies / BLOG_SECURE_COOKIES=false 或设置为 true".into());
        }
        let bind = match addr {
            Some(addr) => addr,
            None => self.string("server.bind")?,
        };
        bind.parse::<std::net::SocketAddr>()
            .map_err(|_| "server.bind / BLOG_BIND / --addr 必须是 IP:端口")?;
        if self.recovery_mode()? {
            crate::recovery::check_bind(&bind)?;
        }
        let trusted_proxies = self
            .value("server.trusted_proxies")?
            .0
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|value| {
                value.as_str().unwrap().parse::<IpAddr>().map_err(|_| {
                    "server.trusted_proxies / BLOG_TRUSTED_PROXIES 必须是精确 IP 列表".to_owned()
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(SiteConfig {
            http: self.http_limits()?,
            secure_cookies,
            public_base_url,
            bind,
            trusted_proxies,
            site: SiteInfo {
                navigation: vec![],
                time_zone: self.legacy_site_time_zone()?.name().into(),
                ..SiteInfo::default()
            },
            theme_dir: self.path_value("paths.theme_dir")?,
            admin_dist: self.path_value("paths.admin_dist")?,
            media_dir: self.media_dir()?,
        })
    }

    pub fn bootstrap_site(&self) -> Result<SiteSettingsValue, String> {
        validate_initial_site(SiteSettingsValue {
            home_page_size: None,
            navigation: vec![],
            time_zone: None,
            title: self.optional_string("bootstrap.title")?,
            description: self.optional_string("bootstrap.description")?,
            logo_media_id: None,
        })
    }

    pub fn check(&self, scope: ConfigScope) -> Result<(), String> {
        self.log_filter()?;
        self.log_json()?;
        self.recovery_mode()?;
        match scope {
            ConfigScope::Serve => {
                self.configured_database_url()?;
                self.database_pool()?;
                self.site(None)?;
                self.bootstrap_site()?;
                self.path_value("database.migrations_dir")?;
            }
            ConfigScope::Database => {
                self.database()?;
            }
            ConfigScope::Maintenance => {
                self.maintenance_url()?;
                self.database_pool()?;
            }
            ConfigScope::Media => {
                self.database()?;
                self.media_dir()?;
            }
            ConfigScope::Resources => {
                self.media_dir()?;
                self.path_value("paths.theme_dir")?;
                self.path_value("paths.admin_dist")?;
            }
            ConfigScope::All => {
                self.check(ConfigScope::Serve)?;
                self.maintenance_url()?;
                self.database_pool()?;
            }
        }
        Ok(())
    }

    pub fn show(&self, scope: ConfigScope, sources: bool) -> Result<serde_json::Value, String> {
        self.check(scope)?;
        let mut fields = Vec::new();
        for field in FIELDS.iter().filter(|field| field.in_scope(scope)) {
            let (mut value, mut source) = self.value(field.key)?;
            if field.key == "maintenance.database_url" && value.is_none() {
                (value, source) = self.value("database.url")?;
                source = format!("fallback:{source}");
            }
            let mut value = value
                .map(|v| serde_json::to_value(v).expect("TOML value serializes"))
                .unwrap_or(serde_json::Value::Null);
            if field.key == "server.secure_cookies" && value.is_null() {
                value = self.site(None)?.secure_cookies.into();
                source = "derived:server.public_base_url".into();
            }
            if field.secret && !value.is_null() {
                value = "[redacted]".into();
            }
            let mut entry = serde_json::json!({"key": field.key, "value": value,
                "effect": if field.key.starts_with("bootstrap.") { "installation-only" } else { "restart-or-next-command" }});
            if sources {
                entry["source"] = source.into();
            }
            fields.push(entry);
        }
        Ok(serde_json::json!({"config_file":self.path, "fields":fields}))
    }

    fn installation_config(
        &self,
        database_url: &str,
        public_base_url: &str,
    ) -> Result<String, String> {
        let source = match &self.original {
            Some(source) => source.clone(),
            None => toml::to_string_pretty(&self.values).map_err(|_| "无法编码 TOML 配置")?,
        };
        let mut document = source
            .parse::<toml_edit::DocumentMut>()
            .map_err(|_| "无法编辑 TOML 配置")?;
        document["config_version"] = toml_edit::value(1);
        for (section, key, value) in [
            ("database", "url", database_url),
            ("server", "public_base_url", public_base_url),
        ] {
            document[section][key] = toml_edit::value(value);
        }
        Ok(document.to_string())
    }
}

fn validate_initial_site(value: SiteSettingsValue) -> Result<SiteSettingsValue, String> {
    application::site_info::initial_site_settings(value).map_err(|e| e.to_string())
}
