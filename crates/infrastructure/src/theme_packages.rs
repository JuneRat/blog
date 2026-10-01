//! Bounded ZIP preflight, atomic directory publication and live theme snapshots.
use std::{
    collections::{BTreeMap, HashSet},
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

use application::{
    UseCaseError,
    audit::AuditContext,
    theme_data::ThemeData,
    themes::{
        MAX_THEME_PACKAGE_BYTES, ThemeOperationGuard, ThemePackageReport, ThemePackages,
        ThemeRegistry,
    },
};
use async_trait::async_trait;
use tokio::sync::{Mutex, OwnedMutexGuard, Semaphore};

use crate::{MiniJinjaThemeRenderer, RenderingRuntime};

const MAX_FILES: usize = 512;
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_EXPANDED_BYTES: u64 = 32 * 1024 * 1024;
const MAX_INSTALLED: usize = 32;

pub struct LocalThemePackages {
    root: PathBuf,
    registry: Arc<ThemeRegistry>,
    snapshots: RwLock<BTreeMap<String, MiniJinjaThemeRenderer>>,
    data: Arc<ThemeData>,
    runtime: Arc<RenderingRuntime>,
    mutations: Arc<Mutex<()>>,
    preflight: Arc<Semaphore>,
    mutations_enabled: bool,
}

impl LocalThemePackages {
    pub async fn load(
        fallback: &Path,
        data: Arc<ThemeData>,
        runtime: Arc<RenderingRuntime>,
    ) -> Result<Self, UseCaseError> {
        let renderer = load_checked(fallback.to_path_buf(), &runtime).await?;
        if fallback.file_name().and_then(|name| name.to_str()) != Some(renderer.slug()) {
            return Err(invalid("默认主题目录名必须与清单 slug 一致"));
        }
        let root = std::fs::canonicalize(
            fallback
                .parent()
                .filter(|path| !path.as_os_str().is_empty())
                .unwrap_or(Path::new(".")),
        )
        .map_err(repo)?;
        let service = Self {
            root,
            registry: Arc::new(ThemeRegistry::new(renderer.slug().into())),
            snapshots: RwLock::default(),
            data,
            runtime,
            mutations: Arc::new(Mutex::new(())),
            preflight: Arc::new(Semaphore::new(1)),
            mutations_enabled: true,
        };
        service.publish(renderer)?;
        for entry in std::fs::read_dir(&service.root).map_err(repo)? {
            let entry = entry.map_err(repo)?;
            let slug = entry.file_name().to_string_lossy().into_owned();
            if slug == service.registry.fallback()
                || !valid_slug(&slug)
                || !entry.file_type().map_err(repo)?.is_dir()
            {
                continue;
            }
            match load_checked(entry.path(), &service.runtime).await {
                Ok(renderer) if renderer.slug() == slug => {
                    if let Err(error) = service.publish(renderer) {
                        tracing::warn!(theme = slug, %error, "跳过无法登记的主题");
                    }
                }
                Ok(_) => tracing::warn!(theme = slug, "跳过目录名与清单不一致的主题"),
                Err(error) => tracing::warn!(theme = slug, %error, "跳过无效主题"),
            }
        }
        service.registry.validate()?;
        Ok(service)
    }

    pub fn registry(&self) -> Arc<ThemeRegistry> {
        self.registry.clone()
    }

    pub fn with_mutations_enabled(mut self, enabled: bool) -> Self {
        self.mutations_enabled = enabled;
        self
    }

    fn publish(&self, renderer: MiniJinjaThemeRenderer) -> Result<(), UseCaseError> {
        let slug = renderer.slug().to_string();
        if self.registry.options().len() >= MAX_INSTALLED {
            return Err(invalid("已安装主题数量达到 32 个上限"));
        }
        self.registry.add_release(
            renderer.name().into(),
            self.runtime
                .theme_renderer(renderer.clone().with_data(self.data.clone())),
            renderer.assets(),
        )?;
        self.snapshots
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(slug, renderer);
        Ok(())
    }

    async fn prepare(
        &self,
        bytes: Vec<u8>,
    ) -> Result<(Staging, MiniJinjaThemeRenderer), UseCaseError> {
        if bytes.is_empty() || bytes.len() > MAX_THEME_PACKAGE_BYTES {
            return Err(invalid("主题 ZIP 包不能为空且不能超过 10 MiB"));
        }
        let permit =
            self.preflight
                .clone()
                .try_acquire_owned()
                .map_err(|_| UseCaseError::RateLimited {
                    retry_after_secs: 1,
                })?;
        let root = self.root.clone();
        let (_permit, staging, renderer) = tokio::task::spawn_blocking(move || {
            let staging = unpack(&root, bytes)?;
            let renderer = MiniJinjaThemeRenderer::load(&staging.0).map_err(package_error)?;
            Ok::<_, UseCaseError>((permit, staging, renderer))
        })
        .await
        .map_err(repo)??;
        renderer
            .validate(&self.runtime)
            .await
            .map_err(package_error)?;
        Ok((staging, renderer))
    }
}

struct OperationGuard {
    _guard: OwnedMutexGuard<()>,
}
impl ThemeOperationGuard for OperationGuard {}

#[async_trait]
impl ThemePackages for LocalThemePackages {
    async fn lock(&self) -> Box<dyn ThemeOperationGuard> {
        Box::new(OperationGuard {
            _guard: self.mutations.clone().lock_owned().await,
        })
    }

    async fn validate_package(&self, bytes: Vec<u8>) -> Result<ThemePackageReport, UseCaseError> {
        let (_staging, renderer) = self.prepare(bytes).await?;
        Ok(renderer.report())
    }

    async fn install(
        &self,
        bytes: Vec<u8>,
        actor: AuditContext,
    ) -> Result<ThemePackageReport, UseCaseError> {
        if !self.mutations_enabled {
            return Err(UseCaseError::Forbidden);
        }
        let (staging, renderer) = self.prepare(bytes).await?;
        let report = renderer.report();
        let destination = self.root.join(&report.slug);
        if self.registry.contains(&report.slug)
            || destination.try_exists().map_err(repo)?
            || std::fs::symlink_metadata(&destination).is_ok()
        {
            return Err(invalid("主题已安装或同名目录已存在，请先卸载同名主题"));
        }
        if self.registry.options().len() >= MAX_INSTALLED {
            return Err(invalid("已安装主题数量达到 32 个上限"));
        }
        // These short publication operations have no await boundary. Cancellation
        // cannot release the application mutation lock between rename and publish.
        std::fs::rename(&staging.0, &destination).map_err(repo)?;
        if let Err(error) = self.publish(renderer) {
            let _ = std::fs::rename(&destination, &staging.0);
            return Err(error);
        }
        tracing::info!(theme = report.slug, release = report.release, actor_id = ?actor.actor_id, ip = ?actor.ip_address, "主题已安装");
        Ok(report)
    }

    async fn validate_installed(&self, slug: &str) -> Result<ThemePackageReport, UseCaseError> {
        let renderer = self
            .snapshots
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(slug)
            .cloned()
            .ok_or_else(|| UseCaseError::NotFound("主题未安装".into()))?;
        renderer
            .validate(&self.runtime)
            .await
            .map_err(package_error)?;
        Ok(renderer.report())
    }

    async fn uninstall(&self, slug: &str, actor: AuditContext) -> Result<(), UseCaseError> {
        if !self.mutations_enabled {
            return Err(UseCaseError::Forbidden);
        }
        if !valid_slug(slug) {
            return Err(invalid("主题 slug 无效"));
        }
        if slug == self.registry.fallback() {
            return Err(invalid("启动默认主题不能卸载"));
        }
        if !self.registry.contains(slug) {
            return Err(UseCaseError::NotFound("主题未安装".into()));
        }
        let path = self.root.join(slug);
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
                return Err(invalid("主题目录必须为普通目录"));
            }
            Ok(_) => {
                let removed = Staging(self.root.join(format!(".removed-{}", uuid::Uuid::now_v7())));
                std::fs::rename(&path, &removed.0).map_err(repo)?;
                self.registry.remove(slug)?;
                // Removing bytes from public serving precedes best-effort garbage
                // collection; leftovers are hidden and never loaded on restart.
                drop(removed);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.registry.remove(slug)?;
            }
            Err(error) => return Err(repo(error)),
        }
        self.snapshots
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(slug);
        tracing::info!(theme = slug, actor_id = ?actor.actor_id, ip = ?actor.ip_address, "主题已卸载");
        Ok(())
    }
}

async fn load_checked(
    path: PathBuf,
    runtime: &RenderingRuntime,
) -> Result<MiniJinjaThemeRenderer, UseCaseError> {
    let renderer = tokio::task::spawn_blocking(move || MiniJinjaThemeRenderer::load(&path))
        .await
        .map_err(repo)??;
    renderer.validate(runtime).await?;
    Ok(renderer)
}

struct Staging(PathBuf);
impl Drop for Staging {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(%error, "清理主题暂存目录失败");
        }
    }
}

fn unpack(root: &Path, bytes: Vec<u8>) -> Result<Staging, UseCaseError> {
    let mut archive =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| invalid("无法读取主题 ZIP 包"))?;
    if archive.is_empty() || archive.len() > MAX_FILES {
        return Err(invalid("主题包最多允许 512 个文件或目录"));
    }
    let mut entries = Vec::new();
    let mut manifests = Vec::new();
    let mut seen = HashSet::new();
    let mut expanded = 0_u64;
    for index in 0..archive.len() {
        let file = archive
            .by_index(index)
            .map_err(|_| invalid("ZIP 条目无效或使用了不支持的压缩/加密格式"))?;
        let name = std::str::from_utf8(file.name_raw())
            .map_err(|_| invalid("主题包路径必须为 UTF-8"))?
            .trim_end_matches('/')
            .to_owned();
        if name.is_empty()
            || name.len() > 240
            || name.contains(['\\', ':'])
            || name.chars().any(char::is_control)
            || name.split('/').count() > 16
            || name
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
        {
            return Err(invalid("主题包含非法路径"));
        }
        if file.encrypted()
            || file.is_symlink()
            || file
                .unix_mode()
                .is_some_and(|mode| !matches!(mode & 0o170000, 0 | 0o100000 | 0o040000))
        {
            return Err(invalid("主题包仅允许未加密的普通文件和目录"));
        }
        if !seen.insert(name.clone()) {
            return Err(invalid("主题包含重复路径"));
        }
        expanded = expanded
            .checked_add(file.size())
            .ok_or_else(|| invalid("主题包解压大小溢出"))?;
        if file.size() > MAX_FILE_BYTES || expanded > MAX_EXPANDED_BYTES {
            return Err(invalid("主题包单文件最多 4 MiB，解压后总计最多 32 MiB"));
        }
        if !file.is_dir() && (name == "theme.json" || name.ends_with("/theme.json")) {
            manifests.push(name.clone());
        }
        entries.push((index, name, file.is_dir()));
    }
    if manifests.len() != 1 {
        return Err(invalid("主题包必须包含唯一的 theme.json"));
    }
    let files: HashSet<_> = entries
        .iter()
        .filter(|(_, _, directory)| !directory)
        .map(|(_, name, _)| name.as_str())
        .collect();
    for (_, name, _) in &entries {
        let mut parent = name.as_str();
        while let Some((prefix, _)) = parent.rsplit_once('/') {
            if files.contains(prefix) {
                return Err(invalid("主题包的文件与目录路径冲突"));
            }
            parent = prefix;
        }
    }
    let prefix = manifests[0].strip_suffix("theme.json").unwrap().to_string();
    if prefix.trim_end_matches('/').contains('/') {
        return Err(invalid("主题包只允许一个顶层主题目录"));
    }
    let staging = Staging(root.join(format!(".staging-{}", uuid::Uuid::now_v7())));
    std::fs::create_dir(&staging.0).map_err(repo)?;
    for (index, name, directory) in entries {
        if directory && name == prefix.trim_end_matches('/') {
            continue;
        }
        let relative = name
            .strip_prefix(&prefix)
            .ok_or_else(|| invalid("主题包含顶层主题目录以外的文件"))?;
        let destination = staging.0.join(relative);
        if directory {
            std::fs::create_dir_all(&destination).map_err(repo)?;
            continue;
        }
        std::fs::create_dir_all(destination.parent().unwrap()).map_err(repo)?;
        let file = archive
            .by_index(index)
            .map_err(|_| invalid("ZIP 条目读取失败"))?;
        let expected_size = file.size();
        let mut content = Vec::new();
        file.take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut content)
            .map_err(|_| invalid("ZIP 内容损坏"))?;
        if content.len() as u64 != expected_size || content.len() as u64 > MAX_FILE_BYTES {
            return Err(invalid("ZIP 条目实际大小与声明不一致"));
        }
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)
            .map_err(|_| invalid("主题包路径冲突或无法写入文件"))?;
        output.write_all(&content).map_err(repo)?;
    }
    Ok(staging)
}

fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 64
        && slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}
fn invalid(message: &str) -> UseCaseError {
    UseCaseError::Invalid(message.into())
}
fn repo(error: impl std::fmt::Display) -> UseCaseError {
    UseCaseError::Repository(error.to_string())
}
fn package_error(error: UseCaseError) -> UseCaseError {
    match error {
        UseCaseError::Render(message) => UseCaseError::Invalid(format!("主题验证失败：{message}")),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zip::write::SimpleFileOptions;

    struct TestRoot(PathBuf);
    impl TestRoot {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("blog-package-test-{}", uuid::Uuid::now_v7()));
            std::fs::create_dir_all(root.join("default/templates")).unwrap();
            for (path, bytes) in theme_files("default", "fallback") {
                let destination = root.join("default").join(path);
                std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
                std::fs::write(destination, bytes).unwrap();
            }
            Self(root)
        }
        async fn load(&self) -> LocalThemePackages {
            LocalThemePackages::load(
                &self.0.join("default"),
                crate::theme_validation::test_data(),
                Arc::new(RenderingRuntime::default()),
            )
            .await
            .unwrap()
        }
        fn entries(&self) -> Vec<String> {
            let mut entries: Vec<_> = std::fs::read_dir(&self.0)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            entries.sort();
            entries
        }
    }
    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn theme_files(slug: &str, index: &str) -> Vec<(String, Vec<u8>)> {
        let mut files = vec![("theme.json".into(), serde_json::to_vec(&serde_json::json!({"schema_version":1,"slug":slug,"name":slug,"theme_api_version":1,"required_functions":[]})).unwrap())];
        for entry in ["index", "post", "page", "tag", "category", "series"] {
            files.push((
                format!("templates/{entry}.html"),
                if entry == "index" { index } else { "page" }
                    .as_bytes()
                    .to_vec(),
            ));
        }
        files.push(("assets/nested/样式.css".into(), b"original css".to_vec()));
        files
    }
    fn archive(files: Vec<(String, Vec<u8>)>, prefix: &str) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, body) in files {
            writer
                .start_file(
                    format!("{prefix}{name}"),
                    SimpleFileOptions::default()
                        .compression_method(zip::CompressionMethod::Deflated),
                )
                .unwrap();
            writer.write_all(&body).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[tokio::test]
    async fn validate_install_reload_verify_and_uninstall_keep_release_bytes_consistent() {
        let root = TestRoot::new();
        let service = root.load().await;
        let bytes = archive(
            theme_files("custom", "{{ asset_url(path='nested/样式.css') }}"),
            "custom/",
        );
        let checked = service.validate_package(bytes.clone()).await.unwrap();
        assert_eq!(checked.template_count, 6);
        assert_eq!(checked.asset_count, 1);
        assert!(!service.registry.contains("custom"));
        assert_eq!(root.entries(), ["default"]);
        let installed = service
            .install(bytes.clone(), AuditContext::system())
            .await
            .unwrap();
        assert_eq!(checked.release, installed.release);
        assert!(service.registry.contains("custom"));
        assert_eq!(root.entries(), ["custom", "default"]);
        let assets = service
            .registry
            .assets("custom", &installed.release)
            .unwrap();
        assert_eq!(&*assets.files["nested/样式.css"], b"original css");
        assert!(
            service
                .install(bytes, AuditContext::system())
                .await
                .is_err()
        );
        assert!(service.registry.assets("custom", "wrong-release").is_none());
        drop(service);
        let service = root.load().await;
        assert_eq!(
            service.validate_installed("custom").await.unwrap().release,
            checked.release
        );
        std::fs::write(root.0.join("custom/templates/index.html"), "{{ missing }}").unwrap();
        // Validation is of the installed snapshot, matching public rendering.
        assert_eq!(
            service.validate_installed("custom").await.unwrap().release,
            checked.release
        );
        service
            .uninstall("custom", AuditContext::system())
            .await
            .unwrap();
        assert!(!service.registry.contains("custom"));
        assert!(
            service
                .registry
                .assets("custom", &checked.release)
                .is_none()
        );
        assert_eq!(root.entries(), ["default"]);
        assert!(service.validate_installed("custom").await.is_err());
        assert!(
            service
                .uninstall("default", AuditContext::system())
                .await
                .is_err()
        );
        drop(service);
        assert!(!root.load().await.registry.contains("custom"));
    }

    #[tokio::test]
    async fn failures_never_publish_or_leave_staging_and_flat_packages_are_supported() {
        let root = TestRoot::new();
        let service = root.load().await;
        let mut missing = theme_files("custom", "page");
        missing.retain(|(path, _)| path != "templates/series.html");
        let broken = theme_files("custom", "{{ missing }}");
        for bytes in [
            vec![],
            b"not zip".to_vec(),
            archive(missing, ""),
            archive(broken, ""),
            archive(theme_files("default", "page"), ""),
        ] {
            assert!(
                service
                    .install(bytes, AuditContext::system())
                    .await
                    .is_err()
            );
            assert_eq!(root.entries(), ["default"]);
            assert_eq!(service.registry.options().len(), 1);
        }
        let checked = service
            .validate_package(archive(theme_files("custom", "page"), ""))
            .await
            .unwrap();
        assert_eq!(checked.slug, "custom");
        assert_eq!(root.entries(), ["default"]);
    }

    #[tokio::test]
    async fn archive_paths_links_and_size_limits_are_checked_before_publication() {
        let root = TestRoot::new();
        let service = root.load().await;
        for path in [
            "../escaped",
            "/absolute",
            "templates/../escaped",
            "assets\\escape",
            "C:/escape",
            "assets//escape",
            "assets/./escape",
            "assets",
        ] {
            let mut files = theme_files("custom", "page");
            files.push((path.into(), b"bad".to_vec()));
            assert!(
                service
                    .install(archive(files, ""), AuditContext::system())
                    .await
                    .is_err(),
                "{path}"
            );
            assert_eq!(root.entries(), ["default"]);
        }
        let mut files = theme_files("custom", "page");
        files.push((
            "assets/huge.css".into(),
            vec![b'x'; MAX_FILE_BYTES as usize + 1],
        ));
        assert!(service.validate_package(archive(files, "")).await.is_err());
        assert!(
            service
                .validate_package(vec![0; MAX_THEME_PACKAGE_BYTES + 1])
                .await
                .is_err()
        );
        let mut files = theme_files("custom", "page");
        for i in 0..9 {
            files.push((
                format!("assets/{i}.css"),
                vec![b'x'; MAX_FILE_BYTES as usize],
            ));
        }
        assert!(service.validate_package(archive(files, "")).await.is_err());
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .add_symlink("theme.json", "/etc/passwd", SimpleFileOptions::default())
            .unwrap();
        assert!(
            service
                .validate_package(writer.finish().unwrap().into_inner())
                .await
                .is_err()
        );
        assert_eq!(root.entries(), ["default"]);
    }

    #[tokio::test]
    async fn recovery_mode_allows_preflight_but_denies_filesystem_mutations() {
        let root = TestRoot::new();
        let service = root.load().await.with_mutations_enabled(false);
        let bytes = archive(theme_files("custom", "page"), "");
        service.validate_package(bytes.clone()).await.unwrap();
        assert!(matches!(
            service.install(bytes, AuditContext::system()).await,
            Err(UseCaseError::Forbidden)
        ));
        assert!(matches!(
            service.uninstall("custom", AuditContext::system()).await,
            Err(UseCaseError::Forbidden)
        ));
        assert_eq!(root.entries(), ["default"]);
    }
}
