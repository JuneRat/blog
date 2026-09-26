//! Binary header inspection belongs to the infrastructure adapter.
//! This reads declared dimensions, not a full decode or integrity check.
use domain::media::{ImageFormat, ImageInfo, MAX_IMAGE_BYTES, MediaError};

pub struct HeaderImageInspector;
impl application::ports::ImageInspector for HeaderImageInspector {
    fn inspect(&self, bytes: &[u8]) -> Result<ImageInfo, MediaError> {
        inspect_image(bytes)
    }
}

/// 从文件头识别格式并读出声明尺寸；无法识别返回 None。
pub fn sniff(bytes: &[u8]) -> Option<(ImageFormat, u32, u32)> {
    if let Some((width, height)) = sniff_png(bytes) {
        return Some((ImageFormat::Png, width, height));
    }
    if let Some((width, height)) = sniff_jpeg(bytes) {
        return Some((ImageFormat::Jpeg, width, height));
    }
    if let Some((width, height)) = sniff_webp(bytes) {
        return Some((ImageFormat::WebP, width, height));
    }
    if let Some((width, height)) = sniff_gif(bytes) {
        return Some((ImageFormat::Gif, width, height));
    }
    None
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
    let (format, width, height) = sniff(bytes).ok_or(MediaError::UnsupportedFormat)?;
    ImageInfo::new(format, width, height)
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
        // RFC 6386 §9.1: 3-byte frame tag, then the key-frame start code.
        if bytes.get(20)? & 1 != 0 {
            return None;
        }
        if bytes.get(23..26)? != b"\x9d\x01\x2a" {
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
    use domain::media::MAX_IMAGE_DIMENSION;
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
        assert_eq!(sniff(&png(320, 200)), Some((ImageFormat::Png, 320, 200)));
        assert_eq!(
            sniff(&jpeg(1024, 768)),
            Some((ImageFormat::Jpeg, 1024, 768))
        );
        assert_eq!(sniff(&gif(64, 48)), Some((ImageFormat::Gif, 64, 48)));

        // VP8X 扩展格式：画布尺寸以「减一」保存。
        // 布局：RIFF(4) size(4) WEBP(4) "VP8X"(4) chunk_size(4) flags+reserved(4) 之后是宽高。
        let mut vp8x = b"RIFF".to_vec();
        vp8x.extend_from_slice(&[0, 0, 0, 0]);
        vp8x.extend_from_slice(b"WEBPVP8X");
        vp8x.extend_from_slice(&[10, 0, 0, 0]);
        vp8x.extend_from_slice(&[0, 0, 0, 0]);
        vp8x.extend_from_slice(&(800u32 - 1).to_le_bytes()[..3]);
        vp8x.extend_from_slice(&(600u32 - 1).to_le_bytes()[..3]);
        assert_eq!(sniff(&vp8x), Some((ImageFormat::WebP, 800, 600)));
    }

    #[test]
    fn rejects_non_image_and_truncated_input() {
        assert_eq!(sniff(b""), None);
        assert_eq!(sniff(b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>"), None);
        assert_eq!(sniff(b"GIF89a"), None, "截断的 GIF 头不能通过");
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n"), None);
        // JPEG 只有 SOI，没有 SOF：无法确定尺寸。
        assert_eq!(sniff(b"\xff\xd8\xff\xd9"), None);
    }

    #[test]
    fn inspect_enforces_size_and_dimension_limits() {
        assert_eq!(inspect_image(b""), Err(MediaError::Empty));
        assert_eq!(
            inspect_image(&png(1, 1)).unwrap().format(),
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
    fn real_encoder_fixtures_include_lossy_webp_frame_tag() {
        let samples: &[(&[u8], ImageFormat)] = &[
            (
                include_bytes!("../tests/fixtures/images/pixel.png"),
                ImageFormat::Png,
            ),
            (
                include_bytes!("../tests/fixtures/images/pixel.jpg"),
                ImageFormat::Jpeg,
            ),
            (
                include_bytes!("../tests/fixtures/images/pixel.gif"),
                ImageFormat::Gif,
            ),
            (
                include_bytes!("../tests/fixtures/images/pixel-lossy.webp"),
                ImageFormat::WebP,
            ),
            (
                include_bytes!("../tests/fixtures/images/pixel-lossless.webp"),
                ImageFormat::WebP,
            ),
        ];
        for (bytes, format) in samples {
            let info = inspect_image(bytes).unwrap();
            assert_eq!(
                (info.format(), info.width(), info.height()),
                (*format, 1, 1)
            );
        }
        let lossy = include_bytes!("../tests/fixtures/images/pixel-lossy.webp");
        for end in 0..30 {
            assert!(inspect_image(&lossy[..end]).is_err());
        }
        let mut invalid = lossy.to_vec();
        invalid[23] = 0;
        assert_eq!(inspect_image(&invalid), Err(MediaError::UnsupportedFormat));
    }
}
