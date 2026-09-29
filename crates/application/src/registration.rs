//! Public registration and administrator-controlled participation settings.
use crate::{
    UseCaseError,
    audit::AuditContext,
    identity::Actor,
    ports::{Clock, PasswordHasher},
};
use async_trait::async_trait;
use domain::identity::{User, validate_password};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AccessPolicy {
    pub registration_enabled: bool,
    pub guest_comments_enabled: bool,
    #[serde(skip)]
    pub version: i64,
}

pub struct RegisterAccount {
    pub username: String,
    pub display_name: Option<String>,
    pub email: String,
    pub password: String,
}

#[async_trait]
pub trait RegistrationStore: Send + Sync {
    async fn policy(&self) -> Result<AccessPolicy, UseCaseError>;
    async fn save_policy(
        &self,
        policy: AccessPolicy,
        audit: AuditContext,
    ) -> Result<AccessPolicy, UseCaseError>;
    /// Recheck registration policy and atomically create account, password, reader role and audit.
    async fn register(
        &self,
        user: &User,
        password_hash: &str,
        audit: AuditContext,
    ) -> Result<(), UseCaseError>;
}

pub struct RegistrationInteractor {
    store: Arc<dyn RegistrationStore>,
    hasher: Arc<dyn PasswordHasher>,
    clock: Arc<dyn Clock>,
}
impl RegistrationInteractor {
    pub fn new(
        store: Arc<dyn RegistrationStore>,
        hasher: Arc<dyn PasswordHasher>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            store,
            hasher,
            clock,
        }
    }
    pub async fn enabled(&self) -> Result<bool, UseCaseError> {
        Ok(self.store.policy().await?.registration_enabled)
    }
    pub async fn policy(
        &self,
        actor: &Actor,
        update: Option<AccessPolicy>,
    ) -> Result<AccessPolicy, UseCaseError> {
        actor.ensure_write_channel()?;
        if !actor.has_permission("settings.manage") {
            return Err(UseCaseError::Forbidden);
        }
        match update {
            Some(update) => self.store.save_policy(update, actor.audit_context()).await,
            None => self.store.policy().await,
        }
    }
    pub async fn register(
        &self,
        input: RegisterAccount,
        ip_address: Option<std::net::IpAddr>,
    ) -> Result<(), UseCaseError> {
        if !self.enabled().await? {
            return Err(UseCaseError::RegistrationClosed);
        }
        let display_name = input
            .display_name
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
        let user = User::new(
            &input.username,
            Some(input.email),
            display_name,
            self.clock.now(),
        )
        .map_err(|e| UseCaseError::Invalid(e.to_string()))?;
        validate_password(&input.password, user.username())
            .map_err(|e| UseCaseError::Invalid(e.to_string()))?;
        let hash = self.hasher.hash(&input.password).await?;
        self.store
            .register(
                &user,
                &hash,
                AuditContext {
                    actor_id: None,
                    ip_address,
                },
            )
            .await
    }
}
