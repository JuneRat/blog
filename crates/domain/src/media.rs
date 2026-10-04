//! 图片资产：文件完成写入后登记，软删除保留文件、地址和引用。

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
    pub owner_id: Option<Uuid>,
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
    pub deleted_at: Option<OffsetDateTime>,
    pub version: i64,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

impl MediaSnapshot {
    pub fn is_available(&self) -> bool {
        self.deleted_at.is_none()
    }
}

/// 媒体资产聚合：只暴露受控的状态迁移，不提供任意 setter。
#[derive(Debug, Clone)]
pub struct Media {
    snapshot: MediaSnapshot,
}

impl Media {
    /// 文件已完整存储后登记媒体，不在业务表中保存上传中间状态。
    #[allow(clippy::too_many_arguments)]
    pub fn uploaded(
        id: Uuid,
        owner_id: Option<Uuid>,
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
        let media = Self {
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
                deleted_at: None,
                version: 1,
                created_at: now,
                updated_at: now,
            },
        };
        Self::reconstitute(media.snapshot)
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
        if snapshot.checksum_sha256.len() != 64
            || !snapshot
                .checksum_sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(MediaError::InvalidState("SHA-256 摘要无效"));
        }
        if snapshot.version < 1 {
            return Err(MediaError::InvalidState("版本必须为正整数"));
        }
        Ok(Self { snapshot })
    }

    /// 软删除或恢复仅改变管理状态，文件及引用保持原样。
    pub fn set_deleted(&mut self, deleted: bool, now: OffsetDateTime) -> bool {
        if self.snapshot.deleted_at.is_some() == deleted {
            return false;
        }
        self.snapshot.deleted_at = deleted.then_some(now);
        self.snapshot.version += 1;
        self.snapshot.updated_at = now;
        true
    }

    pub fn id(&self) -> Uuid {
        self.snapshot.id
    }

    pub fn version(&self) -> i64 {
        self.snapshot.version
    }

    pub fn owner_id(&self) -> Option<Uuid> {
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
    fn trash_and_restore_preserve_identity_and_are_idempotent() {
        let now = OffsetDateTime::UNIX_EPOCH;
        let mut media = Media::uploaded(
            Uuid::now_v7(),
            None,
            "objects/x.png".into(),
            "x.png",
            ImageInfo::new(ImageFormat::Png, 4, 4).unwrap(),
            10,
            "a".repeat(64),
            now,
        )
        .unwrap();
        let original = media.snapshot();
        assert!(media.set_deleted(true, now));
        assert!(!media.set_deleted(true, now));
        assert_eq!(media.version(), 2);
        assert_eq!(media.snapshot().storage_key, original.storage_key);
        assert!(media.set_deleted(false, now));
        assert!(!media.set_deleted(false, now));
        assert_eq!(media.version(), 3);
        assert!(media.snapshot().is_available());
    }

    #[test]
    fn upload_normalizes_storage_key_suffix_to_the_detected_format() {
        let info = ImageInfo::new(ImageFormat::Png, 4, 4).unwrap();
        let media = Media::uploaded(
            Uuid::now_v7(),
            Some(Uuid::now_v7()),
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
        let media = Media::uploaded(
            Uuid::now_v7(),
            Some(Uuid::now_v7()),
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
