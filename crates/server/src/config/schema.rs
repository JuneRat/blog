use interfaces::cli::ConfigScope;

#[derive(Clone, Copy)]
pub(super) enum Kind {
    String,
    Bool,
    Integer,
    Strings,
}

impl Kind {
    pub fn accepts(self, value: &toml::Value) -> bool {
        match self {
            Self::String => value.is_str(),
            Self::Bool => value.is_bool(),
            Self::Integer => value.is_integer(),
            Self::Strings => value
                .as_array()
                .is_some_and(|a| a.iter().all(toml::Value::is_str)),
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::String => "字符串",
            Self::Bool => "布尔值",
            Self::Integer => "整数",
            Self::Strings => "字符串数组",
        }
    }
}

pub(super) struct Field {
    pub key: &'static str,
    pub env: Option<&'static str>,
    pub kind: Kind,
    pub default: Option<&'static str>,
    pub secret: bool,
}

impl Field {
    pub fn parse_env(&self, value: &str) -> Result<toml::Value, String> {
        Ok(match self.kind {
            Kind::String => value.into(),
            Kind::Integer => value
                .parse::<i64>()
                .map(toml::Value::Integer)
                .map_err(|_| format!("{} / {} 必须是整数", self.key, self.env.unwrap_or("TOML")))?,
            Kind::Bool => match value.to_ascii_lowercase().as_str() {
                "true" | "1" => true.into(),
                "false" | "0" => false.into(),
                _ => {
                    return Err(format!(
                        "{} / {} 只能是 true/false 或 1/0",
                        self.key,
                        self.env.unwrap_or("TOML")
                    ));
                }
            },
            Kind::Strings => toml::Value::Array(
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(toml::Value::from)
                    .collect(),
            ),
        })
    }
    pub fn in_scope(&self, scope: ConfigScope) -> bool {
        if self.key.starts_with("logging.") || self.key.starts_with("recovery.") {
            return true;
        }
        match scope {
            ConfigScope::All => true,
            ConfigScope::Serve => !self.key.starts_with("maintenance."),
            ConfigScope::Maintenance => {
                self.key.starts_with("maintenance.")
                    || (self.key.starts_with("database.")
                        && !matches!(self.key, "database.url" | "database.migrations_dir"))
            }
            ConfigScope::Database => self.key.starts_with("database."),
            ConfigScope::Media => {
                self.key.starts_with("database.") || self.key == "paths.media_dir"
            }
            ConfigScope::Resources => self.key.starts_with("paths."),
        }
    }
}

macro_rules! field {
    ($key:literal, $env:expr, $kind:ident, $default:expr, $secret:literal) => {
        Field {
            key: $key,
            env: $env,
            kind: Kind::$kind,
            default: $default,
            secret: $secret,
        }
    };
}

pub(super) const FIELDS: &[Field] = &[
    field!(
        "server.request_timeout_secs",
        Some("BLOG_REQUEST_TIMEOUT_SECS"),
        Integer,
        Some("30"),
        false
    ),
    field!(
        "server.upload_timeout_secs",
        Some("BLOG_UPLOAD_TIMEOUT_SECS"),
        Integer,
        Some("120"),
        false
    ),
    field!(
        "server.header_timeout_secs",
        Some("BLOG_HEADER_TIMEOUT_SECS"),
        Integer,
        Some("10"),
        false
    ),
    field!(
        "server.io_idle_timeout_secs",
        Some("BLOG_IO_IDLE_TIMEOUT_SECS"),
        Integer,
        Some("30"),
        false
    ),
    field!(
        "server.connection_max_age_secs",
        Some("BLOG_CONNECTION_MAX_AGE_SECS"),
        Integer,
        Some("300"),
        false
    ),
    field!(
        "server.shutdown_timeout_secs",
        Some("BLOG_SHUTDOWN_TIMEOUT_SECS"),
        Integer,
        Some("25"),
        false
    ),
    field!(
        "server.max_http_connections",
        Some("BLOG_MAX_HTTP_CONNECTIONS"),
        Integer,
        Some("1024"),
        false
    ),
    field!("database.url", Some("DATABASE_URL"), String, None, true),
    field!(
        "database.max_connections",
        Some("BLOG_DB_MAX_CONNECTIONS"),
        Integer,
        Some("5"),
        false
    ),
    field!(
        "database.min_connections",
        Some("BLOG_DB_MIN_CONNECTIONS"),
        Integer,
        Some("0"),
        false
    ),
    field!(
        "database.acquire_timeout_ms",
        Some("BLOG_DB_ACQUIRE_TIMEOUT_MS"),
        Integer,
        Some("5000"),
        false
    ),
    field!(
        "database.idle_timeout_secs",
        Some("BLOG_DB_IDLE_TIMEOUT_SECS"),
        Integer,
        Some("600"),
        false
    ),
    field!(
        "database.max_lifetime_secs",
        Some("BLOG_DB_MAX_LIFETIME_SECS"),
        Integer,
        Some("1800"),
        false
    ),
    field!(
        "database.statement_timeout_ms",
        Some("BLOG_DB_STATEMENT_TIMEOUT_MS"),
        Integer,
        Some("0"),
        false
    ),
    field!(
        "database.lock_timeout_ms",
        Some("BLOG_DB_LOCK_TIMEOUT_MS"),
        Integer,
        Some("0"),
        false
    ),
    field!(
        "database.idle_in_transaction_timeout_ms",
        Some("BLOG_DB_IDLE_IN_TRANSACTION_TIMEOUT_MS"),
        Integer,
        Some("0"),
        false
    ),
    field!(
        "database.connect_retries",
        Some("BLOG_DB_CONNECT_RETRIES"),
        Integer,
        Some("3"),
        false
    ),
    field!(
        "database.connect_retry_backoff_ms",
        Some("BLOG_DB_CONNECT_RETRY_BACKOFF_MS"),
        Integer,
        Some("250"),
        false
    ),
    field!(
        "database.migrations_dir",
        Some("BLOG_MIGRATIONS_DIR"),
        String,
        Some("migrations/postgres"),
        false
    ),
    field!(
        "maintenance.database_url",
        Some("BLOG_MAINTENANCE_DATABASE_URL"),
        String,
        None,
        true
    ),
    field!(
        "server.bind",
        Some("BLOG_BIND"),
        String,
        Some("127.0.0.1:8080"),
        false
    ),
    // Compatibility fallback for site rows written before time_zone became an admin setting.
    field!(
        "server.time_zone",
        Some("BLOG_TIME_ZONE"),
        String,
        Some("UTC"),
        false
    ),
    field!(
        "server.public_base_url",
        Some("BLOG_PUBLIC_BASE_URL"),
        String,
        Some("http://127.0.0.1:8080"),
        false
    ),
    field!(
        "server.secure_cookies",
        Some("BLOG_SECURE_COOKIES"),
        Bool,
        None,
        false
    ),
    field!(
        "server.trusted_proxies",
        Some("BLOG_TRUSTED_PROXIES"),
        Strings,
        Some(""),
        false
    ),
    field!(
        "paths.theme_dir",
        Some("BLOG_THEME_DIR"),
        String,
        Some("themes/default"),
        false
    ),
    field!(
        "paths.admin_dist",
        Some("BLOG_ADMIN_DIST"),
        String,
        Some("apps/admin/dist"),
        false
    ),
    field!(
        "paths.media_dir",
        Some("BLOG_MEDIA_DIR"),
        String,
        Some("data/media"),
        false
    ),
    field!(
        "logging.format",
        Some("BLOG_LOG_FORMAT"),
        String,
        Some("text"),
        false
    ),
    field!(
        "metrics.bind",
        Some("BLOG_METRICS_BIND"),
        String,
        None,
        false
    ),
    field!(
        "logging.filter",
        Some("RUST_LOG"),
        String,
        Some("info,sqlx=warn"),
        false
    ),
    field!(
        "recovery.enabled",
        Some("BLOG_RECOVERY_MODE"),
        Bool,
        Some("false"),
        false
    ),
    field!("mail.host", Some("BLOG_SMTP_HOST"), String, None, false),
    field!(
        "mail.port",
        Some("BLOG_SMTP_PORT"),
        Integer,
        Some("587"),
        false
    ),
    field!(
        "mail.security",
        Some("BLOG_SMTP_SECURITY"),
        String,
        Some("starttls"),
        false
    ),
    field!(
        "mail.username",
        Some("BLOG_SMTP_USERNAME"),
        String,
        None,
        true
    ),
    field!(
        "mail.password",
        Some("BLOG_SMTP_PASSWORD"),
        String,
        None,
        true
    ),
    field!("mail.from", Some("BLOG_SMTP_FROM"), String, None, false),
    field!("bootstrap.title", None, String, None, false),
    field!("bootstrap.description", None, String, None, false),
];

pub(super) fn check_keys(values: &toml::Table) -> Result<(), String> {
    if let Some(version) = values.get("config_version")
        && version.as_integer() != Some(1)
    {
        return Err("config_version 必须为 1".into());
    }
    for (section, value) in values {
        if section == "config_version" {
            continue;
        }
        if !FIELDS
            .iter()
            .any(|field| field.key.split_once('.').unwrap().0 == section)
        {
            return Err(format!("未知配置分组：{section}"));
        }
        let table = value
            .as_table()
            .ok_or_else(|| format!("{section} 必须是 TOML 表"))?;
        for name in table.keys() {
            let key = format!("{section}.{name}");
            if !FIELDS.iter().any(|field| field.key == key) {
                return Err(format!("未知配置项：{key}"));
            }
        }
    }
    Ok(())
}
