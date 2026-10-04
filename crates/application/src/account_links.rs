//! Email invitations and password recovery. Links are one-use bearer credentials;
//! storage persists only their digest and binds them to the current identity state.
use crate::{
    UseCaseError,
    identity::Actor,
    ports::{Clock, PasswordHasher, SecureRandom},
    seo::PublicBaseUrl,
};
use async_trait::async_trait;
use std::{net::IpAddr, sync::Arc};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

pub enum LinkTarget<'a> {
    Recovery(&'a str),
    Invitation { user_id: Uuid, actor: &'a Actor },
}

#[async_trait]
pub trait AccountLinkStore: Send + Sync {
    /// Recheck administrator permissions inside the identity lock. One pending
    /// link per user, at most one issuance per minute. Return only eligible mail.
    async fn issue(
        &self,
        target: LinkTarget<'_>,
        digest: &str,
        now: OffsetDateTime,
    ) -> Result<Option<String>, UseCaseError>;
    async fn username(
        &self,
        digest: &str,
        now: OffsetDateTime,
    ) -> Result<Option<String>, UseCaseError>;
    /// Consume + password + auth version + session revocation + audit are atomic.
    async fn consume(
        &self,
        digest: &str,
        password_hash: &str,
        now: OffsetDateTime,
        ip: Option<IpAddr>,
    ) -> Result<(), UseCaseError>;
    async fn cancel(&self, digest: &str) -> Result<(), UseCaseError>;
}
#[async_trait]
pub trait AccountMailer: Send + Sync {
    async fn send_link(&self, email: &str, url: &str, invitation: bool)
    -> Result<(), UseCaseError>;
}

pub const LINK_TTL: Duration = Duration::minutes(30);
pub fn invalid_link() -> UseCaseError {
    UseCaseError::Invalid("链接无效或已过期，请重新申请邮件。".into())
}

pub struct AccountLinks {
    pub store: Arc<dyn AccountLinkStore>,
    pub mailer: Arc<dyn AccountMailer>,
    pub random: Arc<dyn SecureRandom>,
    pub hasher: Arc<dyn PasswordHasher>,
    pub clock: Arc<dyn Clock>,
    pub public_url: PublicBaseUrl,
}
impl AccountLinks {
    pub async fn request(&self, target: LinkTarget<'_>) -> Result<(), UseCaseError> {
        let invitation = matches!(target, LinkTarget::Invitation { .. });
        if let LinkTarget::Invitation { actor, .. } = &target {
            actor.ensure_write_channel()?;
            if !actor.has_permission("user.manage") {
                return Err(UseCaseError::Forbidden);
            }
        }
        let token = self.random.token_hex()?;
        let digest = self.random.pkce_s256(&token)?;
        let Some(email) = self.store.issue(target, &digest, self.clock.now()).await? else {
            return Ok(());
        };
        // Fragment is never sent to access logs, the server, or a Referer header.
        let url = format!(
            "{}/admin/#password-reset={token}",
            self.public_url.as_str().trim_end_matches('/')
        );
        if let Err(error) = self.mailer.send_link(&email, &url, invitation).await {
            self.store.cancel(&digest).await?;
            return Err(error);
        }
        Ok(())
    }
    pub async fn reset(
        &self,
        token: &str,
        password: &str,
        ip: Option<IpAddr>,
    ) -> Result<(), UseCaseError> {
        if token.len() != 64 || !token.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(invalid_link());
        }
        let digest = self.random.pkce_s256(token)?;
        let username = self
            .store
            .username(&digest, self.clock.now())
            .await?
            .ok_or_else(invalid_link)?;
        domain::identity::validate_password(password, &username)
            .map_err(|e| UseCaseError::Invalid(e.to_string()))?;
        let hash = self.hasher.hash(password).await?;
        self.store
            .consume(&digest, &hash, self.clock.now(), ip)
            .await
    }
}
