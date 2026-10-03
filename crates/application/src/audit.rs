//! Explicit write provenance and bounded, read-only audit queries.
use crate::{error::UseCaseError, identity::Actor};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{net::IpAddr, sync::Arc};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

/// Supplied by a trusted inbound adapter, never deserialized from a request body.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuditContext {
    pub actor_id: Option<Uuid>,
    pub ip_address: Option<IpAddr>,
    /// Captured by identity resolution, never supplied by an HTTP payload.
    /// Trusted system/bootstrap operations have no user authorization snapshot.
    pub authorization: Option<Arc<WriteAuthorization>>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct WriteAuthorization {
    pub auth_version: i64,
    pub permissions: domain::identity::PermissionSet,
    pub channel: crate::identity::ActorChannel,
}
impl AuditContext {
    pub const fn system() -> Self {
        Self {
            actor_id: None,
            ip_address: None,
            authorization: None,
        }
    }
}
impl From<Option<Uuid>> for AuditContext {
    fn from(actor_id: Option<Uuid>) -> Self {
        Self {
            actor_id,
            ip_address: None,
            authorization: None,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditQuery {
    pub action: Option<String>,
    pub actor_id: Option<Uuid>,
    #[serde(default)]
    pub without_actor: bool,
    pub target_type: Option<String>,
    pub target_id: Option<String>,
    pub from: Option<String>,
    pub until: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct AuditFilter {
    pub action: Option<String>,
    pub actor_id: Option<Uuid>,
    pub without_actor: bool,
    pub target_type: Option<String>,
    pub target_id: Option<String>,
    pub from: Option<OffsetDateTime>,
    pub until: Option<OffsetDateTime>,
    pub before: Option<(OffsetDateTime, Uuid)>,
    pub limit: u32,
}
impl TryFrom<AuditQuery> for AuditFilter {
    type Error = UseCaseError;
    fn try_from(q: AuditQuery) -> Result<Self, Self::Error> {
        let invalid = || UseCaseError::Invalid("审计筛选参数无效".into());
        fn text(value: Option<String>, max: usize) -> Result<Option<String>, UseCaseError> {
            value
                .map(|s| {
                    let s = s.trim();
                    if s.is_empty() || s.chars().count() > max || s.chars().any(char::is_control) {
                        Err(UseCaseError::Invalid("审计筛选字段为空或过长".into()))
                    } else {
                        Ok(s.to_owned())
                    }
                })
                .transpose()
        }
        fn date(value: &str) -> Result<OffsetDateTime, UseCaseError> {
            OffsetDateTime::parse(value, &Rfc3339)
                .map_err(|_| UseCaseError::Invalid("时间须为含时区的 RFC3339 格式".into()))
        }
        let limit = q.limit.unwrap_or(50);
        if !(1..=100).contains(&limit) || (q.without_actor && q.actor_id.is_some()) {
            return Err(invalid());
        }
        let from = q.from.as_deref().map(date).transpose()?;
        let until = q.until.as_deref().map(date).transpose()?;
        if let (Some(a), Some(b)) = (from, until)
            && a >= b
        {
            return Err(invalid());
        }
        let before = q
            .cursor
            .map(|cursor| {
                if cursor.len() > 100 {
                    return Err(invalid());
                }
                let (time, id) = cursor.split_once('|').ok_or_else(invalid)?;
                Ok((date(time)?, Uuid::parse_str(id).map_err(|_| invalid())?))
            })
            .transpose()?;
        Ok(Self {
            action: text(q.action, 128)?,
            actor_id: q.actor_id,
            without_actor: q.without_actor,
            target_type: text(q.target_type, 64)?,
            target_id: text(q.target_id, 256)?,
            from,
            until,
            before,
            limit,
        })
    }
}

/// Metadata is a display summary, not a copy of business entities or credentials.
#[derive(Debug, Serialize)]
pub struct AuditField {
    pub key: String,
    pub value: String,
}
#[derive(Debug, Serialize)]
pub struct AuditRecord {
    pub id: Uuid,
    pub created_at: String,
    pub actor_id: Option<Uuid>,
    pub actor_display: Option<String>,
    pub ip_address: Option<String>,
    pub action: String,
    pub target_type: String,
    pub target_id: String,
    pub summary: Vec<AuditField>,
}
#[derive(Debug, Serialize)]
pub struct AuditPage {
    pub items: Vec<AuditRecord>,
    pub next_cursor: Option<String>,
}
#[async_trait]
pub trait AuditQueryStore: Send + Sync {
    async fn list(&self, filter: &AuditFilter) -> Result<AuditPage, UseCaseError>;
}
pub struct AuditInteractor {
    store: Arc<dyn AuditQueryStore>,
}
impl AuditInteractor {
    pub fn new(store: Arc<dyn AuditQueryStore>) -> Self {
        Self { store }
    }
    pub async fn list(&self, actor: &Actor, query: AuditQuery) -> Result<AuditPage, UseCaseError> {
        if !actor.has_permission("audit.read") {
            return Err(UseCaseError::Forbidden);
        }
        self.store.list(&AuditFilter::try_from(query)?).await
    }
}
