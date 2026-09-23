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

    /// 从文件头识别格式并读出声明尺寸；无法识别返回 None。
    pub fn sniff(bytes: &[u8]) -> Option<(Self, u32, u32)> {
        if let Some((width, height)) = sniff_png(bytes) {
            return Some((Self::Png, width, height));
        }
        if let Some((width, height)) = sniff_jpeg(bytes) {
            return Some((Self::Jpeg, width, height));
        }
        if let Some((width, height)) = sniff_webp(bytes) {
            return Some((Self::WebP, width, height));
        }
        if let Some((width, height)) = sniff_gif(bytes) {
            return Some((Self::Gif, width, height));
        }
        None
    }
}

/// 通过内容校验的图片信息。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageInfo {
    pub format: ImageFormat,
    pub width: u32,
    pub height: u32,
}

/// 校验上传字节：非空、未超限、格式可识别、尺寸在界内。
///
/// 只解析文件头，不完整解码；损坏的文件仍可能通过这里——但格式、声明尺寸与
/// 大小是后续处理与展示的先决条件，必须在入库前确定。
pub fn inspect_image(bytes: &[u8]) -> Result<ImageInfo, MediaError> {
    if bytes.is_empty() {
        return Err(MediaError::Empty);
    }
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        return Err(MediaError::TooLarge);
    }
    let (format, width, height) = ImageFormat::sniff(bytes).ok_or(MediaError::UnsupportedFormat)?;
    if width == 0
        || height == 0
        || width > MAX_IMAGE_DIMENSION
        || height > MAX_IMAGE_DIMENSION
        || u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS
    {
        return Err(MediaError::InvalidDimensions);
    }
    Ok(ImageInfo {
        format,
        width,
        height,
    })
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

    pub fn reconstitute(snapshot: MediaSnapshot) -> Self {
        Self { snapshot }
    }

    /// `staged → ready`：文件已原子移入正式位置。
    pub fn mark_ready(&mut self, now: OffsetDateTime) -> Result<bool, MediaError> {
        if self.snapshot.status != MediaStatus::Staged {
            return Err(MediaError::InvalidState("只有暂存中的媒体可以就绪"));
        }
        self.snapshot.status = MediaStatus::Ready;
        self.snapshot.version += 1;
        self.snapshot.updated_at = now;
        Ok(true)
    }

    /// `ready → pending_deletion`：已决定删除，等待文件删除完成。
    pub fn mark_pending_deletion(&mut self, now: OffsetDateTime) -> Result<bool, MediaError> {
        match self.snapshot.status {
            MediaStatus::PendingDeletion => Ok(false),
            MediaStatus::Ready => {
                self.snapshot.status = MediaStatus::PendingDeletion;
                self.snapshot.version += 1;
                self.snapshot.updated_at = now;
                Ok(true)
            }
            _ => Err(MediaError::InvalidState("只有可用中的媒体可以删除")),
        }
    }

    /// `pending_deletion → deleted`：文件删除已确认完成。
    pub fn mark_deleted(&mut self, now: OffsetDateTime) -> Result<bool, MediaError> {
        match self.snapshot.status {
            MediaStatus::Deleted => Ok(false),
            MediaStatus::PendingDeletion => {
                self.snapshot.status = MediaStatus::Deleted;
                self.snapshot.version += 1;
                self.snapshot.updated_at = now;
                Ok(true)
            }
            _ => Err(MediaError::InvalidState("只有待回收的媒体可以确认删除")),
        }
    }

    /// `staged → pending_deletion`：放弃一次未完成的上传（回收流程的补偿起点）。
    ///
    /// 有意**不直接跳到 `deleted`**：复用 `pending_deletion` 让「已决定回收、但文件
    /// 还没删掉」与「文件已删」保持可区分，回收因此能重试到文件确实消失为止。
    /// 这也是与上传互斥的关键：本迁移与 `staged → ready` 都是对同一行的条件更新，
    /// 两者只有一个能成功；拿到 `staged` 的一方负责文件，另一方绝不能碰。
    pub fn abandon_staged(&mut self, now: OffsetDateTime) -> Result<bool, MediaError> {
        match self.snapshot.status {
            MediaStatus::PendingDeletion => Ok(false),
            MediaStatus::Staged => {
                self.snapshot.status = MediaStatus::PendingDeletion;
                self.snapshot.version += 1;
                self.snapshot.updated_at = now;
                Ok(true)
            }
            _ => Err(MediaError::InvalidState("只有暂存中的上传可以放弃")),
        }
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

// ---------------------------------------------------------------------------
// 文件头解析：只读取声明尺寸，不做完整解码。
// ---------------------------------------------------------------------------

fn be_u32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn be_u16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn le_u16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn le_u32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn le_u24(bytes: &[u8], at: usize) -> Option<u32> {
    let slice = bytes.get(at..at + 3)?;
    Some(u32::from(slice[0]) | (u32::from(slice[1]) << 8) | (u32::from(slice[2]) << 16))
}

fn sniff_png(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.get(..8)? != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    // 第一个块必须是 IHDR（长度 13 + 类型），尺寸紧跟在类型之后。
    if bytes.get(8..16)? != b"\x00\x00\x00\x0dIHDR" {
        return None;
    }
    Some((be_u32(bytes, 16)?, be_u32(bytes, 20)?))
}

fn sniff_jpeg(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.get(..2)? != b"\xff\xd8" {
        return None;
    }
    let mut pos = 2usize;
    loop {
        // 允许 0xFF 填充；随后一个字节是标记本身。
        while *bytes.get(pos)? == 0xff {
            pos += 1;
        }
        let marker = *bytes.get(pos)?;
        pos += 1;
        match marker {
            // 无长度字段的标记：TEM 与 RSTn/SOI。
            0x01 | 0xd0..=0xd8 => continue,
            // EOI 与 SOS：之后不会再有 SOF。
            0xd9 | 0xda => return None,
            _ => {}
        }
        let length = usize::from(be_u16(bytes, pos)?);
        if length < 2 {
            return None;
        }
        // SOF0–SOF15，排除 DHT(0xC4)、JPG(0xC8)、DAC(0xCC)。
        if matches!(marker, 0xc0..=0xcf) && !matches!(marker, 0xc4 | 0xc8 | 0xcc) {
            let height = be_u16(bytes, pos + 3)?;
            let width = be_u16(bytes, pos + 5)?;
            return Some((u32::from(width), u32::from(height)));
        }
        pos = pos.checked_add(length)?;
    }
}

fn sniff_gif(bytes: &[u8]) -> Option<(u32, u32)> {
    let header = bytes.get(..6)?;
    if header != b"GIF87a" && header != b"GIF89a" {
        return None;
    }
    Some((u32::from(le_u16(bytes, 6)?), u32::from(le_u16(bytes, 8)?)))
}

fn sniff_webp(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.get(..4)? != b"RIFF" || bytes.get(8..12)? != b"WEBP" {
        return None;
    }
    let chunk = bytes.get(12..16)?;
    if chunk == b"VP8 " {
        // 有损：关键帧起始码之后是 14 位宽、14 位高。
        if bytes.get(20..23)? != b"\x9d\x01\x2a" {
            return None;
        }
        Some((
            u32::from(le_u16(bytes, 26)? & 0x3fff),
            u32::from(le_u16(bytes, 28)? & 0x3fff),
        ))
    } else if chunk == b"VP8L" {
        // 无损：位流中先宽后高，各 14 位。
        if *bytes.get(20)? != 0x2f {
            return None;
        }
        let bits = le_u32(bytes, 21)?;
        Some(((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1))
    } else if chunk == b"VP8X" {
        // 扩展：画布尺寸以「减一」的 24 位小端保存。
        Some((le_u24(bytes, 24)? + 1, le_u24(bytes, 27)? + 1))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
        bytes
    }

    fn jpeg(width: u16, height: u16) -> Vec<u8> {
        let mut bytes = b"\xff\xd8".to_vec();
        // APP0 段，用于验证解析器会按长度跳过非 SOF 段。
        bytes.extend_from_slice(&[0xff, 0xe0, 0x00, 0x10]);
        bytes.extend_from_slice(b"JFIF\0\x01\x02\x00\x00\x01\x00\x01\x00\x00");
        bytes.extend_from_slice(&[0xff, 0xc0, 0x00, 0x11, 0x08]);
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&[0x03, 0x01, 0x11, 0x00, 0x02, 0x11, 0x01, 0x03, 0x11, 0x01]);
        bytes
    }

    fn gif(width: u16, height: u16) -> Vec<u8> {
        let mut bytes = b"GIF89a".to_vec();
        bytes.extend_from_slice(&width.to_le_bytes());
        bytes.extend_from_slice(&height.to_le_bytes());
        bytes.extend_from_slice(&[0x00, 0x00, 0x00]);
        bytes
    }

    #[test]
    fn sniffs_each_supported_bitmap_format() {
        assert_eq!(
            ImageFormat::sniff(&png(320, 200)),
            Some((ImageFormat::Png, 320, 200))
        );
        assert_eq!(
            ImageFormat::sniff(&jpeg(1024, 768)),
            Some((ImageFormat::Jpeg, 1024, 768))
        );
        assert_eq!(
            ImageFormat::sniff(&gif(64, 48)),
            Some((ImageFormat::Gif, 64, 48))
        );

        // VP8X 扩展格式：画布尺寸以「减一」保存。
        // 布局：RIFF(4) size(4) WEBP(4) "VP8X"(4) chunk_size(4) flags+reserved(4) 之后是宽高。
        let mut vp8x = b"RIFF".to_vec();
        vp8x.extend_from_slice(&[0, 0, 0, 0]);
        vp8x.extend_from_slice(b"WEBPVP8X");
        vp8x.extend_from_slice(&[10, 0, 0, 0]);
        vp8x.extend_from_slice(&[0, 0, 0, 0]);
        vp8x.extend_from_slice(&(800u32 - 1).to_le_bytes()[..3]);
        vp8x.extend_from_slice(&(600u32 - 1).to_le_bytes()[..3]);
        assert_eq!(
            ImageFormat::sniff(&vp8x),
            Some((ImageFormat::WebP, 800, 600))
        );
    }

    #[test]
    fn rejects_non_image_and_truncated_input() {
        assert_eq!(ImageFormat::sniff(b""), None);
        assert_eq!(
            ImageFormat::sniff(b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>"),
            None
        );
        assert_eq!(ImageFormat::sniff(b"GIF89a"), None, "截断的 GIF 头不能通过");
        assert_eq!(ImageFormat::sniff(b"\x89PNG\r\n\x1a\n"), None);
        // JPEG 只有 SOI，没有 SOF：无法确定尺寸。
        assert_eq!(ImageFormat::sniff(b"\xff\xd8\xff\xd9"), None);
    }

    #[test]
    fn inspect_enforces_size_and_dimension_limits() {
        assert_eq!(inspect_image(b""), Err(MediaError::Empty));
        assert_eq!(
            inspect_image(&png(1, 1)).unwrap().format,
            ImageFormat::Png,
            "合法最小图片应通过"
        );
        assert_eq!(
            inspect_image(&png(0, 10)),
            Err(MediaError::InvalidDimensions)
        );
        assert_eq!(
            inspect_image(&png(MAX_IMAGE_DIMENSION + 1, 1)),
            Err(MediaError::InvalidDimensions)
        );
        assert_eq!(
            inspect_image(&png(10_000, 10_000)),
            Err(MediaError::InvalidDimensions),
            "总像素超限必须拒绝"
        );
        assert_eq!(
            inspect_image(&b"\x00".repeat((MAX_IMAGE_BYTES + 1) as usize)),
            Err(MediaError::TooLarge)
        );
        assert_eq!(
            inspect_image(b"not an image at all"),
            Err(MediaError::UnsupportedFormat)
        );
    }

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
        let info = inspect_image(&png(4, 4)).unwrap();
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
        let info = inspect_image(&png(4, 4)).unwrap();
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
        let mut ready = Media::reconstitute(media.snapshot());
        ready.snapshot.status = MediaStatus::Ready;
        assert!(ready.abandon_staged(now).is_err());
        assert!(media.mark_deleted(now).unwrap());
    }

    #[test]
    fn stage_normalizes_storage_key_suffix_to_the_detected_format() {
        let info = inspect_image(&png(4, 4)).unwrap();
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
}
