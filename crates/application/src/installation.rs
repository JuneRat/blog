//! 首次安装的输入边界。部署编排由 server 完成，HTTP 不接触数据库和配置文件。

use async_trait::async_trait;
use domain::identity::{User, validate_password};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{UseCaseError, audit::AuditContext, ports::PasswordHasher};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallInput {
    #[serde(default)]
    pub database_url: String,
    pub public_base_url: String,
    pub username: String,
    pub password: String,
}

#[derive(Serialize)]
pub struct InstallInfo {
    pub database_configured: bool,
    pub public_base_url: Option<String>,
}

#[async_trait]
pub trait Installer: Send + Sync {
    fn info(&self) -> InstallInfo;
    async fn install(&self, input: InstallInput, audit: AuditContext) -> Result<(), UseCaseError>;
}

/// Only validated account data and a PHC hash cross the persistence boundary.
pub struct InitialAdmin {
    pub id: Uuid,
    pub username: String,
    pub password_hash: String,
    pub created_at: OffsetDateTime,
}

impl InitialAdmin {
    pub async fn prepare(
        username: &str,
        password: &str,
        hasher: &dyn PasswordHasher,
        now: OffsetDateTime,
    ) -> Result<Self, UseCaseError> {
        let user = User::new(username, None, None, now)
            .map_err(|e| UseCaseError::Invalid(e.to_string()))?;
        validate_password(password, user.username())
            .map_err(|e| UseCaseError::Invalid(e.to_string()))?;
        Ok(Self {
            id: user.id().0,
            username: user.username().to_owned(),
            password_hash: hasher.hash(password).await?,
            created_at: now,
        })
    }
}

pub fn validate_database_url(raw: &str) -> Result<(), UseCaseError> {
    let invalid = || UseCaseError::Invalid("请填写包含数据库名的 PostgreSQL 连接地址".into());
    if raw.len() > 4096 || raw.trim() != raw {
        return Err(invalid());
    }
    let url = url::Url::parse(raw).map_err(|_| invalid())?;
    if !matches!(url.scheme(), "postgres" | "postgresql")
        || url.host_str().is_none()
        || url.path().trim_matches('/').is_empty()
        || url.fragment().is_some()
    {
        return Err(invalid());
    }
    Ok(())
}
