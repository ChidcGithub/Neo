//! `move_mouse` —— 把鼠标指针移到目标位置，可选分步耗时。

use serde_json::json;

use crate::result::{Args, Outcome, ToolError};
use crate::spec::Param;
use crate::Scope;
use neo_desktop::input;

use super::{screen, screen_uia, target};

pub static PARAMS: &[Param] = &[
    Param::opt_text("screenshot_id", "截图坐标引用；与 x/y 配对。"),
    Param::opt_text("snapshot_id", "与 element_id 配对；动作后失效。"),
    Param::opt_int(
        "element_id",
        "元素编号；需 snapshot_id，不能与 x/y 混用。",
        0,
        0,
        9999,
    ),
    Param::opt_int("x", "目标 x；有截图引用时为图内像素。", 0, -32768, 32768),
    Param::opt_int("y", "目标 y。", 0, -32768, 32768),
    Param::opt_int("duration_ms", "移动耗时毫秒；0 = 直接到位。", 0, 0, 10_000),
];

pub fn preview(args: &Args) -> String {
    let id = args.opt_int("element_id").unwrap_or(0);
    let duration = args.opt_int("duration_ms").unwrap_or(0);
    if id > 0 {
        format!("把鼠标移到元素 #{id}（{duration} ms）")
    } else {
        let x = args.opt_int("x").unwrap_or(0);
        let y = args.opt_int("y").unwrap_or(0);
        format!("把鼠标移到 ({x}, {y})（{duration} ms）")
    }
}

pub fn run(scope: &Scope, args: &Args) -> Outcome {
    match act(scope, args) {
        Ok(outcome) => outcome,
        Err(error) => Outcome::fail("move_mouse", error),
    }
}

fn act(scope: &Scope, args: &Args) -> Result<Outcome, ToolError> {
    let duration_ms = args.opt_int("duration_ms")?.max(0) as u64;

    let _interaction = screen_uia::INTERACTION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let resolved = target::resolve(scope, args, "鼠标移动")?;
    if scope.is_cancelled() {
        return Err(crate::cancelled_error());
    }

    screen::ensure_dpi_aware();
    target::invalidate();
    input::move_mouse(resolved.x, resolved.y, duration_ms, || scope.is_cancelled())
        .map_err(target::tool_error)?;
    target::invalidate();

    Ok(Outcome::ok(
        "move_mouse",
        format!(
            "已把鼠标移到 ({}, {}){}",
            resolved.x,
            resolved.y,
            if duration_ms == 0 {
                String::new()
            } else {
                format!("（{duration_ms} ms）")
            }
        ),
        json!({
            "x": resolved.x,
            "y": resolved.y,
            "duration_ms": duration_ms,
            "located_by": resolved.located_by,
            "screenshot_id": resolved.screenshot_id,
            "dpi_scale": screen::dpi_scale_at(resolved.x, resolved.y),
            "target_identity_verified": resolved.element.is_some(),
            "snapshot_invalidated": true,
            "next": "移动后旧元素快照已失效；需要继续操作时重新运行 screen_elements。",
        }),
    ))
}

#[cfg(test)]
#[path = "move_mouse_tests.rs"]
mod tests;
