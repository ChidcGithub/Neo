//! `app_control` —— 启动应用、切换/缩放窗口。

use std::path::Path;

use serde_json::{json, Value};

use crate::result::{Args, Outcome, ToolError};
use crate::spec::Param;
use crate::Scope;
use neo_desktop::app;
use neo_desktop::display::ScreenRect;
use neo_desktop::window::{self, WindowInfo};

use super::{screen, screen_uia, target};

pub static PARAMS: &[Param] = &[
    Param {
        default: Some(crate::spec::Default::Str("launch")),
        ..Param::opt_text("mode", "launch / launch_executable / switch / resize。")
    },
    Param::opt_text("name", "Start Menu 应用名，或要切换/缩放的窗口标题。"),
    Param::opt_text("executable", "launch_executable 的绝对可执行文件路径。"),
    Param::opt_text(
        "args_json",
        "launch_executable 的参数 JSON 数组，例如 [\"--flag\",\"value\"]。",
    ),
    Param::opt_text("cwd", "launch_executable 的工作目录。"),
    Param::opt_text(
        "window_id",
        "desktop_snapshot 返回的 window_id；优先于 name。",
    ),
    Param::opt_int("x", "resize 的目标 x。", 0, -32768, 32768),
    Param::opt_int("y", "resize 的目标 y。", 0, -32768, 32768),
    Param::opt_int("width", "resize 的目标宽度，必须为正。", 0, 0, 32768),
    Param::opt_int("height", "resize 的目标高度，必须为正。", 0, 0, 32768),
];

pub fn preview(args: &Args) -> String {
    let mode = args.opt_str("mode").unwrap_or_else(|_| "launch".to_owned());
    match mode.trim().to_ascii_lowercase().as_str() {
        "launch" => format!("启动应用「{}」", args.opt_str("name").unwrap_or_default()),
        "launch_executable" => format!(
            "启动可执行文件「{}」",
            args.opt_str("executable").unwrap_or_default()
        ),
        "switch" => format!(
            "切换到窗口「{}」",
            args.opt_str("name")
                .ok()
                .filter(|name| !name.is_empty())
                .or_else(|| args.opt_str("window_id").ok().filter(|id| !id.is_empty()))
                .unwrap_or_else(|| "活动窗口".to_owned())
        ),
        "resize" => format!(
            "缩放窗口「{}」到 {}×{}",
            args.opt_str("name")
                .ok()
                .filter(|name| !name.is_empty())
                .or_else(|| args.opt_str("window_id").ok().filter(|id| !id.is_empty()))
                .unwrap_or_else(|| "活动窗口".to_owned()),
            args.opt_int("width").unwrap_or(0),
            args.opt_int("height").unwrap_or(0)
        ),
        other => format!("未知应用操作「{other}」"),
    }
}

pub fn run(scope: &Scope, args: &Args) -> Outcome {
    match act(scope, args) {
        Ok(outcome) => outcome,
        Err(error) => Outcome::fail("app_control", error),
    }
}

fn act(scope: &Scope, args: &Args) -> Result<Outcome, ToolError> {
    let mode = args.opt_str("mode")?.trim().to_ascii_lowercase();
    let _interaction = screen_uia::INTERACTION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    screen::ensure_dpi_aware();
    if scope.is_cancelled() {
        return Err(crate::cancelled_error());
    }

    match mode.as_str() {
        "launch" => {
            let name = args.require_str("name")?;
            let mut result = app::launch(&name).map_err(target::tool_error)?;
            target::invalidate();
            let activated = activate_window(&result.window);
            if activated {
                if let Some(window) = result.window.as_mut() {
                    window.foreground = true;
                    window.minimized = false;
                }
            }
            let summary = if result.detected {
                format!(
                    "已启动「{name}」{}{}",
                    pid_note(result.pid),
                    if activated {
                        "（已切到前台）"
                    } else {
                        ""
                    }
                )
            } else {
                format!("已发送「{name}」启动请求，但 8 秒内没检测到窗口")
            };
            Ok(Outcome::ok(
                "app_control",
                summary,
                json!({
                    "mode": "launch",
                    "name": name,
                    "pid": result.pid,
                    "detected": result.detected,
                    "activated": activated,
                    "window": result.window.as_ref().map(window_json),
                    "next": if result.detected { Value::Null } else { json!("应用可能仍在启动；稍后用 desktop_snapshot 或 screen_elements 再找窗口。") },
                }),
            ))
        }
        "launch_executable" => {
            let executable = args.require_str("executable")?;
            let program_args = parse_args(&args.opt_str("args_json")?)?;
            let cwd = args.opt_str("cwd")?;
            let cwd = (!cwd.trim().is_empty()).then(|| Path::new(cwd.trim()));
            let mut result = app::launch_executable(Path::new(&executable), &program_args, cwd)
                .map_err(target::tool_error)?;
            target::invalidate();
            let activated = activate_window(&result.window);
            if activated {
                if let Some(window) = result.window.as_mut() {
                    window.foreground = true;
                    window.minimized = false;
                }
            }
            Ok(Outcome::ok(
                "app_control",
                format!(
                    "已启动「{executable}」{}{}{}",
                    pid_note(result.pid),
                    if result.detected {
                        ""
                    } else {
                        "（窗口未在 8 秒内出现）"
                    },
                    if activated {
                        "（已切到前台）"
                    } else {
                        ""
                    }
                ),
                json!({
                    "mode": "launch_executable",
                    "executable": executable,
                    "args": program_args,
                    "cwd": cwd.map(|path| path.display().to_string()),
                    "pid": result.pid,
                    "detected": result.detected,
                    "activated": activated,
                    "window": result.window.as_ref().map(window_json),
                }),
            ))
        }
        "switch" => {
            let window = resolve_window(args, false)?;
            window::switch(&window).map_err(target::tool_error)?;
            target::invalidate();
            Ok(Outcome::ok(
                "app_control",
                format!("已切换到「{}」", window.title),
                json!({
                    "mode": "switch",
                    "window": window_json(&window),
                }),
            ))
        }
        "resize" => {
            let window = resolve_window(args, true)?;
            let rect = resize_rect(args)?;
            window::resize(&window, rect).map_err(target::tool_error)?;
            target::invalidate();
            Ok(Outcome::ok(
                "app_control",
                format!(
                    "已把「{}」缩放到 ({}, {}) {}×{}",
                    window.title, rect.x, rect.y, rect.width, rect.height
                ),
                json!({
                    "mode": "resize",
                    "window": window_json(&window),
                    "rect": { "x": rect.x, "y": rect.y, "width": rect.width, "height": rect.height },
                }),
            ))
        }
        other => Err(ToolError::bad_args(format!(
            "未知 mode {other:?}；支持 launch / launch_executable / switch / resize"
        ))),
    }
}

fn activate_window(window: &Option<WindowInfo>) -> bool {
    window
        .as_ref()
        .is_some_and(|item| item.foreground || window::switch(item).is_ok())
}

fn pid_note(pid: u32) -> String {
    if pid == 0 {
        String::new()
    } else {
        format!("（PID {pid}）")
    }
}

fn parse_args(raw: &str) -> Result<Vec<String>, ToolError> {
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str(raw)
        .map_err(|error| ToolError::bad_args(format!("args_json 不是字符串数组：{error}")))
}

fn resolve_window(args: &Args, allow_active: bool) -> Result<WindowInfo, ToolError> {
    if let Ok(id) = args.opt_str("window_id") {
        if !id.trim().is_empty() {
            let handle = id
                .trim()
                .parse::<isize>()
                .map_err(|_| ToolError::bad_args(format!("window_id 不是有效句柄：{id:?}")))?;
            return window::by_handle(handle).map_err(target::tool_error);
        }
    }
    let name = args.opt_str("name")?;
    if !name.trim().is_empty() {
        return window::find(name.trim()).map_err(target::tool_error);
    }
    if allow_active {
        if let Some(window) = window::active().map_err(target::tool_error)? {
            return Ok(window);
        }
        return Err(ToolError::not_found("当前没有前台窗口可缩放"));
    }
    Err(ToolError::bad_args(
        "switch 需要 name 或 window_id；先用 desktop_snapshot 找窗口",
    ))
}

fn resize_rect(args: &Args) -> Result<ScreenRect, ToolError> {
    if !["x", "y", "width", "height"]
        .iter()
        .all(|key| args.has(key))
    {
        return Err(ToolError::bad_args(
            "resize 需要完整 x / y / width / height 四个参数",
        ));
    }
    let rect = ScreenRect {
        x: args.opt_int("x")? as i32,
        y: args.opt_int("y")? as i32,
        width: args.opt_int("width")? as i32,
        height: args.opt_int("height")? as i32,
    };
    if rect.width <= 0 || rect.height <= 0 {
        return Err(ToolError::bad_args("窗口 width / height 必须为正"));
    }
    Ok(rect)
}

fn window_json(window: &WindowInfo) -> Value {
    json!({
        "window_id": window.handle.to_string(),
        "title": window.title,
        "process_id": window.process_id,
        "x": window.rect.x,
        "y": window.rect.y,
        "width": window.rect.width,
        "height": window.rect.height,
        "foreground": window.foreground,
        "minimized": window.minimized,
    })
}

#[cfg(test)]
#[path = "app_control_tests.rs"]
mod tests;
