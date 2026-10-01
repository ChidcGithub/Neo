//! `drag` —— 按住鼠标从一个位置拖到另一个位置。
//!
//! 用来做"选中一段文字""拖动滑块""把文件拖进窗口""框选"这类事。
//!
//! ## 为什么不是一个 `click` 加一个 `click`
//!
//! 拖动＝**按下 → 移动 → 抬起**，而"移动"是其中的关键：直接"按下、跳到终点、
//! 抬起"在多数应用里会被当成单击（中间没有鼠标移动消息）。所以这里按
//! `duration_ms` 切成若干步，每步之间小睡 —— 观感上就是人手拖了一下。
//!
//! 坐标是虚拟桌面物理像素，不是局部 PNG 图内坐标；本工具不接受截图引用。

use serde_json::json;

use crate::result::{Args, Outcome, ToolError};
use crate::spec::Param;
use crate::Scope;

use super::screen::{self, Button};

pub static PARAMS: &[Param] = &[
    Param::int(
        "x",
        "拖动**起点**的 x（虚拟桌面物理像素）。一般是「按住这里」。",
        -32768,
        32768,
    ),
    Param::int("y", "拖动**起点**的 y（虚拟桌面物理像素）。", -32768, 32768),
    Param::int(
        "to_x",
        "拖动**终点**的 x。从起点匀速移到终点后松手。",
        -32768,
        32768,
    ),
    Param::int("to_y", "拖动**终点**的 y。", -32768, 32768),
    Param::opt_text("button", "`left`（默认，左键拖动）或 `right`（右键拖动）。"),
    Param::opt_int(
        "duration_ms",
        "从起点移到终点的耗时毫秒数。默认 300（约 20 步的小幅移动）。\
         拖得太快（值太小）时有些应用会当成单击，框选/滑块尤其明显。",
        300,
        0,
        10_000,
    ),
];

pub fn preview(args: &Args) -> String {
    let a = (
        args.opt_int("x").unwrap_or(0),
        args.opt_int("y").unwrap_or(0),
    );
    let b = (
        args.opt_int("to_x").unwrap_or(0),
        args.opt_int("to_y").unwrap_or(0),
    );
    let ms = args.opt_int("duration_ms").unwrap_or(300);
    format!("从 ({}, {}) 拖到 ({}, {})（{} ms）", a.0, a.1, b.0, b.1, ms)
}

pub fn run(scope: &Scope, args: &Args) -> Outcome {
    match act(scope, args) {
        Ok(o) => o,
        Err(e) => Outcome::fail("drag", e),
    }
}

fn act(scope: &Scope, args: &Args) -> Result<Outcome, ToolError> {
    super::screenshot_space::validate_args(args)?;
    let from = (args.require_int("x")? as i32, args.require_int("y")? as i32);
    let to = (
        args.require_int("to_x")? as i32,
        args.require_int("to_y")? as i32,
    );
    let button = Button::parse(&args.opt_str("button")?)?;
    let duration_ms = args.opt_int("duration_ms")?.max(0) as u64;

    let _interaction = super::screen_uia::INTERACTION.lock().unwrap();
    if scope.is_cancelled() { return Err(crate::cancelled_error()); }
    screen::ensure_dpi_aware();
    super::screen_uia::cache_invalidate();
    super::screenshot_space::invalidate();
    screen::drag_cancellable(from, to, button, duration_ms, || scope.is_cancelled())?;

    Ok(Outcome::ok(
        "drag",
        format!(
            "已从 ({}, {}) 拖到 ({}, {})（{} ms）",
            from.0, from.1, to.0, to.1, duration_ms
        ),
        json!({
            "from": { "x": from.0, "y": from.1 },
            "to": { "x": to.0, "y": to.1 },
            "button": button.name(),
            "duration_ms": duration_ms,
            "dpi_scale": screen::dpi_scale_at(to.0, to.1),
            "snapshot_invalidated": true,
            "screenshot_references_invalidated": true,
            "coordinate_space": "desktop_physical_pixels",
            "target_identity_verified": false,
            "next": "坐标拖动未验证目标身份；旧元素快照已失效，请运行 screen_elements 或 screenshot 确认结果。",
        }),
    ))
}

#[cfg(test)]
#[path = "drag_tests.rs"]
mod tests;
