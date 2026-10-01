//! `screenshot` —— 截取屏幕（或其中一块），把图**直接交给模型看**。
//!
//! 这是"AI 看得见屏幕"的那只眼睛：截下来的 PNG 存在工作区里（模型之后可以
//! 用它做别的），同时作为图片附在结果上 —— 多模态模型**真的会看到屏幕内容**。
//!
//! ## 坐标系
//!
//! 无引用时参数是虚拟桌面物理像素；给 screenshot_id 时参数是该 PNG 的图内像素。
//! PNG 的 (0,0) 对应 source_rect 的左上角，由程序累加偏移，不做 DPI 乘除。
//! 引用裁图是重新捕获当前桌面的对应区域，不是对旧 PNG 做离线裁剪。
//!
//! ## 局部截屏
//!
//! `x` / `y` / `width` / `height` **要么四个都给、要么都不给**：
//! 都不给 = 整个虚拟桌面；只给一部分会被明确拒绝 —— 半开区间比"缺的按 0 算"
//! 好排查得多（后者会静默截到一块莫名其妙的区域）。

use std::path::PathBuf;

use serde_json::json;

use crate::result::{Args, Outcome, ToolError};
#[cfg(test)]
use crate::result::ErrorKind;
use crate::spec::Param;
use crate::Scope;

use super::screen::{self, Rect};

pub static PARAMS: &[Param] = &[
    Param::opt_text("screenshot_id", "可选短期截图坐标参考；给了就必须同时给 x/y/width/height（图内像素），程序映射后重新截取当前桌面。未知或过期拒绝，不验证目标身份。"),
    Param { required: false, ..Param::int("x", "区域左上角 x：有 screenshot_id 时为图内像素，无引用时为桌面物理像素。四个区域参数要么全给，要么全省略。", -32768, 32768) },
    Param { required: false, ..Param::int("y", "区域左上角 y：有 screenshot_id 时为图内像素，否则为桌面物理像素。", -32768, 32768) },
    Param { required: false, ..Param::int("width", "区域宽度（正像素数），不是 right 右边界；局部截图必填。", 1, 32768) },
    Param { required: false, ..Param::int("height", "区域高度（正像素数），不是 bottom 下边界；局部截图必填。", 1, 32768) },
];

pub fn preview(args: &Args) -> String {
    match region(args) {
        Ok(None) => "截取整个屏幕（并交给模型看）".to_owned(),
        Ok(Some(r)) => format!(
            "截取{}区域 ({}, {}) {}×{}（并交给模型看）",
            if args.has("screenshot_id") { "截图引用图内" } else { "桌面物理" }, r.x, r.y, r.width, r.height
        ),
        Err(_) => "截取屏幕（参数不完整）".to_owned(),
    }
}

/// 解析区域：`Ok(None)` = 整屏。**不在这里校验越界**（那要知道屏幕尺寸，
/// 交给 `run` 统一做，错误信息里才能带上合法范围）。
fn region(args: &Args) -> Result<Option<Rect>, ToolError> {
    super::screenshot_space::validate_args(args)?;
    const KEYS: [&str; 4] = ["x", "y", "width", "height"];
    let given = KEYS.iter().filter(|k| args.has(k)).count();
    if args.has("screenshot_id") {
        args.require_str("screenshot_id")?;
        if given != 4 { return Err(ToolError::bad_args("screenshot_id 必须与完整 x/y/width/height 配对，不能回退整屏")); }
    }
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
    use super::screenshot_space::{self, ImageSpace};
    // 参数错误在读取真实桌面之前拒绝，尤其不能把 null/拼错参数当成整屏。
    let requested = region(args)?;
    let reference = args.has("screenshot_id").then(|| args.require_str("screenshot_id")).transpose()?;
    let _interaction = super::screen_uia::INTERACTION.lock().unwrap();
    if scope.is_cancelled() { return Err(crate::cancelled_error()); }
    let generation = screenshot_space::generation();
    if let Some(id) = &reference { screenshot_space::require_known(id)?; }
    screen::ensure_dpi_aware();
    let topology = screen::monitors()?;
    let vs = screen::monitor_bounds(&topology)?;
    let rect = match (requested, &reference) {
        (Some(r), Some(id)) => screenshot_space::resolve(id, topology.clone())?.crop(r)?,
        (Some(r), None) => r,
        (None, None) => vs,
        (None, Some(_)) => return Err(ToolError::bad_args("截图引用缺少裁图区域")),
    };
    let space = ImageSpace::new(rect, rect.width as u32, rect.height as u32, topology.clone())?;
    let img = screen::capture(rect)?;
    if img.width != space.sent_width || img.height != space.sent_height {
        return Err(ToolError::io("实际截图尺寸与坐标参考不匹配，已拒绝发送"));
    }
    if screen::monitors()? != topology {
        screenshot_space::invalidate();
        return Err(ToolError::bad_args("截屏期间显示拓扑改变，请重新截图"));
    }
    let png = img.to_png()?;
    if scope.is_cancelled() { return Err(crate::cancelled_error()); }

    // 存进工作区：模型之后能用这个路径做别的事（裁剪、比对、交给用户看）。
    let path = save_png(scope, &png)?;

    let scale = screen::dpi_scale_at(rect.x + rect.width / 2, rect.y + rect.height / 2);
    let shown = scope.display(&path);
    let prepared = prepare_image(&png, (img.width, img.height));
    if scope.is_cancelled() { return Err(crate::cancelled_error()); }
    let attachable = prepared.is_ok();
    let mapping = space.metadata();
    let screenshot_id = if attachable { Some(screenshot_space::register(space, scope, generation)?) } else { None };
    let sent_format = prepared.as_ref().ok().map(|image| if image.url.starts_with("data:image/png;") { "png" } else { "jpeg" });
    let sent_bytes = prepared.as_ref().ok().map_or(0, |image| {
        let encoded = image.url.split_once(',').unwrap().1;
        encoded.len() / 4 * 3 - encoded.bytes().rev().take_while(|b| *b == b'=').count()
    });
    let mut data = json!({
        "screenshot_id": screenshot_id,
        "reference_ttl_seconds": screenshot_space::TTL_SECS,
        "parent_screenshot_id": reference,
        "image_space": mapping["image_space"],
        "desktop_space": mapping["desktop_space"],
        "image_to_desktop": mapping["image_to_desktop"],
        "display_topology": mapping["display_topology"],
        "contains_display_gaps": mapping["contains_display_gaps"],
        "gap_note": "显示器空洞不是可点击屏幕；图内坐标仍须落在实际显示器上",
        "source_width": img.width, "source_height": img.height,
        "source_size": { "width": img.width, "height": img.height },
        "sent_size": prepared.as_ref().ok().map(|image| json!({ "width": image.sent_size.0, "height": image.sent_size.1 })),
        "sent_width": if attachable { Some(img.width) } else { None },
        "sent_height": if attachable { Some(img.height) } else { None },
        "sent_bytes": sent_bytes,
        "format": "png", "saved_format": "png", "sent_format": sent_format,
        "lossless": sent_format.map(|format| format == "png"), "resized": false,
        "target_identity_verified": false,
        "reference_note": "截图引用仅作坐标参考；图像变化不会自动被检测。优先 UIA 元素点击，动作后重新观察，不自动重试。引用裁图会读取当前桌面。",
        "path": shown,
        "saved_bytes": png.len(),
        "region": { "x": rect.x, "y": rect.y, "width": rect.width, "height": rect.height },
        "virtual_screen": { "x": vs.x, "y": vs.y, "width": vs.width, "height": vs.height },
        "dpi_scale": scale,
        "image_attached": attachable,
        "coordinate_space": "原尺寸附图的图内像素；desktop = image + source_rect.origin，不做 DPI 缩放",
    });
    let mut note = "，已把图交给模型";
    if let Err(reason) = &prepared {
        note = "（原尺寸附图准备失败，未随请求发送）";
        data["reason"] = json!(reason);
        data["next"] = json!("用桌面物理 x/y/width/height 改截更小区域；view_image 可能缩图，其坐标不能套用本截图映射");
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
    if let Ok(image) = prepared {
        outcome = outcome.with_image(image.url);
    }
    Ok(outcome)
}

fn prepare_image(png: &[u8], source: (u32, u32)) -> Result<super::view_image::ModelImage, String> {
    let image = super::view_image::model_image_with_sizes(png, false)?;
    if image.source_size != source || image.sent_size != source {
        return Err("实际附图尺寸与截图坐标参考不一致，已拒绝发送".into());
    }
    Ok(image)
}

fn save_png(scope: &Scope, png: &[u8]) -> Result<PathBuf, ToolError> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    for _ in 0..32 {
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let rel = format!("screenshots/shot-{}-{}-{sequence}.png", epoch_ms(), std::process::id());
        let path = scope.resolve(&rel)?;
        scope.verify_new(&path)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ToolError::io(format!("创建 {} 失败：{e}", scope.display(parent))))?;
        }
        scope.verify_new(&path)?;
        let mut file = match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(ToolError::io(format!("创建 {} 失败：{e}", scope.display(&path)))),
        };
        file.write_all(png).map_err(|e| ToolError::io(format!("保存 {} 失败：{e}", scope.display(&path))))?;
        return Ok(path);
    }
    Err(ToolError::io("无法分配唯一截图文件名，未覆盖已有文件"))
}

/// 毫秒时间戳，用来给截图起不重名的文件名。
fn epoch_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "screenshot_tests.rs"]
mod tests;
