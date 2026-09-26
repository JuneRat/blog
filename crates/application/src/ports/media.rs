//! 媒体资产、文件存储与附着授权端口。

use async_trait::async_trait;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::UseCaseError;

/// 引用来源的内容类型，与 `content_media_refs.content_type` 一一对应。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaContentKind {
    Post,
    Page,
    /// 系列封面。系列目录页对任何已存在系列公开可达，因此系列封面即公开来源。
    Series,
    /// 用户头像。公开来源 = 账号未软删除（用户确认的规则）。
    User,
    /// 站点 logo。站点配置本身公开，因此 logo 只要有引用即公开来源。
    Site,
}

/// `content_type = 'site'` 的占位内容 id。
///
/// settings 是单例、以 `key` 为主键且没有 uuid：站点 logo 的引用行需要
/// `content_id`（复合主键的一部分），这里用固定的 nil UUID 占位。单例语义由
/// 「站点设置保存时整体替换这组引用」保证，与其它内容类型互不干扰。
pub const SITE_MEDIA_CONTENT_ID: Uuid = Uuid::from_bytes([0u8; 16]);

impl MediaContentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Post => "post",
            Self::Page => "page",
            Self::Series => "series",
            Self::User => "user",
            Self::Site => "site",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "post" => Some(Self::Post),
            "page" => Some(Self::Page),
            "series" => Some(Self::Series),
            "user" => Some(Self::User),
            "site" => Some(Self::Site),
            _ => None,
        }
    }
}

/// 媒体库列表行：资产元数据 + 上传者展示名 + 引用计数。
///
/// 两个计数都由 `content_media_refs` 与内容可见性谓词实时求出：冗余计数列会在
/// 发布/撤回后立刻失真，而「能否删除」与「能否匿名读取」都直接依赖它们。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaWithUsage {
    pub snapshot: domain::media::MediaSnapshot,
    pub owner_display: String,
    /// 全部引用数（含草稿/私密/回收站）——决定「能否删除」。
    pub reference_count: i64,
    /// 其中构成公开来源的引用数——决定「匿名能否读取」。
    pub public_reference_count: i64,
}

/// 「使用位置」行：媒体被哪个内容引用、该内容此刻是否公开可见。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaUsageRow {
    pub kind: MediaContentKind,
    pub content_id: Uuid,
    /// Post 的作者；Page 无作者，为 None。
    ///
    /// 使用位置**按调用者权限过滤**需要它（own 只能看自己的草稿/私密），
    /// 但删除保护的引用计数不过滤——引用是否存在与调用者能否看见无关。
    pub author_id: Option<Uuid>,
    pub slug: String,
    pub title: String,
    /// draft/published/archived。
    pub status: String,
    /// public/private。
    pub visibility: String,
    /// Post 回收站中的内容（Page 无软删除，恒 false）。
    pub deleted: bool,
    /// 该引用是否构成公开来源（与 `public_reference_count` 同一谓词）。
    pub public: bool,
}

/// 媒体删除的受控结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaDeleteOutcome {
    /// 已进入 `pending_deletion`；重复请求时状态已是 `pending_deletion`，同样报 Marked。
    Marked,
    StaleVersion,
    /// 仍被内容引用（含草稿/私密/回收站）：引用保护拒绝删除，携带引用数。
    Referenced {
        count: i64,
    },
    /// 记录不存在或已删除。
    Gone,
}

/// [`MediaRefGuard::attachable_status`] 的结论：只回答授权问题，不携带内容。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaAttachStatus {
    /// 上传者；「本人上传」的判据。
    pub owner_id: Uuid,
    /// 是否已有公开来源引用：匿名本就可读，再附着一处不产生新的暴露面。
    pub publicly_referenced: bool,
}

/// 附着媒体引用前的归属授权读取面（头像/封面/logo）。
///
/// 完整的媒体库操作在 [`MediaRepository`]；这个窄端口只回答一个问题：
/// 「这个资产现在处于什么归属与公开状态」。用例层据此执行不变量
/// 「不得把他人私有的图片经引用变成匿名可读」——用户头像、系列封面与
/// 站点 logo 的引用是无条件公开来源，这正是归属校验必须发生在写入前的
/// 原因。做成独立端口而不是塞进 MediaRepository，是为了让只做附着检查的
/// 用例（内容/身份/设置）不必背上整个媒体库的读写面。
#[async_trait]
pub trait MediaRefGuard: Send + Sync {
    /// 资产的归属与公开性；None = 不存在或不是 `ready`（不可引用）。
    async fn attachable_status(&self, id: Uuid) -> Result<Option<MediaAttachStatus>, UseCaseError>;
}

/// 媒体元数据仓储。
///
/// 引用关系（`content_media_refs`）由 Post/Page 仓储在保存的同一事务内整体替换，
/// 「是否仍被使用」以该表为唯一判据，不靠搜索 Markdown 文本。
#[async_trait]
pub trait MediaRepository: Send + Sync {
    /// 登记已校验的上传聚合；实现必须拒绝非 `staged` 状态。
    async fn insert_staged(&self, aggregate: &domain::media::Media) -> Result<(), UseCaseError>;

    /// `staged → ready`。未命中（记录消失或已被回收）返回 false。
    async fn mark_ready(&self, id: Uuid, now: OffsetDateTime) -> Result<bool, UseCaseError>;

    /// 任意状态读取（读取授权与删除流程用）。
    async fn find_by_id(
        &self,
        id: Uuid,
    ) -> Result<Option<domain::media::MediaSnapshot>, UseCaseError>;

    /// 仅 `ready` 资产 + 上传者展示名 + 引用计数。
    async fn find_view(&self, id: Uuid) -> Result<Option<MediaWithUsage>, UseCaseError>;

    /// 媒体库分页（仅 `ready`，按创建时间倒序），返回当页与总数。
    async fn list(
        &self,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<MediaWithUsage>, i64), UseCaseError>;

    /// 该资产的全部使用位置（按内容类型与 slug 排序）。
    async fn usage_of(&self, id: Uuid) -> Result<Vec<MediaUsageRow>, UseCaseError>;

    /// 是否存在**公开来源**引用：匿名读取媒体文件的唯一依据。
    async fn has_public_reference(&self, id: Uuid) -> Result<bool, UseCaseError>;

    /// `ready → pending_deletion`：行锁内先校验引用，再按版本迁移状态。
    async fn begin_delete(
        &self,
        id: Uuid,
        expected_version: i64,
        now: OffsetDateTime,
    ) -> Result<MediaDeleteOutcome, UseCaseError>;

    /// `pending_deletion → deleted`：文件删除已确认。幂等，已是 deleted 返回 false。
    async fn confirm_deleted(&self, id: Uuid, now: OffsetDateTime) -> Result<bool, UseCaseError>;

    /// 认领**超过宽限期**仍未完成的 `staged` 上传：`staged → pending_deletion`。
    ///
    /// 必须是**单语句条件更新**并在同一条语句里返回被认领的行：这样它与
    /// `staged → ready` 只有一个能成功，回收不会删掉刚被推进到 `ready` 的文件。
    /// 认领后停在 `pending_deletion`，因此文件删除失败仍可重试。
    async fn claim_abandoned_staged(
        &self,
        created_before: OffsetDateTime,
        now: OffsetDateTime,
        limit: i64,
    ) -> Result<Vec<domain::media::MediaSnapshot>, UseCaseError>;

    /// 待回收资产（`pending_deletion`，按创建时间升序，便于逐个重试）。
    async fn list_pending_deletion(
        &self,
        limit: i64,
    ) -> Result<Vec<domain::media::MediaSnapshot>, UseCaseError>;
}

/// 媒体文件存储端口。
///
/// `key` 是相对于媒体根目录的存储路径（形如 `objects/<uuid>.png`），由应用层按随机
/// id 生成；实现方负责映射到具体存储位置并拒绝越界路径。
#[async_trait]
pub trait MediaStorage: Send + Sync {
    /// 写入暂存区，返回内容 SHA-256（十六进制小写）。同 key 重复写入是覆盖。
    async fn put_staged(&self, key: &str, bytes: &[u8]) -> Result<String, UseCaseError>;

    /// 暂存 → 正式（原子重命名）。正式已存在视为成功。
    async fn promote(&self, key: &str) -> Result<(), UseCaseError>;

    /// 读取正式文件；不存在返回 None。
    async fn read(&self, key: &str) -> Result<Option<Vec<u8>>, UseCaseError>;

    /// 幂等删除正式与暂存文件；两处都不存在按成功处理。
    async fn delete(&self, key: &str) -> Result<(), UseCaseError>;

    /// 清理**没有数据库记录**的暂存残留，返回删除个数。
    ///
    /// 覆盖两类真实缺口：写入暂存后数据库插入失败（如约束冲突、连接中断），
    /// 以及进程在两步之间退出——它们都留下一个没有任何行指向的暂存文件，
    /// 按数据库状态扫描的回收永远找不到它。
    ///
    /// `older_than` 是安全门槛：更新的暂存文件可能属于正在进行的上传（包括
    /// 尚未插入行的窗口），一律保留。`.part` 等写入残留也在这个范围内清理。
    async fn discard_orphaned_staging(
        &self,
        older_than: OffsetDateTime,
    ) -> Result<i64, UseCaseError>;
}

/// Inspect binary image headers without coupling the domain to file formats.
pub trait ImageInspector: Send + Sync {
    fn inspect(&self, bytes: &[u8]) -> Result<domain::media::ImageInfo, domain::media::MediaError>;
}
