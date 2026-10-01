//! `view_image` —— 查看图片。
//!
//! 元数据查询只读文件头；附图时有界解码并转换为 PNG/JPEG。
//! 仅发送首帧，必要时等比缩小，原文件不变；识别由多模态模型完成。

use super::base64_lite::encode as b64;
use serde_json::json;

use crate::result::{Args, ErrorKind, Outcome, ToolError};
use crate::spec::Param;
use crate::Scope;

pub const MAX_MODEL_IMAGE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_MODEL_IMAGE_EDGE: u32 = 4096;
pub const MAX_MODEL_IMAGE_PIXELS: u64 = 4096 * 2160;
pub const MAX_SOURCE_IMAGE_BYTES: usize = 32 * 1024 * 1024;
const MAX_SOURCE_IMAGE_EDGE: u32 = 8192;
const MAX_SOURCE_IMAGE_PIXELS: u64 = 16_000_000;
const PREVIEW_EDGE: u32 = 1536;

pub struct ModelImage {
    pub url: String,
    pub source_size: (u32, u32),
    pub sent_size: (u32, u32),
}

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
        format!("查看图片 {path} 并附送模型兼容图片")
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
    if meta.len() > MAX_SOURCE_IMAGE_BYTES as u64 {
        return Err(ToolError::new(
            ErrorKind::TooLarge,
            format!(
                "图片 {} 字节，超过 {} 字节上限",
                meta.len(),
                MAX_SOURCE_IMAGE_BYTES
            ),
        )
        .with_hint("先缩小图片再查看，或只用 `bash` 的 file 命令看格式"));
    }

    let bytes = super::read_file::read_bounded(&path, MAX_SOURCE_IMAGE_BYTES as u64)?;
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
        "source_size": { "width": img.width, "height": img.height },
        "sent_size": null,
        "coordinate_space": "image_local",
        "desktop_locatable": false,
        "image_note": "普通图片文件没有可信的桌面 region/origin；不得用于桌面定位，不从文件名或像素猜测原点。",
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
        if img.width == 0 || img.height == 0 || img.width > MAX_SOURCE_IMAGE_EDGE || img.height > MAX_SOURCE_IMAGE_EDGE
            || u64::from(img.width) * u64::from(img.height) > MAX_SOURCE_IMAGE_PIXELS {
            return Err(ToolError::new(ErrorKind::TooLarge, "图片尺寸未知或超过 8192 边长 / 1600 万像素上限"));
        }
        let prepared = model_image_with_sizes(&bytes, true)
            .map_err(|e| ToolError::new(ErrorKind::Unsupported, e))?;
        let (sw, sh) = prepared.source_size;
        let (w, h) = prepared.sent_size;
        outcome.data["source_size"] = json!({"width": sw, "height": sh});
        outcome.data["sent_size"] = json!({"width": w, "height": h});
        outcome.data["source_to_sent_scale"] = json!({
            "x": {"numerator": w, "denominator": sw},
            "y": {"numerator": h, "denominator": sh},
        });
        outcome.data["image_note"] = json!("仅发送首帧，最长边1536；source_size 为原图，sent_size 为实际附图，比例按各轴精确分数给出。普通图片没有可信的桌面 region/origin，不得用于桌面定位；需要定位时重新 screenshot 获取引用。");
        outcome = outcome.with_image(prepared.url);
    }

    Ok(outcome)
}

/// 有界转换供工具与应用回灌共用。桌面图传 false，绝不改变坐标尺度。
pub fn model_image(bytes: &[u8], resize: bool) -> Result<String, String> {
    model_image_with_sizes(bytes, resize).map(|image| image.url)
}

/// 返回真实解码/编码尺寸；resize=false 即使转 JPEG 也保持像素尺寸。
pub fn model_image_with_sizes(bytes: &[u8], resize: bool) -> Result<ModelImage, String> {
    use std::io::Cursor;
    if bytes.len() > MAX_SOURCE_IMAGE_BYTES {
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
    let (side, pixels) = if resize {
        (MAX_SOURCE_IMAGE_EDGE, MAX_SOURCE_IMAGE_PIXELS)
    } else {
        (MAX_MODEL_IMAGE_EDGE, MAX_MODEL_IMAGE_PIXELS)
    };
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
    if resize && (width > PREVIEW_EDGE || height > PREVIEW_EDGE) {
        decoded = decoded.thumbnail(PREVIEW_EDGE, PREVIEW_EDGE);
    } else if !resize && matches!(format, image::ImageFormat::Png | image::ImageFormat::Jpeg)
        && bytes.len() <= MAX_MODEL_IMAGE_BYTES {
        let mime = if format == image::ImageFormat::Png { "image/png" } else { "image/jpeg" };
        return Ok(ModelImage {
            url: format!("data:{mime};base64,{}", b64(bytes)),
            source_size: (width, height),
            sent_size: (decoded.width(), decoded.height()),
        });
    }
    let mut encoded = Cursor::new(Vec::new());
    decoded.write_to(&mut encoded, image::ImageFormat::Png)
        .map_err(|e| format!("图片转换失败：{e}"))?;
    let mut mime = "image/png";
    if encoded.get_ref().len() > MAX_MODEL_IMAGE_BYTES {
        encoded = Cursor::new(Vec::new());
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut encoded, 85)
            .encode_image(&decoded.to_rgb8()).map_err(|e| format!("图片转换失败：{e}"))?;
        mime = "image/jpeg";
    }
    if encoded.get_ref().len() > MAX_MODEL_IMAGE_BYTES {
        return Err("图片转换后仍超过4 MiB，请缩小图片区域".into());
    }
    Ok(ModelImage {
        url: format!("data:{mime};base64,{}", b64(encoded.get_ref())),
        source_size: (width, height),
        sent_size: (decoded.width(), decoded.height()),
    })
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
#[path = "view_image_tests.rs"]
mod tests;
