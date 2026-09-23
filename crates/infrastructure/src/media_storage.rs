//! 本地文件系统媒体存储。
//!
//! ```text
//! <root>/
//! ├── staging/<uuid>.<ext>   # 上传暂存；promote 成功后不再存在
//! └── objects/<uuid>.<ext>   # 正式对象，文件名只能是随机 id
//! ```
//!
//! 暂存与正式分离让「写入完成」与「可以被引用」成为两步：只有 promote 成功后
//! 应用层才把资产标记为 ready，因此不存在「记录可用但文件没写完」的窗口。
//! 删除同时清理两处，因此失败重试与中断补偿是同一个操作。

use std::path::{Component, Path, PathBuf};

use application::error::UseCaseError;
use application::ports::MediaStorage;
use async_trait::async_trait;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

/// 暂存子目录名（相对媒体根）。
const STAGING_DIR: &str = "staging";

pub struct LocalMediaStorage {
    root: PathBuf,
}

impl LocalMediaStorage {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 把存储 key 映射为根目录下的正式路径。
    ///
    /// 只接受普通相对路径段：拒绝绝对路径、`..`、空段与平台前缀。key 由应用层按
    /// 随机 id 生成，这里是纵深防御——存储层不应依赖调用方守规矩。
    fn resolve(&self, key: &str) -> Result<PathBuf, UseCaseError> {
        let relative = Path::new(key);
        if relative.as_os_str().is_empty() || relative.is_absolute() {
            return Err(invalid_key(key));
        }
        for component in relative.components() {
            if !matches!(component, Component::Normal(_)) {
                return Err(invalid_key(key));
            }
        }
        Ok(self.root.join(relative))
    }

    /// 暂存路径：与正式文件同目录层级无关，统一平铺在 `<root>/staging/` 下。
    fn staged_path(&self, key: &str) -> Result<PathBuf, UseCaseError> {
        let resolved = self.resolve(key)?;
        let file_name = resolved.file_name().ok_or_else(|| invalid_key(key))?;
        Ok(self.root.join(STAGING_DIR).join(file_name))
    }
}

#[async_trait]
impl MediaStorage for LocalMediaStorage {
    async fn put_staged(&self, key: &str, bytes: &[u8]) -> Result<String, UseCaseError> {
        let staged = self.staged_path(key)?;
        let dir = staged.parent().ok_or_else(|| invalid_key(key))?;
        tokio::fs::create_dir_all(dir).await.map_err(map_io)?;
        // 先写 `.part` 再重命名：半截文件永远不会出现在暂存位置被 promote。
        let temp = staged.with_extension("part");
        tokio::fs::write(&temp, bytes).await.map_err(map_io)?;
        tokio::fs::rename(&temp, &staged).await.map_err(map_io)?;
        Ok(format!("{:x}", Sha256::digest(bytes)))
    }

    async fn promote(&self, key: &str) -> Result<(), UseCaseError> {
        let staged = self.staged_path(key)?;
        let target = self.resolve(key)?;
        if tokio::fs::try_exists(&target).await.map_err(map_io)? {
            // 幂等：正式文件已就位时清理暂存残留并视为成功。
            let _ = tokio::fs::remove_file(&staged).await;
            return Ok(());
        }
        if let Some(parent) = target.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(map_io)?;
        }
        match tokio::fs::rename(&staged, &target).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(UseCaseError::Repository(
                format!("上传暂存文件缺失，无法完成存储：{key}"),
            )),
            Err(e) => Err(map_io(e)),
        }
    }

    async fn read(&self, key: &str) -> Result<Option<Vec<u8>>, UseCaseError> {
        let target = self.resolve(key)?;
        match tokio::fs::read(&target).await {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(map_io(e)),
        }
    }

    async fn delete(&self, key: &str) -> Result<(), UseCaseError> {
        let target = self.resolve(key)?;
        let staged = self.staged_path(key)?;
        // 两处都不存在即成功：回收流程会被反复重放，删除必须幂等。
        remove_if_exists(&target).await?;
        remove_if_exists(&staged).await?;
        Ok(())
    }

    async fn discard_orphaned_staging(
        &self,
        older_than: OffsetDateTime,
    ) -> Result<i64, UseCaseError> {
        let dir = self.root.join(STAGING_DIR);
        let mut entries = match tokio::fs::read_dir(&dir).await {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(map_io(e)),
        };
        let mut removed = 0i64;
        while let Some(entry) = entries.next_entry().await.map_err(map_io)? {
            let metadata = entry.metadata().await.map_err(map_io)?;
            if !metadata.is_file() {
                continue;
            }
            // 只看修改时间：更新的文件可能属于正在进行的上传（包括「已写入、
            // 尚未插入行」的窗口），一律不动。
            let modified = metadata.modified().map_err(map_io)?;
            if OffsetDateTime::from(modified) >= older_than {
                continue;
            }
            match tokio::fs::remove_file(entry.path()).await {
                Ok(()) => removed += 1,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(map_io(e)),
            }
        }
        Ok(removed)
    }
}

async fn remove_if_exists(path: &Path) -> Result<(), UseCaseError> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(map_io(e)),
    }
}

fn invalid_key(key: &str) -> UseCaseError {
    UseCaseError::Repository(format!("非法媒体存储路径：{key}"))
}

fn map_io(e: std::io::Error) -> UseCaseError {
    UseCaseError::Repository(format!("媒体文件操作失败：{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("blog-media-{name}-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn staged_write_promote_read_and_idempotent_delete() {
        let root = temp_root("roundtrip");
        let storage = LocalMediaStorage::new(&root);
        let key = "objects/00000000-0000-0000-0000-000000000001.png";

        let checksum = storage.put_staged(key, b"png-bytes").await.unwrap();
        assert_eq!(checksum.len(), 64);
        assert!(
            root.join("staging")
                .join("00000000-0000-0000-0000-000000000001.png")
                .is_file()
        );
        assert_eq!(
            storage.read(key).await.unwrap(),
            None,
            "promote 之前不可读取，正式位置必须是空的"
        );

        storage.promote(key).await.unwrap();
        assert_eq!(
            storage.read(key).await.unwrap().as_deref(),
            Some(&b"png-bytes"[..])
        );

        // 重复 promote 幂等（正式已存在时清理暂存）。
        storage.put_staged(key, b"png-bytes").await.unwrap();
        storage.promote(key).await.unwrap();
        assert_eq!(
            storage.read(key).await.unwrap().as_deref(),
            Some(&b"png-bytes"[..])
        );

        storage.delete(key).await.unwrap();
        storage.delete(key).await.unwrap();
        assert_eq!(storage.read(key).await.unwrap(), None);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn promote_without_staged_file_is_a_loud_failure() {
        let root = temp_root("missing-staged");
        let storage = LocalMediaStorage::new(&root);
        let err = storage
            .promote("objects/00000000-0000-0000-0000-000000000002.png")
            .await
            .unwrap_err();
        assert!(matches!(err, UseCaseError::Repository(_)));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn rejects_keys_that_escape_the_root() {
        let storage = LocalMediaStorage::new(temp_root("escape"));
        for key in ["../etc/passwd", "/etc/passwd", "objects/../../x.png", ""] {
            let err = storage.put_staged(key, b"x").await.unwrap_err();
            assert!(
                matches!(err, UseCaseError::Repository(ref m) if m.contains("非法媒体存储路径")),
                "{key} 必须被拒绝，实际：{err:?}"
            );
        }
    }
}
