//! 媒体上下文：图片资产的身份、内容校验与生命周期约束。
//!
//! 第一版只服务 Post/Page 正文图片，只接受常见**位图**格式。
//! 有意不开放 SVG（矢量、可内嵌脚本与外部引用，清洗边界完全不同）、
//! 视频与任意附件：这些需要的校验、转码与访问规则不属于本版范围。
//!
//! 资产状态机（docs/content-lifecycle.md §5）：
//!
//! ```text
//! staged ──promote──▶ ready ──删除请求───▶ pending_deletion ──文件删除成功──▶ deleted
//!    │                                          ▲
//!    └──reclaim 放弃（超过宽限期，CAS）─────────┘
//! ```
//!
//! `ready` 才可能被内容引用；`pending_deletion` 起不再接受新引用，
//! 但文件是否已删除与数据库状态是两件事，因此回收必须可重入。
//!
//! **`staged → ready` 与「放弃未完成上传」是对同一行的互斥条件更新**：
//! 上传与回收只有一个能拿到 `staged` 行，拿到的一方负责文件，另一方绝不触碰。
//! 缺少这层互斥时，回收会删掉一个刚被推进到 `ready` 的资产的文件，
//! 留下指向缺失文件的可用记录。

use time::OffsetDateTime;
use uuid::Uuid;

/// 单张图片的字节上限。
pub const MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;

/// 单边像素上限（防止声明尺寸离谱的头部把后续处理拖垮）。
pub const MAX_IMAGE_DIMENSION: u32 = 12_000;

/// 总像素上限（约 6000 万；即使不解码也先拒绝明显不合理的声明）。
pub const MAX_IMAGE_PIXELS: u64 = 60_000_000;

/// 原始文件名保留的字符上限（只用于展示，从不参与路径拼接）。
pub const MAX_ORIGINAL_NAME_CHARS: usize = 200;

/// 未识别出文件名时的展示占位。
pub const UNTITLED_NAME: &str = "未命名图片";

/// 资产生命周期状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaStatus {
    /// 已落暂存、尚未原子移入正式位置：上传中断即停在这里，等待补偿清理。
    Staged,
    /// 可用状态：可被内容引用，也可能因被公开内容引用而允许匿名读取。
    Ready,
    /// 已决定删除且文件删除可能尚未完成：不再接受新引用。
    PendingDeletion,
    /// 文件已删除（幂等重试会再次经过这里，行保留以便重放与审计）。
    Deleted,
}

impl MediaStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Staged => "staged",
            Self::Ready => "ready",
            Self::PendingDeletion => "pending_deletion",
            Self::Deleted => "deleted",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "staged" => Some(Self::Staged),
            "ready" => Some(Self::Ready),
            "pending_deletion" => Some(Self::PendingDeletion),
            "deleted" => Some(Self::Deleted),
            _ => None,
        }
    }

    pub fn is_ready(self) -> bool {
        matches!(self, Self::Ready)
    }
}

/// One transition table shared by the aggregate and conditional SQL writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaTransition {
    Ready,
    RequestDeletion,
    ConfirmDeleted,
    AbandonStaged,
}
impl MediaTransition {
    pub fn source(self) -> MediaStatus {
        match self {
            Self::Ready | Self::AbandonStaged => MediaStatus::Staged,
            Self::RequestDeletion => MediaStatus::Ready,
            Self::ConfirmDeleted => MediaStatus::PendingDeletion,
        }
    }
    pub fn target(self) -> MediaStatus {
        match self {
            Self::Ready => MediaStatus::Ready,
            Self::RequestDeletion | Self::AbandonStaged => MediaStatus::PendingDeletion,
            Self::ConfirmDeleted => MediaStatus::Deleted,
        }
    }
    pub fn apply(self, current: MediaStatus) -> Result<MediaStatus, MediaError> {
        if current == self.source() || (self != Self::Ready && current == self.target()) {
            Ok(self.target())
        } else {
            Err(MediaError::InvalidState("当前状态不允许该迁移"))
        }
    }
}

/// 允许上传的位图格式。格式由**文件内容**判定，不信扩展名与 Content-Type。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    Jpeg,
    Gif,
    WebP,
}

impl ImageFormat {
    pub fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
            Self::WebP => "image/webp",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Gif => "gif",
            Self::WebP => "webp",
        }
    }
}

/// 通过内容校验的图片信息。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageInfo {
    format: ImageFormat,
    width: u32,
    height: u32,
}

impl ImageInfo {
    pub fn new(format: ImageFormat, width: u32, height: u32) -> Result<Self, MediaError> {
        if width == 0
            || height == 0
            || width > MAX_IMAGE_DIMENSION
            || height > MAX_IMAGE_DIMENSION
            || u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS
        {
            return Err(MediaError::InvalidDimensions);
        }
        Ok(Self {
            format,
            width,
            height,
        })
    }
    pub fn format(self) -> ImageFormat {
        self.format
    }
    pub fn width(self) -> u32 {
        self.width
    }
    pub fn height(self) -> u32 {
        self.height
    }
}

/// 只保留展示所需的文件名：去掉任何目录成分与控制字符。
///
/// 原始名永远不参与路径拼接（存储路径由随机 id 决定），这里仅是防展示层
/// 被 `../../etc/passwd` 之类的输入误导，并给出稳定的空值占位。
pub fn normalize_original_name(raw: &str) -> String {
    let base = raw
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(raw)
        .chars()
        .filter(|ch| !ch.is_control())
        .collect::<String>();
    let trimmed = base.trim();
    if trimmed.is_empty() {
        return UNTITLED_NAME.to_string();
    }
    trimmed.chars().take(MAX_ORIGINAL_NAME_CHARS).collect()
}

/// 媒体资产快照：领域与持久化之间的完整状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaSnapshot {
    pub id: Uuid,
    /// 上传者；媒体库是共享资源，但「删除本人上传」按它判定归属。
    pub owner_id: Uuid,
    /// 相对存储路径，由随机 id 与格式后缀组成（例如 `media/<uuid>.png`）。
    pub storage_key: String,
    /// 展示用文件名，已规范化。
    pub original_name: String,
    pub mime: String,
    pub byte_size: i64,
    pub width: i32,
    pub height: i32,
    /// 内容 SHA-256（十六进制小写）；文件完整性与幂等重传的判据。
    pub checksum_sha256: String,
    pub status: MediaStatus,
    pub version: i64,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

impl MediaSnapshot {
    pub fn is_ready(&self) -> bool {
        self.status.is_ready()
    }
}

/// 媒体资产聚合：只暴露受控的状态迁移，不提供任意 setter。
#[derive(Debug, Clone)]
pub struct Media {
    snapshot: MediaSnapshot,
}

impl Media {
    /// 登记一次上传：先进入 `staged`，文件落正式位置后再 [`Media::mark_ready`]。
    #[allow(clippy::too_many_arguments)]
    pub fn stage(
        id: Uuid,
        owner_id: Uuid,
        mut storage_key: String,
        original_name: &str,
        info: ImageInfo,
        byte_size: u64,
        checksum_sha256: String,
        now: OffsetDateTime,
    ) -> Result<Self, MediaError> {
        if byte_size == 0 {
            return Err(MediaError::Empty);
        }
        if byte_size > MAX_IMAGE_BYTES {
            return Err(MediaError::TooLarge);
        }
        let expected_suffix = format!(".{}", info.format.extension());
        if !storage_key.ends_with(&expected_suffix) {
            // 存储路径与识别出的格式必须一致：否则 Content-Type 与文件内容会打架。
            storage_key.push_str(&expected_suffix);
        }
        Ok(Self {
            snapshot: MediaSnapshot {
                id,
                owner_id,
                storage_key,
                original_name: normalize_original_name(original_name),
                mime: info.format.mime().to_string(),
                byte_size: byte_size as i64,
                width: info.width as i32,
                height: info.height as i32,
                checksum_sha256,
                status: MediaStatus::Staged,
                version: 1,
                created_at: now,
                updated_at: now,
            },
        })
    }

    pub fn reconstitute(snapshot: MediaSnapshot) -> Result<Self, MediaError> {
        let format = [
            ImageFormat::Png,
            ImageFormat::Jpeg,
            ImageFormat::Gif,
            ImageFormat::WebP,
        ]
        .into_iter()
        .find(|format| format.mime() == snapshot.mime)
        .ok_or(MediaError::UnsupportedFormat)?;
        ImageInfo::new(
            format,
            u32::try_from(snapshot.width).map_err(|_| MediaError::InvalidDimensions)?,
            u32::try_from(snapshot.height).map_err(|_| MediaError::InvalidDimensions)?,
        )?;
        if snapshot.byte_size <= 0 {
            return Err(MediaError::Empty);
        }
        if snapshot.byte_size as u64 > MAX_IMAGE_BYTES {
            return Err(MediaError::TooLarge);
        }
        if snapshot.version < 1 {
            return Err(MediaError::InvalidState("版本必须为正整数"));
        }
        Ok(Self { snapshot })
    }

    fn transition(
        &mut self,
        action: MediaTransition,
        now: OffsetDateTime,
    ) -> Result<bool, MediaError> {
        let next = action.apply(self.snapshot.status)?;
        if next == self.snapshot.status {
            return Ok(false);
        }
        self.snapshot.status = next;
        self.snapshot.version += 1;
        self.snapshot.updated_at = now;
        Ok(true)
    }

    /// `staged → ready`：文件已原子移入正式位置。
    pub fn mark_ready(&mut self, now: OffsetDateTime) -> Result<bool, MediaError> {
        self.transition(MediaTransition::Ready, now)
    }

    /// `ready → pending_deletion`：已决定删除，等待文件删除完成。
    pub fn mark_pending_deletion(&mut self, now: OffsetDateTime) -> Result<bool, MediaError> {
        self.transition(MediaTransition::RequestDeletion, now)
    }

    /// `pending_deletion → deleted`：文件删除已确认完成。
    pub fn mark_deleted(&mut self, now: OffsetDateTime) -> Result<bool, MediaError> {
        self.transition(MediaTransition::ConfirmDeleted, now)
    }

    /// `staged → pending_deletion`：放弃一次未完成的上传（回收流程的补偿起点）。
    ///
    /// 有意**不直接跳到 `deleted`**：复用 `pending_deletion` 让「已决定回收、但文件
    /// 还没删掉」与「文件已删」保持可区分，回收因此能重试到文件确实消失为止。
    /// 这也是与上传互斥的关键：本迁移与 `staged → ready` 都是对同一行的条件更新，
    /// 两者只有一个能成功；拿到 `staged` 的一方负责文件，另一方绝不能碰。
    pub fn abandon_staged(&mut self, now: OffsetDateTime) -> Result<bool, MediaError> {
        self.transition(MediaTransition::AbandonStaged, now)
    }

    pub fn id(&self) -> Uuid {
        self.snapshot.id
    }

    pub fn version(&self) -> i64 {
        self.snapshot.version
    }

    pub fn owner_id(&self) -> Uuid {
        self.snapshot.owner_id
    }

    pub fn snapshot(&self) -> MediaSnapshot {
        self.snapshot.clone()
    }
}

/// 媒体规则错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MediaError {
    #[error("图片内容为空")]
    Empty,
    #[error("图片超过 {MAX_IMAGE_BYTES} 字节上限")]
    TooLarge,
    #[error("无法识别的图片内容；仅支持 PNG、JPEG、GIF、WebP")]
    UnsupportedFormat,
    #[error(
        "图片尺寸无效或超出上限（单边 ≤ {MAX_IMAGE_DIMENSION}，总计 ≤ {MAX_IMAGE_PIXELS} 像素）"
    )]
    InvalidDimensions,
    #[error("媒体状态不允许该操作：{0}")]
    InvalidState(&'static str),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn original_name_is_stripped_to_a_safe_display_value() {
        assert_eq!(normalize_original_name("../../etc/passwd"), "passwd");
        assert_eq!(normalize_original_name("C:\\photos\\a.png"), "a.png");
        assert_eq!(normalize_original_name("   "), UNTITLED_NAME);
        assert_eq!(normalize_original_name("a\u{0}b.png"), "ab.png");
        assert_eq!(
            normalize_original_name(&"x".repeat(500)).chars().count(),
            MAX_ORIGINAL_NAME_CHARS
        );
    }

    #[test]
    fn lifecycle_only_allows_staged_to_ready_to_deleted() {
        let now = OffsetDateTime::UNIX_EPOCH;
        let info = ImageInfo::new(ImageFormat::Png, 4, 4).unwrap();
        let mut media = Media::stage(
            Uuid::now_v7(),
            Uuid::now_v7(),
            "media/x.png".into(),
            "x.png",
            info,
            10,
            "a".repeat(64),
            now,
        )
        .unwrap();
        assert_eq!(media.snapshot().status, MediaStatus::Staged);
        // 未就绪即按「删除」处理是非法迁移（只有回收能通过 abandon_staged 放弃暂存）。
        assert!(media.mark_pending_deletion(now).is_err());
        assert!(media.mark_ready(now).unwrap());
        assert_eq!(media.version(), 2);
        assert!(media.mark_pending_deletion(now).unwrap());
        // 重复请求幂等：不报错、不再递增版本。
        assert!(!media.mark_pending_deletion(now).unwrap());
        assert!(media.mark_deleted(now).unwrap());
        assert!(!media.mark_deleted(now).unwrap());
        assert_eq!(media.snapshot().status, MediaStatus::Deleted);
    }

    #[test]
    fn abandoning_a_staged_upload_goes_through_pending_deletion_so_removal_is_retryable() {
        let now = OffsetDateTime::UNIX_EPOCH;
        let info = ImageInfo::new(ImageFormat::Png, 4, 4).unwrap();
        let mut media = Media::stage(
            Uuid::now_v7(),
            Uuid::now_v7(),
            "objects/x.png".into(),
            "x.png",
            info,
            10,
            "a".repeat(64),
            now,
        )
        .unwrap();

        assert!(media.abandon_staged(now).unwrap());
        assert_eq!(
            media.snapshot().status,
            MediaStatus::PendingDeletion,
            "放弃的上传必须停在待回收，而不是直接标记已删除"
        );
        // 幂等：重复放弃不报错、不再递增版本。
        assert!(!media.abandon_staged(now).unwrap());
        // 已就绪的资产不能被当成「未完成上传」放弃。
        let mut ready = Media::reconstitute(media.snapshot()).unwrap();
        ready.snapshot.status = MediaStatus::Ready;
        assert!(ready.abandon_staged(now).is_err());
        assert!(media.mark_deleted(now).unwrap());
    }

    #[test]
    fn stage_normalizes_storage_key_suffix_to_the_detected_format() {
        let info = ImageInfo::new(ImageFormat::Png, 4, 4).unwrap();
        let media = Media::stage(
            Uuid::now_v7(),
            Uuid::now_v7(),
            "media/x".into(),
            "x",
            info,
            10,
            "a".repeat(64),
            OffsetDateTime::UNIX_EPOCH,
        )
        .unwrap();
        assert!(media.snapshot().storage_key.ends_with(".png"));
    }

    #[test]
    fn image_info_and_reconstitution_enforce_dimensions() {
        for (width, height) in [
            (0, 1),
            (1, 0),
            (u32::MAX, 1),
            (1, u32::MAX),
            (10_000, 10_000),
        ] {
            assert_eq!(
                ImageInfo::new(ImageFormat::WebP, width, height),
                Err(MediaError::InvalidDimensions)
            );
        }
        assert!(ImageInfo::new(ImageFormat::Png, MAX_IMAGE_DIMENSION, 5000).is_ok());
        let media = Media::stage(
            Uuid::now_v7(),
            Uuid::now_v7(),
            "objects/x.png".into(),
            "x",
            ImageInfo::new(ImageFormat::Png, 1, 1).unwrap(),
            10,
            "a".repeat(64),
            OffsetDateTime::UNIX_EPOCH,
        )
        .unwrap();
        for (width, height) in [(0, 1), (1, -1)] {
            let mut invalid = media.snapshot();
            invalid.width = width;
            invalid.height = height;
            assert_eq!(
                Media::reconstitute(invalid).unwrap_err(),
                MediaError::InvalidDimensions
            );
        }
    }
}
