//! `view_image` —— 查看图片。
//!
//! 边界：**不解码像素、不做识别**。它回答"这是张什么图、多大、能不能给模型看"。
//! 真要"看图说话"，靠的是把 `data_url` 交给多模态模型，而不是在这里做图像处理。
//!
//! 尺寸解析是读文件头的：PNG 的 IHDR、JPEG 的 SOFn、GIF 的逻辑屏幕描述符、
//! BMP 的 DIB 头、WEBP 的三种容器。只为报元数据去拖一个解码库不划算。

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

    let bytes = std::fs::read(&path).map_err(|e| ToolError::io(format!("读取 {raw} 失败：{e}")))?;
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
        let url = format!("data:{};base64,{}", img.mime, b64(&bytes));
        outcome = outcome.with_image(url);
    }

    Ok(outcome)
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
            // 高位为 1 表示自顶向下存储；取绝对值语义即低 16 位可读，这里直接取无符号。
            height: le32(22),
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
    fn rejects_non_image() {
        assert!(sniff(b"hello world, not an image at all").is_none());
    }
}
