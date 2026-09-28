//! 媒体元数据、引用可用性与文件存储端口。

use crate::error::UseCaseError;
use async_trait::async_trait;
use domain::content::{Visibility, page::PageStatus, post::PostStatus};
use domain::identity::UserStatus;
use time::OffsetDateTime;
use uuid::Uuid;

/// 与 media_refs.source_type 对应；引用只记录使用位置，不决定图片公开性。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaContentKind {
    Post,
    Page,
    Series,
    User,
    Site,
}

/// 站点配置单例使用 nil UUID，其他来源使用实体 UUID。
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaWithUsage {
    pub snapshot: domain::media::MediaSnapshot,
    pub owner_display: String,
    /// 含草稿、私密、回收站的全部引用，供物理清理保护；不阻止软删除。
    pub reference_count: i64,
}

/// 引用来源与其状态一起表达，避免把账号状态当作发布状态。
/// Series/Site 没有发布状态；其传输展示值由接口层决定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaUsageSource {
    Post(PostStatus),
    Page(PageStatus),
    User(UserStatus),
    Series,
    Site,
}

impl MediaUsageSource {
    pub fn kind(self) -> MediaContentKind {
        match self {
            Self::Post(_) => MediaContentKind::Post,
            Self::Page(_) => MediaContentKind::Page,
            Self::User(_) => MediaContentKind::User,
            Self::Series => MediaContentKind::Series,
            Self::Site => MediaContentKind::Site,
        }
    }
}

/// 来源可见性仅用于过滤使用位置，不能用来授权媒体文件读取。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaUsageRow {
    pub source: MediaUsageSource,
    pub content_id: Uuid,
    pub author_id: Option<Uuid>,
    pub slug: String,
    pub title: String,
    pub visibility: Visibility,
    pub deleted: bool,
    pub public: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaChangeOutcome {
    Updated,
    Unchanged,
    StaleVersion,
    Gone,
}

/// 新引用要求记录存在且未软删除；保留历史引用由提交事务另外校验。
#[async_trait]
pub trait MediaRefGuard: Send + Sync {
    async fn is_attachable(&self, id: Uuid) -> Result<bool, UseCaseError>;
}

#[async_trait]
pub trait MediaRepository: Send + Sync {
    /// 文件完成写入后登记；元数据与上传审计同事务提交。
    async fn insert(
        &self,
        aggregate: &domain::media::Media,
        actor_id: crate::audit::AuditContext,
    ) -> Result<(), UseCaseError>;
    /// 含软删除记录，用于独立公开的文件读取。
    async fn find_by_id(
        &self,
        id: Uuid,
    ) -> Result<Option<domain::media::MediaSnapshot>, UseCaseError>;
    async fn find_view(&self, id: Uuid) -> Result<Option<MediaWithUsage>, UseCaseError>;
    /// 正常库与回收站分别分页，上传者可以为空。
    async fn list(
        &self,
        limit: i64,
        offset: i64,
        trash: bool,
    ) -> Result<(Vec<MediaWithUsage>, i64), UseCaseError>;
    async fn usage_of(&self, id: Uuid) -> Result<Vec<MediaUsageRow>, UseCaseError>;
    /// 同事务校验版本、软删除/恢复与追加审计，不清除文件或引用。
    async fn set_deleted(
        &self,
        id: Uuid,
        expected_version: i64,
        deleted: bool,
        now: OffsetDateTime,
        actor_id: crate::audit::AuditContext,
    ) -> Result<MediaChangeOutcome, UseCaseError>;
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

    /// 清理超过宽限期的暂存残留，返回删除个数。
    ///
    /// 处理写入中断、promote 失败等没有数据库记录的暂存残留。
    /// 数据库登记在 promote 之后；登记失败留下的正式对象不由此方法清理。
    ///
    /// `older_than` 是安全门槛：更新的暂存文件可能属于正在进行的上传（包括
    /// 尚未登记数据库的窗口），一律保留。`.part` 等写入残留也在这个范围内清理。
    async fn discard_orphaned_staging(
        &self,
        older_than: OffsetDateTime,
    ) -> Result<i64, UseCaseError>;
}

/// Inspect binary image headers without coupling the domain to file formats.
pub trait ImageInspector: Send + Sync {
    fn inspect(&self, bytes: &[u8]) -> Result<domain::media::ImageInfo, domain::media::MediaError>;
}
