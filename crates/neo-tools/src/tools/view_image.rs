//! `view_image` —— 查看图片。
//!
//! 元数据查询只读文件头；附图时有界解码并转换为 PNG/JPEG。
//! 仅发送首帧，必要时等比缩小，原文件不变；识别由多模态模型完成。

use super::base64_lite::encode as b64;
use serde_json::json;

use crate::result::{Args, ErrorKind, Outcome, ToolError};
use crate::spec::Param;
use crate::Scope;

use super::limits;

pub static PARAMS: &[Param] = &[
    Param::text("path", "图片路径，相对工作区；例如 `docs/screens/01-hero.png`。"),
    Param::flag(
        "include_data",
        "true = **把图直接交给模型看**（多模态模型会真的看到这张图，可用于认图、读图上的文字）；\
         false = 只返回格式 / 尺寸 / 体积这些元信息。要看图内容就设 true；只是想知道文件多大就不必。",
    ),
];

pub fn preview(args: &Args) -> String {
    let path = args.opt_str("path").unwrap_or_default();
    if args.flag("include_data").unwrap_or(false) {
        format!("查看图片 {path} 并取回原始数据")
    } else {
        format!("查看图片 {path} 的信息")
    }
}

pub fn run(scope: &Scope, args: &Args) -> Outcome {
    match view(scope, args) {
        Ok(o) => o,
        Err(e) => Outcome::fail("view_image", e),
    }
}

fn view(scope: &Scope, args: &Args) -> Result<Outcome, ToolError> {
    let raw = args.require_str("path")?;
    let include_data = args.flag("include_data")?;

    let path = scope.resolve(&raw)?;
    let meta = std::fs::metadata(&path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => ToolError::not_found(format!("图片不存在：{raw}"))
            .with_hint("用 `bash` 的 `ls` 确认实际路径"),
        _ => ToolError::io(format!("无法访问 {raw}：{e}")),
    })?;
    if meta.is_dir() {
        return Err(ToolError::new(
            ErrorKind::Unsupported,
            format!("{raw} 是目录不是文件"),
        ));
    }
    let path = scope.verify_existing(&path)?;
    if meta.len() > limits::IMAGE_BYTES {
        return Err(ToolError::new(
            ErrorKind::TooLarge,
            format!(
                "图片 {} 字节，超过 {} 字节上限",
                meta.len(),
                limits::IMAGE_BYTES
            ),
        )
        .with_hint("先缩小图片再查看，或只用 `bash` 的 file 命令看格式"));
    }

    let bytes = super::read_file::read_bounded(&path, limits::IMAGE_BYTES)?;
    let Some(img) = sniff(&bytes) else {
        return Err(ToolError::new(
            ErrorKind::Unsupported,
            format!("{raw} 不是可识别的图片（只支持 PNG / JPEG / GIF / BMP / WEBP）"),
        )
        .with_hint("若是文本请用 `read_file`；不确定格式可用 `bash` 跑 `file`"));
    };

    let shown = scope.display(&path);
    let mut data = json!({
        "path": shown,
        "format": img.format,
        "mime": img.mime,
        "width": img.width,
        "height": img.height,
        "bytes": meta.len(),
    });
    // `include_data` = 把图**交给模型看**：图作为独立的一块附在工具结果后面，
    // 不再把 base64 塞进 JSON —— 塞进去模型"看不见"（它读的是文本），
    // 只会把上下文一次吃掉几 MB。
    data["image_attached"] = json!(include_data);
    let mut outcome = Outcome::ok(
        "view_image",
        format!(
            "{}：{}×{} {}（{:.1} KB）{}",
            raw,
            img.width,
            img.height,
            img.format.to_uppercase(),
            meta.len() as f64 / 1024.0,
            if include_data {
                "，已把图交给模型"
            } else {
                ""
            }
        ),
        data,
    );
    if include_data {
        if img.width == 0 || img.height == 0 || img.width > 8192 || img.height > 8192
            || u64::from(img.width) * u64::from(img.height) > 16_000_000 {
            return Err(ToolError::new(ErrorKind::TooLarge, "图片尺寸未知或超过 8192 边长 / 1600 万像素上限"));
        }
        let url = model_image(&bytes, true)
            .map_err(|e| ToolError::new(ErrorKind::Unsupported, e))?;
        outcome.data["image_note"] = json!("仅发送首帧，最长边1536；原文件和上面的原始尺寸不变。");
        outcome = outcome.with_image(url);
    }

    Ok(outcome)
}

/// 有界转换供工具与应用回灌共用。桌面图传 false，绝不改变坐标尺度。
pub fn model_image(bytes: &[u8], resize: bool) -> Result<String, String> {
    use std::io::Cursor;
    if bytes.len() > 32 * 1024 * 1024 {
        return Err("图片编码体积超过32 MiB".into());
    }
    let reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format()
        .map_err(|e| format!("无法识别图片：{e}"))?;
    let format = reader.format().ok_or("无法识别图片格式")?;
    if !matches!(format, image::ImageFormat::Png | image::ImageFormat::Jpeg
        | image::ImageFormat::Gif | image::ImageFormat::Bmp | image::ImageFormat::WebP) {
        return Err("仅支持 PNG/JPEG/GIF/BMP/WebP".into());
    }
    let (width, height) = reader.into_dimensions().map_err(|e| format!("图片尺寸损坏：{e}"))?;
    let (side, pixels) = if resize { (8192, 16_000_000) } else { (4096, 8_000_000) };
    if width == 0 || height == 0 || width > side || height > side
        || u64::from(width) * u64::from(height) > pixels {
        return Err(format!("图片超过{side}边长或{pixels}像素；桌面截图请缩小区域"));
    }
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(side);
    limits.max_image_height = Some(side);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    let mut decoded = reader.decode().map_err(|e| format!("图片解码失败：{e}"))?;
    if resize && (width > 1536 || height > 1536) {
        decoded = decoded.thumbnail(1536, 1536);
    } else if !resize && matches!(format, image::ImageFormat::Png | image::ImageFormat::Jpeg)
        && bytes.len() <= 4 * 1024 * 1024 {
        let mime = if format == image::ImageFormat::Png { "image/png" } else { "image/jpeg" };
        return Ok(format!("data:{mime};base64,{}", b64(bytes)));
    }
    let mut encoded = Cursor::new(Vec::new());
    decoded.write_to(&mut encoded, image::ImageFormat::Png)
        .map_err(|e| format!("图片转换失败：{e}"))?;
    let mut mime = "image/png";
    if encoded.get_ref().len() > 4 * 1024 * 1024 {
        encoded = Cursor::new(Vec::new());
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut encoded, 85)
            .encode_image(&decoded.to_rgb8()).map_err(|e| format!("图片转换失败：{e}"))?;
        mime = "image/jpeg";
    }
    if encoded.get_ref().len() > 4 * 1024 * 1024 {
        return Err("图片转换后仍超过4 MiB，请缩小图片区域".into());
    }
    Ok(format!("data:{mime};base64,{}", b64(encoded.get_ref())))
}

struct ImageInfo {
    format: &'static str,
    mime: &'static str,
    width: u32,
    height: u32,
}

/// 按文件头识别格式并取尺寸；识别不出返回 `None`。
fn sniff(b: &[u8]) -> Option<ImageInfo> {
    let be32 = |o: usize| u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
    let le16 = |o: usize| u16::from_le_bytes([b[o], b[o + 1]]) as u32;
    let le32 = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);

    // PNG：IHDR 紧跟在 8 字节签名 + 4 字节长度 + 4 字节类型之后。
    if b.len() > 24 && b.starts_with(&[0x89, b'P', b'N', b'G']) {
        return Some(ImageInfo {
            format: "png",
            mime: "image/png",
            width: be32(16),
            height: be32(20),
        });
    }
    // GIF：逻辑屏幕描述符在偏移 6。
    if b.len() > 10 && (b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a")) {
        return Some(ImageInfo {
            format: "gif",
            mime: "image/gif",
            width: le16(6),
            height: le16(8),
        });
    }
    // BMP：DIB 头里的宽高（偏移 18 / 22，小端）。
    if b.len() > 26 && b.starts_with(b"BM") {
        return Some(ImageInfo {
            format: "bmp",
            mime: "image/bmp",
            width: le32(18),
            // Windows DIB 的负高度表示自顶向下存储。
            height: (le32(22) as i32).unsigned_abs(),
        });
    }
    // WEBP：RIFF 容器，三种子格式尺寸位置不同。
    if b.len() > 30 && b.starts_with(b"RIFF") && &b[8..12] == b"WEBP" {
        let (width, height) = match &b[12..16] {
            b"VP8 " => (le16(26) & 0x3fff, le16(28) & 0x3fff),
            b"VP8L" => {
                let bits = le32(21);
                ((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1)
            }
            b"VP8X" => (
                (b[24] as u32 | (b[25] as u32) << 8 | (b[26] as u32) << 16) + 1,
                (b[27] as u32 | (b[28] as u32) << 8 | (b[29] as u32) << 16) + 1,
            ),
            _ => (0, 0),
        };
        return Some(ImageInfo {
            format: "webp",
            mime: "image/webp",
            width,
            height,
        });
    }
    // JPEG：扫 SOFn 段（跳过其它段）。
    if b.len() > 4 && b.starts_with(&[0xff, 0xd8]) {
        let mut i = 2usize;
        while i + 9 < b.len() {
            if b[i] != 0xff {
                i += 1;
                continue;
            }
            let marker = b[i + 1];
            // SOF0..SOF15，排除 DHT(0xc4) / JPG(0xc8) / DAC(0xcc)。
            if (0xc0..=0xcf).contains(&marker) && marker != 0xc4 && marker != 0xc8 && marker != 0xcc
            {
                let h = u16::from_be_bytes([b[i + 5], b[i + 6]]) as u32;
                let w = u16::from_be_bytes([b[i + 7], b[i + 8]]) as u32;
                return Some(ImageInfo {
                    format: "jpeg",
                    mime: "image/jpeg",
                    width: w,
                    height: h,
                });
            }
            if marker == 0xd8 || (0xd0..=0xd9).contains(&marker) {
                i += 2;
                continue;
            }
            let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
            i += 2 + len;
        }
        return Some(ImageInfo {
            format: "jpeg",
            mime: "image/jpeg",
            width: 0,
            height: 0,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_image_formats_dimensions_and_corruption() {
        use base64::Engine;
        for format in [image::ImageFormat::Png, image::ImageFormat::Jpeg,
            image::ImageFormat::Bmp, image::ImageFormat::Gif, image::ImageFormat::WebP] {
            let mut bytes = std::io::Cursor::new(Vec::new());
            image::DynamicImage::new_rgb8(2, 3).write_to(&mut bytes, format).unwrap();
            let url = model_image(bytes.get_ref(), true).unwrap();
            assert!(url.starts_with("data:image/png;base64,") || url.starts_with("data:image/jpeg;base64,"));
            let data = base64::engine::general_purpose::STANDARD.decode(url.split_once(',').unwrap().1).unwrap();
            let image = image::load_from_memory(&data).unwrap();
            assert_eq!((image.width(), image.height()), (2, 3));
        }
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(4100, 2).write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        assert!(model_image(bytes.get_ref(), false).is_err());
        let url = model_image(bytes.get_ref(), true).unwrap();
        let data = base64::engine::general_purpose::STANDARD.decode(url.split_once(',').unwrap().1).unwrap();
        assert_eq!(image::load_from_memory(&data).unwrap().width(), 1536);
        assert!(model_image(&bytes.get_ref()[..24], true).is_err());
    }

    #[test]
    fn sniffs_png_header() {
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        png.extend_from_slice(&[0, 0, 0, 13]);
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&1920u32.to_be_bytes());
        png.extend_from_slice(&1080u32.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0]);
        let info = sniff(&png).unwrap();
        assert_eq!((info.format, info.width, info.height), ("png", 1920, 1080));
    }

    #[test]
    fn top_down_bmp_has_positive_height() {
        let mut bmp = vec![0; 54];
        bmp[..2].copy_from_slice(b"BM");
        bmp[14..18].copy_from_slice(&40u32.to_le_bytes());
        bmp[18..22].copy_from_slice(&320i32.to_le_bytes());
        bmp[22..26].copy_from_slice(&(-240i32).to_le_bytes());
        let info = sniff(&bmp).unwrap();
        assert_eq!((info.width, info.height), (320, 240));
    }

    #[test]
    fn oversized_pixel_header_is_not_attached() {
        let root = std::env::temp_dir().join(format!("neo-image-budget-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let mut png = vec![0x89, b'P', b'N', b'G', 13, 10, 26, 10];
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&5000u32.to_be_bytes());
        png.extend_from_slice(&5000u32.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0]);
        std::fs::write(root.join("超大.png"), png).unwrap();
        let out = crate::dispatch(&Scope::new(&root), "view_image", &json!({"path": "超大.png", "include_data": true}));
        assert_eq!(out.error.unwrap().kind, ErrorKind::TooLarge);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_non_image() {
        assert!(sniff(b"hello world, not an image at all").is_none());
    }
}
