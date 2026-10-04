use uuid::Uuid;
#[derive(serde::Serialize, ts_rs::TS)]
pub struct AuditField {
    pub key: String,
    pub value: String,
}
impl From<application::audit::AuditField> for AuditField {
    fn from(value: application::audit::AuditField) -> Self {
        let application::audit::AuditField { key, value } = value;
        Self { key, value }
    }
}
#[derive(serde::Serialize, ts_rs::TS)]
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
impl From<application::audit::AuditRecord> for AuditRecord {
    fn from(value: application::audit::AuditRecord) -> Self {
        let application::audit::AuditRecord {
            id,
            created_at,
            actor_id,
            actor_display,
            ip_address,
            action,
            target_type,
            target_id,
            summary,
        } = value;
        Self {
            id,
            created_at,
            actor_id,
            actor_display,
            ip_address,
            action,
            target_type,
            target_id,
            summary: summary.into_iter().map(Into::into).collect(),
        }
    }
}
#[derive(serde::Serialize, ts_rs::TS)]
pub struct AuditPage {
    pub items: Vec<AuditRecord>,
    pub next_cursor: Option<String>,
}
impl From<application::audit::AuditPage> for AuditPage {
    fn from(value: application::audit::AuditPage) -> Self {
        let application::audit::AuditPage { items, next_cursor } = value;
        Self {
            items: items.into_iter().map(Into::into).collect(),
            next_cursor,
        }
    }
}
