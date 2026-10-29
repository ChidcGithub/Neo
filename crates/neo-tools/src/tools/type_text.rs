//! `type_text` —— 点击目标后输入文字（可清空、定位光标、回车提交）。

use serde_json::json;

use crate::result::{Args, Outcome, ToolError};
use crate::spec::Param;
use crate::Scope;
use neo_desktop::input::{self, CaretPosition, TypeOptions};

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
        "输入位置 x；有截图引用时为图内像素。",
        0,
        -32768,
        32768,
    ),
    Param::opt_int("y", "输入位置 y。", 0, -32768, 32768),
    Param::text("text", "要输入的文字；空串配合 clear 清空。"),
    Param::flag("clear", "先全选删除原内容。"),
    Param::flag("press_enter", "输入后回车。"),
    Param {
        default: Some(crate::spec::Default::Str("idle")),
        ..Param::opt_text("caret", "start / end；默认 idle。")
    },
];

pub fn preview(args: &Args) -> String {
    let text = args.opt_str("text").unwrap_or_default();
    let shown: String = text.chars().take(40).collect();
    let suffix = if text.chars().count() > 40 { "…" } else { "" };
    let clear = if args.flag("clear").unwrap_or(false) {
        "（先清空）"
    } else {
        ""
    };
    let enter = if args.flag("press_enter").unwrap_or(false) {
        "（回车）"
    } else {
        ""
    };
    let id = args.opt_int("element_id").unwrap_or(0);
    if id > 0 {
        format!("在元素 #{id} 输入「{shown}{suffix}」{clear}{enter}")
    } else {
        let x = args.opt_int("x").unwrap_or(0);
        let y = args.opt_int("y").unwrap_or(0);
        format!("在 ({x}, {y}) 输入「{shown}{suffix}」{clear}{enter}")
    }
}

pub fn run(scope: &Scope, args: &Args) -> Outcome {
    match act(scope, args) {
        Ok(outcome) => outcome,
        Err(error) => Outcome::fail("type_text", error),
    }
}

fn act(scope: &Scope, args: &Args) -> Result<Outcome, ToolError> {
    let text = args.require_present_str("text")?;
    let clear = args.flag("clear")?;
    let press_enter = args.flag("press_enter")?;
    let caret = CaretPosition::parse(&args.opt_str("caret")?).map_err(target::tool_error)?;

    if text.is_empty() && !clear && !press_enter {
        return Err(ToolError::bad_args(
            "text 为空，且没有 clear / press_enter；没有可执行的动作",
        ));
    }

    let _interaction = screen_uia::INTERACTION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let resolved = target::resolve(scope, args, "输入")?;

    if scope.is_cancelled() {
        return Err(crate::cancelled_error());
    }
    screen::ensure_dpi_aware();
    target::invalidate();
    screen::click(resolved.x, resolved.y, screen::Button::Left, false)?;

    if scope.is_cancelled() {
        return Err(crate::cancelled_error());
    }
    let options = TypeOptions {
        clear,
        press_enter,
        caret,
    };
    input::type_text(&text, options, || scope.is_cancelled()).map_err(target::tool_error)?;
    target::invalidate();

    let chars = text.chars().count();
    let mut summary = if text.is_empty() {
        if clear {
            format!("已清空 ({}, {}) 的输入框", resolved.x, resolved.y)
        } else {
            format!("已在 ({}, {}) 按回车", resolved.x, resolved.y)
        }
    } else {
        format!("已在 ({}, {}) 输入 {chars} 个字符", resolved.x, resolved.y)
    };
    if clear && !text.is_empty() {
        summary.push_str("（先清空）");
    }
    if press_enter {
        summary.push_str("（已回车）");
    }

    Ok(Outcome::ok(
        "type_text",
        summary,
        json!({
            "x": resolved.x,
            "y": resolved.y,
            "located_by": resolved.located_by,
            "screenshot_id": resolved.screenshot_id,
            "text_chars": chars,
            "text_bytes": text.len(),
            "cleared": clear,
            "pressed_enter": press_enter,
            "caret": format!("{caret:?}").to_lowercase(),
            "dpi_scale": screen::dpi_scale_at(resolved.x, resolved.y),
            "target_identity_verified": resolved.element.is_some(),
            "snapshot_invalidated": true,
            "next": "输入后旧元素快照已失效；需要继续操作时重新运行 screen_elements。",
        }),
    ))
}

#[cfg(test)]
#[path = "type_text_tests.rs"]
mod tests;
