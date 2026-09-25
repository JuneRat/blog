//! 媒体附着授权的测试替身：`MediaRefGuard` 只回答归属与公开性，
//! 因此 fake 也只登记这两项；未登记的 id 视为「不存在或未就绪」，
//! 与生产侧 attachable_status 只看 `ready` 资产的语义一致。
//!
//! 各测试二进制按需使用登记方法（不是每个文件都用全），允许死代码。
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Mutex;

use application::error::UseCaseError;
use application::ports::{MediaAttachStatus, MediaRefGuard};
use uuid::Uuid;

pub struct FakeMediaGuard {
    entries: Mutex<HashMap<Uuid, MediaAttachStatus>>,
}

impl FakeMediaGuard {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// 登记一个可附着的资产（owner 与公开性）。
    pub fn allow(&self, id: Uuid, owner_id: Uuid, publicly_referenced: bool) {
        self.entries.lock().unwrap().insert(
            id,
            MediaAttachStatus {
                owner_id,
                publicly_referenced,
            },
        );
    }

    /// 登记一个属于 owner 的**私有**资产（最常见的越权场景素材）。
    pub fn allow_private(&self, id: Uuid, owner_id: Uuid) {
        self.allow(id, owner_id, false);
    }

    /// 登记一个已有公开来源引用的资产（任何人附着都不扩大暴露面）。
    pub fn allow_public(&self, id: Uuid, owner_id: Uuid) {
        self.allow(id, owner_id, true);
    }
}

impl Default for FakeMediaGuard {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl MediaRefGuard for FakeMediaGuard {
    async fn attachable_status(&self, id: Uuid) -> Result<Option<MediaAttachStatus>, UseCaseError> {
        Ok(self.entries.lock().unwrap().get(&id).copied())
    }
}
