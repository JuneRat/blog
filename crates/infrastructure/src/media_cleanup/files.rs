use application::{UseCaseError, media_cleanup::*};
use async_trait::async_trait;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub struct LocalMediaPurgePlans;
pub struct LocalMediaPurgeFiles {
    root: PathBuf,
}
impl LocalMediaPurgeFiles {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}
fn invalid(message: impl Into<String>) -> UseCaseError {
    UseCaseError::Invalid(message.into())
}
fn io(error: std::io::Error) -> UseCaseError {
    UseCaseError::Repository(error.to_string())
}

async fn blocking<T: Send + 'static>(
    action: impl FnOnce() -> Result<T, UseCaseError> + Send + 'static,
) -> Result<T, UseCaseError> {
    tokio::task::spawn_blocking(action)
        .await
        .map_err(|error| UseCaseError::Repository(error.to_string()))?
}

fn checksum(value: &Value) -> Result<String, UseCaseError> {
    // serde_json's default map is sorted, matching Python's sort_keys=True,
    // separators=(",",":"), ensure_ascii=False for the version-1 plan schema.
    let bytes = serde_json::to_vec(value).map_err(|e| invalid(e.to_string()))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

#[async_trait]
impl MediaPurgePlans for LocalMediaPurgePlans {
    async fn load(&self, path: &Path) -> Result<VerifiedPlan, UseCaseError> {
        let path = path.to_owned();
        blocking(move || {
            let file = File::open(path).map_err(io)?;
            if file.metadata().map_err(io)?.len() > 16 * 1024 * 1024 {
                return Err(invalid("plan is too large"));
            }
            let document: Value =
                serde_json::from_reader(file).map_err(|e| invalid(e.to_string()))?;
            let raw = document
                .get("plan")
                .ok_or_else(|| invalid("plan missing"))?;
            let sha256 = checksum(raw)?;
            if document.get("sha256").and_then(Value::as_str) != Some(&sha256) {
                return Err(invalid("unsupported or damaged media cleanup plan"));
            }
            let plan: PurgePlan =
                serde_json::from_value(raw.clone()).map_err(|e| invalid(e.to_string()))?;
            plan.validate()?;
            Ok(VerifiedPlan { plan, sha256 })
        })
        .await
    }
    async fn save(&self, path: &Path, plan: &PurgePlan) -> Result<(), UseCaseError> {
        let path = path.to_owned();
        let plan = plan.clone();
        blocking(move || {
            plan.validate()?;
            let raw = serde_json::to_value(&plan).map_err(|e| invalid(e.to_string()))?;
            let document = serde_json::json!({"sha256":checksum(&raw)?,"plan":raw});
            let bytes = serde_json::to_vec_pretty(&document).map_err(|e| invalid(e.to_string()))?;
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(path).map_err(io)?;
            file.write_all(&bytes).map_err(io)?;
            file.write_all(b"\n").map_err(io)?;
            file.sync_all().map_err(io)
        })
        .await
    }
}

fn safe_path(root: &str, item: &PurgeItem) -> Result<PathBuf, UseCaseError> {
    let mut path = PathBuf::from(root);
    if !path.is_absolute() || !fs::symlink_metadata(&path).map_err(io)?.is_dir() {
        return Err(invalid(
            "media root must be an existing directory, not a symbolic link",
        ));
    }
    if item.path.is_empty() {
        return Err(invalid("empty media path"));
    }
    for part in item.path.split('/') {
        if part.is_empty()
            || matches!(part, "." | "..")
            || !Path::new(part)
                .components()
                .all(|component| matches!(component, std::path::Component::Normal(_)))
        {
            return Err(invalid("unsafe media path"));
        }
        path.push(part);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(invalid("media path contains a symbolic link"));
            }
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(io(error)),
        }
    }
    Ok(path)
}
fn validate(
    root: &str,
    item: &PurgeItem,
    missing_ok: bool,
) -> Result<Option<PathBuf>, UseCaseError> {
    let path = safe_path(root, item)?;
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && missing_ok => {
            return Ok(None);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(invalid("media file missing or changed"));
        }
        Err(error) => return Err(io(error)),
    };
    if !metadata.is_file() || metadata.len() != item.size as u64 {
        return Err(invalid("media file missing or changed"));
    }
    let mut file = File::open(&path).map_err(io)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let read = file.read(&mut buffer).map_err(io)?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    if format!("{:x}", hash.finalize()) != item.sha256 {
        return Err(invalid("media file missing or changed"));
    }
    Ok(Some(path))
}
#[async_trait]
impl MediaPurgeFiles for LocalMediaPurgeFiles {
    async fn root(&self) -> Result<String, UseCaseError> {
        let root = self.root.clone();
        blocking(move || {
            if !fs::symlink_metadata(&root).map_err(io)?.is_dir() {
                return Err(invalid(
                    "media root must be a directory, not a symbolic link",
                ));
            }
            let path = fs::canonicalize(root).map_err(io)?;
            path.to_str()
                .map(str::to_owned)
                .ok_or_else(|| invalid("media root is not UTF-8"))
        })
        .await
    }
    async fn validate(
        &self,
        root: &str,
        item: &PurgeItem,
        missing_ok: bool,
    ) -> Result<(), UseCaseError> {
        let root = root.to_owned();
        let item = item.clone();
        blocking(move || validate(&root, &item, missing_ok).map(|_| ())).await
    }
    async fn remove(&self, root: &str, item: &PurgeItem) -> Result<bool, UseCaseError> {
        let root = root.to_owned();
        let item = item.clone();
        blocking(move || match validate(&root, &item, true)? {
            Some(path) => {
                fs::remove_file(path).map_err(io)?;
                Ok(true)
            }
            None => Ok(false),
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_python_v1_plan_checksum_without_rewriting_receipt_identity() {
        // Generated by the former Python tool, including Unicode and a quoted path.
        let raw: Value = serde_json::from_str(r#"{"format":1,"operation_id":"33333333-3333-4333-8333-333333333333","database":{"name":"old","oid":"42","endpoint":{"host":"localhost","port":"5432"}},"media_root":"/media","created_at":"2026-01-01T00:00:00+00:00","items":[{"id":"11111111-1111-4111-8111-111111111111","path":"objects/有'引号.png","size":11,"sha256":"0000000000000000000000000000000000000000000000000000000000000000","version":2,"deleted_at":"2026-01-01T00:00:00.000000Z"}]}"#).unwrap();
        let expected = "60ad2453fe095064bc86900b6e9ed6d051a6e50530c0cfae4f00f3ffcf375162";
        assert_eq!(checksum(&raw).unwrap(), expected);
        let plan: PurgePlan = serde_json::from_value(raw).unwrap();
        plan.validate().unwrap();
        assert_eq!(
            checksum(&serde_json::to_value(plan).unwrap()).unwrap(),
            expected
        );
    }
}
