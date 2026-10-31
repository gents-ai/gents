//! Claude's image limits applied to tool-result images, and the header
//! readers that size an image without decoding it.

use base64::Engine as _;
use gents_protocol::message::{
    DocumentSourceKind, Image, ImageMediaType, Message, ToolResultContent, UserContent,
};

pub(super) fn media_type(image: &Image) -> Option<&'static str> {
    match image.media_type {
        Some(ImageMediaType::JPEG) => Some("image/jpeg"),
        Some(ImageMediaType::PNG) => Some("image/png"),
        Some(ImageMediaType::GIF) => Some("image/gif"),
        Some(ImageMediaType::WEBP) => Some("image/webp"),
        _ => None,
    }
}

/// Anthropic's request limits: 8000 px per side, 2000 px for requests over 20
/// images, and 100 images maximum.
const CLAUDE_IMAGE_MAX_SIDE: u32 = 8000;
const CLAUDE_MANY_IMAGES: usize = 20;
const CLAUDE_MANY_IMAGES_MAX_SIDE: u32 = 2000;
const CLAUDE_MAX_IMAGES: usize = 100;

pub(super) fn image_omissions(history: &[Message]) -> Vec<Option<String>> {
    let images: Vec<Option<&str>> = history
        .iter()
        .filter_map(|message| match message {
            Message::User { content } => Some(content),
            _ => None,
        })
        .flatten()
        .filter_map(|block| match block {
            UserContent::ToolResult(result) => Some(&result.content),
            _ => None,
        })
        .flatten()
        .filter_map(|part| match part {
            ToolResultContent::Image(image) => match (&image.data, media_type(image)) {
                (DocumentSourceKind::Base64(data), Some(_)) => Some(Some(data.as_str())),
                (DocumentSourceKind::Url(_), _) => Some(None),
                _ => None,
            },
            _ => None,
        })
        .collect();
    let mut notes = vec![None; images.len()];
    let (mut kept, mut kept_large) = (0, false);
    for (index, data) in images.iter().enumerate().rev() {
        if kept == CLAUDE_MAX_IMAGES {
            notes[index] = Some(format!(
                "[image omitted: a Claude request carries at most {CLAUDE_MAX_IMAGES} images and later ones were kept; ask the tool again to see it]"
            ));
            continue;
        }
        let (width, height) = data.and_then(base64_image_dimensions).unwrap_or((0, 0));
        let side = width.max(height);
        let large = side > CLAUDE_MANY_IMAGES_MAX_SIDE;
        notes[index] = if side > CLAUDE_IMAGE_MAX_SIDE {
            Some(format!(
                "[image omitted: {width}×{height} px exceeds Claude's {CLAUDE_IMAGE_MAX_SIDE} px limit; ask the tool for a smaller view]"
            ))
        } else if kept >= CLAUDE_MANY_IMAGES && large {
            Some(format!(
                "[image omitted: {width}×{height} px exceeds Claude's {CLAUDE_MANY_IMAGES_MAX_SIDE} px limit for requests with more than {CLAUDE_MANY_IMAGES} images; ask the tool for a smaller view]"
            ))
        } else if kept >= CLAUDE_MANY_IMAGES && kept_large {
            Some(format!(
                "[image omitted: a Claude request with an image over {CLAUDE_MANY_IMAGES_MAX_SIDE} px carries at most {CLAUDE_MANY_IMAGES} images and later ones were kept; ask the tool again to see it]"
            ))
        } else {
            kept += 1;
            kept_large |= large;
            None
        };
    }
    notes
}

fn base64_image_dimensions(data: &str) -> Option<(u32, u32)> {
    let decode = |chars: usize| {
        let prefix = if chars >= data.len() {
            data.as_bytes()
        } else {
            &data.as_bytes()[..chars / 4 * 4]
        };
        base64::engine::general_purpose::STANDARD
            .decode(prefix)
            .ok()
    };
    let head = decode(64)?;
    if !head.starts_with(&[0xFF, 0xD8]) {
        return image_header_dimensions(&head);
    }
    let mut chars = 4096;
    loop {
        if let Some(size) = jpeg_dimensions(&decode(chars)?) {
            return Some(size);
        }
        if chars >= data.len() {
            return None;
        }
        chars *= 2;
    }
}

fn image_header_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let be32 = |at: usize| Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?));
    let le16 = |at: usize| Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?) as u32);
    let le24 = |at: usize| {
        let b = bytes.get(at..at + 3)?;
        Some(u32::from_le_bytes([b[0], b[1], b[2], 0]))
    };
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") && bytes.get(12..16)? == b"IHDR" {
        return Some((be32(16)?, be32(20)?));
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some((le16(6)?, le16(8)?));
    }
    if bytes.starts_with(b"RIFF") && bytes.get(8..12)? == b"WEBP" {
        return match bytes.get(12..16)? {
            b"VP8 " => Some((le16(26)? & 0x3FFF, le16(28)? & 0x3FFF)),
            b"VP8L" => {
                let bits = u32::from_le_bytes(bytes.get(21..25)?.try_into().ok()?);
                Some(((bits & 0x3FFF) + 1, ((bits >> 14) & 0x3FFF) + 1))
            }
            b"VP8X" => Some((le24(24)? + 1, le24(27)? + 1)),
            _ => None,
        };
    }
    None
}

fn jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let be16 = |at: usize| Some(u16::from_be_bytes(bytes.get(at..at + 2)?.try_into().ok()?) as u32);
    let mut at = 2;
    loop {
        if *bytes.get(at)? != 0xFF {
            return None;
        }
        while *bytes.get(at)? == 0xFF {
            at += 1;
        }
        let marker = *bytes.get(at)?;
        at += 1;
        match marker {
            0xC0..=0xCF if !matches!(marker, 0xC4 | 0xC8 | 0xCC) => {
                return Some((be16(at + 5)?, be16(at + 3)?));
            }
            0x01 | 0xD0..=0xD7 => {}
            0xD8 | 0xD9 => return None,
            _ => at += usize::try_from(be16(at)?).ok()?,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    #[test]
    fn dimensions_come_from_each_supported_header() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        png.extend(9000u32.to_be_bytes());
        png.extend(4000u32.to_be_bytes());
        let mut gif = b"GIF89a".to_vec();
        gif.extend(640u16.to_le_bytes());
        gif.extend(480u16.to_le_bytes());
        let mut vp8 = b"RIFF\0\0\0\0WEBPVP8 \0\0\0\0\0\0\0\x9d\x01\x2a".to_vec();
        vp8.extend(2100u16.to_le_bytes());
        vp8.extend(300u16.to_le_bytes());
        let mut vp8l = b"RIFF\0\0\0\0WEBPVP8L\0\0\0\0\x2f".to_vec();
        vp8l.extend(((8191u32 - 1) | ((20 - 1) << 14)).to_le_bytes());
        let mut vp8x = b"RIFF\0\0\0\0WEBPVP8X\0\0\0\0\0\0\0\0".to_vec();
        vp8x.extend(&(9999u32 - 1).to_le_bytes()[..3]);
        vp8x.extend(&(5u32 - 1).to_le_bytes()[..3]);
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE1, 0x00, 0x06, 1, 2, 3, 4];
        jpeg.extend([0xFF, 0xC0, 0x00, 0x11, 8]);
        jpeg.extend(1200u16.to_be_bytes());
        jpeg.extend(8001u16.to_be_bytes());
        for (bytes, size) in [
            (png, (9000, 4000)),
            (gif, (640, 480)),
            (vp8, (2100, 300)),
            (vp8l, (8191, 20)),
            (vp8x, (9999, 5)),
            (jpeg, (8001, 1200)),
        ] {
            assert_eq!(
                base64_image_dimensions(&b64(&bytes)),
                Some(size),
                "{bytes:?}"
            );
        }
        let mut late = vec![0xFF, 0xD8, 0xFF, 0xE1, 0x27, 0x12];
        late.extend(vec![0; 0x2710]);
        late.extend([0xFF, 0xC2, 0x00, 0x11, 8]);
        late.extend(30u16.to_be_bytes());
        late.extend(40u16.to_be_bytes());
        assert_eq!(base64_image_dimensions(&b64(&late)), Some((40, 30)));
        assert_eq!(base64_image_dimensions(&b64(b"not an image")), None);
        assert_eq!(base64_image_dimensions("not base64!"), None);
    }
}
