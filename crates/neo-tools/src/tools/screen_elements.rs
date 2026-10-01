//! `screen_elements` —— 屏幕上所有可交互元素的编号清单（SoM）。
//!
//! 这是"感知"与"行动"之间的桥：`screenshot` 给模型看图，`click` 让模型动手，
//! 而这个工具把"屏幕上有什么可点"变成**结构化的编号列表** —— 模型回答
//! 「点击 7」，不用再从图里手算像素坐标。
//!
//! 算法与验证数据见 `docs/screen-elements-research.md`。要点：
//!
//! - 枚举走 **UIA 辅助功能树**（系统免费提供，本机实测 144 元素 / ~540ms /
//!   96% 带名 / 物理像素坐标）；
//! - 编号按**上 → 下、左 → 右**（阅读顺序），模型看截图能对上空间关系；
//! - `click` 必须同时给快照 ID 和元素编号，60 秒内且实时核验通过才可执行。

use std::sync::Mutex;

use serde_json::{json, Value};

use crate::result::{Args, Outcome, ToolError};
use crate::spec::Param;
use crate::Scope;

use super::base64_lite::encode as b64;
use super::{screen, screen_uia};

pub static PARAMS: &[Param] = &[
    Param::opt_text("mode", "elements（默认）查询控件；overview 仅列全桌面顶层窗口，不遍历控件树。"),
    Param::opt_text("window_id", "overview 返回的窗口标识，精确限定窗口，避免同名窗口混淆。"),
    Param::opt_text("query", "控件名称关键词，大小写不敏感；过滤后才消耗元素预算。"),
    Param::opt_int("x", "区域左上角物理像素，需同时给 y/width/height。", 0, -32768, 32768),
    Param::opt_int("y", "区域左上角物理像素。", 0, -32768, 32768),
    Param::opt_int("width", "区域宽度，必须为正。", 0, 0, 65536),
    Param::opt_int("height", "区域高度，必须为正。", 0, 0, 65536),
    Param::opt_text(
        "window",
        "只枚举标题**包含**该串的顶层窗口（大小写不敏感）。给了该参数会覆盖默认的焦点窗口范围。\
         元素太多时用它缩小范围。",
    ),
    Param::flag(
        "annotate",
        "true = 同时返回一张画好编号框的截图（作为图片随结果交给模型）。\
         默认 false，只返回元素清单。",
    ),
    Param::flag(
        "all",
        "true = 枚举所有窗口；默认只枚举当前焦点窗口。每次最多返回 50 个元素；\
         元素超过 50 个时，使用相同参数重复调用会轮换到下一页。",
    ),
];

pub fn preview(args: &Args) -> String {
    if args.opt_str("mode").unwrap_or_default() == "overview" {
        return "查看全桌面顶层窗口概览（不展开控件树）".to_owned();
    }
    match args.opt_str("window").ok().as_deref() {
        Some(w) if !w.trim().is_empty() => format!("枚举「{}」的可交互元素并编号", w.trim()),
        _ if args.flag("all").unwrap_or(false) => "枚举所有窗口的可交互元素并编号".to_owned(),
        _ => "枚举当前焦点窗口的可交互元素并编号".to_owned(),
    }
}

pub fn run(scope: &Scope, args: &Args) -> Outcome {
    match act(scope, args) {
        Ok(o) => o,
        Err(e) => Outcome::fail("screen_elements", e),
    }
}

/// 最终模型 JSON（包含 Outcome 外壳）的 UTF-8 字节预算；图片走独立通道。
pub(super) const JSON_BYTES_BUDGET: usize = 12 * 1024;

pub(super) fn outcome_bytes(tool: &'static str, summary: &str, data: &Value) -> usize {
    Outcome::ok(tool, summary, data.clone()).to_model_json(usize::MAX).len()
}

pub(super) fn element_json(e: &screen_uia::ScreenElement) -> Value {
    let label: String = e.label.chars().take(96).collect();
    let window: String = e.window.chars().take(48).collect();
    json!({
        "id": e.id, "element_id": e.id, "role": e.role, "name": label, "label": label,
        "label_source": e.label_source, "window": window, "window_id": e.identity.hwnd.to_string(),
        "enabled": e.enabled, "foreground": e.foreground, "identity_verifiable": e.identity_verifiable(),
        "clickable": e.non_executable_reason().map(|_| false),
        "click_candidate": e.non_executable_reason().is_none(),
        "non_executable_reason": e.non_executable_reason().unwrap_or("requires_live_validation"),
        "text_truncated": label != e.label || window != e.window || (e.label_source == "name" && e.label != e.name.trim()),
        "intent_hint": if e.label.is_empty() { "用途未知" } else { intent_hint(e.role) },
        "x": e.rect[0], "y": e.rect[1], "w": e.rect[2], "h": e.rect[3],
    })
}

/// 每次最多给模型 50 个元素，避免可访问树淹没上下文。
const PAGE_SIZE: usize = 50;

#[derive(Clone, Default)]
struct PageCursor {
    key: String,
    next_start: usize,
    snapshot: Vec<screen_uia::ScreenElement>,
    snapshot_id: String,
}

static PAGE_CURSOR: Mutex<PageCursor> = Mutex::new(PageCursor {
    key: String::new(),
    next_start: 0,
    snapshot: Vec::new(),
    snapshot_id: String::new(),
});

struct ElementPage {
    items: Vec<Value>,
    start: usize,
    end: usize,
    page: usize,
    pages: usize,
    truncated: bool,
}

fn publish_page<T>(scope: &Scope, generation: u64, key: &str, els: &[screen_uia::ScreenElement],
    cursor: &mut PageCursor, build: impl FnOnce(&ElementPage) -> Result<T, ToolError>,
) -> Result<(String, ElementPage, T), ToolError> {
    screen_uia::cache_check(scope, generation)?;
    let mut next = cursor.clone();
    let reuse = screen_uia::cache_snapshot().filter(|(id, cached)|
        next.key == key && next.snapshot_id == *id && cached == els && next.snapshot == els)
        .map(|(id, _)| id);
    if reuse.is_none() {
        next.next_start = 0;
        // 新 ID 尚未发布，先以最大宽度占位，保证最终 JSON 预算不变松。
        next.snapshot_id = "4294967295-18446744073709551615-18446744073709551615".into();
    }
    let page = take_page(key, els, &mut next);
    screen_uia::cache_check(scope, generation)?;
    let built = build(&page)?;
    let id = screen_uia::cache_publish_checked(els, scope, generation, reuse.as_deref())?;
    next.snapshot_id = id.clone();
    // 所有潜在失败操作完成且受检发布成功，才推进真实游标。
    *cursor = next;
    Ok((id, page, built))
}

#[cfg(test)]
fn page_snapshot(key: &str, els: &[screen_uia::ScreenElement], cursor: &mut PageCursor) -> String {
    if let Some((id, cached)) = screen_uia::cache_snapshot() {
        if cursor.key == key && cursor.snapshot_id == id && cached == els && cursor.snapshot == els {
            return id;
        }
    }
    // 动作、搜索刷新、过期或内容变化后，不能延续旧快照的下一页。
    cursor.next_start = 0;
    cursor.snapshot_id = screen_uia::cache_store(els);
    cursor.snapshot_id.clone()
}

fn take_page(key: &str, els: &[screen_uia::ScreenElement], cursor: &mut PageCursor) -> ElementPage {
    if cursor.key != key || cursor.snapshot != els || cursor.next_start >= els.len() {
        cursor.key = key.to_owned();
        cursor.next_start = 0;
        cursor.snapshot = els.to_vec();
    }
    let mut items = Vec::with_capacity(els.len());
    let mut ranges = Vec::new();
    let mut start = 0;
    for e in els {
        let item = element_json(e);
        let count = items.len() - start;
        let mut candidate = items[start..].to_vec();
        candidate.push(item.clone());
        // 使用最终外壳与保守元数据测量，选定整项后才推进游标。
        let data = page_data(&cursor.snapshot_id, els.len(), &candidate, els.len(), els.len(), false, false, false,
            screen::Rect { x: i32::MIN, y: i32::MIN, width: i32::MAX, height: i32::MAX }, 60);
        if count > 0 && (count == PAGE_SIZE || outcome_bytes("screen_elements", &page_summary(els.len()), &data) > JSON_BYTES_BUDGET) {
            ranges.push((start, items.len(), count < PAGE_SIZE));
            start = items.len();
        }
        items.push(item);
    }
    ranges.push((start, items.len(), false));
    let page = ranges.iter().position(|&(_, end, _)| end > cursor.next_start).unwrap_or(0);
    let (_, end, truncated) = ranges[page];
    let start = cursor.next_start;
    cursor.next_start = if end < els.len() { end } else { 0 };
    ElementPage {
        items: items.drain(start..end).collect(),
        start,
        end,
        page: page + 1,
        pages: ranges.len(),
        truncated,
    }
}

fn intent_hint(role: &str) -> &'static str {
    match role {
        "Button" | "SplitButton" => "执行",
        "Hyperlink" => "打开链接或跳转到",
        "MenuItem" => "选择菜单项",
        "TabItem" => "切换到",
        "ListItem" | "TreeItem" | "DataItem" => "选择",
        "CheckBox" => "切换选项",
        "RadioButton" => "选择选项",
        "ComboBox" => "展开并选择",
        "Edit" => "输入或编辑",
        "Slider" | "Spinner" => "调整",
        "Calendar" => "选择日期",
        _ => "用途未知",
    }
}

fn page_summary(count: usize) -> String { format!("屏幕上 {count} 个元素（阅读顺序编号）") }

fn page_data(snapshot: &str, count: usize, items: &[Value], page: usize, pages: usize,
    has_more: bool, truncated: bool, all: bool, vs: screen::Rect, expires: u64) -> Value {
    json!({
        "snapshot_id": snapshot, "expires_in_seconds": expires,
        "enumeration_complete": false,
        "coverage_note": "UIA 有节点、深度、数量和协作式时间预算；未找到不代表不存在。请用 window_id/query/区域缩小范围。intent_hint 仅为类型提示，不证明用途。click_candidate 仅通过快照预检；clickable=null 表示遮挡与实时命中尚未核验，不保证可执行。",
        "count": count, "shown": items.len(), "page": page, "pages": pages,
        "has_more": has_more, "truncated": truncated, "byte_budget": JSON_BYTES_BUDGET,
        "scope": if all { "all_windows" } else { "focused_window" },
        "coordinate_space": "虚拟桌面物理像素",
        "virtual_screen": {"x": vs.x, "y": vs.y, "width": vs.width, "height": vs.height},
        "elements": items,
        "next": "相同范围与内容轮换下一页，不延长原60秒TTL；click 需 snapshot_id+element_id 且即时核验。后台窗口不可直接点击，本工具不激活窗口。刷新、过期或动作后旧引用失效。",
    })
}

pub(super) fn query_region(args: &Args) -> Result<Option<[i32; 4]>, ToolError> {
    let keys = ["x", "y", "width", "height"];
    let count = keys.iter().filter(|k| args.has(k)).count();
    if count == 0 { return Ok(None); }
    if count != 4 { return Err(ToolError::bad_args("区域必须同时给 x/y/width/height")); }
    let r = [args.opt_int("x")? as i32, args.opt_int("y")? as i32,
        args.opt_int("width")? as i32, args.opt_int("height")? as i32];
    if r[2] <= 0 || r[3] <= 0 { return Err(ToolError::bad_args("区域宽高必须为正")); }
    Ok(Some(r))
}

pub(super) fn parse_window_id(value: &str) -> Result<Option<isize>, ToolError> {
    if value.is_empty() { return Ok(None); }
    value.parse::<isize>().ok().filter(|id| *id != 0).map(Some)
        .ok_or_else(|| ToolError::bad_args("window_id 必须使用窗口概览返回的非零标识"))
}

fn overview_data(windows: &[screen_uia::WindowSummary], incomplete: bool) -> Value {
    let mut data = json!({"mode": "overview", "windows": [], "shown": 0,
        "truncated": incomplete, "enumerated": windows.len(), "byte_budget": 8000,
        "next": "用 screen_elements 的 window_id 精确查询目标窗口，配合 query 或 x/y/width/height 缩小范围；概览不产生可点击编号。"});
    for w in windows {
        let item = json!({"window_id": w.hwnd.to_string(), "title": w.title.chars().take(160).collect::<String>(),
            "title_truncated": w.title.chars().count() > 160, "process_id": w.process_id,
            "rect": w.rect, "foreground": w.foreground, "minimized": w.minimized});
        data["windows"].as_array_mut().unwrap().push(item);
        data["shown"] = json!(data["windows"].as_array().unwrap().len());
        if outcome_bytes("screen_elements", "全桌面顶层窗口概览", &data) > 8000 {
            data["windows"].as_array_mut().unwrap().pop();
            data["shown"] = json!(data["windows"].as_array().unwrap().len());
            data["truncated"] = json!(true);
            break;
        }
    }
    data
}

fn act(scope: &Scope, args: &Args) -> Result<Outcome, ToolError> {
    let generation = screen_uia::cache_generation();
    screen_uia::cache_check(scope, generation)?;
    let mode = args.opt_str("mode")?;
    if !matches!(mode.as_str(), "" | "elements" | "overview") {
        return Err(ToolError::bad_args("mode 只能是 elements 或 overview"));
    }
    if mode == "overview" {
        if ["window", "window_id", "query", "x", "y", "width", "height"].iter().any(|k| args.has(k))
            || args.flag("annotate")? {
            return Err(ToolError::bad_args("overview 只返回全局窗口摘要，不接受元素过滤或 annotate"));
        }
        let _interaction = screen_uia::INTERACTION.lock().unwrap();
        screen_uia::cache_check(scope, generation)?;
        let result = screen_uia::desktop_overview();
        screen_uia::cache_check(scope, generation)?;
        let (windows, incomplete) = result?;
        return Ok(Outcome::ok("screen_elements", "全桌面顶层窗口概览", overview_data(&windows, incomplete)));
    }
    let region = query_region(args)?;
    let window_id = parse_window_id(&args.opt_str("window_id")?)?;
    let keyword = args.opt_str("query")?;
    let window = args.opt_str("window")?;
    let annotate = args.flag("annotate")?;
    let all = args.flag("all")?;
    let filter = Some(window.trim()).filter(|s| !s.is_empty());
    let all_windows = all || filter.is_some() || window_id.is_some();
    let _interaction = screen_uia::INTERACTION.lock().unwrap();
    screen_uia::cache_check(scope, generation)?;
    let query = screen_uia::Query { window_id, keyword: keyword.trim(), region, ..Default::default() };
    // 单次 COM 调用无法硬中断；返回后先拒绝取消/旧代，不能清掉较新的缓存。
    let result = screen_uia::enumerate_query(filter, all_windows, 600, 800, &query);
    screen_uia::cache_check(scope, generation)?;
    let els = result.inspect_err(|_| { let _ = screen_uia::cache_invalidate_checked(scope, generation); })?;
    if els.is_empty() {
        screen_uia::cache_invalidate_checked(scope, generation)?;
        return Err(
            ToolError::not_found("屏幕上没有枚举到可交互元素").with_hint(match filter {
                Some(f) => {
                    format!("窗口过滤「{f}」可能太严：去掉 window 参数枚举全屏，或核对窗口标题")
                }
                None => {
                    "确认屏幕上有可见的应用窗口；刚切换窗口时 UIA 树在重建，等一秒重试".to_owned()
                }
            }),
        );
    }
    // 范围用结构化键，避免标题/关键词中的分隔符让不同查询碰撞。
    let key = json!([all_windows, filter, window_id, keyword.trim(), region]).to_string();
    let mut cursor = PAGE_CURSOR.lock().unwrap();
    screen_uia::cache_check(scope, generation)?;
    let vs = screen::virtual_screen();
    let (snapshot_id, page, image) = publish_page(scope, generation, &key, &els, &mut cursor, |page| {
        if !annotate { return Ok(None); }
        screen_uia::cache_check(scope, generation)?;
        let shot = screen::capture(vs)?;
        screen_uia::cache_check(scope, generation)?;
        let marked = annotate_shot(&shot, &els[page.start..page.end], (vs.x, vs.y))?;
        let png = marked.to_png()?;
        // 标注图超限就不附（32 MiB 是官方单图上限），清单仍可用。
        Ok((png.len() <= super::limits::INLINE_IMAGE_BYTES)
            .then(|| format!("data:image/png;base64,{}", b64(&png))))
    })?;
    drop(cursor);
    let data = page_data(&snapshot_id, els.len(), &page.items, page.page, page.pages, page.end < els.len(),
        page.truncated, all_windows, vs, screen_uia::cache_remaining_seconds());
    let mut outcome = Outcome::ok("screen_elements", page_summary(els.len()), data);
    if let Some(image) = image { outcome = outcome.with_image(image); }
    Ok(outcome)
}

// ==== SoM 标注图 ==========================================================

/// 在截图上画编号框：高对比红框 + 黑底白字的编号标签（5×7 点阵数字放大 2×）。
///
/// 为什么不用真正的字体渲染：工具层没有字体基建，而 5×7 点阵在 2× 缩放下
/// 对视觉模型完全可读 —— 简单、零依赖、可测。
fn annotate_shot(
    shot: &screen::Shot,
    els: &[screen_uia::ScreenElement],
    shot_origin: (i32, i32),
) -> Result<screen::Shot, ToolError> {
    let mut img = image::RgbaImage::from_raw(shot.width, shot.height, shot.rgba.clone())
        .ok_or_else(|| ToolError::io("截图位图尺寸与宽高不一致"))?;

    const BOX: [u8; 3] = [255, 72, 72]; // 高对比红：深浅主题下都醒目
    for e in els {
        draw_rect(
            &mut img,
            [
                e.rect[0].saturating_sub(shot_origin.0),
                e.rect[1].saturating_sub(shot_origin.1),
                e.rect[2],
                e.rect[3],
            ],
            BOX,
            2,
        );
        draw_label(
            &mut img,
            e.id,
            e.rect[0].saturating_sub(shot_origin.0),
            e.rect[1].saturating_sub(shot_origin.1),
            shot.width,
            shot.height,
        );
    }
    Ok(screen::Shot {
        width: shot.width,
        height: shot.height,
        rgba: img.into_raw(),
    })
}

/// 画 2px 边框矩形（超出画布的部分自动裁掉）。
fn draw_rect(img: &mut image::RgbaImage, rect: [i32; 4], color: [u8; 3], thick: u32) {
    let (w, h) = (img.width() as i64, img.height() as i64);
    let [x, y, rw, rh] = rect.map(i64::from);
    let (x0, y0) = (x.max(0), y.max(0));
    let (x1, y1) = ((x + rw).min(w - 1), (y + rh).min(h - 1));
    if x1 < x0 || y1 < y0 {
        return;
    }
    let (x0, y0, x1, y1) = (x0 as u32, y0 as u32, x1 as u32, y1 as u32);
    let px = |img: &mut image::RgbaImage, px: u32, py: u32| {
        if px < img.width() && py < img.height() {
            img.get_pixel_mut(px, py).0 = [color[0], color[1], color[2], 255];
        }
    };
    for t in 0..thick {
        for xx in x0..=x1 {
            px(img, xx, y0 + t);
            px(img, xx, y1.saturating_sub(t));
        }
        for yy in y0..=y1 {
            px(img, x0 + t, yy);
            px(img, x1.saturating_sub(t), yy);
        }
    }
}

/// 5×7 点阵数字（MSB 在左）。只有 0-9 —— 编号就是数字。
const DIGITS: [[u8; 7]; 10] = [
    [
        0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110,
    ], // 0
    [
        0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
    ], // 1
    [
        0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111,
    ], // 2
    [
        0b11111, 0b00010, 0b00100, 0b00010, 0b00001, 0b10001, 0b01110,
    ], // 3
    [
        0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
    ], // 4
    [
        0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110,
    ], // 5
    [
        0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
    ], // 6
    [
        0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
    ], // 7
    [
        0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
    ], // 8
    [
        0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100,
    ], // 9
];

/// 在框的左上角画「编号标签」：黑底白字（放不进屏幕就贴着框内画）。
fn draw_label(img: &mut image::RgbaImage, id: usize, x: i32, y: i32, w: u32, h: u32) {
    let text = id.to_string();
    let scale = 2u32; // 每点阵点 2×2 像素
    let gw = 5 * scale as i32;
    let gh = 7 * scale as i32;
    let gap = scale as i32; // 字符间距
    let text_w = text.len() as i32 * (gw + gap) - gap;
    let pad = scale as i32 + 1;

    // 标签默认贴在框上方；y 太靠顶就放进框里。
    let y = y.clamp(0, h.saturating_sub(1) as i32);
    let mut ly = y - gh - pad * 2;
    if ly < 0 {
        ly = y + pad;
    }
    let mut lx = x.clamp(0, w.saturating_sub(1) as i32);
    // 右侧放不下就往左挪（仍从元素左缘起画，保持"标签属于哪个框"的归属感）
    if lx + text_w + pad * 2 > w as i32 {
        lx = (w as i32) - text_w - pad * 2;
    }
    if lx < 0 {
        return; // 整个标签都放不下（极端小屏），框本身仍在
    }

    let bg = [0, 0, 0, 230];
    for yy in (ly - pad)..(ly + gh + pad) {
        for xx in (lx - pad)..(lx + text_w + pad) {
            if xx >= 0 && yy >= 0 && (xx as u32) < w && (yy as u32) < h {
                img.get_pixel_mut(xx as u32, yy as u32).0 = bg;
            }
        }
    }
    let fg = [255, 255, 255, 255];
    for (ci, ch) in text.chars().enumerate() {
        let digit = match ch.to_digit(10) {
            Some(d) => d as usize,
            None => continue,
        };
        let cx = lx + ci as i32 * (gw + gap);
        for (ry, row) in DIGITS[digit].iter().enumerate() {
            for rx in 0..5 {
                if row & (0b10000 >> rx) != 0 {
                    for dy in 0..scale {
                        for dx in 0..scale {
                            let px = cx + rx * scale as i32 + dx as i32;
                            let py = ly + ry as i32 * scale as i32 + dy as i32;
                            if px >= 0 && py >= 0 && (px as u32) < w && (py as u32) < h {
                                img.get_pixel_mut(px as u32, py as u32).0 = fg;
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "screen_elements_tests.rs"]
mod tests;
