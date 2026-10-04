//! Explicit media purge: validate every object, commit deletion receipts, then remove files.
//! The operator owns the maintenance window; a retry always uses the original plan.
use crate::{UseCaseError, ports::Clock};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, path::Path, sync::Arc};
use uuid::Uuid;

pub const PLAN_FORMAT: u32 = 1;
pub const MAX_ITEMS: usize = 1000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DatabaseEndpoint {
    Tcp { host: String, port: String },
    Container { container: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatabaseIdentity {
    pub name: String,
    pub oid: String,
    pub endpoint: DatabaseEndpoint,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PurgeItem {
    pub id: Uuid,
    pub path: String,
    pub size: i64,
    pub sha256: String,
    pub version: i64,
    pub deleted_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PurgePlan {
    pub format: u32,
    pub operation_id: Uuid,
    pub database: DatabaseIdentity,
    pub media_root: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    pub items: Vec<PurgeItem>,
}

impl PurgePlan {
    pub fn validate(&self) -> Result<(), UseCaseError> {
        if self.format != PLAN_FORMAT || !Path::new(&self.media_root).is_absolute() {
            return Err(invalid("unsupported plan format or media root"));
        }
        let ids: BTreeSet<_> = self.items.iter().map(|item| item.id).collect();
        let paths: BTreeSet<_> = self.items.iter().map(|item| &item.path).collect();
        if !(1..=MAX_ITEMS).contains(&self.items.len())
            || ids.len() != self.items.len()
            || paths.len() != self.items.len()
        {
            return Err(invalid(
                "plan must contain 1–1000 distinct media IDs and paths",
            ));
        }
        for item in &self.items {
            if item.deleted_at.as_ref().is_none_or(|date| date.is_empty())
                || item.version < 1
                || item.size < 0
                || item.sha256.len() != 64
                || !item.sha256.bytes().all(|b| b.is_ascii_hexdigit())
                || item.path.is_empty()
                || Path::new(&item.path).is_absolute()
                || item
                    .path
                    .split('/')
                    .any(|part| part.is_empty() || matches!(part, "." | ".."))
            {
                return Err(invalid("invalid media snapshot in plan"));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct VerifiedPlan {
    pub plan: PurgePlan,
    pub sha256: String,
}

#[async_trait]
pub trait MediaPurgePlans: Send + Sync {
    /// Verify the complete canonical JSON checksum before decoding the plan.
    async fn load(&self, path: &Path) -> Result<VerifiedPlan, UseCaseError>;
    /// Exclusive, private creation; never replace an existing or partly applied plan.
    async fn save(&self, path: &Path, plan: &PurgePlan) -> Result<(), UseCaseError>;
}

#[async_trait]
pub trait MediaPurgeStore: Send + Sync {
    /// Return current database identity, refusing a recovery-isolated database.
    async fn identity(&self) -> Result<DatabaseIdentity, UseCaseError>;
    /// Refuse known references, including historical and explicit foreign-key references.
    async fn candidates(&self, ids: &[Uuid]) -> Result<Vec<PurgeItem>, UseCaseError>;
    async fn has_receipt(
        &self,
        plan: &VerifiedPlan,
        item: &PurgeItem,
    ) -> Result<bool, UseCaseError>;
    /// Atomic business commit: serialize with restore/reference writers, recheck identity,
    /// version, trash state and references; delete all rows and append all receipts together.
    /// Existing receipts permit retry, but an absent row alone does not. This is a transaction
    /// requirement, not a particular lock implementation. An uncertain commit returns an error.
    async fn commit(&self, plan: &VerifiedPlan) -> Result<(), UseCaseError>;
    /// Verify the receipt still exists and neither ID nor storage path has been registered again.
    async fn may_remove_file(
        &self,
        plan: &VerifiedPlan,
        item: &PurgeItem,
    ) -> Result<bool, UseCaseError>;
}

#[async_trait]
pub trait MediaPurgeFiles: Send + Sync {
    async fn root(&self) -> Result<String, UseCaseError>;
    /// Check path containment, symlinks, size and checksum; only a committed receipt allows absence.
    async fn validate(
        &self,
        root: &str,
        item: &PurgeItem,
        missing_ok: bool,
    ) -> Result<(), UseCaseError>;
    /// Recheck the object immediately before unlink. Return false if already absent.
    async fn remove(&self, root: &str, item: &PurgeItem) -> Result<bool, UseCaseError>;
}

#[derive(Debug, Serialize)]
pub struct PurgeFailure {
    pub id: Uuid,
    pub error: String,
}
#[derive(Debug, Serialize)]
pub struct PurgeResult {
    pub operation_id: Uuid,
    pub records_purged: usize,
    pub files_deleted: usize,
    pub files_already_absent: usize,
    pub failures: Vec<PurgeFailure>,
}

pub struct MediaCleanup {
    store: Arc<dyn MediaPurgeStore>,
    files: Arc<dyn MediaPurgeFiles>,
    plans: Arc<dyn MediaPurgePlans>,
    clock: Arc<dyn Clock>,
}

impl MediaCleanup {
    pub fn new(
        store: Arc<dyn MediaPurgeStore>,
        files: Arc<dyn MediaPurgeFiles>,
        plans: Arc<dyn MediaPurgePlans>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            store,
            files,
            plans,
            clock,
        }
    }

    pub async fn plan(&self, ids: Vec<Uuid>, output: &Path) -> Result<PurgePlan, UseCaseError> {
        let ids: Vec<_> = ids
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if !(1..=MAX_ITEMS).contains(&ids.len()) {
            return Err(invalid("select 1–1000 media IDs explicitly"));
        }
        let database = self.store.identity().await?;
        let media_root = self.files.root().await?;
        let items = self.store.candidates(&ids).await?;
        if items.iter().map(|item| item.id).collect::<BTreeSet<_>>()
            != ids.iter().copied().collect()
        {
            return Err(invalid("one or more selected media records do not exist"));
        }
        let plan = PurgePlan {
            format: PLAN_FORMAT,
            operation_id: Uuid::now_v7(),
            database,
            media_root,
            items,
            created_at: Some(
                self.clock
                    .now()
                    .format(&time::format_description::well_known::Rfc3339)
                    .map_err(|error| invalid(&error.to_string()))?,
            ),
        };
        plan.validate()?;
        for item in &plan.items {
            self.files.validate(&plan.media_root, item, false).await?;
        }
        self.plans.save(output, &plan).await?;
        Ok(plan)
    }

    pub async fn apply(
        &self,
        path: &Path,
        maintenance_confirmed: bool,
        break_links_confirmed: bool,
    ) -> Result<PurgeResult, UseCaseError> {
        if !maintenance_confirmed || !break_links_confirmed {
            return Err(invalid(
                "stop every writer and confirm maintenance and permanent link removal",
            ));
        }
        let verified = self.plans.load(path).await?;
        let plan = &verified.plan;
        plan.validate()?;
        if self.store.identity().await? != plan.database {
            return Err(invalid("plan belongs to a different database/endpoint"));
        }
        for item in &plan.items {
            let committed = self.store.has_receipt(&verified, item).await?;
            self.files
                .validate(&plan.media_root, item, committed)
                .await?;
        }
        self.store.commit(&verified).await?;
        let mut result = PurgeResult {
            operation_id: plan.operation_id,
            records_purged: plan.items.len(),
            files_deleted: 0,
            files_already_absent: 0,
            failures: vec![],
        };
        for item in &plan.items {
            let removed = async {
                if !self.store.may_remove_file(&verified, item).await? {
                    return Err(invalid(
                        "purge receipt missing or storage path is registered again",
                    ));
                }
                self.files.remove(&plan.media_root, item).await
            }
            .await;
            match removed {
                Ok(true) => result.files_deleted += 1,
                Ok(false) => result.files_already_absent += 1,
                Err(error) => result.failures.push(PurgeFailure {
                    id: item.id,
                    error: error.to_string(),
                }),
            }
        }
        Ok(result)
    }
}

fn invalid(message: &str) -> UseCaseError {
    UseCaseError::Invalid(message.into())
}
