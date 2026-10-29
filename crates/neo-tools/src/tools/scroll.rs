//! `scroll` —— 在目标位置滚动鼠标滚轮。

use serde_json::json;

use crate::result::{Args, Outcome, ToolError};
use crate::spec::Param;
use crate::Scope;
use neo_desktop::input::{self, ScrollDirection};

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
    Param::opt_int(
        "x",
        "滚动位置 x；有截图引用时为图内像素。",
        0,
        -32768,
        32768,
    ),
    Param::opt_int("y", "滚动位置 y。", 0, -32768, 32768),
    Param {
        default: Some(crate::spec::Default::Str("down")),
        ..Param::opt_text("direction", "up / down / left / right；默认 down。")
    },
    Param::opt_int("amount", "滚动格数，默认 3，范围 1–20。", 3, 1, 20),
];

pub fn preview(args: &Args) -> String {
    let direction = args
        .opt_str("direction")
        .unwrap_or_else(|_| "down".to_owned());
    let amount = args.opt_int("amount").unwrap_or(3);
    let id = args.opt_int("element_id").unwrap_or(0);
    if id > 0 {
        format!("在元素 #{id} 向 {direction} 滚动 {amount} 格")
    } else {
        let x = args.opt_int("x").unwrap_or(0);
        let y = args.opt_int("y").unwrap_or(0);
        format!("在 ({x}, {y}) 向 {direction} 滚动 {amount} 格")
    }
}

pub fn run(scope: &Scope, args: &Args) -> Outcome {
    match act(scope, args) {
        Ok(outcome) => outcome,
        Err(error) => Outcome::fail("scroll", error),
    }
}

fn act(scope: &Scope, args: &Args) -> Result<Outcome, ToolError> {
    let direction =
        ScrollDirection::parse(&args.opt_str("direction")?).map_err(target::tool_error)?;
    let amount = args.opt_int("amount")? as u32;
    if !(1..=20).contains(&amount) {
        return Err(ToolError::bad_args("amount 需在 1–20 之间"));
    }

    let _interaction = screen_uia::INTERACTION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let resolved = target::resolve(scope, args, "滚动")?;
    if scope.is_cancelled() {
        return Err(crate::cancelled_error());
    }

    screen::ensure_dpi_aware();
    target::invalidate();
    input::move_mouse(resolved.x, resolved.y, 0, || scope.is_cancelled())
        .map_err(target::tool_error)?;
    input::scroll(direction, amount, || scope.is_cancelled()).map_err(target::tool_error)?;
    target::invalidate();

    Ok(Outcome::ok(
        "scroll",
        format!(
            "已在 ({}, {}) 向 {} 滚动 {} 格",
            resolved.x,
            resolved.y,
            direction.as_str(),
            amount
        ),
        json!({
            "x": resolved.x,
            "y": resolved.y,
            "located_by": resolved.located_by,
            "screenshot_id": resolved.screenshot_id,
            "direction": direction.as_str(),
            "amount": amount,
            "dpi_scale": screen::dpi_scale_at(resolved.x, resolved.y),
            "target_identity_verified": resolved.element.is_some(),
            "snapshot_invalidated": true,
            "next": "滚动后旧元素快照已失效；需要继续操作时重新运行 screen_elements。",
        }),
    ))
}

#[cfg(test)]
#[path = "scroll_tests.rs"]
mod tests;
