//! Replace an installed release without losing its configuration or selection.
use super::*;
use application::{theme_config::ThemeRecord, themes::ThemeUpdateIdentity};

impl LocalThemePackages {
    pub(super) fn previous_path(&self, slug: &str) -> PathBuf {
        self.root.join(format!(".theme-previous-{slug}"))
    }

    pub(super) async fn replace_owned(
        &self,
        slug: &str,
        bytes: Option<Vec<u8>>,
        actor: AuditContext,
        identity: ThemeUpdateIdentity,
    ) -> Result<ThemePackageReport, UseCaseError> {
        if !self.mutations_enabled || self.uncertain.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(UseCaseError::Forbidden);
        }
        if !valid_slug(slug) || !self.registry.contains(slug) {
            return Err(UseCaseError::NotFound("主题未安装".into()));
        }
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| invalid("主题配置存储不可用"))?;
        let action = if bytes.is_some() {
            "theme.upgrade"
        } else {
            "theme.rollback"
        };
        let (staging, renderer) = match bytes {
            Some(bytes) => self.prepare(bytes).await?,
            None => {
                let source = self.previous_path(slug);
                if !source.try_exists().map_err(repo)? {
                    return Err(invalid("没有可回退的上一版主题"));
                }
                let root = self.root.clone();
                let staging = tokio::task::spawn_blocking(move || {
                    let staging = Staging(root.join(format!(".staging-{}", uuid::Uuid::now_v7())));
                    let mut budget = (0, 0);
                    copy_tree(&source, &staging.0, &mut budget)?;
                    Ok::<_, UseCaseError>(staging)
                })
                .await
                .map_err(repo)??;
                let renderer = load_checked(staging.0.clone(), &self.runtime).await?;
                (staging, renderer)
            }
        };
        let report = renderer.report();
        if report.slug != slug {
            return Err(invalid("升级包必须与已安装主题的 slug 一致"));
        }
        let mut tx = crate::persistence::begin_authorized_write(&store.pool, &actor).await?;
        crate::themes::lock(&mut tx).await?;
        let current = crate::themes::PostgresThemesStore::find_on(&mut tx, slug)
            .await?
            .ok_or(UseCaseError::VersionConflict)?;
        let snapshot = self.registry.snapshot(slug)?;
        if current.id != identity.id
            || current.version != identity.version
            || current.release != identity.release
            || snapshot.release != identity.release
        {
            return Err(UseCaseError::VersionConflict);
        }
        if report.release == identity.release {
            return Err(invalid("主题包与当前版本相同"));
        }
        let schema = renderer.schema();
        schema
            .validate_config(&current.config)
            .map_err(|e| invalid(&format!("新版本不兼容现有配置，原版本和配置已保留：{e}")))?;
        // A field cannot reinterpret an existing scalar as a new media reference.
        if schema
            .media_ids(&current.config)
            .iter()
            .any(|id| !snapshot.schema.media_ids(&current.config).contains(id))
        {
            return Err(invalid(
                "升级不能将已有普通字段转换为媒体引用，请先调整配置",
            ));
        }
        let destination = self.root.join(slug);
        check_release(&destination, slug, Some(&current.release))?;
        sync_tree(&staging.0)?;
        let operation = Journal::prepare_entry(
            &self.root,
            JournalEntry {
                kind: "upgrade".into(),
                slug: slug.into(),
                id: current.id,
                release: report.release.clone(),
                previous_release: Some(current.release.clone()),
            },
        )?;
        self.checkpoint("upgrade.prepared");
        let result = async {
            std::fs::rename(&staging.0, operation.path.join("payload")).map_err(repo)?;
            sync_dir(&operation.path)?; sync_dir(&self.root)?;
            std::fs::rename(&destination, operation.path.join("old")).map_err(repo)?;
            sync_dir(&operation.path)?; sync_dir(&self.root)?;
            self.checkpoint("upgrade.quarantined");
            std::fs::rename(operation.path.join("payload"), &destination).map_err(repo)?;
            sync_dir(&operation.path)?; sync_dir(&self.root)?;
            self.checkpoint("upgrade.published");
            crate::persistence::sync_media_refs(&mut tx, application::ports::MediaContentKind::Theme,
                current.id, &schema.media_ids(&current.config)).await?;
            sqlx::query("UPDATE themes SET release=$2,config_schema_version=$3,media_fields=$4,version=version+1,updated_at=now() WHERE id=$1")
                .bind(current.id).bind(&report.release).bind(schema.config_schema_version as i32).bind(schema.media_fields())
                .execute(&mut *tx).await.map_err(repo)?;
            crate::audit::record_change(&mut tx, actor, action, "theme", &current.id.to_string(),
                serde_json::json!({"slug":slug,"previous_release":current.release,"release":report.release,"version":current.version+1})).await?;
            tx.commit().await.map_err(repo)
        }.await;
        if result.is_ok() {
            self.checkpoint("upgrade.committed");
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
            return Err(result.err().unwrap_or_else(|| invalid("主题升级未提交")));
        }
        self.registry.replace_configured_release(
            renderer.name().into(),
            self.runtime
                .theme_renderer(renderer.clone().with_data(self.data.clone())),
            renderer.assets(),
            schema,
        )?;
        self.snapshots
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(slug.into(), renderer);
        Ok(report)
    }
}

impl Journal {
    pub(super) fn resolve_upgrade(
        &self,
        root: &Path,
        record: Option<&ThemeRecord>,
    ) -> Result<bool, UseCaseError> {
        let previous_release = self
            .entry
            .previous_release
            .as_deref()
            .ok_or_else(|| invalid("升级日志缺少原版本"))?;
        let record = record.ok_or_else(|| invalid("升级恢复的主题配置记录缺失"))?;
        if record.id != self.entry.id
            || (record.release != self.entry.release && record.release != previous_release)
        {
            return Err(invalid("升级日志与数据库身份冲突，请人工核验"));
        }
        let destination = root.join(&self.entry.slug);
        let old = self.path.join("old");
        let payload = self.path.join("payload");
        let previous = root.join(format!(".theme-previous-{}", self.entry.slug));
        let committed = record.release == self.entry.release;
        if committed {
            check_release(&destination, &self.entry.slug, Some(&self.entry.release))?;
            if old.try_exists().map_err(repo)? {
                check_release(&old, &self.entry.slug, Some(previous_release))?;
                if previous.try_exists().map_err(repo)? {
                    check_release(&previous, &self.entry.slug, None)?;
                    let discarded = self.path.join("discarded");
                    if discarded.try_exists().map_err(repo)? {
                        return Err(invalid("主题历史目录冲突"));
                    }
                    std::fs::rename(&previous, discarded).map_err(repo)?;
                    sync_dir(&self.path)?;
                    sync_dir(root)?;
                }
                std::fs::rename(&old, &previous).map_err(repo)?;
                sync_dir(&self.path)?;
                sync_dir(root)?;
            } else {
                // Recovery may already have archived the old release before a crash.
                check_release(&previous, &self.entry.slug, Some(previous_release))?;
            }
        } else if old.try_exists().map_err(repo)? {
            check_release(&old, &self.entry.slug, Some(previous_release))?;
            if destination.try_exists().map_err(repo)? {
                check_release(&destination, &self.entry.slug, Some(&self.entry.release))?;
                if payload.try_exists().map_err(repo)? {
                    return Err(invalid("主题升级恢复存在重复包"));
                }
                std::fs::rename(&destination, &payload).map_err(repo)?;
                sync_dir(&self.path)?;
                sync_dir(root)?;
            }
            std::fs::rename(&old, &destination).map_err(repo)?;
            sync_dir(&self.path)?;
            sync_dir(root)?;
        } else {
            check_release(&destination, &self.entry.slug, Some(previous_release))?;
        }
        Ok(committed)
    }
}

pub(super) fn check_release(
    path: &Path,
    slug: &str,
    release: Option<&str>,
) -> Result<(), UseCaseError> {
    let renderer = MiniJinjaThemeRenderer::load(path)?;
    if renderer.slug() != slug || release.is_some_and(|r| renderer.assets().version != r) {
        return Err(invalid("主题目录与操作版本不一致，请人工核验"));
    }
    Ok(())
}

fn copy_tree(
    source: &Path,
    destination: &Path,
    budget: &mut (usize, u64),
) -> Result<(), UseCaseError> {
    let meta = std::fs::symlink_metadata(source).map_err(repo)?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(invalid("主题历史必须为普通目录"));
    }
    std::fs::create_dir(destination).map_err(repo)?;
    for entry in std::fs::read_dir(source).map_err(repo)? {
        let entry = entry.map_err(repo)?;
        let meta = entry.file_type().map_err(repo)?;
        budget.0 += 1;
        if budget.0 > MAX_FILES {
            return Err(invalid("主题历史文件数量超限"));
        }
        if meta.is_dir() {
            copy_tree(&entry.path(), &destination.join(entry.file_name()), budget)?;
        } else if meta.is_file() {
            let size = entry.metadata().map_err(repo)?.len();
            budget.1 += size;
            if size > MAX_FILE_BYTES || budget.1 > MAX_EXPANDED_BYTES {
                return Err(invalid("主题历史大小超限"));
            }
            std::fs::copy(entry.path(), destination.join(entry.file_name())).map_err(repo)?;
        } else {
            return Err(invalid("主题历史不能包含链接或特殊文件"));
        }
    }
    Ok(())
}
