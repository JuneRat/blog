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

mod upgrade;

#[cfg(feature = "sqlx-test-support")]
type FailureHook = Arc<dyn Fn(&str) + Send + Sync>;

const MAX_FILES: usize = 512;
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_EXPANDED_BYTES: u64 = 32 * 1024 * 1024;
const MAX_INSTALLED: usize = 32;

#[derive(Clone)]
pub struct LocalThemePackages {
    root: PathBuf,
    registry: Arc<ThemeRegistry>,
    snapshots: Arc<RwLock<BTreeMap<String, MiniJinjaThemeRenderer>>>,
    store: Option<crate::themes::PostgresThemesStore>,
    _lease: Option<Arc<std::fs::File>>,
    uncertain: Arc<std::sync::atomic::AtomicBool>,
    #[cfg(feature = "sqlx-test-support")]
    failure_hook: Option<FailureHook>,
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
            snapshots: Arc::default(),
            store: None,
            _lease: None,
            uncertain: Arc::default(),
            #[cfg(feature = "sqlx-test-support")]
            failure_hook: None,
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
        for option in service.registry.options() {
            let path = service.previous_path(&option.slug);
            if path.try_exists().map_err(repo)? {
                let previous = load_checked(path, &service.runtime).await?;
                if previous.slug() != option.slug {
                    return Err(invalid("上一版主题目录与清单不一致"));
                }
                service.registry.retain_previous_assets(previous.assets());
            }
        }
        service.registry.validate()?;
        Ok(service)
    }

    /// Hold a filesystem lease for the live registry; multiple processes sharing
    /// a theme root cannot keep mutually stale snapshots. Recovery never writes.
    pub async fn load_persistent(
        fallback: &Path,
        data: Arc<ThemeData>,
        runtime: Arc<RenderingRuntime>,
        store: crate::themes::PostgresThemesStore,
        enabled: bool,
    ) -> Result<Self, UseCaseError> {
        Self::load_mode(fallback, data, runtime, store, enabled, true).await
    }

    /// First-install assembly must validate files without making the bootstrap
    /// database nonempty. Retain the lease until initialization after commit.
    pub async fn load_for_installation(
        fallback: &Path,
        data: Arc<ThemeData>,
        runtime: Arc<RenderingRuntime>,
        store: crate::themes::PostgresThemesStore,
    ) -> Result<Self, UseCaseError> {
        Self::load_mode(fallback, data, runtime, store, true, false).await
    }

    async fn load_mode(
        fallback: &Path,
        data: Arc<ThemeData>,
        runtime: Arc<RenderingRuntime>,
        store: crate::themes::PostgresThemesStore,
        enabled: bool,
        initialize: bool,
    ) -> Result<Self, UseCaseError> {
        let root =
            std::fs::canonicalize(fallback.parent().unwrap_or(Path::new("."))).map_err(repo)?;
        let lease = if enabled {
            if std::fs::symlink_metadata(root.join(".theme-owner.lock"))
                .is_ok_and(|meta| meta.file_type().is_symlink() || !meta.is_file())
            {
                return Err(invalid("主题目录租约必须为普通文件"));
            }
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(root.join(".theme-owner.lock"))
                .map_err(repo)?;
            file.try_lock()
                .map_err(|e| invalid(&format!("主题目录已由另一服务使用：{e}")))?;
            if initialize {
                recover_operations(&root, &store).await?;
            } else {
                for entry in std::fs::read_dir(&root).map_err(repo)? {
                    if entry
                        .map_err(repo)?
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".theme-op-")
                    {
                        return Err(invalid(
                            "首次安装不能使用带有未完成主题操作的目录，请先恢复原站点",
                        ));
                    }
                }
            }
            Some(Arc::new(file))
        } else {
            None
        };
        let mut service = Self::load(fallback, data, runtime).await?;
        service._lease = lease;
        service.mutations_enabled = enabled;
        service.store = Some(store);
        if enabled && initialize {
            service.initialize_records().await?;
        } else if !enabled {
            service.validate_records().await?;
        }
        Ok(service)
    }

    /// Called before serving requests, including after the installation
    /// bootstrap transaction commits. Repeated startup preserves existing data.
    pub async fn initialize_records(&self) -> Result<(), UseCaseError> {
        if !self.mutations_enabled {
            return Err(invalid("恢复隔离模式不能初始化主题配置"));
        }
        let _guard = self.mutations.lock().await;
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| invalid("主题配置存储不可用"))?;
        for option in self.registry.options() {
            if let Err(error) = store
                .initialize(
                    &option.slug,
                    &option.release,
                    &self.registry.schema(&option.slug),
                )
                .await
            {
                self.handle_incompatible(&option.slug, error)?;
            }
        }
        Ok(())
    }

    async fn validate_records(&self) -> Result<(), UseCaseError> {
        use application::theme_config::ThemeConfigStore;
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| invalid("主题配置存储不可用"))?;
        for option in self.registry.options() {
            let result = match store.find(&option.slug).await? {
                Some(record) => record
                    .effective(&option.release, &self.registry.schema(&option.slug))
                    .map(|_| ()),
                None => Ok(()),
            };
            if let Err(error) = result {
                self.handle_incompatible(&option.slug, error)?;
            }
        }
        Ok(())
    }

    fn handle_incompatible(&self, slug: &str, error: UseCaseError) -> Result<(), UseCaseError> {
        if slug == self.registry.fallback() {
            return Err(error);
        }
        tracing::error!(theme=slug, %error, "主题配置不兼容，保留数据并停止提供主题");
        self.retire(slug)
    }

    #[cfg(feature = "sqlx-test-support")]
    pub fn with_failure_hook(mut self, hook: FailureHook) -> Self {
        self.failure_hook = Some(hook);
        self
    }
    fn checkpoint(&self, _stage: &str) {
        #[cfg(feature = "sqlx-test-support")]
        if let Some(hook) = &self.failure_hook {
            hook(_stage);
        }
    }

    fn retire(&self, slug: &str) -> Result<(), UseCaseError> {
        self.registry.remove(slug)?;
        self.snapshots
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(slug);
        Ok(())
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
        self.registry.add_configured_release(
            renderer.name().into(),
            self.runtime
                .theme_renderer(renderer.clone().with_data(self.data.clone())),
            renderer.assets(),
            renderer.schema(),
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
    async fn previous(&self, slug: &str) -> Result<Option<ThemePackageReport>, UseCaseError> {
        if !valid_slug(slug) || !self.registry.contains(slug) {
            return Err(UseCaseError::NotFound("主题未安装".into()));
        }
        let path = self.previous_path(slug);
        if !path.try_exists().map_err(repo)? {
            return Ok(None);
        }
        let renderer = load_checked(path, &self.runtime).await?;
        if renderer.slug() != slug {
            return Err(invalid("上一版主题与目录不一致"));
        }
        Ok(Some(renderer.report()))
    }

    async fn upgrade(
        &self,
        slug: &str,
        bytes: Vec<u8>,
        actor: AuditContext,
        identity: application::themes::ThemeUpdateIdentity,
        guard: Box<dyn ThemeOperationGuard>,
    ) -> Result<ThemePackageReport, UseCaseError> {
        let service = self.clone();
        let slug = slug.to_string();
        tokio::spawn(async move {
            let _guard = guard;
            service
                .replace_owned(&slug, Some(bytes), actor, identity)
                .await
        })
        .await
        .map_err(repo)?
    }

    async fn rollback(
        &self,
        slug: &str,
        actor: AuditContext,
        identity: application::themes::ThemeUpdateIdentity,
        guard: Box<dyn ThemeOperationGuard>,
    ) -> Result<ThemePackageReport, UseCaseError> {
        let service = self.clone();
        let slug = slug.to_string();
        tokio::spawn(async move {
            let _guard = guard;
            service.replace_owned(&slug, None, actor, identity).await
        })
        .await
        .map_err(repo)?
    }

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
        guard: Box<dyn ThemeOperationGuard>,
    ) -> Result<ThemePackageReport, UseCaseError> {
        let service = self.clone();
        tokio::spawn(async move {
            let _guard = guard;
            service.install_owned(bytes, actor).await
        })
        .await
        .map_err(repo)?
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

    async fn uninstall(
        &self,
        slug: &str,
        actor: AuditContext,
        identity: application::themes::ThemeUninstallIdentity,
        guard: Box<dyn ThemeOperationGuard>,
    ) -> Result<(), UseCaseError> {
        let service = self.clone();
        let slug = slug.to_string();
        tokio::spawn(async move {
            let _guard = guard;
            service.uninstall_owned(&slug, actor, identity).await
        })
        .await
        .map_err(repo)?
    }
}

impl LocalThemePackages {
    async fn install_owned(
        &self,
        bytes: Vec<u8>,
        actor: AuditContext,
    ) -> Result<ThemePackageReport, UseCaseError> {
        if !self.mutations_enabled || self.uncertain.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(UseCaseError::Forbidden);
        }
        let (staging, renderer) = self.prepare(bytes).await?;
        let report = renderer.report();
        let destination = self.root.join(&report.slug);
        if self.registry.contains(&report.slug)
            || destination.try_exists().map_err(repo)?
            || std::fs::symlink_metadata(&destination).is_ok()
        {
            return Err(invalid(
                "主题已安装或同名目录已存在，请验证主题包后使用升级",
            ));
        }
        if self.registry.options().len() >= MAX_INSTALLED {
            return Err(invalid("已安装主题数量达到 32 个上限"));
        }
        sync_tree(&staging.0)?;
        if let Some(store) = &self.store {
            let mut tx = store.pool.begin().await.map_err(repo)?;
            crate::themes::lock(&mut tx).await?;
            if crate::themes::PostgresThemesStore::find_on(&mut tx, &report.slug)
                .await?
                .is_some()
            {
                return Err(invalid("同名主题配置记录仍存在，请先处理未完成的主题操作"));
            }
            let operation = Journal::prepare(
                &self.root,
                "install",
                &report.slug,
                uuid::Uuid::now_v7(),
                &report.release,
            )?;
            self.checkpoint("install.prepared");
            let result = async {
                std::fs::rename(&staging.0, operation.path.join("payload")).map_err(repo)?;
                sync_dir(&operation.path)?;
                std::fs::rename(operation.path.join("payload"), &destination).map_err(repo)?;
                sync_dir(&operation.path)?;
                sync_dir(&self.root)?;
                self.checkpoint("install.published");
                crate::themes::PostgresThemesStore::insert_on(
                    &mut tx,
                    operation.entry.id,
                    &report.slug,
                    &report.release,
                    &renderer.schema(),
                )
                .await?;
                crate::audit::record_change(
                    &mut tx,
                    actor,
                    "theme.install",
                    "theme",
                    &operation.entry.id.to_string(),
                    serde_json::json!({"slug":report.slug,"release":report.release}),
                )
                .await?;
                tx.commit().await.map_err(repo)
            }
            .await;
            if result.is_ok() {
                self.checkpoint("install.committed");
            }
            let committed = match operation.resolve(&self.root, store).await {
                Ok(committed) => committed,
                Err(error) => {
                    self.uncertain
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                    return Err(error);
                }
            };
            if !committed {
                return Err(result.err().unwrap_or_else(|| invalid("主题安装未提交")));
            }
            self.publish(renderer)?;
            return Ok(report);
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

    async fn uninstall_owned(
        &self,
        slug: &str,
        actor: AuditContext,
        identity: application::themes::ThemeUninstallIdentity,
    ) -> Result<(), UseCaseError> {
        if !self.mutations_enabled || self.uncertain.load(std::sync::atomic::Ordering::SeqCst) {
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
        let meta = std::fs::symlink_metadata(&path)
            .map_err(|e| invalid(&format!("主题目录缺失或不可读，配置已保留：{e}")))?;
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return Err(invalid("主题目录必须为普通目录"));
        }
        if let Some(store) = &self.store {
            let mut tx = store.pool.begin().await.map_err(repo)?;
            crate::themes::lock(&mut tx).await?;
            let current = crate::themes::PostgresThemesStore::find_on(&mut tx, slug)
                .await?
                .ok_or(UseCaseError::VersionConflict)?;
            let selection: Option<(String, i64)> =
                sqlx::query_as("SELECT value->>'slug',version FROM settings WHERE key='theme'")
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(repo)?;
            if current.id != identity.id
                || current.version != identity.version
                || current.release != identity.release
                || current.config_schema_version != identity.config_schema_version
                || selection.as_ref().map_or(0, |s| s.1) != identity.selection_version
            {
                return Err(UseCaseError::VersionConflict);
            }
            if selection.is_some_and(|s| s.0 == slug) {
                return Err(invalid("当前已保存选择的主题不能卸载"));
            }
            let renderer = self
                .snapshots
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(slug)
                .cloned()
                .ok_or(UseCaseError::VersionConflict)?;
            let operation =
                Journal::prepare(&self.root, "uninstall", slug, current.id, &current.release)?;
            self.checkpoint("uninstall.prepared");
            let result = async {
                std::fs::rename(&path, operation.path.join("payload")).map_err(repo)?;
                sync_dir(&operation.path)?; sync_dir(&self.root)?;
                self.retire(slug)?;
                self.checkpoint("uninstall.quarantined");
                sqlx::query("DELETE FROM media_refs WHERE source_type='theme' AND source_id=$1").bind(current.id).execute(&mut *tx).await.map_err(repo)?;
                sqlx::query("DELETE FROM themes WHERE id=$1").bind(current.id).execute(&mut *tx).await.map_err(repo)?;
                crate::audit::record_change(&mut tx, actor, "theme.uninstall", "theme", &current.id.to_string(), serde_json::json!({"slug":slug,"release":current.release,"version":current.version})).await?;
                tx.commit().await.map_err(repo)
            }.await;
            if result.is_ok() {
                self.checkpoint("uninstall.committed");
            }
            let committed = match operation.resolve(&self.root, store).await {
                Ok(committed) => committed,
                Err(error) => {
                    self.uncertain
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                    return Err(error);
                }
            };
            if !committed {
                if !self.registry.contains(slug) {
                    self.publish(renderer)?;
                }
                return Err(result.err().unwrap_or_else(|| invalid("主题卸载未提交")));
            }
        } else {
            let removed = Staging(self.root.join(format!(".removed-{}", uuid::Uuid::now_v7())));
            std::fs::rename(&path, &removed.0).map_err(repo)?;
            self.retire(slug)?;
        }
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

    fn test_identity() -> application::themes::ThemeUninstallIdentity {
        application::themes::ThemeUninstallIdentity {
            id: uuid::Uuid::now_v7(),
            version: 1,
            release: String::new(),
            config_schema_version: 1,
            selection_version: 0,
        }
    }
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
            .install(bytes.clone(), AuditContext::system(), service.lock().await)
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
                .install(bytes, AuditContext::system(), service.lock().await)
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
            .uninstall(
                "custom",
                AuditContext::system(),
                test_identity(),
                service.lock().await,
            )
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
                .uninstall(
                    "default",
                    AuditContext::system(),
                    test_identity(),
                    service.lock().await
                )
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
                    .install(bytes, AuditContext::system(), service.lock().await)
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
                    .install(
                        archive(files, ""),
                        AuditContext::system(),
                        service.lock().await
                    )
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
            service
                .install(bytes, AuditContext::system(), service.lock().await)
                .await,
            Err(UseCaseError::Forbidden)
        ));
        assert!(matches!(
            service
                .uninstall(
                    "custom",
                    AuditContext::system(),
                    test_identity(),
                    service.lock().await
                )
                .await,
            Err(UseCaseError::Forbidden)
        ));
        assert_eq!(root.entries(), ["default"]);
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct JournalEntry {
    kind: String,
    slug: String,
    id: uuid::Uuid,
    release: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous_release: Option<String>,
}
struct Journal {
    path: PathBuf,
    entry: JournalEntry,
}
impl Journal {
    fn prepare(
        root: &Path,
        kind: &str,
        slug: &str,
        id: uuid::Uuid,
        release: &str,
    ) -> Result<Self, UseCaseError> {
        Self::prepare_entry(
            root,
            JournalEntry {
                kind: kind.into(),
                slug: slug.into(),
                id,
                release: release.into(),
                previous_release: None,
            },
        )
    }
    fn prepare_entry(root: &Path, entry: JournalEntry) -> Result<Self, UseCaseError> {
        let path = root.join(format!(".theme-op-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&path).map_err(repo)?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path.join("operation.pending"))
            .map_err(repo)?;
        file.write_all(&serde_json::to_vec(&entry).map_err(repo)?)
            .map_err(repo)?;
        file.sync_all().map_err(repo)?;
        std::fs::rename(path.join("operation.pending"), path.join("operation.json"))
            .map_err(repo)?;
        sync_dir(&path)?;
        sync_dir(root)?;
        Ok(Self { path, entry })
    }
    /// The committed theme identity is the durable decision. No row is deleted
    /// because a folder failed to load; only this prepared operation can remove it.
    async fn resolve(
        &self,
        root: &Path,
        store: &crate::themes::PostgresThemesStore,
    ) -> Result<bool, UseCaseError> {
        // A lost commit response does not prove the original backend has ended
        // its transaction. Fence it with the same lock before choosing a file
        // compensation; keep this read transaction through directory recovery.
        let mut decision = store.pool.begin().await.map_err(repo)?;
        crate::themes::lock(&mut decision).await?;
        let record =
            crate::themes::PostgresThemesStore::find_on(&mut decision, &self.entry.slug).await?;
        if self.entry.kind == "upgrade" {
            let committed = self.resolve_upgrade(root, record.as_ref())?;
            std::fs::remove_dir_all(&self.path).map_err(repo)?;
            sync_dir(root)?;
            return Ok(committed);
        }
        if record
            .as_ref()
            .is_some_and(|r| r.id != self.entry.id || r.release != self.entry.release)
        {
            return Err(invalid("主题操作日志身份与数据库冲突，请人工核验"));
        }
        let final_path = root.join(&self.entry.slug);
        let payload = self.path.join("payload");
        let exists = record.is_some();
        let committed = if self.entry.kind == "install" {
            exists
        } else {
            !exists
        };
        let keep = if self.entry.kind == "install" {
            committed
        } else {
            !committed
        };
        if keep {
            if payload.try_exists().map_err(repo)? {
                if final_path.try_exists().map_err(repo)? {
                    return Err(invalid("主题恢复时正式目录与隔离目录同时存在"));
                }
                std::fs::rename(&payload, &final_path).map_err(repo)?;
                sync_dir(&self.path)?;
                sync_dir(root)?;
            } else if !final_path.try_exists().map_err(repo)? {
                return Err(invalid("已保留的主题目录缺失，配置未清除"));
            }
        } else if self.entry.kind == "uninstall" && final_path.try_exists().map_err(repo)? {
            return Err(invalid("已提交卸载的主题仍存在正式目录，请人工核验"));
        } else if self.entry.kind == "install" && final_path.try_exists().map_err(repo)? {
            // A prepared install owned this destination. Check its release before
            // touching it, so unrelated disk changes never become compensation.
            let renderer = MiniJinjaThemeRenderer::load(&final_path)?;
            if renderer.slug() != self.entry.slug || renderer.assets().version != self.entry.release
            {
                return Err(invalid("安装补偿目录与操作日志不一致"));
            }
            std::fs::rename(&final_path, &payload).map_err(repo)?;
            sync_dir(&self.path)?;
            sync_dir(root)?;
        }
        if committed && self.entry.kind == "uninstall" {
            let previous = root.join(format!(".theme-previous-{}", self.entry.slug));
            if previous.try_exists().map_err(repo)? {
                upgrade::check_release(&previous, &self.entry.slug, None)?;
                std::fs::remove_dir_all(previous).map_err(repo)?;
            }
        }
        std::fs::remove_dir_all(&self.path).map_err(repo)?;
        sync_dir(root)?;
        Ok(committed)
    }
}
async fn recover_operations(
    root: &Path,
    store: &crate::themes::PostgresThemesStore,
) -> Result<(), UseCaseError> {
    for entry in std::fs::read_dir(root).map_err(repo)? {
        let entry = entry.map_err(repo)?;
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with(".theme-op-")
        {
            continue;
        }
        if !entry.file_type().map_err(repo)?.is_dir() {
            return Err(invalid("主题操作日志目录无效"));
        }
        let path = entry.path();
        let manifest = path.join("operation.json");
        if !manifest.try_exists().map_err(repo)? {
            if path.join("payload").try_exists().map_err(repo)?
                || path.join("old").try_exists().map_err(repo)?
            {
                return Err(invalid("主题操作缺少日志但存在隔离文件，请人工核验"));
            }
            std::fs::remove_dir_all(&path).map_err(repo)?;
            sync_dir(root)?;
            continue;
        }
        let meta = std::fs::symlink_metadata(&manifest).map_err(repo)?;
        if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > 4096 {
            return Err(invalid("主题操作日志无效，请人工核验"));
        }
        let operation: JournalEntry =
            serde_json::from_slice(&std::fs::read(manifest).map_err(repo)?).map_err(repo)?;
        if !valid_slug(&operation.slug)
            || !matches!(operation.kind.as_str(), "install" | "uninstall" | "upgrade")
            || operation.release.len() != 64
            || !operation.release.bytes().all(|b| b.is_ascii_hexdigit())
            || (operation.kind == "upgrade") != operation.previous_release.is_some()
            || operation.previous_release.as_ref().is_some_and(|s| {
                s.len() != 64
                    || !s.bytes().all(|b| b.is_ascii_hexdigit())
                    || *s == operation.release
            })
        {
            return Err(invalid("主题操作日志内容无效"));
        }
        Journal {
            path,
            entry: operation,
        }
        .resolve(root, store)
        .await?;
    }
    // With the live-root lease held and every journal resolved, these owned
    // preflight directories cannot be an installed or quarantined package.
    for entry in std::fs::read_dir(root).map_err(repo)? {
        let entry = entry.map_err(repo)?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(id) = name.strip_prefix(".staging-") else {
            continue;
        };
        if !uuid::Uuid::parse_str(id)
            .is_ok_and(|parsed| parsed.to_string() == id && !parsed.is_nil())
        {
            continue;
        }
        if !entry.file_type().map_err(repo)?.is_dir() {
            return Err(invalid("主题暂存路径必须为普通目录"));
        }
        std::fs::remove_dir_all(entry.path()).map_err(repo)?;
        sync_dir(root)?;
    }
    Ok(())
}
fn sync_dir(path: &Path) -> Result<(), UseCaseError> {
    std::fs::File::open(path)
        .and_then(|f| f.sync_all())
        .map_err(repo)
}
fn sync_tree(path: &Path) -> Result<(), UseCaseError> {
    for entry in std::fs::read_dir(path).map_err(repo)? {
        let entry = entry.map_err(repo)?;
        if entry.file_type().map_err(repo)?.is_dir() {
            sync_tree(&entry.path())?;
        } else {
            std::fs::File::open(entry.path())
                .and_then(|f| f.sync_all())
                .map_err(repo)?;
        }
    }
    sync_dir(path)
}
