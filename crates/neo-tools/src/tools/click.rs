//! `click` —— 在屏幕上的某个位置点击鼠标（支持左/右键、双击）。
//!
//! 这是"AI 能动手"的那只手。三点约定：
//!
//! 1. **两种定位方式，不可混用**：先跑 `screen_elements` 拿到快照和编号清单，
//!    然后给 `element_id` 让它自动点元素中心 —— 比手算坐标准得多；
//!    UIA 照不到的东西（游戏、自绘界面）才退回 `x/y` 直连。
//! 2. 有 screenshot_id 时 x/y 是 PNG 图内像素，由程序映射到桌面；无引用才是
//!    虚拟桌面物理像素。截图引用不验证目标身份，也不检测旧图内容变化。
//! 3. **双击是一个动作**，不是两次 `click`：两次调用之间隔着一次网络往返，
//!    早就超过系统的双击间隔了。要双击就把 `double` 设为 true。

use serde_json::json;

use crate::result::{Args, Outcome, ToolError};
use crate::spec::Param;
use crate::Scope;

use super::screen::{self, Button};

pub static PARAMS: &[Param] = &[
    Param::opt_text("screenshot_id", "可选：screenshot 返回的短期坐标参考。与完整 x/y 配对时按该 PNG 图内像素自动映射桌面；不可与 element_id/snapshot_id 混用。不验证目标身份，优先 UIA 元素点击。"),
    Param::opt_text("snapshot_id", "元素点击必填：与 element_id 同一结果返回的 snapshot_id；刷新或动作后旧快照失效。"),
    Param::opt_int(
        "element_id",
        "`screen_elements` 给出的元素编号 —— 点击该元素的**中心**。\
         必须同时给 snapshot_id；不可与 x/y 混用。",
        0,
        0,
        9999,
    ),
    Param::opt_int(
        "x",
        "点击 x：有 screenshot_id 时为 PNG 图内像素；无引用时为虚拟桌面物理像素（可负），不能直接照局部图位置写。",
        0,
        -32768,
        32768,
    ),
    Param::opt_int(
        "y",
        "点击 y：有 screenshot_id 时为 PNG 图内像素；无引用时为虚拟桌面物理像素。不做 DPI 乘除。",
        0,
        -32768,
        32768,
    ),
    Param::opt_text(
        "button",
        "`left`（默认，左键）或 `right`（右键，常用于呼出上下文菜单）。",
    ),
    Param::flag(
        "double",
        "true = 双击（一次调用内完成，符合系统的双击判定）；false = 单击。\
         需要双击时**必须**用它，不要连着调两次 click —— 两次调用之间隔着网络往返，\
         早就超过系统的双击间隔了。",
    ),
];

pub fn preview(args: &Args) -> String {
    let which = match args.opt_str("button").unwrap_or_default().as_str() {
        "right" | "r" | "secondary" => "右键",
        _ => "左键",
    };
    let times = if args.flag("double").unwrap_or(false) {
        "双击"
    } else {
        "点击"
    };
    let id = args.opt_int("element_id").unwrap_or(0);
    if id > 0 {
        return format!("{times}{which}：元素 #{id}");
    }
    let x = args.opt_int("x").unwrap_or(0);
    let y = args.opt_int("y").unwrap_or(0);
    if args.has("screenshot_id") {
        format!("在截图图内 ({x}, {y}) {times}{which}（程序映射到桌面）")
    } else {
        format!("在 ({x}, {y}) {times}{which}")
    }
}

// #region debug-point A: opt-in tool boundary; never report arguments or error text
static CLICK_DEBUG_ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
static CLICK_DEBUG_POSTING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(super) fn debug_envelope(point: &str, message: &str, data: serde_json::Value, run_id: &str) -> serde_json::Value {
    json!({
        "sessionId": "agent-click-routing", "runId": run_id,
        "hypothesisId": point, "location": "neo-tools/click+screen",
        "message": message, "data": data,
    })
}

fn post_debug(events: Vec<serde_json::Value>) {
    use std::sync::atomic::Ordering;
    // At most one bounded reporter; a busy/offline collector must not queue tool calls.
    if CLICK_DEBUG_POSTING.compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed).is_err() {
        return;
    }
    let spawned = std::thread::Builder::new().name("click-debug".into()).spawn(move || {
        use std::io::Write;
        use std::net::{SocketAddr, TcpStream};
        use std::time::{Duration, Instant};
        let deadline = Instant::now() + Duration::from_millis(100);
        let address = SocketAddr::from(([127, 0, 0, 1], 7777));
        for event in events {
            let body = event.to_string();
            let request = format!("POST /event HTTP/1.1\r\nHost: 127.0.0.1:7777\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
            let Some(remaining) = deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero()) else { break; };
            let Ok(mut stream) = TcpStream::connect_timeout(&address, remaining) else { break; };
            let Some(remaining) = deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero()) else { break; };
            if stream.set_read_timeout(Some(remaining)).is_err() { break; }
            let mut bytes = request.as_bytes();
            while !bytes.is_empty() {
                let Some(remaining) = deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero()) else { break; };
                if stream.set_write_timeout(Some(remaining)).is_err() { break; }
                match stream.write(bytes) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => bytes = &bytes[n..],
                }
            }
            if !bytes.is_empty() { break; }
            // No response read or retry: delivery is best effort, never a click prerequisite.
        }
        CLICK_DEBUG_POSTING.store(false, Ordering::Relaxed);
    });
    if spawned.is_err() { CLICK_DEBUG_POSTING.store(false, Ordering::Relaxed); }
}
// #endregion debug-point A

pub fn run(scope: &Scope, args: &Args) -> Outcome {
    // #region debug-point A: environment opt-in is read only at the click tool boundary
    let enabled = *CLICK_DEBUG_ENABLED.get_or_init(|| std::env::var("NEO_CLICK_DEBUG").as_deref() == Ok("1"));
    let mut evidence = enabled.then(screen::ClickEvidence::default);
    let started = enabled.then(std::time::Instant::now);
    // #endregion debug-point A
    let result = act(scope, args, evidence.as_mut());
    // #region debug-point A: post only after all input, including existing release cleanup
    if let (Some(evidence), Some(started)) = (evidence, started) {
        let elapsed_us = started.elapsed().as_micros();
        let run_id = format!("live-{}-{}", std::process::id(), screen::epoch_millis());
        let error_kind = result.as_ref().err().map(|e| e.kind.as_str());
        let rejected = matches!(result.as_ref().err().map(|e| e.kind),
            Some(crate::result::ErrorKind::BadArguments | crate::result::ErrorKind::NotAllowed));
        let mut events = vec![debug_envelope("A", "click_entered_and_completed", json!({
            "entered": true, "completed": true, "rejected": rejected,
            "error_kind": error_kind, "elapsed_us": elapsed_us,
            "entry_record_deferred_until_completion": true,
            "upstream_policy_observed": false, "target_response_verified": false,
        }), &run_id)];
        evidence.append_events(&mut events, &run_id);
        post_debug(events);
    }
    // #endregion debug-point A
    match result {
        Ok(o) => o,
        Err(e) => Outcome::fail("click", e),
    }
}

fn act(scope: &Scope, args: &Args, evidence: Option<&mut screen::ClickEvidence>) -> Result<Outcome, ToolError> {
    super::screenshot_space::validate_args(args)?;
    let reference = args.has("screenshot_id").then(|| args.require_str("screenshot_id")).transpose()?;
    if reference.is_some() && (args.has("element_id") || args.has("snapshot_id")) {
        return Err(ToolError::bad_args("screenshot_id 与 element_id/snapshot_id 不可混用"));
    }
    let button = Button::parse(&args.opt_str("button")?)?;
    let double = args.flag("double")?;

    // 两种定位方式必须给一种 —— 缺了就明说，不能被悄悄当成 (0,0) 点下去。
    // （`opt_int` 带默认值，"给没给"只能看 `has()`，不能看值。）
    let has_id = args.has("element_id");
    let has_x = args.has("x");
    let has_y = args.has("y");
    if !has_id && !has_x && !has_y {
        return Err(ToolError::bad_args(
            "缺少定位：给 element_id（推荐，来自 screen_elements 的编号）或完整的 x/y 坐标",
        )
        .with_hint("先跑 screen_elements 拿编号清单，再按编号点击"));
    }
    if !has_id && has_x != has_y {
        let missing = if has_x { "y" } else { "x" };
        return Err(
            ToolError::bad_args(format!("坐标定位必须同时给 x 和 y；缺少 `{missing}`"))
                .with_hint("补全 x/y，或改用 screen_elements 返回的 element_id"),
        );
    }

    if has_id && (has_x || has_y) {
        return Err(ToolError::bad_args("element_id 与 x/y 不可混用；歧义定位已拒绝"));
    }
    if !has_id && args.has("snapshot_id") {
        return Err(ToolError::bad_args("snapshot_id 必须与 element_id 配对，不验证坐标模式"));
    }
    let _interaction = super::screen_uia::INTERACTION.lock().unwrap();
    if scope.is_cancelled() { return Err(crate::cancelled_error()); }
    let located_by;
    let (x, y) = if has_id {
        let id = args.opt_int("element_id")? as usize;
        let snapshot = args.opt_str("snapshot_id")?;
        let Some(el) = super::screen_uia::cache_consume(&snapshot, id) else {
            return Err(ToolError::bad_args(format!("编号 #{id} 的 snapshot_id 缺失、陈旧或不匹配"))
                .with_hint("重新运行 screen_elements，同时传入同一结果的 snapshot_id 和 element_id"));
        };
        // 已原子消费快照；无论验证/输入是否成功，都不能重试本次引用。
        located_by = format!("validated element_id #{id}");
        super::screen_uia::validate_target(&el)?
    } else {
        let point = (args.opt_int("x")? as i32, args.opt_int("y")? as i32);
        if let Some(id) = &reference {
            super::screenshot_space::require_known(id)?;
            let topology = screen::monitors()?;
            let space = super::screenshot_space::resolve(id, topology.clone())?;
            let mapped = space.point(point.0, point.1)?;
            screen::require_monitor_point(&topology, "截图引用点击位置", mapped.0, mapped.1)?;
            located_by = "screenshot_id + image x/y (coordinate reference only)".to_owned();
            mapped
        } else {
            located_by = "desktop x/y".to_owned();
            point
        }
    };

    if scope.is_cancelled() { return Err(crate::cancelled_error()); }
    screen::ensure_dpi_aware();
    super::screen_uia::cache_invalidate();
    super::screenshot_space::invalidate();
    // #region debug-point B/C: pass opt-in evidence without changing click semantics
    screen::click_with_evidence(x, y, button, double, evidence)?;
    // #endregion debug-point B/C

    let what = if double { "双击" } else { "点击" };
    let scale = screen::dpi_scale_at(x, y);
    Ok(Outcome::ok(
        "click",
        format!("已在 ({x}, {y}) {what}{}", button_label(button)),
        json!({
            "x": x,
            "y": y,
            "located_by": located_by,
            "screenshot_id": reference,
            "coordinate_space": "desktop_physical_pixels",
            "screenshot_references_invalidated": true,
            "button": button.name(),
            "double": double,
            "dpi_scale": scale,
            "snapshot_invalidated": true,
            "target_identity_verified": has_id,
            "next": "旧元素快照已失效，请重新运行 screen_elements 或 screenshot 确认结果。坐标模式不验证目标身份；元素模式也不能消除核验与输入之间的竞态。",
        }),
    ))
}

fn button_label(b: Button) -> &'static str {
    match b {
        Button::Left => "（左键）",
        Button::Right => "（右键）",
    }
}

#[cfg(test)]
#[path = "click_tests.rs"]
mod tests;
