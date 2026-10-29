//! 元素/坐标目标的统一解析。调用方必须已持有
//! `screen_uia::INTERACTION` 锁，并在动作后自行失效快照。

use crate::result::{Args, ToolError};
use crate::Scope;
use neo_desktop::error::DesktopError;

use super::{screen, screen_uia, screenshot_space};

pub struct ResolvedTarget {
    pub x: i32,
    pub y: i32,
    pub located_by: String,
    pub element: Option<screen_uia::ScreenElement>,
    pub screenshot_id: Option<String>,
}

/// 解析 `element_id + snapshot_id` 或 `x/y`（可带 `screenshot_id`）。
/// 两种定位方式不可混用；直接坐标会校验是否落在真实显示器上。
pub fn resolve(scope: &Scope, args: &Args, what: &str) -> Result<ResolvedTarget, ToolError> {
    screenshot_space::validate_args(args)?;
    let reference = args
        .has("screenshot_id")
        .then(|| args.require_str("screenshot_id"))
        .transpose()?;
    let has_id = args.has("element_id");
    let has_x = args.has("x");
    let has_y = args.has("y");

    if !has_id && !has_x && !has_y {
        return Err(ToolError::bad_args(format!(
            "缺少{what}目标：给 element_id（推荐，来自 screen_elements）或完整的 x/y"
        ))
        .with_hint(
            "先运行 screen_elements 拿 snapshot_id + element_id，或 screenshot 拿图内坐标",
        ));
    }
    if has_id && (has_x || has_y) {
        return Err(ToolError::bad_args(
            "element_id 与 x/y 不可混用；歧义定位已拒绝",
        ));
    }
    if !has_id && has_x != has_y {
        let missing = if has_x { "y" } else { "x" };
        return Err(ToolError::bad_args(format!(
            "坐标定位必须同时给 x 和 y；缺少 `{missing}`"
        )));
    }
    if !has_id && args.has("snapshot_id") {
        return Err(ToolError::bad_args(
            "snapshot_id 必须与 element_id 配对，不能单独用于坐标模式",
        ));
    }
    if has_id && reference.is_some() {
        return Err(ToolError::bad_args(
            "screenshot_id 不能与 element_id/snapshot_id 混用",
        ));
    }
    if scope.is_cancelled() {
        return Err(crate::cancelled_error());
    }

    screen::ensure_dpi_aware();
    if has_id {
        let id = args.opt_int("element_id")? as usize;
        let snapshot = args.opt_str("snapshot_id")?;
        let Some(element) = screen_uia::cache_consume(&snapshot, id) else {
            return Err(ToolError::bad_args(format!(
                "编号 #{id} 的 snapshot_id 缺失、陈旧或不匹配"
            ))
            .with_hint("重新运行 screen_elements，同时传入同一结果的 snapshot_id 和 element_id"));
        };
        // 已原子消费快照；无论验证/输入是否成功，都不能重试本次引用。
        let (x, y) = screen_uia::validate_target(&element)?;
        return Ok(ResolvedTarget {
            x,
            y,
            located_by: format!("validated element_id #{id}"),
            element: Some(element),
            screenshot_id: None,
        });
    }

    let point = (args.opt_int("x")? as i32, args.opt_int("y")? as i32);
    if let Some(id) = reference {
        screenshot_space::require_known(&id)?;
        let topology = screen::monitors()?;
        let space = screenshot_space::resolve(&id, topology.clone())?;
        let mapped = space.point(point.0, point.1)?;
        screen::require_monitor_point(
            &topology,
            &format!("截图引用{what}位置"),
            mapped.0,
            mapped.1,
        )?;
        return Ok(ResolvedTarget {
            x: mapped.0,
            y: mapped.1,
            located_by: "screenshot_id + image x/y (coordinate reference only)".to_owned(),
            element: None,
            screenshot_id: Some(id),
        });
    }

    let topology = screen::monitors()?;
    screen::require_monitor_point(&topology, &format!("{what}位置"), point.0, point.1)?;
    Ok(ResolvedTarget {
        x: point.0,
        y: point.1,
        located_by: "desktop x/y".to_owned(),
        element: None,
        screenshot_id: None,
    })
}

/// 输入动作结束后统一失效旧快照与截图引用。
pub fn invalidate() {
    screen_uia::cache_invalidate();
    screenshot_space::invalidate();
}

/// 把底层错误映射为模型可读的 `ToolError`，保留原始错误到本地诊断。
pub(crate) fn tool_error(error: DesktopError) -> ToolError {
    let kind = error.kind();
    let message = error.to_string();
    let mapped = match kind {
        "bad_arguments" => ToolError::bad_args(message),
        "unsupported" => ToolError::new(crate::result::ErrorKind::Unsupported, message),
        "not_found" => ToolError::not_found(message),
        "not_allowed" => ToolError::not_allowed(message),
        "timeout" => ToolError::new(crate::result::ErrorKind::Timeout, message),
        "conflict" => ToolError::new(crate::result::ErrorKind::Conflict, message),
        "io" => ToolError::io(message),
        _ => ToolError::new(crate::result::ErrorKind::Internal, message),
    };
    mapped.with_source(&error)
}
