//! 本地文件系统媒体存储。
//!
//! ```text
//! <root>/
//! ├── staging/<uuid>.<ext>   # 上传暂存；promote 成功后不再存在
//! └── objects/<uuid>.<ext>   # 正式对象，文件名只能是随机 id
//! ```
//!
//! 暂存与正式分离让「写入完成」与「可以被引用」成为两步：只有 promote 成功后
//! 应用层才登记媒体行，因此不存在「记录可用但文件没写完」的窗口。
//! 回收站不调用物理删除；常规维护只清理超期暂存文件。

use std::path::{Component, Path, PathBuf};

use application::error::UseCaseError;
use application::ports::{MediaReader, MediaStorage, OpenedMedia};
use async_trait::async_trait;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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
        durable_directory(dir).await?;
        // 先写 `.part` 再重命名：半截文件永远不会出现在暂存位置被 promote。
        let temp = staged.with_extension("part");
        let mut file = tokio::fs::File::create(&temp).await.map_err(map_io)?;
        file.write_all(bytes).await.map_err(map_io)?;
        file.sync_all().await.map_err(map_io)?;
        drop(file);
        tokio::fs::rename(&temp, &staged).await.map_err(map_io)?;
        sync_directory(dir).await?;
        Ok(format!("{:x}", Sha256::digest(bytes)))
    }

    async fn promote(&self, key: &str) -> Result<(), UseCaseError> {
        let staged = self.staged_path(key)?;
        let target = self.resolve(key)?;
        if tokio::fs::try_exists(&target).await.map_err(map_io)? {
            // 幂等：正式文件已就位时清理暂存残留并视为成功。
            let _ = tokio::fs::remove_file(&staged).await;
            tokio::fs::File::open(&target)
                .await
                .map_err(map_io)?
                .sync_all()
                .await
                .map_err(map_io)?;
            sync_directory(target.parent().ok_or_else(|| invalid_key(key))?).await?;
            if let Some(parent) = staged.parent() {
                sync_directory(parent).await?;
            }
            return Ok(());
        }
        if let Some(parent) = target.parent() {
            durable_directory(parent).await?;
        }
        match tokio::fs::rename(&staged, &target).await {
            Ok(()) => {
                // Atomic rename alone is not durable. Persist both directory
                // entries before the caller is allowed to commit media metadata.
                sync_directory(target.parent().ok_or_else(|| invalid_key(key))?).await?;
                sync_directory(staged.parent().ok_or_else(|| invalid_key(key))?).await
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(UseCaseError::Repository(
                format!("上传暂存文件缺失，无法完成存储：{key}"),
            )),
            Err(e) => Err(map_io(e)),
        }
    }

    async fn open(&self, key: &str) -> Result<Option<OpenedMedia>, UseCaseError> {
        let target = self.resolve(key)?;
        let mut file = match tokio::fs::File::open(&target).await {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(map_io(e)),
        };
        let metadata = file.metadata().await.map_err(map_io)?;
        if !metadata.is_file() {
            return Err(UseCaseError::Repository("媒体对象不是普通文件".into()));
        }
        // Bound Tokio's internal blocking file buffer as well as the caller's chunks.
        file.set_max_buf_size(64 * 1024);
        Ok(Some(OpenedMedia {
            byte_size: metadata.len(),
            reader: Box::new(LocalMediaReader { file }),
        }))
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

struct LocalMediaReader {
    file: tokio::fs::File,
}

#[async_trait]
impl MediaReader for LocalMediaReader {
    async fn read_chunk(&mut self, max_bytes: usize) -> Result<Option<Vec<u8>>, UseCaseError> {
        if max_bytes == 0 {
            return Err(UseCaseError::Repository("媒体读取块上限必须为正数".into()));
        }
        let mut bytes = vec![0; max_bytes.min(64 * 1024)];
        let read = self.file.read(&mut bytes).await.map_err(map_io)?;
        bytes.truncate(read);
        Ok((read != 0).then_some(bytes))
    }
}

async fn sync_directory(path: &Path) -> Result<(), UseCaseError> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || std::fs::File::open(path)?.sync_all())
        .await
        .map_err(|e| UseCaseError::Repository(e.to_string()))?
        .map_err(map_io)
}

async fn durable_directory(path: &Path) -> Result<(), UseCaseError> {
    tokio::fs::create_dir_all(path).await.map_err(map_io)?;
    // Newly created ancestors must themselves be linked durably. Include '.'
    // for a relative media root whose parent is represented by an empty path.
    // Also sync existing directories: another upload may have just created them
    // without finishing its sync, or a prior attempt may have failed midway.
    for ancestor in path.ancestors() {
        sync_directory(if ancestor.as_os_str().is_empty() {
            Path::new(".")
        } else {
            ancestor
        })
        .await?;
    }
    Ok(())
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
    async fn opened_files_report_length_and_are_read_in_bounded_chunks() {
        let root = temp_root("stream");
        let storage = LocalMediaStorage::new(&root);
        tokio::fs::create_dir_all(root.join("objects"))
            .await
            .unwrap();
        let key = "objects/image.png";
        let bytes = vec![7; 3 * 64 * 1024 + 19];
        tokio::fs::write(root.join(key), &bytes).await.unwrap();
        let mut opened = storage.open(key).await.unwrap().unwrap();
        assert_eq!(opened.byte_size, bytes.len() as u64);
        assert!(opened.reader.read_chunk(0).await.is_err());
        let mut actual = Vec::new();
        while let Some(chunk) = opened.reader.read_chunk(4096).await.unwrap() {
            assert!(!chunk.is_empty() && chunk.len() <= 4096);
            actual.extend(chunk);
        }
        assert_eq!(actual, bytes);
        assert!(opened.reader.read_chunk(4096).await.unwrap().is_none());
        assert!(storage.open("objects/missing.png").await.unwrap().is_none());
        assert!(
            storage.open("objects").await.is_err(),
            "directories cannot masquerade as media"
        );
        drop(opened);
        tokio::fs::remove_dir_all(root).await.unwrap();
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
