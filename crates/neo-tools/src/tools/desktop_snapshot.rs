//! `desktop_snapshot` —— 一次拿到窗口、焦点窗口、元素清单与可选截图。

use serde_json::{json, Value};

use crate::result::{Args, Outcome, ToolError};
use crate::spec::Param;
use crate::Scope;

use super::{screen, screen_elements, screen_uia, view_image};

pub static PARAMS: &[Param] = &[
    Param::opt_text("window", "只枚举标题包含该串的窗口；默认焦点窗口。"),
    Param::flag("all", "true = 枚举所有可见窗口的元素；默认只看焦点窗口。"),
    Param {
        default: Some(crate::spec::Default::Bool(true)),
        ..Param::flag(
            "include_elements",
            "false = 只返回窗口列表，不遍历 UIA 元素。",
        )
    },
    Param::opt_int(
        "max_elements",
        "最多返回多少元素，默认 120，范围 1–400。",
        120,
        1,
        400,
    ),
    Param::flag(
        "use_vision",
        "true = 同时截取整个虚拟桌面并把图交给模型；默认关闭。",
    ),
];

pub fn preview(args: &Args) -> String {
    let window = args.opt_str("window").unwrap_or_default();
    let all = args.flag("all").unwrap_or(false);
    let elements = args.flag("include_elements").unwrap_or(true);
    let vision = args.flag("use_vision").unwrap_or(false);
    let scope = if window.trim().is_empty() {
        if all {
            "所有窗口"
        } else {
            "焦点窗口"
        }
    } else {
        "指定窗口"
    };
    format!(
        "查看{scope}{}{}",
        if elements {
            "的元素"
        } else {
            "的窗口列表"
        },
        if vision { "，并截图" } else { "" }
    )
}

pub fn run(scope: &Scope, args: &Args) -> Outcome {
    match act(scope, args) {
        Ok(outcome) => outcome,
        Err(error) => Outcome::fail("desktop_snapshot", error),
    }
}

fn act(scope: &Scope, args: &Args) -> Result<Outcome, ToolError> {
    let include_elements = args.flag("include_elements")?;
    let use_vision = args.flag("use_vision")?;
    let max_elements = args.opt_int("max_elements")? as usize;
    let filter = args.opt_str("window")?;
    let filter = filter.trim();
    let all = args.flag("all")?;

    let _interaction = screen_uia::INTERACTION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    screen::ensure_dpi_aware();
    let (windows, windows_truncated) = screen_uia::desktop_overview()?;
    let active = windows.iter().find(|window| window.foreground);
    let windows_json: Vec<Value> = windows
        .iter()
        .map(|window| {
            json!({
                "window_id": window.hwnd.to_string(),
                "title": window.title,
                "process_id": window.process_id,
                "x": window.rect[0],
                "y": window.rect[1],
                "width": window.rect[2],
                "height": window.rect[3],
                "foreground": window.foreground,
                "minimized": window.minimized,
            })
        })
        .collect();

    let mut data = json!({
        "active_window": active.map(|window| json!({
            "window_id": window.hwnd.to_string(),
            "title": window.title,
            "process_id": window.process_id,
        })),
        "windows": windows_json,
        "windows_truncated": windows_truncated,
        "coordinate_space": "desktop_physical_pixels",
    });

    let mut summary = format!(
        "看到 {} 个窗口{}",
        windows.len(),
        active.map_or(String::new(), |window| format!(
            "，焦点「{}」",
            window.title
        ))
    );

    if include_elements {
        if scope.is_cancelled() {
            return Err(crate::cancelled_error());
        }
        let elements = screen_uia::enumerate_query(
            (!filter.is_empty()).then_some(filter),
            all,
            max_elements,
            max_elements,
            &screen_uia::Query::default(),
        )?;
        let snapshot_id = screen_uia::cache_store(&elements);
        let elements_json: Vec<Value> =
            elements.iter().map(screen_elements::element_json).collect();
        data["snapshot_id"] = json!(snapshot_id);
        data["expires_in_seconds"] = json!(screen_uia::cache_remaining_seconds());
        data["elements"] = json!(elements_json);
        data["count"] = json!(elements.len());
        data["truncated"] = json!(elements.len() >= max_elements);
        summary.push_str(&format!("；{} 个元素", elements.len()));
    }

    if use_vision {
        match capture() {
            Ok((url, sent, bytes)) => {
                data["image_attached"] = json!(true);
                data["image_size"] = json!({ "width": sent.0, "height": sent.1 });
                data["image_bytes"] = json!(bytes);
                let outcome = Outcome::ok("desktop_snapshot", format!("{summary}，已附图"), data)
                    .with_image(url);
                return Ok(outcome);
            }
            Err(error) => {
                data["image_attached"] = json!(false);
                data["image_error"] = json!(error.message);
            }
        }
    }

    Ok(Outcome::ok("desktop_snapshot", summary, data))
}

fn capture() -> Result<(String, (u32, u32), usize), ToolError> {
    use crate::tools::limits;

    let topology = screen::monitors()?;
    let virtual_screen = screen::monitor_bounds(&topology)?;
    let shot = screen::capture(virtual_screen)?;
    let png = shot.to_png()?;
    if png.len() > limits::INLINE_IMAGE_BYTES {
        return Err(ToolError::new(
            crate::result::ErrorKind::TooLarge,
            "截图超过内联图片上限，已跳过附图",
        ));
    }
    let prepared = view_image::model_image_with_sizes(&png, false).map_err(ToolError::io)?;
    let sent = prepared.sent_size;
    Ok((prepared.url, sent, png.len()))
}

#[cfg(test)]
#[path = "desktop_snapshot_tests.rs"]
mod tests;
