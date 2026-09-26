//! 媒体用例：上传、浏览、使用位置、引用保护删除与可重试回收；公开读取授权。
//!
//! 公开访问边界（docs/content-lifecycle.md §5）：上传后默认不公开，后台预览需要
//! `media.read`；只有当图片被**公开发布的 Post 或 Page** 引用时，匿名请求才可读取。
//! 文章改为私密、撤回或移入回收站后，若没有其他公开引用，匿名访问立即停止——
//! 判定在每次读取时实时查询引用与内容可见性，不使用会失真的缓存快照。
//!
//! 「是否仍被使用」以 `content_media_refs` 关系表为唯一判据；Markdown 文本只在
//! 保存内容时被解析一次并固化，删除流程不回头搜索正文（docs/content-lifecycle.md §5）。

use std::sync::Arc;

use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::UseCaseError;
use crate::identity::Actor;
use crate::ports::ImageInspector;
use crate::ports::{
    Clock, MediaDeleteOutcome, MediaRepository, MediaStorage, MediaUsageRow, MediaWithUsage,
};

/// 媒体库每页条目数。
pub const MEDIA_PAGE_SIZE: i64 = 24;

/// 单次回收扫描的批上限：避免一条命令把内存与文件句柄拖满。
pub const RECLAIM_BATCH: i64 = 500;

/// 未完成上传的回收宽限期（秒）。
///
/// 只有创建时间早于 `now - 宽限期` 的 `staged` 资产才会被回收认领：
/// 更新的资产可能正被上传推进到 `ready`（含「文件已写入、行尚未插入」的窗口），
/// 把它们当作垃圾会在上传中途釜底抽薪。一小时远大于任何正常上传耗时。
pub const STAGED_GRACE_SECS: i64 = 3600;

/// 站内媒体地址前缀。插入、复制地址与正文解析共用同一形状。
pub const MEDIA_URL_PREFIX: &str = "/media/";

/// 生成站内媒体地址。
pub fn media_url(id: Uuid) -> String {
    format!("{MEDIA_URL_PREFIX}{id}")
}

/// 显式附着媒体引用的归属授权（头像/封面/logo 共用）。
///
/// 这些字段与正文图片不同：调用者直接提交外部资产 id，而头像、系列封面与
/// 站点 logo 的引用是**无条件**的公开来源——行落库即匿名可读。因此必须防止
/// 「拿到他人图片 UUID → 附着为头像」把私有图片变成公开图片。
///
/// 放行三种情况（任一满足即可）：
/// - 本人上传：`owner_id` 与调用者一致；
/// - 已有公开来源引用：匿名本就可读，再附着一处不产生新的暴露面；
/// - 持 `media.read`：本就可预览库内全部资产，附着不扩大其可见范围。
///
/// 资产不存在或未就绪按「引用了不存在或已不可用的图片」报 Invalid——
/// 与存储层 `sync_media_refs` 的既有口径一致；归属不满足才报
/// [`UseCaseError::MediaNotAttachable`]。调用方只在**值发生变化**时调用
/// （重复保存当前值不重新授权），避免编辑他人内容时被历史引用卡住。
pub async fn ensure_attachable(
    guard: &dyn crate::ports::MediaRefGuard,
    actor: &Actor,
    id: Uuid,
) -> Result<(), UseCaseError> {
    match guard.attachable_status(id).await? {
        None => Err(UseCaseError::Invalid("引用了不存在或已不可用的图片".into())),
        Some(status) => {
            let allowed = status.owner_id == actor.user_id.0
                || status.publicly_referenced
                || actor.has_permission("media.read");
            if allowed {
                Ok(())
            } else {
                Err(UseCaseError::MediaNotAttachable)
            }
        }
    }
}

/// 由随机 id 与格式后缀生成存储路径；文件永远不按原始文件名落盘。
///
/// `objects/` 前缀把「正式对象」与存储根目录下的 `staging/`（飞行中的上传）分开，
/// 让运维在根目录里一眼能区分两类文件，也让键值本身自描述。
pub fn storage_key_for(id: Uuid, extension: &str) -> String {
    format!("objects/{id}.{extension}")
}

/// 上传命令。字节已在接口层限长，这里再做内容与尺寸校验。
#[derive(Debug, Clone)]
pub struct UploadMediaCmd {
    pub file_name: String,
    pub bytes: Vec<u8>,
}

/// 面向后台的媒体视图。
#[derive(Debug, Clone, serde::Serialize)]
pub struct MediaDto {
    pub id: Uuid,
    pub original_name: String,
    pub mime: String,
    pub byte_size: i64,
    pub width: i32,
    pub height: i32,
    pub status: &'static str,
    pub version: i64,
    pub created_at: OffsetDateTime,
    pub owner_id: Uuid,
    pub owner_display: String,
    /// 站内地址：正文插入与「复制地址」共用。
    pub url: String,
    /// 全部引用数（含草稿/私密/回收站）；> 0 时拒绝删除。
    pub reference_count: i64,
    /// 构成公开来源的引用数；> 0 时匿名可读取。
    pub public_reference_count: i64,
}

impl From<&MediaWithUsage> for MediaDto {
    fn from(view: &MediaWithUsage) -> Self {
        Self {
            id: view.snapshot.id,
            original_name: view.snapshot.original_name.clone(),
            mime: view.snapshot.mime.clone(),
            byte_size: view.snapshot.byte_size,
            width: view.snapshot.width,
            height: view.snapshot.height,
            status: view.snapshot.status.as_str(),
            version: view.snapshot.version,
            created_at: view.snapshot.created_at,
            owner_id: view.snapshot.owner_id,
            owner_display: view.owner_display.clone(),
            url: media_url(view.snapshot.id),
            reference_count: view.reference_count,
            public_reference_count: view.public_reference_count,
        }
    }
}

/// 一处使用位置。
#[derive(Debug, Clone, serde::Serialize)]
pub struct MediaUsageDto {
    pub kind: &'static str,
    pub content_id: Uuid,
    pub slug: String,
    pub title: String,
    pub status: String,
    pub visibility: String,
    pub deleted: bool,
    /// 该引用是否让图片匿名可读。
    pub public: bool,
}

impl From<&MediaUsageRow> for MediaUsageDto {
    fn from(row: &MediaUsageRow) -> Self {
        Self {
            kind: row.kind.as_str(),
            content_id: row.content_id,
            slug: row.slug.clone(),
            title: row.title.clone(),
            status: row.status.clone(),
            visibility: row.visibility.clone(),
            deleted: row.deleted,
            public: row.public,
        }
    }
}

/// 媒体详情 + 使用位置（删除前提示用）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct MediaUsageView {
    pub media: MediaDto,
    /// 调用者有权查看的使用位置（Post 按 own/any，Page 按站点级 `page.read`）。
    pub references: Vec<MediaUsageDto>,
    /// 存在但调用者无权查看的引用数。
    ///
    /// 引用计数决定「能否删除」因而必须按全部引用计算，但展示必须过滤；
    /// 不把这部分差额说出来，界面就会显示「被 3 处引用」却只列 1 处。
    pub hidden_references: i64,
}

/// 媒体库分页。
#[derive(Debug, Clone, serde::Serialize)]
pub struct MediaPage {
    pub items: Vec<MediaDto>,
    pub total: i64,
    pub page: i64,
    pub per_page: i64,
}

/// 读取到的媒体文件内容。
#[derive(Debug, Clone)]
pub struct MediaContent {
    pub mime: String,
    pub bytes: Vec<u8>,
    pub checksum_sha256: String,
    /// 本次读取是否由「公开来源引用」放行（而非调用者权限）。
    ///
    /// 接口层据此选择缓存策略：公开引用只能重校验（撤回后必须立即失效），
    /// 后台预览则完全不可缓存。
    pub public_reference: bool,
}

/// 回收流程报告（可核对、可重试）。
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ReclaimReport {
    /// 已认领并放弃的未完成上传（超期 `staged`，从未成为可引用资产）。
    pub abandoned_staged: i64,
    /// 完成删除的待回收资产（`pending_deletion → deleted`）。
    pub deleted: i64,
    /// 清理掉的暂存孤儿文件数（没有任何行指向它们，因此数据库扫描找不到）。
    pub orphaned_staging_files: i64,
    /// 失败项：保留原状态等待下次重试，附资产 id、存储路径与原因。
    pub failures: Vec<String>,
}

pub struct MediaInteractor {
    inspector: Arc<dyn ImageInspector>,
    media: Arc<dyn MediaRepository>,
    storage: Arc<dyn MediaStorage>,
    clock: Arc<dyn Clock>,
}

impl MediaInteractor {
    pub fn new(
        inspector: Arc<dyn ImageInspector>,
        media: Arc<dyn MediaRepository>,
        storage: Arc<dyn MediaStorage>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            inspector,
            media,
            storage,
            clock,
        }
    }

    /// 上传：校验内容 → 落暂存 → 登记 staged → 原子移入正式位置 → 标记 ready。
    ///
    /// `ready` 只在文件确实就位后写入，因此「可被引用」与「文件存在」不会错位。
    /// 中途失败留在 `staged`，由 `media reclaim` 丢弃，不产生半成品资产。
    pub async fn upload(
        &self,
        actor: &Actor,
        cmd: UploadMediaCmd,
    ) -> Result<MediaDto, UseCaseError> {
        actor.ensure_write_channel()?;
        if !actor.has_permission("media.upload") {
            return Err(UseCaseError::Forbidden);
        }
        let info = self
            .inspector
            .inspect(&cmd.bytes)
            .map_err(|e| UseCaseError::Invalid(e.to_string()))?;
        let id = Uuid::now_v7();
        let key = storage_key_for(id, info.format().extension());
        let now = self.clock.now();

        let checksum = self.storage.put_staged(&key, &cmd.bytes).await?;
        let media = domain::media::Media::stage(
            id,
            actor.user_id.0,
            key.clone(),
            &cmd.file_name,
            info,
            cmd.bytes.len() as u64,
            checksum,
            now,
        )
        .map_err(|e| UseCaseError::Invalid(e.to_string()))?;
        if let Err(e) = self.media.insert_staged(&media).await {
            // 文件已写入但**没有行**指向它：按数据库状态扫描的回收永远找不到这个孤儿，
            // 因此这里尽力删除。清理失败也不改变用户看到的错误（仍如实报插入失败）：
            // 文件留在暂存区，由 reclaim 的暂存清扫按宽限期兜底。
            let _ = self.storage.delete(&key).await;
            return Err(e);
        }

        // promote 失败时**不**回滚数据库：留着 staged 行，补偿流程才有的放矢。
        self.storage.promote(&key).await?;
        if !self.media.mark_ready(id, now).await? {
            // 回收先认领了这次上传（超过宽限期的极端慢上传）：如实报错而不是假装成功。
            return Err(UseCaseError::Repository(
                "媒体上传就绪时记录已进入回收（可能被并发回收认领）".into(),
            ));
        }
        self.detail_of(actor, id).await.map(|view| view.media)
    }

    /// 媒体库分页（按上传时间倒序）。
    pub async fn list(&self, actor: &Actor, page: i64) -> Result<MediaPage, UseCaseError> {
        if !actor.has_permission("media.read") {
            return Err(UseCaseError::Forbidden);
        }
        if !(1..=i64::MAX / MEDIA_PAGE_SIZE).contains(&page) {
            return Err(UseCaseError::Invalid("页码超出范围".into()));
        }
        let (rows, total) = self
            .media
            .list(MEDIA_PAGE_SIZE, (page - 1) * MEDIA_PAGE_SIZE)
            .await?;
        Ok(MediaPage {
            items: rows.iter().map(MediaDto::from).collect(),
            total,
            page,
            per_page: MEDIA_PAGE_SIZE,
        })
    }

    /// 单个资产详情与**调用者有权查看**的使用位置（删除前提示、删除被拒后定位引用）。
    pub async fn detail(&self, actor: &Actor, id: Uuid) -> Result<MediaUsageView, UseCaseError> {
        if !actor.has_permission("media.read") {
            return Err(UseCaseError::Forbidden);
        }
        self.detail_of(actor, id).await
    }

    /// 删除：引用保护 → 标记待回收 → 删文件 → 确认删除。
    ///
    /// 文件删除失败时保留 `pending_deletion` 并如实报错；`media reclaim` 可重试，
    /// 期间媒体已不再出现在库中、也不再接受新引用，不会留下可用的坏引用。
    pub async fn delete(
        &self,
        actor: &Actor,
        id: Uuid,
        expected_version: i64,
    ) -> Result<(), UseCaseError> {
        actor.ensure_write_channel()?;
        let Some(snapshot) = self.media.find_by_id(id).await? else {
            return Err(media_not_found());
        };
        let is_owner = snapshot.owner_id == actor.user_id.0;
        let allowed = (is_owner && actor.has_permission("media.delete"))
            || actor.has_permission("media.delete_any");
        if !allowed {
            // 不区分「不存在」与「非本人上传」：媒体库是共享资源，
            // 但删除他人上传需要显式 any 权限。
            return Err(UseCaseError::Forbidden);
        }

        let now = self.clock.now();
        match self.media.begin_delete(id, expected_version, now).await? {
            MediaDeleteOutcome::Marked => {}
            MediaDeleteOutcome::StaleVersion => return Err(UseCaseError::VersionConflict),
            MediaDeleteOutcome::Gone => return Err(media_not_found()),
            MediaDeleteOutcome::Referenced { count } => {
                return Err(UseCaseError::MediaInUse(count));
            }
        }
        self.storage.delete(&snapshot.storage_key).await?;
        self.media.confirm_deleted(id, now).await?;
        Ok(())
    }

    /// 读取媒体文件。
    ///
    /// `viewer` 为已认证调用者：持有 `media.read` 时可预览任意可用图片（后台预览）。
    /// 否则只在存在公开来源引用时放行；其余一律按「不存在」处理，不泄漏资产存在性。
    pub async fn read(
        &self,
        id: Uuid,
        viewer: Option<&Actor>,
    ) -> Result<MediaContent, UseCaseError> {
        let privileged = viewer.is_some_and(|actor| actor.has_permission("media.read"));
        let Some(snapshot) = self.media.find_by_id(id).await? else {
            return Err(media_not_found());
        };
        if !snapshot.status.is_ready() {
            return Err(media_not_found());
        }
        let public_reference = self.media.has_public_reference(id).await?;
        if !privileged && !public_reference {
            return Err(media_not_found());
        }
        let bytes = self
            .storage
            .read(&snapshot.storage_key)
            .await?
            .ok_or_else(|| {
                UseCaseError::Repository(format!(
                    "媒体记录存在但文件缺失：{}",
                    snapshot.storage_key
                ))
            })?;
        Ok(MediaContent {
            mime: snapshot.mime,
            bytes,
            checksum_sha256: snapshot.checksum_sha256,
            public_reference,
        })
    }

    /// 回收流程：认领并放弃超期未完成的上传，完成 `pending_deletion` 的文件删除，
    /// 清理没有数据库记录的暂存残留。
    ///
    /// 幂等且可反复执行；`failures` 逐项给出原因（含存储路径），供运维定位。
    ///
    /// 顺序不可颠倒：
    /// 1. **先认领**（`staged → pending_deletion`，带宽限期）再删文件——认领是与
    ///    `staged → ready` 互斥的条件更新，只有拿到 `staged` 的一方才碰文件。
    ///    先删文件后改状态会删掉刚完成的上传，留下指向缺失文件的可用记录。
    /// 2. 认领结果与上次残留的 `pending_deletion` 一起处理：文件删除失败时停在
    ///    `pending_deletion`，下次执行继续重试，不会被跳过。
    /// 3. 最后清扫暂存区里没有任何行指向的文件（插入失败或进程中途退出留下的），
    ///    同样受宽限期保护。
    pub async fn reclaim(&self, actor: &Actor) -> Result<ReclaimReport, UseCaseError> {
        actor.ensure_write_channel()?;
        if !actor.has_permission("media.delete_any") {
            return Err(UseCaseError::Forbidden);
        }
        let now = self.clock.now();
        let cutoff = now - time::Duration::seconds(STAGED_GRACE_SECS);

        // 1. 认领被放弃的未完成上传（只认领创建时间早于宽限期的）。
        let abandoned_staged = self
            .media
            .claim_abandoned_staged(cutoff, now, RECLAIM_BATCH)
            .await?
            .len() as i64;

        // 2. 统一删除：本次认领的资产此刻已是 pending_deletion，自然落在这次扫描里。
        let mut deleted = 0i64;
        let mut failures = Vec::new();
        for snapshot in self.media.list_pending_deletion(RECLAIM_BATCH).await? {
            match self.storage.delete(&snapshot.storage_key).await {
                Ok(()) => {
                    if self.media.confirm_deleted(snapshot.id, now).await? {
                        deleted += 1;
                    }
                }
                Err(e) => {
                    failures.push(format!("{}（{}）：{e}", snapshot.id, snapshot.storage_key))
                }
            }
        }

        // 3. 清扫没有任何行指向的暂存残留。
        let orphaned_staging_files = self.storage.discard_orphaned_staging(cutoff).await?;
        Ok(ReclaimReport {
            abandoned_staged,
            deleted,
            orphaned_staging_files,
            failures,
        })
    }

    /// 组装详情，并按调用者权限过滤使用位置。
    ///
    /// 媒体库是共享资源，`media.read` 让人能**浏览**资产；它不等于能读任意内容。
    /// 使用位置会暴露草稿/私密文章的标题与 slug，因此必须逐条按内容权限过滤：
    /// Post 走 own/any（`post.read` / `post.read_any`），Page 走站点级 `page.read`。
    ///
    /// 引用**计数**不过滤——它决定「能不能删」，与调用者能否看见引用无关；
    /// 被过滤掉的条数如实返回给界面，避免出现「被 3 处引用」却只列出 1 处。
    async fn detail_of(&self, actor: &Actor, id: Uuid) -> Result<MediaUsageView, UseCaseError> {
        let Some(view) = self.media.find_view(id).await? else {
            return Err(media_not_found());
        };
        let rows = self.media.usage_of(id).await?;
        let visible: Vec<&MediaUsageRow> = rows
            .iter()
            .filter(|row| can_see_reference(actor, row))
            .collect();
        let hidden_references = rows.len() as i64 - visible.len() as i64;
        Ok(MediaUsageView {
            media: MediaDto::from(&view),
            references: visible
                .iter()
                .map(|row| MediaUsageDto::from(*row))
                .collect(),
            hidden_references,
        })
    }
}

/// 未找到、未就绪与无权读取共用同一响应，避免泄漏资产存在性。
fn media_not_found() -> UseCaseError {
    UseCaseError::NotFound("图片".into())
}

/// 调用者能否查看这条使用位置。
///
/// 规则是「调用者有权读该内容，**或**该内容本身就公开可读」：
///
/// - Post 按 own/any 权限对（`post.read` / `post.read_any`），Page 按站点级 `page.read`；
/// - 公开可读的内容（`row.public`，例如已公开发布的文章）直接可见——它的标题与
///   slug 本来就能匿名访问，正文里也带着同一个图片地址；过滤它保护不到任何东西，
///   反而会让 `hidden_references` 把「公开引用」误报成「无权查看的引用」。
///
/// 因此过滤只隐藏调用者**确实读不到**的内容：他人草稿、私密内容，以及自己没有
/// 读取权限的页面。`media.read` 只授予「浏览媒体库」，不附带任何内容阅读权。
fn can_see_reference(actor: &Actor, row: &MediaUsageRow) -> bool {
    if row.public {
        return true;
    }
    match row.kind {
        crate::ports::MediaContentKind::Post => {
            if actor.has_permission("post.read_any") {
                return true;
            }
            // own：只有本人文章可见。作者未知（行已消失）时按不可见处理。
            actor.has_permission("post.read") && row.author_id == Some(actor.user_id.0)
        }
        crate::ports::MediaContentKind::Page => actor.has_permission("page.read"),
        // 系列目录公开可达，因此公开引用在上面的 `row.public` 分支已被放行；
        // 走到这里说明系列行已消失（悬挂引用），按目录管理权限处理。
        crate::ports::MediaContentKind::Series => actor.has_permission("series.manage"),
        // 头像的公开来源是「账号未软删除」；已软删除账号的头像只有账号管理员可见。
        crate::ports::MediaContentKind::User => actor.has_permission("user.manage"),
        // 站点 logo 只有 settings.manage 能改；公开分支已放行，这里处理异常情况。
        crate::ports::MediaContentKind::Site => actor.has_permission("settings.manage"),
    }
}
