//! 新引用只要求媒体存在且未进入回收站；公开链接没有上传者限制。
#![allow(dead_code)]
use application::{error::UseCaseError, ports::MediaRefGuard};
use std::{collections::HashSet, sync::Mutex};
use uuid::Uuid;
#[derive(Default)]
pub struct FakeMediaGuard {
    entries: Mutex<HashSet<Uuid>>,
}
impl FakeMediaGuard {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn allow(&self, id: Uuid) {
        self.entries.lock().unwrap().insert(id);
    }
    pub fn trash(&self, id: Uuid) {
        self.entries.lock().unwrap().remove(&id);
    }
}
#[async_trait::async_trait]
impl MediaRefGuard for FakeMediaGuard {
    async fn is_attachable(&self, id: Uuid) -> Result<bool, UseCaseError> {
        Ok(self.entries.lock().unwrap().contains(&id))
    }
}
