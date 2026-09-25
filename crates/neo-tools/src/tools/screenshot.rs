//! `screenshot` —— 截取屏幕（或其中一块），把图**直接交给模型看**。
//!
//! 这是"AI 看得见屏幕"的那只眼睛：截下来的 PNG 存在工作区里（模型之后可以
//! 用它做别的），同时作为图片附在结果上 —— 多模态模型**真的会看到屏幕内容**。
//!
//! ## 坐标系
//!
//! 全部是**虚拟桌面的物理像素**，原点在虚拟桌面左上角（多显示器时可能为负）。
//! 这和 `click` / `drag` 用的是同一套坐标 —— **照图里的像素位置写，不用换算缩放**。
//! 见 [`super::screen`] 里关于 DPI 的说明。
//!
//! ## 局部截屏
//!
//! `x` / `y` / `width` / `height` **要么四个都给、要么都不给**：
//! 都不给 = 整个虚拟桌面；只给一部分会被明确拒绝 —— 半开区间比"缺的按 0 算"
//! 好排查得多（后者会静默截到一块莫名其妙的区域）。

use std::path::PathBuf;

use serde_json::json;

use crate::result::{Args, ErrorKind, Outcome, ToolError};
use crate::spec::Param;
use crate::Scope;

use super::base64_lite::encode as b64;
use super::limits;
use super::screen::{self, Rect};

pub static PARAMS: &[Param] = &[
    Param::opt_int(
        "x",
        "截取区域左上角的 x（虚拟桌面物理像素，多显示器时可能是负数）。\
         不给就截整个虚拟桌面；给了就要连 y/width/height 一起给。",
        0,
        -32768,
        32768,
    ),
    Param::opt_int(
        "y",
        "截取区域左上角的 y（虚拟桌面物理像素）。同上：要么四个都给，要么都不给。",
        0,
        -32768,
        32768,
    ),
    Param::opt_int(
        "width",
        "截取区域宽度（像素），必须为正。只给局部时必填。",
        0,
        0,
        32768,
    ),
    Param::opt_int(
        "height",
        "截取区域高度（像素），必须为正。只给局部时必填。",
        0,
        0,
        32768,
    ),
];

pub fn preview(args: &Args) -> String {
    match region(args) {
        Ok(None) => "截取整个屏幕（并交给模型看）".to_owned(),
        Ok(Some(r)) => format!(
            "截取屏幕区域 ({}, {}) {}×{}（并交给模型看）",
            r.x, r.y, r.width, r.height
        ),
        Err(_) => "截取屏幕（参数不完整）".to_owned(),
    }
}

/// 解析区域：`Ok(None)` = 整屏。**不在这里校验越界**（那要知道屏幕尺寸，
/// 交给 `run` 统一做，错误信息里才能带上合法范围）。
fn region(args: &Args) -> Result<Option<Rect>, ToolError> {
    const KEYS: [&str; 4] = ["x", "y", "width", "height"];
    let given = KEYS.iter().filter(|k| args.has(k)).count();
    if given == 0 {
        return Ok(None);
    }
    if given != KEYS.len() {
        let missing: Vec<&str> = KEYS.iter().copied().filter(|k| !args.has(k)).collect();
        return Err(ToolError::bad_args(format!(
            "局部截屏要给全四个参数，缺了 {}",
            missing.join(" / ")
        ))
        .with_hint("要么 x/y/width/height 都给（局部），要么都不给（整屏）"));
    }
    Ok(Some(Rect {
        x: args.opt_int("x")? as i32,
        y: args.opt_int("y")? as i32,
        width: args.opt_int("width")? as i32,
        height: args.opt_int("height")? as i32,
    }))
}

pub fn run(scope: &Scope, args: &Args) -> Outcome {
    match shot(scope, args) {
        Ok(o) => o,
        Err(e) => Outcome::fail("screenshot", e),
    }
}

fn shot(scope: &Scope, args: &Args) -> Result<Outcome, ToolError> {
    screen::ensure_dpi_aware();
    let vs = screen::virtual_screen();
    if vs.width <= 0 || vs.height <= 0 {
        return Err(ToolError::new(
            ErrorKind::Unsupported,
            "拿不到屏幕尺寸 —— 没有可交互的桌面会话",
        )
        .with_hint("确认 Neo 跑在真实登录的桌面会话里（不是服务或无头会话）"));
    }

    let rect = match region(args)? {
        None => vs,
        Some(r) => {
            if r.width <= 0 || r.height <= 0 {
                return Err(ToolError::bad_args("截取区域的宽高必须为正")
                    .with_hint("width / height 是整数像素"));
            }
            // 两端都要在桌面内：只查左上角会漏掉"起点在图内、右下角跑到屏外"。
            if !vs.contains(r.x, r.y) {
                return Err(vs.outside_error("截取区域左上角", r.x, r.y));
            }
            if !vs.contains(r.x + r.width - 1, r.y + r.height - 1) {
                return Err(vs.outside_error(
                    "截取区域右下角",
                    r.x + r.width - 1,
                    r.y + r.height - 1,
                ));
            }
            r
        }
    };

    let img = screen::capture(rect)?;
    let png = img.to_png()?;

    // 存进工作区：模型之后能用这个路径做别的事（裁剪、比对、交给用户看）。
    let rel = format!("screenshots/shot-{}.png", epoch_ms());
    let path: PathBuf = scope.resolve(&rel)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ToolError::io(format!("创建 {} 失败：{e}", scope.display(parent))))?;
    }
    std::fs::write(&path, &png)
        .map_err(|e| ToolError::io(format!("保存 {} 失败：{e}", scope.display(&path))))?;

    let scale = screen::dpi_scale_at(rect.x + rect.width / 2, rect.y + rect.height / 2);
    let shown = scope.display(&path);
    // 单张内联图官方上限 32 MiB。超了就**不发图**、只给路径，并说清下一步怎么办 ——
    // 静静发一个超限的请求只会换来一个难懂的 400。
    let attachable = png.len() <= limits::INLINE_IMAGE_BYTES;
    let mut data = json!({
        "path": shown,
        "saved_bytes": png.len(),
        "region": { "x": rect.x, "y": rect.y, "width": rect.width, "height": rect.height },
        "virtual_screen": { "x": vs.x, "y": vs.y, "width": vs.width, "height": vs.height },
        "dpi_scale": scale,
        "image_attached": attachable,
        "coordinate_space": "虚拟桌面物理像素（与 click / drag 同一套坐标）",
    });
    let mut note = "，已把图交给模型";
    if !attachable {
        note = "（图太大，未随请求发送）";
        data["reason"] = json!(format!(
            "PNG 超过 {} MiB 的内联上限",
            limits::INLINE_IMAGE_BYTES / 1048576
        ));
        data["next"] =
            json!("改截一块更小的区域（给 x/y/width/height），或用 view_image 看这张已保存的文件");
    }

    let mut outcome = Outcome::ok(
        "screenshot",
        format!(
            "截屏 {}×{} 已保存到 {}（{:.1} MB）{note}",
            rect.width,
            rect.height,
            shown,
            png.len() as f64 / 1048576.0
        ),
        data,
    );
    if attachable {
        outcome = outcome.with_image(format!("data:image/png;base64,{}", b64(&png)));
    }
    Ok(outcome)
}

/// 毫秒时间戳，用来给截图起不重名的文件名。
fn epoch_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn region_needs_all_four_or_none() {
        let tool = crate::find("screenshot").unwrap();

        let empty = json!({});
        assert!(
            region(&Args::new(tool, &empty)).unwrap().is_none(),
            "都不给 = 整屏"
        );

        let full = json!({ "x": 10, "y": 20, "width": 100, "height": 50 });
        let r = region(&Args::new(tool, &full)).unwrap().unwrap();
        assert_eq!((r.x, r.y, r.width, r.height), (10, 20, 100, 50));

        // 只给两个 → 明确拒绝，并指出缺了什么
        let half = json!({ "x": 10, "y": 20 });
        let e = region(&Args::new(tool, &half)).unwrap_err();
        assert_eq!(e.kind, ErrorKind::BadArguments);
        assert!(e.message.contains("width"), "{}", e.message);
        assert!(e.message.contains("height"), "{}", e.message);
    }

    #[test]
    fn negative_coordinates_are_allowed() {
        // 副屏排在左边时 x 是负数 —— 范围声明必须容得下
        let tool = crate::find("screenshot").unwrap();
        let v = json!({ "x": -1920, "y": -100, "width": 800, "height": 600 });
        let r = region(&Args::new(tool, &v)).unwrap().unwrap();
        assert_eq!(r.x, -1920);
        assert_eq!(r.y, -100);
    }

    #[test]
    fn preview_says_what_will_be_captured() {
        let tool = crate::find("screenshot").unwrap();
        let empty = json!({});
        assert!(preview(&Args::new(tool, &empty)).contains("整个屏幕"));
        let v = json!({ "x": 0, "y": 0, "width": 10, "height": 10 });
        assert!(
            preview(&Args::new(tool, &v)).contains("10×10"),
            "{}",
            preview(&Args::new(tool, &v))
        );
    }
}
