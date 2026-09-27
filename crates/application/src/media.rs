//! 媒体上传、共享媒体库、软删除/恢复与独立公开读取。

use std::sync::Arc;

use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::UseCaseError;
use crate::identity::Actor;
use crate::ports::ImageInspector;
use crate::ports::{
    Clock, MediaChangeOutcome, MediaRepository, MediaStorage, MediaUsageRow, MediaWithUsage,
};

/// 媒体库每页条目数。
pub const MEDIA_PAGE_SIZE: i64 = 24;

/// 只清理一小时前的暂存残留；正式对象不参与自动清扫。
pub const STAGED_GRACE_SECS: i64 = 3600;

/// 站内媒体地址前缀。插入、复制地址与正文解析共用同一形状。
pub const MEDIA_URL_PREFIX: &str = "/media/";

/// 生成站内媒体地址。
pub fn media_url(id: Uuid) -> String {
    format!("{MEDIA_URL_PREFIX}{id}")
}

/// 新引用要求媒体存在且未软删除；公开媒体没有基于上传者的附着限制。
pub async fn ensure_attachable(
    guard: &dyn crate::ports::MediaRefGuard,
    id: Uuid,
) -> Result<(), UseCaseError> {
    if guard.is_attachable(id).await? {
        Ok(())
    } else {
        Err(UseCaseError::Invalid(
            "引用了不存在或已移入回收站的图片".into(),
        ))
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
    pub deleted_at: Option<OffsetDateTime>,
    pub version: i64,
    pub created_at: OffsetDateTime,
    pub owner_id: Option<Uuid>,
    pub owner_display: String,
    /// 站内地址：正文插入与「复制地址」共用。
    pub url: String,
    /// 全部引用数（含草稿/私密/回收站），软删除保留这些引用。
    pub reference_count: i64,
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
            deleted_at: view.snapshot.deleted_at,
            version: view.snapshot.version,
            created_at: view.snapshot.created_at,
            owner_id: view.snapshot.owner_id,
            owner_display: view.owner_display.clone(),
            url: media_url(view.snapshot.id),
            reference_count: view.reference_count,
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
    /// 来源内容是否公开，用于使用位置的权限过滤。
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

/// 媒体详情与使用位置。
#[derive(Debug, Clone, serde::Serialize)]
pub struct MediaUsageView {
    pub media: MediaDto,
    /// 调用者有权查看的使用位置（Post 按 own/any，Page 按站点级 `page.read`）。
    pub references: Vec<MediaUsageDto>,
    /// 存在但调用者无权查看的引用数。
    ///
    /// 引用计数按全部引用计算，展示来源必须过滤；
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

    /// 校验图片 → 暂存 → 原子移入正式位置 → 元数据与审计一起登记。
    /// 数据库失败时不删除正式对象：提交结果可能不确定，避免损坏已提交的记录。
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
        let media = domain::media::Media::uploaded(
            id,
            audit_actor_id(actor),
            key.clone(),
            &cmd.file_name,
            info,
            cmd.bytes.len() as u64,
            checksum,
            now,
        )
        .map_err(|e| UseCaseError::Invalid(e.to_string()))?;
        self.storage.promote(&key).await?;
        self.media.insert(&media, actor.audit_context()).await?;
        self.detail_of(actor, id).await.map(|view| view.media)
    }

    /// 媒体库分页（按上传时间倒序）。
    pub async fn list(
        &self,
        actor: &Actor,
        page: i64,
        trash: bool,
    ) -> Result<MediaPage, UseCaseError> {
        if !actor.has_permission("media.read") {
            return Err(UseCaseError::Forbidden);
        }
        if !(1..=i64::MAX / MEDIA_PAGE_SIZE).contains(&page) {
            return Err(UseCaseError::Invalid("页码超出范围".into()));
        }
        let (rows, total) = self
            .media
            .list(MEDIA_PAGE_SIZE, (page - 1) * MEDIA_PAGE_SIZE, trash)
            .await?;
        Ok(MediaPage {
            items: rows.iter().map(MediaDto::from).collect(),
            total,
            page,
            per_page: MEDIA_PAGE_SIZE,
        })
    }

    /// 单个资产详情与调用者有权查看的使用位置。
    pub async fn detail(&self, actor: &Actor, id: Uuid) -> Result<MediaUsageView, UseCaseError> {
        if !actor.has_permission("media.read") {
            return Err(UseCaseError::Forbidden);
        }
        self.detail_of(actor, id).await
    }

    /// 软删除或恢复；文件和已有引用不变，归属权限与版本由本次请求明确指定。
    pub async fn set_deleted(
        &self,
        actor: &Actor,
        id: Uuid,
        expected_version: i64,
        deleted: bool,
    ) -> Result<(), UseCaseError> {
        actor.ensure_write_channel()?;
        let snapshot = self
            .media
            .find_by_id(id)
            .await?
            .ok_or_else(media_not_found)?;
        let allowed = (snapshot.owner_id == Some(actor.user_id.0)
            && actor.has_permission("media.delete"))
            || actor.has_permission("media.delete_any");
        if !allowed {
            return Err(UseCaseError::Forbidden);
        }
        match self
            .media
            .set_deleted(
                id,
                expected_version,
                deleted,
                self.clock.now(),
                actor.audit_context(),
            )
            .await?
        {
            MediaChangeOutcome::Updated | MediaChangeOutcome::Unchanged => Ok(()),
            MediaChangeOutcome::StaleVersion => Err(UseCaseError::VersionConflict),
            MediaChangeOutcome::Gone => Err(media_not_found()),
        }
    }

    /// 链接独立公开，软删除记录同样可读；不访问会话或来源内容。
    pub async fn read(&self, id: Uuid) -> Result<MediaContent, UseCaseError> {
        let snapshot = self
            .media
            .find_by_id(id)
            .await?
            .ok_or_else(media_not_found)?;
        let bytes = self
            .storage
            .read(&snapshot.storage_key)
            .await?
            .ok_or_else(|| {
                UseCaseError::Repository(format!("媒体记录存在但文件缺失：{}", snapshot.id))
            })?;
        Ok(MediaContent {
            mime: snapshot.mime,
            bytes,
            checksum_sha256: snapshot.checksum_sha256,
        })
    }

    /// 只清理未完成上传的暂存文件。正式对象即使零引用也不自动删除。
    pub async fn cleanup_staging(&self, actor: &Actor) -> Result<i64, UseCaseError> {
        actor.ensure_write_channel()?;
        if !actor.has_permission("media.delete_any") {
            return Err(UseCaseError::Forbidden);
        }
        self.storage
            .discard_orphaned_staging(self.clock.now() - time::Duration::seconds(STAGED_GRACE_SECS))
            .await
    }

    /// 组装详情，并按调用者权限过滤使用位置。
    ///
    /// 媒体库是共享资源，`media.read` 让人能**浏览**资产；它不等于能读任意内容。
    /// 使用位置会暴露草稿/私密文章的标题与 slug，因此必须逐条按内容权限过滤：
    /// Post 走 own/any（`post.read` / `post.read_any`），Page 走站点级 `page.read`。
    ///
    /// 引用计数不过滤，供使用统计与独立物理清理检查；
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

/// 媒体记录不存在。
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
        // 图片公开不代表用户资料可公开浏览。
        crate::ports::MediaContentKind::User => {
            row.content_id == actor.user_id.0 || actor.has_permission("user.manage")
        }
        // 站点 logo 只有 settings.manage 能改；公开分支已放行，这里处理异常情况。
        crate::ports::MediaContentKind::Site => actor.has_permission("settings.manage"),
    }
}

fn audit_actor_id(actor: &Actor) -> Option<Uuid> {
    if actor.channel == crate::identity::ActorChannel::ControlledCli && actor.user_id.0.is_nil() {
        None
    } else {
        Some(actor.user_id.0)
    }
}
