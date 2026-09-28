//! Private installation journal and durable configuration publication. The
//! journal is published first; its snapshot repairs a crash before TOML publication.
use super::DeploymentConfig;
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
};

const MAX_FILE_SIZE: u64 = 256 * 1024;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallJournal {
    pub installation_id: String,
    config_before: Option<String>,
    config_after: String,
}

impl InstallJournal {
    pub fn path(config: &Path) -> PathBuf {
        config.with_extension("install-state.json")
    }

    pub fn read(config: &Path) -> Result<Option<Self>, String> {
        let Some(text) = read_private(&Self::path(config))? else {
            return Ok(None);
        };
        let journal: Self = serde_json::from_str(&text).map_err(|_| "安装状态记录无效")?;
        validate_id(&journal.installation_id)?;
        journal.pending_database_url()?;
        Ok(Some(journal))
    }

    pub fn prepare(
        config: &DeploymentConfig,
        database_url: &str,
        public_base_url: &str,
        installation_id: String,
    ) -> Result<Self, String> {
        validate_id(&installation_id)?;
        Ok(Self {
            installation_id,
            config_before: config.original.clone(),
            config_after: config.installation_config(database_url, public_base_url)?,
        })
    }

    pub fn deployment(&self, config: &DeploymentConfig) -> Result<DeploymentConfig, String> {
        DeploymentConfig::parse(
            config.path.clone(),
            Some(self.config_after.clone()),
            config.env.clone(),
        )
    }

    pub fn pending_database_url(&self) -> Result<String, String> {
        DeploymentConfig::parse(
            PathBuf::new(),
            Some(self.config_after.clone()),
            Default::default(),
        )?
        .configured_database_url()?
        .ok_or_else(|| "安装记录缺少数据库目标".into())
    }

    pub fn publish(&self, config: &DeploymentConfig) -> Result<(), String> {
        if read_private(&config.path)? != self.config_before {
            return Err("配置文件已被其他进程修改，未覆盖；请重启后重试".into());
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(|_| "无法编码安装状态")?;
        publish_new(&Self::path(&config.path), &bytes)?;
        replace_checked(
            &config.path,
            self.config_before.as_deref(),
            &self.config_after,
        )
    }

    pub fn recover_config(&self, config: &DeploymentConfig) -> Result<DeploymentConfig, String> {
        if config.original == self.config_before {
            replace_checked(
                &config.path,
                self.config_before.as_deref(),
                &self.config_after,
            )?;
            return config.reload();
        }
        if config.original.is_none() {
            return Err("安装配置缺失；请恢复 TOML 配置，不会重新开启安装".into());
        }
        // Once published, the TOML remains operator-owned. Normal edits (URL,
        // credential rotation, recovered DB) must not be replaced by the snapshot.
        Ok(config.clone())
    }
}

fn validate_id(id: &str) -> Result<(), String> {
    if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("安装记录中的 installation_id 无效".into());
    }
    Ok(())
}

pub(super) fn read_private(path: &Path) -> Result<Option<String>, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("无法读取配置/安装记录，请检查路径和权限".into()),
    };
    if !metadata.is_file() || metadata.len() > MAX_FILE_SIZE {
        return Err("配置/安装记录须为不超过 256 KiB 的普通文件，不能是符号链接".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err("配置/安装记录可能包含数据库凭据，请将文件权限设为 600".into());
        }
    }
    let mut text = String::new();
    std::fs::File::open(path)
        .map_err(|_| "无法打开配置/安装记录")?
        .take(MAX_FILE_SIZE + 1)
        .read_to_string(&mut text)
        .map_err(|_| "无法读取配置/安装记录（须为 UTF-8）")?;
    if text.len() as u64 > MAX_FILE_SIZE {
        return Err("配置/安装记录过大".into());
    }
    Ok(Some(text))
}

fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn temporary_file(path: &Path, bytes: &[u8]) -> Result<PathBuf, String> {
    use application::ports::SecureRandom;
    if bytes.len() as u64 > MAX_FILE_SIZE {
        return Err("配置/安装记录过大".into());
    }
    let parent = parent(path);
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(parent).map_err(|_| "无法创建配置目录")?;
    let suffix = infrastructure::SystemSecureRandom
        .token_hex()
        .map_err(|_| "无法生成临时文件名")?;
    let temporary = parent.join(format!(".blog-config-{suffix}.tmp"));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
        return Err("无法安全写入配置；请检查目录权限及磁盘空间".into());
    }
    Ok(temporary)
}

fn sync_parent(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    std::fs::File::open(parent(path))
        .and_then(|file| file.sync_all())
        .map_err(|_| "无法同步配置目录；请重启后检查配置和安装状态")?;
    Ok(())
}

pub(super) fn publish_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let temporary = temporary_file(path, bytes)?;
    // An installation journal is immutable; a concurrent owner always wins.
    let result = std::fs::hard_link(&temporary, path);
    let _ = std::fs::remove_file(&temporary);
    result.map_err(|_| "无法发布配置/安装记录；已有文件不会被覆盖，请重启后检查".to_owned())?;
    sync_parent(path)
}

fn replace_checked(path: &Path, before: Option<&str>, after: &str) -> Result<(), String> {
    if before.is_none() {
        return publish_new(path, after.as_bytes());
    }
    let temporary = temporary_file(path, after.as_bytes())?;
    let result = (|| {
        if read_private(path)?.as_deref() != before {
            return Err("配置文件已被修改，未覆盖；请重启后检查".into());
        }
        std::fs::rename(&temporary, path).map_err(|_| "无法发布 TOML 配置".to_owned())?;
        sync_parent(path)
    })();
    let _ = std::fs::remove_file(&temporary);
    result
}
