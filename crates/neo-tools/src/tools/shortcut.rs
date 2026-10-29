//! `shortcut` —— 向当前焦点窗口发送组合键。

use serde_json::json;

use crate::result::{Args, Outcome, ToolError};
use crate::spec::Param;
use crate::Scope;
use neo_desktop::input;

use super::{screen_uia, target};

pub static PARAMS: &[Param] = &[
    Param::text(
        "keys",
        "组合键，用 + 连接，例如 `ctrl+c`、`alt+tab`、`win+r`、`ctrl+shift+esc`。加号键写 `plus`。",
    ),
    Param::opt_int("repeat", "重复次数，默认 1，范围 1–10。", 1, 1, 10),
];

pub fn preview(args: &Args) -> String {
    let keys = args.opt_str("keys").unwrap_or_default();
    let repeat = args.opt_int("repeat").unwrap_or(1);
    if repeat > 1 {
        format!("按 {keys}（{repeat} 次）")
    } else {
        format!("按 {keys}")
    }
}

pub fn run(scope: &Scope, args: &Args) -> Outcome {
    match act(scope, args) {
        Ok(outcome) => outcome,
        Err(error) => Outcome::fail("shortcut", error),
    }
}

fn act(scope: &Scope, args: &Args) -> Result<Outcome, ToolError> {
    let keys = args.require_str("keys")?;
    let repeat = args.opt_int("repeat")? as u32;
    input::parse_shortcut(&keys).map_err(target::tool_error)?;

    let _interaction = screen_uia::INTERACTION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if scope.is_cancelled() {
        return Err(crate::cancelled_error());
    }
    target::invalidate();
    input::shortcut(&keys, repeat, || scope.is_cancelled()).map_err(target::tool_error)?;
    target::invalidate();

    Ok(Outcome::ok(
        "shortcut",
        format!(
            "已按 {keys}{}",
            if repeat > 1 {
                format!("（{repeat} 次）")
            } else {
                String::new()
            }
        ),
        json!({
            "keys": keys,
            "repeat": repeat,
            "snapshot_invalidated": true,
            "next": "快捷键可能改变界面；继续操作前重新观察屏幕。",
        }),
    ))
}

#[cfg(test)]
#[path = "shortcut_tests.rs"]
mod tests;
