//! Recovery orchestration: local capabilities, persistent jobs and a writer drain.
//! The worker owns archive/database I/O; the listener remains alive throughout.
use crate::managed::LiveSite;
use application::{UseCaseError, backup::*, ports::SecureRandom};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::PathBuf,
    process::Stdio,
    sync::{Arc, Mutex, Weak, atomic::Ordering},
    time::{Duration, Instant},
};
use tokio::{
    io::AsyncWriteExt,
    sync::{Mutex as AsyncMutex, watch},
};

struct Session {
    csrf: String,
    expires: Instant,
    mutations_until: Instant,
    owner: Option<(uuid::Uuid, i64)>,
    bootstrap: bool,
}

pub(crate) struct Controller {
    live: Arc<LiveSite>,
    root: PathBuf,
    worker: PathBuf,
    operation: Arc<AsyncMutex<()>>,
    sessions: Mutex<HashMap<String, Session>>,
    attempts: Mutex<Vec<Instant>>,
    jobs: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    this: Weak<Self>,
}

fn invalid(message: impl Into<String>) -> UseCaseError {
    UseCaseError::Invalid(message.into())
}
fn random() -> Result<String, UseCaseError> {
    infrastructure::SystemSecureRandom.token_hex()
}
fn busy() -> UseCaseError {
    invalid("已有任务正在进行，请等待完成")
}
fn unix_time() -> u64 {
    time::OffsetDateTime::now_utc().unix_timestamp().max(0) as u64
}

impl Controller {
    pub async fn new(live: Arc<LiveSite>) -> Result<Arc<Self>, String> {
        let config = live.config();
        let root = std::env::var_os("BLOG_BACKUP_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                config
                    .path
                    .parent()
                    .unwrap_or(std::path::Path::new("."))
                    .join("recovery")
            });
        let root = if root.is_absolute() {
            root
        } else {
            std::env::current_dir()
                .map_err(|e| e.to_string())?
                .join(root)
        };
        let worker = std::env::var_os("BLOG_BACKUP_WORKER")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(
                    if std::path::Path::new("/opt/blog/scripts/browser_recovery.py").is_file() {
                        "/opt/blog/scripts/browser_recovery.py"
                    } else {
                        "scripts/browser_recovery.py"
                    },
                )
            });
        let controller = Arc::new_cyclic(|this| Self {
            live,
            root,
            worker,
            operation: Arc::new(AsyncMutex::new(())),
            sessions: Mutex::new(HashMap::new()),
            attempts: Mutex::new(Vec::new()),
            jobs: Mutex::new(Vec::new()),
            this: this.clone(),
        });
        controller
            .call("initialize", json!({}))
            .await
            .map_err(|e| e.to_string())?;
        Ok(controller)
    }
    pub fn recovery_required(&self) -> bool {
        self.root.join("RECOVERY_REQUIRED").exists()
    }
    fn context(&self, action: &str, input: Value) -> Result<Value, UseCaseError> {
        let config = self.live.config();
        let site = config.site(Some(self.live.bind.clone())).map_err(invalid)?;
        let mut body = input.as_object().cloned().unwrap_or_default();
        // Client-controlled JSON never chooses filesystem paths, tools or database credentials.
        body.insert("action".into(), json!(action));
        body.insert("state_dir".into(), json!(self.root));
        body.insert("config_path".into(), json!(config.path));
        body.insert(
            "database_url".into(),
            json!(
                config
                    .configured_database_url()
                    .map_err(invalid)?
                    .unwrap_or_default()
            ),
        );
        body.insert("theme_dir".into(), json!(site.theme_dir));
        body.insert("media_dir".into(), json!(site.media_dir));
        body.insert("scratch_dir".into(), json!("/tmp"));
        Ok(Value::Object(body))
    }
    async fn call(&self, action: &str, input: Value) -> Result<Value, UseCaseError> {
        let request = self.context(action, input)?;
        let mut child = tokio::process::Command::new("python3")
            .arg("-B")
            .arg(&self.worker)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| invalid("备份工具不可用，请使用包含恢复工具的博客镜像"))?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| invalid("无法启动恢复任务"))?;
        stdin
            .write_all(&serde_json::to_vec(&request).map_err(|_| invalid("恢复参数无效"))?)
            .await
            .map_err(|_| invalid("无法启动恢复任务"))?;
        drop(stdin);
        let output = child
            .wait_with_output()
            .await
            .map_err(|_| invalid("恢复任务被中断"))?;
        let value: Value = serde_json::from_slice(&output.stdout)
            .map_err(|_| invalid("恢复任务意外退出，请查看任务状态"))?;
        if output.status.success() && value["ok"] == true {
            Ok(value["result"].clone())
        } else {
            Err(invalid(value["error"].as_str().unwrap_or("恢复任务失败")))
        }
    }
    async fn authorize(
        &self,
        credential: &RecoveryCredential,
        mutation: bool,
    ) -> Result<String, UseCaseError> {
        let (csrf, owner, bootstrap) = {
            let mut sessions = self.sessions.lock().expect("session lock");
            sessions.retain(|_, session| session.expires > Instant::now());
            let session = sessions
                .get(&credential.token)
                .ok_or(UseCaseError::Unauthenticated)?;
            if mutation
                && (session.mutations_until <= Instant::now()
                    || !credential.csrf.as_deref().is_some_and(|token| {
                        interfaces::http_support::csrf_token_matches(token, &session.csrf)
                    }))
            {
                return Err(UseCaseError::Unauthenticated);
            }
            (session.csrf.clone(), session.owner, session.bootstrap)
        };
        if mutation && bootstrap && !self.live.installing.load(Ordering::Acquire) {
            return Err(UseCaseError::Unauthenticated);
        }
        // Keep read access to job progress after old business sessions are revoked.
        if mutation
            && !self.live.paused.load(Ordering::Acquire)
            && let Some((id, version)) = owner
        {
            let _reader = self.live.gate.read().await;
            let admin = self
                .live
                .admin
                .read()
                .expect("admin lock")
                .clone()
                .ok_or(UseCaseError::Unauthenticated)?;
            let (_, current) = admin
                .users
                .actor_with_revision(id, application::identity::ActorChannel::Session)
                .await?;
            if current != version
                || !admin
                    .users
                    .roles_of_user(id)
                    .await?
                    .iter()
                    .any(|role| role == "admin")
            {
                self.sessions
                    .lock()
                    .expect("session lock")
                    .remove(&credential.token);
                return Err(UseCaseError::Forbidden);
            }
        }
        Ok(csrf)
    }
    fn write_job(&self, id: &str, value: &Value) -> Result<(), UseCaseError> {
        use std::io::Write;
        let target = self.root.join("jobs").join(format!("{id}.json"));
        let temporary = target.with_extension("pending");
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .map_err(|_| invalid("无法记录任务，请检查备份存储空间"))?;
        file.write_all(&serde_json::to_vec(value).map_err(|_| invalid("任务记录无效"))?)
            .map_err(|_| invalid("无法写入任务记录"))?;
        file.sync_all().map_err(|_| invalid("无法保存任务记录"))?;
        std::fs::rename(temporary, target).map_err(|_| invalid("无法保存任务记录"))?;
        Ok(())
    }
    fn finish_error(&self, id: &str, error: &UseCaseError) {
        let path = self.root.join("jobs").join(format!("{id}.json"));
        if let Ok(bytes) = std::fs::read(path)
            && let Ok(mut job) = serde_json::from_slice::<Value>(&bytes)
        {
            job["status"] = json!("failed");
            job["message"] = json!(error.to_string());
            job["finished_at"] = json!(unix_time());
            let _ = self.write_job(id, &job);
        }
    }
    async fn start_job(&self, action: &str, mut input: Value) -> Result<Value, UseCaseError> {
        let guard = self
            .operation
            .clone()
            .try_lock_owned()
            .map_err(|_| busy())?;
        let id = uuid::Uuid::now_v7().simple().to_string();
        if !input.is_object() {
            input = json!({});
        }
        input["job_id"] = json!(id);
        let job = json!({"id": id, "kind": action, "status": "running", "phase": "queued", "started_at": unix_time(),
                         "source": input.get("name"), "rollback": null, "requested_by": input.get("requested_by")});
        self.write_job(&id, &job)?;
        let controller = self.this.upgrade().ok_or_else(|| invalid("服务正在关闭"))?;
        let job_id = id.clone();
        let action = action.to_string();
        let handle = tokio::spawn(async move {
            let _operation = guard;
            let maintenance = action == "backup" || action == "restore";
            let result = async {
                let _drain = if maintenance {
                    controller.live.paused.store(true, Ordering::Release);
                    let drain = tokio::time::timeout(
                        Duration::from_secs(150),
                        controller.live.gate.write(),
                    )
                    .await
                    .map_err(|_| invalid("正在处理的请求未能及时结束，请重试"))?;
                    controller.live.stop().await;
                    Some(drain)
                } else {
                    None
                };
                let result = controller.call(&action, input).await;
                if maintenance && (!controller.recovery_required() || result.is_ok()) {
                    if let Err(error) = controller.live.activate().await {
                        return Err(invalid(format!(
                            "数据操作已结束，站点暂未能重新启动：{error}。修复后点击重新启动站点"
                        )));
                    }
                    if action == "restore" && result.is_ok() {
                        controller.clear_recovery_flag()?;
                        for session in controller
                            .sessions
                            .lock()
                            .expect("session lock")
                            .values_mut()
                        {
                            session.mutations_until = Instant::now();
                        }
                    }
                    controller.live.paused.store(false, Ordering::Release);
                    controller.live.tasks().start();
                }
                drop(_drain);
                if action == "backup"
                    && let Ok(ref backup) = result
                {
                    return controller
                        .call(
                            "finalize-backup",
                            json!({"name": backup["name"], "job_id": job_id}),
                        )
                        .await;
                }
                result
            }
            .await;
            if let Err(error) = result {
                controller.finish_error(&job_id, &error);
            }
        });
        let mut handles = self.jobs.lock().expect("jobs lock");
        handles.retain(|h| !h.is_finished());
        handles.push(handle);
        Ok(json!({"job_id": id}))
    }
    fn clear_recovery_flag(&self) -> Result<(), UseCaseError> {
        if self.recovery_required() {
            std::fs::remove_file(self.root.join("RECOVERY_REQUIRED"))
                .map_err(|_| invalid("无法更新恢复状态"))?;
            std::fs::File::open(&self.root)
                .and_then(|f| f.sync_all())
                .map_err(|_| invalid("无法保存恢复状态"))?;
        }
        Ok(())
    }
    pub async fn schedule(self: Arc<Self>, mut stopped: watch::Receiver<bool>) {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(30)) => {},
                _ = stopped.changed() => break,
            }
            if self.live.paused.load(Ordering::Acquire)
                || self.live.installing.load(Ordering::Acquire)
            {
                continue;
            }
            if let Ok(status) = self.call("status", json!({})).await {
                let due = status["settings"]["next_run"].as_f64().unwrap_or(0.0);
                if due > 0.0 && due <= unix_time() as f64 {
                    let _ = self
                        .start_job("backup", json!({"requested_by": "schedule"}))
                        .await;
                }
            }
        }
    }
    pub async fn shutdown(&self) {
        let handles = std::mem::take(&mut *self.jobs.lock().expect("jobs lock"));
        for handle in handles {
            handle.abort();
            let _ = handle.await;
        }
    }
}

#[async_trait::async_trait]
impl RecoveryControl for Controller {
    async fn login(
        &self,
        input: RecoveryLogin,
        client: Option<String>,
    ) -> Result<RecoverySession, UseCaseError> {
        {
            let mut attempts = self.attempts.lock().expect("attempt lock");
            attempts.retain(|at| at.elapsed() < Duration::from_secs(60));
            if attempts.len() >= 10 {
                return Err(UseCaseError::RateLimited {
                    retry_after_secs: 60,
                });
            }
            attempts.push(Instant::now());
        }
        let mut owner = None;
        let mut bootstrap = false;
        if !input.installation_token.is_empty() {
            let valid = self
                .live
                .installation_token
                .read()
                .expect("installation lock")
                .as_ref()
                .is_some_and(|token| {
                    interfaces::http_support::csrf_token_matches(token, &input.installation_token)
                });
            if !valid || !self.live.installing.load(Ordering::Acquire) {
                return Err(UseCaseError::InvalidCredentials);
            }
            bootstrap = true;
        } else if !input.key.is_empty() {
            self.call("authenticate", json!({"key": input.key}))
                .await
                .map_err(|_| UseCaseError::InvalidCredentials)?;
        } else {
            if self.live.paused.load(Ordering::Acquire) {
                return Err(invalid("站点维护中，请使用下载保存的恢复密钥登录"));
            }
            let _reader = self.live.gate.read().await;
            let admin = self
                .live
                .admin
                .read()
                .expect("admin lock")
                .clone()
                .ok_or(UseCaseError::Unauthenticated)?;
            let session = admin
                .passwords
                .login(
                    &input.username,
                    &input.password,
                    client.as_deref(),
                    "/admin/",
                    None,
                )
                .await?;
            let actor = admin.auth.session_actor(&session.token).await;
            admin.auth.logout(&session.token).await?;
            let (_, actor) = actor?;
            if !admin
                .users
                .roles_of_user(session.user_id)
                .await?
                .iter()
                .any(|role| role == "admin")
            {
                return Err(UseCaseError::Forbidden);
            }
            let (_, version) = admin
                .users
                .actor_with_revision(
                    session.user_id,
                    application::identity::ActorChannel::Session,
                )
                .await?;
            let _ = actor;
            owner = Some((session.user_id, version));
        }
        let token = random()?;
        let csrf = random()?;
        let mut sessions = self.sessions.lock().expect("session lock");
        sessions.retain(|_, s| s.expires > Instant::now());
        if sessions.len() >= 64 {
            sessions.clear();
        }
        sessions.insert(
            token.clone(),
            Session {
                csrf: csrf.clone(),
                expires: Instant::now() + Duration::from_secs(3600),
                mutations_until: Instant::now() + Duration::from_secs(900),
                owner,
                bootstrap,
            },
        );
        Ok(RecoverySession { token, csrf })
    }
    async fn status(&self, credential: RecoveryCredential) -> Result<Value, UseCaseError> {
        let csrf = self.authorize(&credential, false).await?;
        let mut value = self.call("status", json!({})).await?;
        value["csrf"] = json!(csrf);
        value["maintenance"] = json!(self.live.paused.load(Ordering::Acquire));
        value["installing"] = json!(self.live.installing.load(Ordering::Acquire));
        value["busy"] = json!(self.operation.try_lock().is_err());
        value["database_configured"] = json!(
            self.live
                .config()
                .configured_database_url()
                .map_err(invalid)?
                .is_some()
        );
        Ok(value)
    }
    async fn execute(
        &self,
        credential: RecoveryCredential,
        command: RecoveryCommand,
    ) -> Result<Value, UseCaseError> {
        self.authorize(&credential, true).await?;
        let action = serde_json::to_value(&command.action).map_err(|_| invalid("操作无效"))?;
        let action = action.as_str().unwrap();
        let mut input = command.input;
        if !input.is_object() {
            input = json!({});
        }
        let actor = self
            .sessions
            .lock()
            .expect("session lock")
            .get(&credential.token)
            .map(|session| {
                session
                    .owner
                    .map(|(id, _)| format!("user:{id}"))
                    .unwrap_or_else(|| {
                        if session.bootstrap {
                            "installation".into()
                        } else {
                            "recovery-key".into()
                        }
                    })
            })
            .ok_or(UseCaseError::Unauthenticated)?;
        input["requested_by"] = json!(actor);
        if self.live.installing.load(Ordering::Acquire)
            && !matches!(
                command.action,
                RecoveryAction::Inspect
                    | RecoveryAction::DiscardUpload
                    | RecoveryAction::Restore
                    | RecoveryAction::RemoteSave
                    | RecoveryAction::RemoteList
                    | RecoveryAction::RemoteDownload
            )
        {
            return Err(invalid("请先完成安装或从备份恢复"));
        }
        if matches!(command.action, RecoveryAction::Backup)
            && (self.live.paused.load(Ordering::Acquire) || self.recovery_required())
        {
            return Err(invalid("请先完成恢复并重新启动站点，再创建备份"));
        }
        if matches!(command.action, RecoveryAction::Restore) {
            if input["confirm"] != "恢复此站点" {
                return Err(invalid("请填写“恢复此站点”确认覆盖当前数据"));
            }
            if self
                .live
                .config()
                .configured_database_url()
                .map_err(invalid)?
                .is_none()
            {
                let url = input["database_url"].as_str().unwrap_or_default();
                let origin = input["public_base_url"].as_str().unwrap_or_default();
                let config = self
                    .live
                    .config()
                    .save_recovery_target(url, origin)
                    .map_err(invalid)?;
                *self.live.config.write().expect("config lock") = config;
            }
            // Existing installation journals must not re-publish pre-restore configuration.
            input
                .as_object_mut()
                .ok_or_else(|| invalid("恢复参数无效"))?
                .remove("database_url");
        }
        if matches!(command.action, RecoveryAction::Resume) {
            let _operation = self.operation.try_lock().map_err(|_| busy())?;
            if self.recovery_required() {
                let flag: Value = serde_json::from_slice(
                    &std::fs::read(self.root.join("RECOVERY_REQUIRED"))
                        .map_err(|_| invalid("无法读取恢复记录"))?,
                )
                .map_err(|_| invalid("恢复记录无效"))?;
                let status = self.call("status", json!({})).await?;
                let completed = status["jobs"].as_array().is_some_and(|jobs| {
                    jobs.iter().any(|j| {
                        j["id"] == flag["job"]
                            && (j["status"] == "succeeded"
                                || j["result"]["needs_activation"] == true)
                    })
                });
                if !completed {
                    return Err(invalid("恢复尚未完成，请重试恢复或选择恢复前副本回滚"));
                }
            }
            let _drain = self.live.gate.write().await;
            self.live.stop().await;
            self.live.activate().await.map_err(invalid)?;
            self.clear_recovery_flag()?;
            self.live.paused.store(false, Ordering::Release);
            self.live.tasks().start();
            return Ok(json!({"resumed": true}));
        }
        if matches!(
            command.action,
            RecoveryAction::Backup
                | RecoveryAction::Inspect
                | RecoveryAction::Restore
                | RecoveryAction::RemoteDownload
                | RecoveryAction::RemoteUpload
                | RecoveryAction::RemoteList
                | RecoveryAction::RemoteSave
        ) {
            self.start_job(action, input).await
        } else {
            let _operation = self.operation.try_lock().map_err(|_| busy())?;
            self.call(action, input).await
        }
    }
    async fn authorize_upload(&self, credential: RecoveryCredential) -> Result<(), UseCaseError> {
        self.authorize(&credential, true).await.map(|_| ())
    }
    async fn upload(
        &self,
        credential: RecoveryCredential,
        id: Option<String>,
        offset: u64,
        complete: bool,
        bytes: Vec<u8>,
    ) -> Result<Value, UseCaseError> {
        self.authorize(&credential, true).await?;
        let _operation = self.operation.try_lock().map_err(|_| busy())?;
        if offset == 0 {
            if !bytes.starts_with(b"age-encryption.org/v1\n") {
                return Err(invalid("请选择加密的 .tar.gz.age 备份文件"));
            }
            self.call("clean-uploads", json!({})).await?;
            let mut entries = tokio::fs::read_dir(self.root.join("uploads"))
                .await
                .map_err(|_| invalid("无法读取上传目录"))?;
            let mut total = 0u64;
            while let Some(entry) = entries
                .next_entry()
                .await
                .map_err(|_| invalid("无法读取上传目录"))?
            {
                total += entry
                    .metadata()
                    .await
                    .map_err(|_| invalid("无法读取上传文件"))?
                    .len();
            }
            if total > 8 * 1024 * 1024 * 1024 {
                return Err(invalid("暂存的上传文件过多，请完成恢复并在一天后重试"));
            }
        }
        let id = id.unwrap_or_else(|| uuid::Uuid::now_v7().simple().to_string());
        if id.len() != 32
            || !id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || offset + bytes.len() as u64 > 2 * 1024 * 1024 * 1024
        {
            return Err(invalid("上传编号或文件大小无效（最多 2 GiB）"));
        }
        let name = format!("upload-{id}.age");
        let path = self.root.join("uploads").join(&name);
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true);
        if offset == 0 {
            options.create_new(true);
        } else {
            options.append(true);
        }
        #[cfg(unix)]
        {
            options.mode(0o600);
        }
        let mut file = options
            .open(&path)
            .await
            .map_err(|_| invalid("无法保存上传文件，请检查空间或重新选择文件"))?;
        if file
            .metadata()
            .await
            .map_err(|_| invalid("无法读取上传状态"))?
            .len()
            != offset
        {
            return Err(invalid("上传偏移不匹配，请重新上传"));
        }
        file.write_all(&bytes)
            .await
            .map_err(|_| invalid("上传失败，请检查存储空间"))?;
        if complete {
            file.sync_all()
                .await
                .map_err(|_| invalid("无法保存上传文件"))?;
        }
        Ok(
            json!({"id": id, "name": name, "imported": true, "offset": offset + bytes.len() as u64, "complete": complete}),
        )
    }
    async fn download(
        &self,
        credential: RecoveryCredential,
        name: String,
    ) -> Result<BackupFile, UseCaseError> {
        self.authorize(&credential, false).await?;
        if name.contains(['/', '\\'])
            || !name.starts_with("blog-")
            || !name.ends_with(".tar.gz.age")
            || name.len() > 80
        {
            return Err(invalid("备份文件名无效"));
        }
        let path = self.root.join("backups").join(&name);
        let metadata = tokio::fs::symlink_metadata(&path)
            .await
            .map_err(|_| UseCaseError::NotFound("备份".into()))?;
        if !metadata.is_file() || metadata.is_symlink() {
            return Err(UseCaseError::NotFound("备份".into()));
        }
        Ok(BackupFile {
            path,
            size: metadata.len(),
            name,
        })
    }
    async fn logout(&self, credential: RecoveryCredential) -> Result<(), UseCaseError> {
        self.authorize(&credential, true).await?;
        self.sessions
            .lock()
            .expect("session lock")
            .remove(&credential.token);
        Ok(())
    }
}
