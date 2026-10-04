//! Browser recovery boundary. Recovery sessions and files must remain usable
//! while the business database is unavailable or being replaced.
use crate::UseCaseError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryLogin {
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub installation_token: String,
}

pub struct RecoverySession {
    pub token: String,
    pub csrf: String,
}

#[derive(Clone, Default)]
pub struct RecoveryCredential {
    pub token: String,
    pub csrf: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RecoveryAction {
    Keygen,
    KeyConfirm,
    Backup,
    Inspect,
    Restore,
    Resume,
    Delete,
    DiscardUpload,
    Schedule,
    RemoteSave,
    RemoteList,
    RemoteUpload,
    RemoteDownload,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryCommand {
    pub action: RecoveryAction,
    #[serde(default)]
    pub input: Value,
}

pub struct BackupFile {
    pub path: PathBuf,
    pub size: u64,
    pub name: String,
}

#[async_trait::async_trait]
pub trait RecoveryControl: Send + Sync {
    async fn login(
        &self,
        input: RecoveryLogin,
        client: Option<String>,
    ) -> Result<RecoverySession, UseCaseError>;
    async fn status(&self, credential: RecoveryCredential) -> Result<Value, UseCaseError>;
    async fn execute(
        &self,
        credential: RecoveryCredential,
        command: RecoveryCommand,
    ) -> Result<Value, UseCaseError>;
    async fn authorize_upload(&self, credential: RecoveryCredential) -> Result<(), UseCaseError>;
    async fn upload(
        &self,
        credential: RecoveryCredential,
        id: Option<String>,
        offset: u64,
        complete: bool,
        bytes: Vec<u8>,
    ) -> Result<Value, UseCaseError>;
    async fn download(
        &self,
        credential: RecoveryCredential,
        name: String,
    ) -> Result<BackupFile, UseCaseError>;
    async fn logout(&self, credential: RecoveryCredential) -> Result<(), UseCaseError>;
}
