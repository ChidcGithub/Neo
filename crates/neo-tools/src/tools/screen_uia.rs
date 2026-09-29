//! UIA 树枚举 —— 屏幕上所有可交互元素（`screen_elements` 工具的实现，研究见
//! `docs/screen-elements-research.md`）。
//!
//! 三条来之不易的约定（都是本机实测踩出来的）：
//!
//! 1. **不在祖先节点上用 `IsOffscreen` 剪枝**。Chromium 对这个属性的报告不可靠
//!    （窗口非激活时整棵树被判 offscreen），第一次原型就把 WorkBuddy 整个窗口弄丢了。
//!    可见性过滤放到**叶子级**：用 bbox 与虚拟桌面求交判断（见 [`finalize`]）。
//! 2. **坐标是物理像素**（与 `screenshot`/`click`/`drag` 同一坐标系）——
//!    前提是进程已设 `PER_MONITOR_AWARE_V2`（见 [`super::screen`]）。
//! 3. **数据要节流**：全屏 144 个元素 ≈ 2.5K tokens 是舒适区，
//!    所以有预算制（每窗 ≤600、总 ≤800、8 秒协作式时间预算；无法中断单次 COM 调用）。
//!
//! 顶层窗口的"隐藏"判定用 DWM `DWMWA_CLOAKED`（被虚拟桌面收起的窗口），
//! 比 UIA 自己的 `IsOffscreen` 可靠。

use std::time::{Duration, Instant};

use crate::result::ToolError;

/// 枚举出的一个可交互元素。
///
/// `id` 是给模型引用的编号（SoM：模型答「点击 7」，不用算坐标）；
/// `rect` 是 `[x, y, w, h]`，虚拟桌面物理像素。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScreenElement {
    pub id: usize,
    pub role: &'static str,
    /// 未裁剪的 CurrentName，只用于身份核验；显示与搜索使用 label。
    pub name: String,
    pub label: String,
    pub label_source: &'static str,
    pub enabled: bool,
    pub foreground: bool,
    pub offscreen: bool,
    pub actionable: bool,
    pub clickable_point: Option<(i32, i32)>,
    /// 元素属于哪个顶层窗口（标题）。
    pub window: String,
    pub rect: [i32; 4],
    pub identity: ElementIdentity,
}

/// 只缓存值类型；COM 接口始终在创建它的线程内释放。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ElementIdentity {
    pub hwnd: isize,
    pub process_id: i32,
    pub process_started: u64,
    pub window_process_id: u32,
    pub window_process_started: u64,
    pub window_runtime_id: Vec<i32>,
    pub runtime_id: Vec<i32>,
}

impl ScreenElement {
    pub fn identity_verifiable(&self) -> bool {
        let i = &self.identity;
        i.hwnd != 0 && i.process_id > 0 && i.process_started != 0
            && i.window_process_id != 0 && i.window_process_started != 0
            && !i.runtime_id.is_empty() && !i.window_runtime_id.is_empty()
    }

    pub fn non_executable_reason(&self) -> Option<&'static str> {
        if !self.enabled { Some("disabled_or_unknown") }
        else if !self.foreground { Some("background_window") }
        else if self.offscreen { Some("offscreen_or_unknown") }
        else if !self.actionable { Some("no_click_action") }
        else if !self.identity_verifiable() { Some("unverifiable_identity") }
        else { None }
    }

    /// 原始矩形中心；点击还需选择可见候选并即时核验。
    pub fn center(&self) -> (i32, i32) {
        (
            self.rect[0].saturating_add(self.rect[2] / 2),
            self.rect[1].saturating_add(self.rect[3] / 2),
        )
    }
}

/// 过滤/去重前的原始元素（枚举线程收集）。
pub struct RawElement {
    pub role: &'static str,
    pub name: String,
    pub label: String,
    pub label_source: &'static str,
    pub enabled: bool,
    pub foreground: bool,
    pub offscreen: bool,
    pub actionable: bool,
    pub clickable_point: Option<(i32, i32)>,
    pub window: String,
    pub rect: [i32; 4],
    /// 父元素的 ControlType id（滚动条子按钮靠它过滤；`None` = 顶层）。
    pub parent_role: Option<ControlTypeId>,
    pub identity: ElementIdentity,
}

/// UIA ControlType id（Windows 使用 `i32` newtype 的内层值；非 Windows 同样可测）。
pub type ControlTypeId = i32;

pub const SCROLLBAR: ControlTypeId = 50_003;

// ==== 与实现无关的纯逻辑（可测） ==========================================

/// 滚动条的步进按钮：UIA 给它们起了「垂直小幅下降」这类名字，
/// 是枚举结果里最大的噪音源，且对 agent 毫无操作价值。
fn is_scrollbar_noise(name: &str) -> bool {
    (name.contains("小幅") || name.contains("大幅") || name.contains("一页"))
        && (name.contains("增长")
            || name.contains("下降")
            || name.contains("左移")
            || name.contains("右移")
            || name.contains("上移")
            || name.contains("下移"))
}

/// 收尾：过滤 → 排序 → 编号。
///
/// - **叶子级可见性**：bbox 与虚拟桌面求交（不信任 UIA 的 IsOffscreen，见模块注释）
/// - 无显示标签且没有明确操作能力的元素丢弃；有能力的保留为 unlabeled
/// - 丢滚动条步进按钮（父元素是 ScrollBar，或名字特征命中）
/// - 去重（同窗口同名同 bbox 的只留一个）
/// - 按 **上 → 下、左 → 右** 排序后编号 —— 与人的阅读顺序一致，
///   模型看截图时能对上「编号 7 在编号 6 右边」这类空间关系
pub fn finalize(
    raw: Vec<RawElement>,
    vs: super::screen::Rect,
    budget: usize,
) -> Vec<ScreenElement> {
    let mut seen: Vec<(ElementIdentity, String, String, [i32; 4])> = Vec::new();
    let mut els: Vec<ScreenElement> = Vec::new();
    for e in raw {
        let [x, y, w, h] = e.rect;
        // 与虚拟桌面相交（部分露出的也算 —— 被裁掉一角的按钮仍然可点）
        let visible = intersects([x, y, w, h], [vs.x, vs.y, vs.width, vs.height]);
        if !visible {
            continue;
        }
        let name = e.name;
        if (e.label.is_empty() && !e.actionable) || is_scrollbar_noise(&e.label) {
            continue;
        }
        if e.parent_role == Some(SCROLLBAR) {
            continue;
        }
        // 去重
        let key = (e.identity.clone(), e.window.clone(), name.clone(), e.rect);
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        els.push(ScreenElement {
            id: 0, // 排序后统一编
            role: e.role,
            name,
            label: e.label,
            label_source: e.label_source,
            enabled: e.enabled,
            foreground: e.foreground,
            offscreen: e.offscreen,
            actionable: e.actionable,
            clickable_point: e.clickable_point,
            window: e.window,
            rect: e.rect,
            identity: e.identity,
        });
    }
    els.sort_by(|a, b| {
        a.rect[1]
            .cmp(&b.rect[1])
            .then(a.rect[0].cmp(&b.rect[0]))
            .then(a.rect[3].cmp(&b.rect[3]))
    });
    els.truncate(budget);
    for (i, e) in els.iter_mut().enumerate() {
        e.id = i + 1;
    }
    els
}

// ==== 缓存（供 click 的 element_id 查表） ==================================

static CACHE: std::sync::Mutex<Option<Cached>> = std::sync::Mutex::new(None);

struct Cached {
    at: Instant,
    id: String,
    els: Vec<ScreenElement>,
}

/// 串行化观察与本工具集的动作；不持有任何 COM 对象。
pub static INTERACTION: std::sync::Mutex<()> = std::sync::Mutex::new(());
const CACHE_TTL: Duration = Duration::from_secs(60);
static NEXT_SNAPSHOT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

// 代次的所有读取和修改都在 CACHE 锁内；空缓存失效同样推进代次。
static CACHE_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn store_locked(cell: &mut Option<Cached>, els: &[ScreenElement]) -> String {
    let serial = NEXT_SNAPSHOT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let id = format!("{}-{}-{serial}", std::process::id(), super::screen::epoch_millis());
    *cell = Some(Cached { at: Instant::now(), id: id.clone(), els: els.to_vec() });
    CACHE_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    id
}

#[cfg(test)]
pub fn cache_store(els: &[ScreenElement]) -> String {
    store_locked(&mut CACHE.lock().unwrap(), els)
}

pub fn cache_generation() -> u64 {
    let _cell = CACHE.lock().unwrap();
    CACHE_GENERATION.load(std::sync::atomic::Ordering::Relaxed)
}

fn check_locked(scope: &crate::Scope, expected_generation: u64) -> Result<(), ToolError> {
    if scope.is_cancelled() { return Err(crate::cancelled_error()); }
    if CACHE_GENERATION.load(std::sync::atomic::Ordering::Relaxed) != expected_generation {
        return Err(ToolError::bad_args("屏幕元素观察期间缓存已失效或刷新；请重新枚举"));
    }
    Ok(())
}

pub fn cache_check(scope: &crate::Scope, expected_generation: u64) -> Result<(), ToolError> {
    let _cell = CACHE.lock().unwrap();
    check_locked(scope, expected_generation)
}

pub fn cache_store_checked(els: &[ScreenElement], scope: &crate::Scope, expected_generation: u64) -> Result<String, ToolError> {
    cache_publish_checked(els, scope, expected_generation, None)
}

/// 只在发布时检查 Scope，不把短命任务的取消标记保存到成功快照中。
/// 同内容分页可复用原快照，绝不刷新其 TTL；检查与写入在同一锁内。
pub fn cache_publish_checked(els: &[ScreenElement], scope: &crate::Scope, expected_generation: u64, reuse: Option<&str>) -> Result<String, ToolError> {
    let mut cell = CACHE.lock().unwrap();
    check_locked(scope, expected_generation)?;
    if let Some(cached) = cell.as_ref().filter(|c| Some(c.id.as_str()) == reuse && c.els == els && c.at.elapsed() <= CACHE_TTL) {
        return Ok(cached.id.clone());
    }
    Ok(store_locked(&mut cell, els))
}

/// 调用方取消任务时无需等待 INTERACTION；迟到的旧代发布必然失败。
pub fn cache_invalidate() {
    let mut cell = CACHE.lock().unwrap();
    *cell = None;
    CACHE_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

pub fn cache_invalidate_checked(scope: &crate::Scope, expected_generation: u64) -> Result<u64, ToolError> {
    let mut cell = CACHE.lock().unwrap();
    check_locked(scope, expected_generation)?;
    *cell = None;
    Ok(CACHE_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1)
}

pub fn cache_remaining_seconds() -> u64 {
    CACHE.lock().unwrap().as_ref()
        .map(|cached| CACHE_TTL.saturating_sub(cached.at.elapsed()).as_secs()).unwrap_or(0)
}

pub fn cache_snapshot() -> Option<(String, Vec<ScreenElement>)> {
    let cell = CACHE.lock().unwrap();
    let cached = cell.as_ref()?;
    if cached.at.elapsed() > CACHE_TTL { return None; }
    Some((cached.id.clone(), cached.els.clone()))
}

fn lookup(cached: &Cached, snapshot: &str, id: usize) -> Option<ScreenElement> {
    if snapshot.is_empty() || cached.id != snapshot || cached.at.elapsed() > CACHE_TTL {
        return None;
    }
    let mut matches = cached.els.iter().filter(|e| e.id == id);
    let first = matches.next()?.clone();
    if matches.next().is_some() || cached.els.iter().filter(|e| e.identity == first.identity).count() != 1 { return None; }
    Some(first)
}

#[cfg(test)]
pub fn cache_lookup(snapshot: &str, id: usize) -> Option<ScreenElement> {
    lookup(CACHE.lock().unwrap().as_ref()?, snapshot, id)
}

/// 查找与消费共用缓存锁，即使调用方遗漏动作锁也不可能重复消费。
pub fn cache_consume(snapshot: &str, id: usize) -> Option<ScreenElement> {
    let mut cell = CACHE.lock().unwrap();
    let element = lookup(cell.as_ref()?, snapshot, id)?;
    *cell = None;
    CACHE_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Some(element)
}

/// 验证失败绝不退回旧坐标；成功也只代表输入前的即时核验，不是原子安全保证。
pub fn validate_target(element: &ScreenElement) -> Result<(i32, i32), ToolError> {
    if let Some(reason) = element.non_executable_reason() {
        return Err(ToolError::bad_args(format!("该快照目标不可直接点击：{reason}"))
            .with_hint("请确认前台窗口并重新枚举；不会自动激活窗口或退回旧坐标"));
    }
    if !same_target(element, element, true, false) || element.rect[2] <= 0 || element.rect[3] <= 0 {
        return Err(ToolError::bad_args("快照缺少可验证的目标身份或矩形；请重新枚举"));
    }
    let _dpi = super::screen::physical_pixels()?;
    let started = Instant::now();
    let result = imp::validate(element).map_err(|e| ToolError::bad_args(format!("目标已变化、被遮挡或无法验证：{}", e.message))
        .with_hint("重新运行 screen_elements 获取 snapshot_id 和 element_id；不要重试旧坐标"))?;
    if started.elapsed() > Duration::from_secs(2) {
        return Err(ToolError::bad_args("即时核验耗时过长，已拒绝输入")
            .with_hint("重新运行 screen_elements；不要使用旧坐标"));
    }
    Ok(result)
}

fn same_target(expected: &ScreenElement, current: &ScreenElement, enabled: bool, offscreen: bool) -> bool {
    enabled && !offscreen && expected.identity.hwnd != 0
        && expected.identity.process_id > 0 && expected.identity.process_started != 0
        && expected.identity.window_process_id != 0 && expected.identity.window_process_started != 0
        && !expected.identity.runtime_id.is_empty() && !expected.identity.window_runtime_id.is_empty()
        && expected.identity == current.identity && expected.rect == current.rect
        && expected.name == current.name && expected.role == current.role
        && expected.window == current.window
}

/// 纯几何过滤使用宽整数，允许负坐标及跨显示器区域。
pub fn intersects(a: [i32; 4], b: [i32; 4]) -> bool {
    let [x, y, w, h] = a.map(i64::from);
    let [bx, by, bw, bh] = b.map(i64::from);
    w > 0 && h > 0 && bw > 0 && bh > 0 && x + w > bx && y + h > by && x < bx + bw && y < by + bh
}

const LABEL_CHARS: usize = 96;
const LABEL_VISITS: usize = 16;
const LABEL_PIECES: usize = 3;
const LABEL_DEPTH: usize = 2;
const HIT_ANCESTORS: usize = 4;

fn display_label(name: &str) -> String { name.trim().chars().take(LABEL_CHARS).collect() }

fn protected_name<E>(password: bool, read: impl FnOnce() -> Result<String, E>) -> Result<String, E> {
    if password { Ok(String::new()) } else { read() }
}

fn visible_on_monitors(rect: [i32; 4], monitors: &[[i32; 4]]) -> bool {
    monitors.iter().any(|monitor| intersects(rect, *monitor))
}

fn verified_candidate<E>(points: Vec<(i32, i32)>, mut verify: impl FnMut((i32, i32)) -> Result<bool, E>) -> Option<(i32, i32)> {
    points.into_iter().find(|point| verify(*point).unwrap_or(false))
}

fn passive_content(role: &str, action: bool, focusable: bool, password: bool) -> bool {
    matches!(role, "Text" | "Image") && !action && !focusable && !password
}

fn label_node_allowed(role: &str, action: bool, focusable: bool, password: bool, depth: usize, visits: usize) -> bool {
    depth <= LABEL_DEPTH && visits <= LABEL_VISITS && !password && !action && !focusable
        && matches!(role, "Text" | "Image" | "Pane" | "Group")
}

fn may_ascend_hit(expected: &ScreenElement, current: &ScreenElement, passive: bool, depth: usize, enabled: bool, offscreen: bool) -> bool {
    depth < HIT_ANCESTORS && passive && enabled && !offscreen
        && current.identity.process_id == expected.identity.process_id
        && current.identity.process_started == expected.identity.process_started
        && current.identity.hwnd == expected.identity.hwnd
        && current.identity.window_runtime_id == expected.identity.window_runtime_id
        && current.identity.window_process_id == expected.identity.window_process_id
        && current.identity.window_process_started == expected.identity.window_process_started
}

fn add_label_piece(pieces: &mut Vec<String>, name: &str) {
    let text = display_label(name);
    if !text.is_empty() && pieces.len() < LABEL_PIECES && !pieces.contains(&text) {
        pieces.push(text);
    }
}

fn point_in(rect: [i32; 4], point: (i32, i32)) -> bool {
    super::screen::Rect { x: rect[0], y: rect[1], width: rect[2], height: rect[3] }.contains(point.0, point.1)
}

fn candidate_points(rect: [i32; 4], preferred: Option<(i32, i32)>, monitors: &[[i32; 4]]) -> Vec<(i32, i32)> {
    let mut points = Vec::new();
    if let Some(p) = preferred.filter(|p| point_in(rect, *p) && monitors.iter().any(|m| point_in(*m, *p))) {
        points.push(p);
    }
    for m in monitors.iter().take(32) {
        if !intersects(rect, *m) { continue; }
        let [x, y, w, h] = rect.map(i64::from);
        let [mx, my, mw, mh] = m.map(i64::from);
        let (left, top, right, bottom) = (x.max(mx), y.max(my), (x + w).min(mx + mw), (y + h).min(my + mh));
        for (nx, ny) in [(2, 2), (1, 1), (3, 1), (1, 3), (3, 3)] {
            let p = ((left + (right - left - 1) * nx / 4) as i32,
                (top + (bottom - top - 1) * ny / 4) as i32);
            if !points.contains(&p) { points.push(p); }
        }
    }
    points
}

pub fn parse_role(value: &str) -> Result<Option<&'static str>, ToolError> {
    const ROLES: &[&str] = &["Button", "Hyperlink", "MenuItem", "TabItem", "ListItem", "CheckBox",
        "RadioButton", "ComboBox", "Edit", "Slider", "TreeItem", "DataItem", "Calendar", "Spinner", "SplitButton", "Custom"];
    if value.trim().is_empty() { return Ok(None); }
    ROLES.iter().copied().find(|r| r.eq_ignore_ascii_case(value.trim())).map(Some)
        .ok_or_else(|| ToolError::bad_args(format!("未知 role；支持 {}", ROLES.join("、"))))
}

#[derive(Default)]
pub struct Query<'a> {
    pub window_id: Option<isize>,
    pub keyword: &'a str,
    pub role: Option<&'static str>,
    pub region: Option<[i32; 4]>,
}

impl Query<'_> {
    fn matches_role(&self, role: &str) -> bool { self.role.is_none_or(|wanted| wanted == role) }

    fn matches(&self, name: &str, rect: [i32; 4]) -> bool {
        name.to_lowercase().contains(&self.keyword.to_lowercase())
            && self.region.is_none_or(|r| intersects(rect, r))
    }
}

#[derive(Clone, Debug)]
pub struct WindowSummary {
    pub hwnd: isize,
    pub title: String,
    pub process_id: i32,
    pub rect: [i32; 4],
    pub foreground: bool,
    pub minimized: bool,
}

pub fn desktop_overview() -> Result<(Vec<WindowSummary>, bool), ToolError> {
    let _dpi = super::screen::physical_pixels()?;
    imp::overview()
}

// ==== 真正的枚举 ==========================================================

/// 枚举元素；默认可限定到当前焦点窗口，`all_windows=true` 时遍历整个桌面。
#[cfg(test)]
pub fn enumerate_mode(
    window_filter: Option<&str>,
    all_windows: bool,
    per_window_budget: usize,
    total_budget: usize,
) -> Result<Vec<ScreenElement>, ToolError> {
    enumerate_query(window_filter, all_windows, per_window_budget, total_budget, &Query::default())
}

pub fn enumerate_query(
    window_filter: Option<&str>, all_windows: bool, per_window_budget: usize,
    total_budget: usize, query: &Query<'_>,
) -> Result<Vec<ScreenElement>, ToolError> {
    let _dpi = super::screen::physical_pixels()?;
    let raw = imp::collect(per_window_budget, total_budget, all_windows, window_filter, query).map_err(|e| {
        ToolError::io(format!("Windows UI Automation 枚举失败：{e}"))
            .with_hint("刚切换窗口时 UIA 树可能正在重建；等一秒后重试")
    })?;
    Ok(finalize(raw, super::screen::virtual_screen(), total_budget))
}

fn window_matches(title: &str, filter: Option<&str>) -> bool {
    filter.is_none_or(|f| title.to_lowercase().contains(&f.to_lowercase()))
}

#[cfg(windows)]
mod imp {
    //! COM 侧：CoInitialize → CoCreateInstance(CUIAutomation) → ControlViewWalker DFS。
    //!
    //! 用 **Current** 属性而不是 Cached：DFS 是即时树，Cached 需要额外建请求对象，
    //! 收益在这个预算规模（≤800 元素）下不值得。实测 Python 同款遍历 540ms，
    //! Rust 侧只会更快。

    use std::time::{Duration, Instant};

    use windows::core::Result as WinResult;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
        COINIT_MULTITHREADED,
    };
    use windows::Win32::UI::Accessibility::{
        CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationTreeWalker,
        UIA_ButtonControlTypeId, UIA_CalendarControlTypeId, UIA_CheckBoxControlTypeId,
        UIA_ComboBoxControlTypeId, UIA_DataItemControlTypeId, UIA_EditControlTypeId,
        UIA_HyperlinkControlTypeId, UIA_ListItemControlTypeId, UIA_MenuItemControlTypeId,
        UIA_RadioButtonControlTypeId, UIA_SliderControlTypeId, UIA_SpinnerControlTypeId,
        UIA_SplitButtonControlTypeId, UIA_TabItemControlTypeId, UIA_TreeItemControlTypeId,
    };

    use super::{ControlTypeId, RawElement, Query, ElementIdentity, ScreenElement, WindowSummary};
    use crate::result::ToolError;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowTextW, GetWindowThreadProcessId, GetWindowRect,
        IsWindowVisible, IsIconic, GetForegroundWindow, WindowFromPoint, GetAncestor, GA_ROOT,
    };

    // 现有 windows feature 未启用 Ole；只声明读取运行时 ID 所需的 SAFEARRAY API。
    #[link(name = "oleaut32")]
    unsafe extern "system" {
        fn SafeArrayGetDim(array: *mut core::ffi::c_void) -> u32;
        fn SafeArrayGetElemsize(array: *mut core::ffi::c_void) -> u32;
        fn SafeArrayGetLBound(array: *mut core::ffi::c_void, dim: u32, bound: *mut i32) -> i32;
        fn SafeArrayGetUBound(array: *mut core::ffi::c_void, dim: u32, bound: *mut i32) -> i32;
        fn SafeArrayGetElement(array: *mut core::ffi::c_void, index: *const i32, value: *mut core::ffi::c_void) -> i32;
        fn SafeArrayDestroy(array: *mut core::ffi::c_void) -> i32;
    }

    fn runtime_id(el: &IUIAutomationElement) -> WinResult<Vec<i32>> {
        unsafe {
            let array = el.GetRuntimeId()?;
            if array.is_null() { return Err(windows::core::Error::from_hresult(windows::core::HRESULT(0x80004005u32 as i32))); }
            let ptr = array.cast();
            let (mut lo, mut hi) = (0, -1);
            let mut values = Vec::new();
            if SafeArrayGetDim(ptr) == 1 && SafeArrayGetElemsize(ptr) == 4 && SafeArrayGetLBound(ptr, 1, &mut lo) >= 0
                && SafeArrayGetUBound(ptr, 1, &mut hi) >= 0 && (1..=128).contains(&(i64::from(hi) - i64::from(lo) + 1)) {
                for i in lo..=hi {
                    let mut value = 0i32;
                    if SafeArrayGetElement(ptr, &i, (&mut value as *mut i32).cast()) < 0 {
                        values.clear(); break;
                    }
                    values.push(value);
                }
            }
            let destroyed = SafeArrayDestroy(ptr);
            if destroyed < 0 { return Err(windows::core::Error::from_hresult(windows::core::HRESULT(destroyed))); }
            Ok(values)
        }
    }

    fn process_started(pid: u32) -> WinResult<u64> {
        use windows_sys::Win32::System::Threading::{OpenProcess, GetProcessTimes, PROCESS_QUERY_LIMITED_INFORMATION};
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() { return Err(windows::core::Error::from_thread()); }
            let mut created = core::mem::zeroed();
            let mut exited = core::mem::zeroed();
            let mut kernel = core::mem::zeroed();
            let mut user = core::mem::zeroed();
            let ok = GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user);
            let error = windows::core::Error::from_thread();
            let closed = windows_sys::Win32::Foundation::CloseHandle(handle);
            if ok == 0 { return Err(error); }
            if closed == 0 { return Err(windows::core::Error::from_thread()); }
            Ok((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
        }
    }

    fn identity(el: &IUIAutomationElement, hwnd: isize, window_runtime_id: Vec<i32>) -> WinResult<ElementIdentity> {
        unsafe {
            let mut window_process_id = 0;
            if GetWindowThreadProcessId(hwnd as _, &mut window_process_id) == 0 {
                return Err(windows::core::Error::from_thread());
            }
            let process_id = el.CurrentProcessId()?;
            Ok(ElementIdentity {
                hwnd, process_id, process_started: process_started(process_id as u32)?,
                window_process_id, window_process_started: process_started(window_process_id)?,
                window_runtime_id, runtime_id: runtime_id(el)?,
            })
        }
    }

    fn rect_values(r: windows::Win32::Foundation::RECT) -> WinResult<[i32; 4]> {
        let width = r.right.checked_sub(r.left).filter(|n| *n > 0);
        let height = r.bottom.checked_sub(r.top).filter(|n| *n > 0);
        match (width, height) {
            (Some(w), Some(h)) => Ok([r.left, r.top, w, h]),
            _ => Err(windows::core::Error::from_hresult(windows::core::HRESULT(0x80004005u32 as i32))),
        }
    }

    struct ComGuard;
    impl ComGuard {
        fn new() -> WinResult<Self> {
            unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()?; }
            Ok(Self)
        }
    }
    impl Drop for ComGuard {
        fn drop(&mut self) { unsafe { CoUninitialize(); } }
    }

    fn monitors() -> WinResult<Vec<[i32; 4]>> {
        use windows_sys::Win32::Graphics::Gdi::{EnumDisplayMonitors, HMONITOR, HDC};
        unsafe extern "system" fn visit(_: HMONITOR, _: HDC, r: *mut windows_sys::Win32::Foundation::RECT, data: isize) -> i32 {
            unsafe {
                let out = &mut *(data as *mut Vec<[i32; 4]>);
                let r = &*r;
                if let (Some(w), Some(h)) = (r.right.checked_sub(r.left), r.bottom.checked_sub(r.top)) {
                    if w > 0 && h > 0 { out.push([r.left, r.top, w, h]); }
                }
                1
            }
        }
        let mut out = Vec::new();
        if unsafe { EnumDisplayMonitors(core::ptr::null_mut(), core::ptr::null(), Some(visit), (&mut out as *mut Vec<[i32; 4]>) as isize) } == 0 || out.is_empty() {
            return Err(windows::core::Error::from_thread());
        }
        Ok(out)
    }

    fn action_pattern_state(el: &IUIAutomationElement) -> Option<bool> {
        use windows::Win32::UI::Accessibility::{UIA_InvokePatternId, UIA_TogglePatternId, UIA_SelectionItemPatternId, UIA_ExpandCollapsePatternId};
        let mut known = true;
        for id in [UIA_InvokePatternId, UIA_TogglePatternId, UIA_SelectionItemPatternId, UIA_ExpandCollapsePatternId] {
            match unsafe { el.GetCurrentPattern(id) } {
                Ok(_) => return Some(true), // 仅查询接口，绝不调用 Invoke/Toggle。
                Err(e) if matches!(e.code().0 as u32, 0x80004002 | 0x80004003 | 0x80040204) => {},
                Err(_) => known = false,
            }
        }
        known.then_some(false)
    }

    fn action_pattern(el: &IUIAutomationElement) -> bool { action_pattern_state(el) == Some(true) }

    fn passive(el: &IUIAutomationElement) -> WinResult<bool> {
        unsafe {
            let role = match el.CurrentControlType()?.0 { 50020 => "Text", 50006 => "Image", _ => "" };
            Ok(super::passive_content(role, action_pattern_state(el) != Some(false), el.CurrentIsKeyboardFocusable()?.as_bool(), el.CurrentIsPassword()?.as_bool()))
        }
    }

    fn label(el: &IUIAutomationElement, walker: &IUIAutomationTreeWalker, name: &str, password: bool) -> (String, &'static str) {
        if password { return (String::new(), "unlabeled"); }
        let own = super::display_label(name);
        if !own.is_empty() { return (own, "name"); }
        unsafe {
            if let Ok(other) = el.CurrentLabeledBy() {
                if passive(&other).unwrap_or(false) {
                    if let Ok(text) = other.CurrentName() {
                        let text = super::display_label(&text.to_string());
                        if !text.is_empty() { return (text, "labeled_by"); }
                    }
                }
            }
            // 输入控件不向内部取文本；从不读取 Value/TextPattern。
            if el.CurrentControlType().map(|t| t.0 == 50004).unwrap_or(true) {
                return (String::new(), "unlabeled");
            }
            let mut stack = Vec::new();
            if let Ok(child) = walker.GetFirstChildElement(el) { stack.push((child, 1)); }
            let mut pieces = Vec::new();
            let mut visits = 1; // 为 LabeledBy 探测预留一次，整条回退路径最多 16 个节点。
            while let Some((node, depth)) = stack.pop() {
                if visits >= super::LABEL_VISITS || pieces.len() >= super::LABEL_PIECES { break; }
                visits += 1;
                if visits < super::LABEL_VISITS {
                    if let Ok(sibling) = walker.GetNextSiblingElement(&node) { stack.push((sibling, depth)); }
                }
                let role = match node.CurrentControlType().map(|t| t.0).unwrap_or(0) {
                    50020 => "Text", 50006 => "Image", 50033 => "Pane", 50026 => "Group", _ => "",
                };
                if !super::label_node_allowed(role, action_pattern_state(&node) != Some(false),
                    node.CurrentIsKeyboardFocusable().map(|v| v.as_bool()).unwrap_or(true),
                    node.CurrentIsPassword().map(|v| v.as_bool()).unwrap_or(true), depth, visits) { continue; }
                if role == "Text" {
                    if let Ok(text) = node.CurrentName() { super::add_label_piece(&mut pieces, &text.to_string()); }
                }
                if depth < super::LABEL_DEPTH && visits < super::LABEL_VISITS && pieces.len() < super::LABEL_PIECES {
                    if let Ok(child) = walker.GetFirstChildElement(&node) { stack.push((child, depth + 1)); }
                }
            }
            let text = super::display_label(&pieces.join(" "));
            let source = if text.is_empty() { "unlabeled" } else { "child_text" };
            (text, source)
        }
    }

    fn resolve_hit(uia: &IUIAutomation, walker: &IUIAutomationTreeWalker, expected: &ScreenElement, point: (i32, i32)) -> WinResult<Option<IUIAutomationElement>> {
        unsafe {
            let top = uia.ElementFromHandle(HWND(expected.identity.hwnd as _))?;
            let top_id = runtime_id(&top)?;
            let mut el = uia.ElementFromPoint(windows::Win32::Foundation::POINT { x: point.0, y: point.1 })?;
            for depth in 0..=super::HIT_ANCESTORS {
                if el.CurrentIsPassword()?.as_bool() { return Ok(None); }
                let current = ScreenElement {
                    role: role_of(el.CurrentControlType()?.0).unwrap_or(""),
                    name: el.CurrentName()?.to_string(), window: top.CurrentName()?.to_string(),
                    rect: rect_values(el.CurrentBoundingRectangle()?)?,
                    identity: identity(&el, expected.identity.hwnd, top_id.clone())?,
                    ..Default::default()
                };
                if !current.identity_verifiable() { return Ok(None); }
                let enabled = el.CurrentIsEnabled()?.as_bool();
                let offscreen = el.CurrentIsOffscreen()?.as_bool();
                if super::same_target(expected, &current, enabled, offscreen) {
                    return Ok(actionable(&el, current.role).then_some(el));
                }
                if !super::may_ascend_hit(expected, &current, passive(&el)?, depth, enabled, offscreen) {
                    return Ok(None);
                }
                el = walker.GetParentElement(&el)?;
            }
            Ok(None)
        }
    }

    fn actionable(el: &IUIAutomationElement, role: &str) -> bool {
        action_pattern(el) || (role == "Edit" && unsafe { el.CurrentIsKeyboardFocusable().map(|v| v.as_bool()).unwrap_or(false) })
    }

    pub fn validate(expected: &ScreenElement) -> Result<(i32, i32), ToolError> {
        let _com = ComGuard::new().map_err(|e| ToolError::io(e.to_string()))?;
        let check = || -> WinResult<Option<(i32, i32)>> { unsafe {
            let uia: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)?;
            let walker = uia.RawViewWalker()?;
            let hwnd = expected.identity.hwnd as windows_sys::Win32::Foundation::HWND;
            if hwnd.is_null() || GetForegroundWindow() != hwnd || IsWindowVisible(hwnd) == 0 || IsIconic(hwnd) != 0 { return Ok(None); }
            let mut cloaked = 0u32;
            DwmGetWindowAttribute(HWND(hwnd), DWMWA_CLOAKED, (&mut cloaked as *mut u32).cast(), 4)?;
            if cloaked != 0 { return Ok(None); }
            let monitors = monitors()?;
            let started = Instant::now();
            Ok(super::verified_candidate(super::candidate_points(expected.rect, expected.clickable_point, &monitors), |point| -> WinResult<bool> {
                if started.elapsed() > Duration::from_secs(2) { return Ok(false); }
                let window_hit = || GetForegroundWindow() == hwnd && GetAncestor(WindowFromPoint(windows_sys::Win32::Foundation::POINT { x: point.0, y: point.1 }), GA_ROOT) == hwnd;
                if !window_hit() { return Ok(false); }
                let Some(el) = resolve_hit(&uia, &walker, expected, point)? else { return Ok(false); };
                let Some(latest) = resolve_hit(&uia, &walker, expected, point)? else { return Ok(false); };
                use windows_sys::Win32::Graphics::Gdi::{MonitorFromPoint, MONITOR_DEFAULTTONULL};
                Ok(uia.CompareElements(&el, &latest)?.as_bool() && window_hit()
                    && started.elapsed() <= Duration::from_secs(2)
                    && !MonitorFromPoint(windows_sys::Win32::Foundation::POINT { x: point.0, y: point.1 }, MONITOR_DEFAULTTONULL).is_null())
            }))
        }};
        check().map_err(|e| ToolError::io(e.to_string()))?.ok_or_else(|| ToolError::bad_args("实时命中与快照不一致"))
    }

    pub fn overview() -> Result<(Vec<WindowSummary>, bool), ToolError> {
        struct Context { windows: Vec<WindowSummary>, truncated: bool }
        unsafe extern "system" fn visit(hwnd: windows_sys::Win32::Foundation::HWND, param: isize) -> i32 {
            unsafe {
                let ctx = &mut *(param as *mut Context);
                if ctx.windows.len() >= 256 { ctx.truncated = true; return 0; }
                if IsWindowVisible(hwnd) == 0 { return 1; }
                let mut cloaked = 0u32;
                if DwmGetWindowAttribute(HWND(hwnd), DWMWA_CLOAKED, (&mut cloaked as *mut u32).cast(), 4).is_ok() && cloaked != 0 { return 1; }
                let mut title = [0u16; 512];
                let n = GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32).max(0) as usize;
                let mut pid = 0;
                if GetWindowThreadProcessId(hwnd, &mut pid) == 0 { return 1; }
                let mut r = core::mem::zeroed();
                if GetWindowRect(hwnd, &mut r) == 0 { return 1; }
                let (Some(width), Some(height)) = (r.right.checked_sub(r.left), r.bottom.checked_sub(r.top)) else { return 1; };
                ctx.windows.push(WindowSummary {
                    hwnd: hwnd as isize, title: String::from_utf16_lossy(&title[..n]), process_id: pid as i32,
                    rect: [r.left, r.top, width, height],
                    foreground: GetForegroundWindow() == hwnd, minimized: IsIconic(hwnd) != 0,
                });
                1
            }
        }
        let mut ctx = Context { windows: Vec::new(), truncated: false };
        let ok = unsafe { EnumWindows(Some(visit), (&mut ctx as *mut Context) as isize) };
        if ok == 0 && !ctx.truncated { return Err(ToolError::io("顶层窗口概览失败")); }
        ctx.windows.sort_by_key(|w| !w.foreground);
        Ok((ctx.windows, ctx.truncated))
    }

    /// UIA ControlType id → Neo 的角色名。只收**可交互**的 16 类。
    fn role_of(id: ControlTypeId) -> Option<&'static str> {
        Some(match id {
            id if id == UIA_ButtonControlTypeId.0 => "Button",
            id if id == UIA_HyperlinkControlTypeId.0 => "Hyperlink",
            id if id == UIA_MenuItemControlTypeId.0 => "MenuItem",
            id if id == UIA_TabItemControlTypeId.0 => "TabItem",
            id if id == UIA_ListItemControlTypeId.0 => "ListItem",
            id if id == UIA_CheckBoxControlTypeId.0 => "CheckBox",
            id if id == UIA_RadioButtonControlTypeId.0 => "RadioButton",
            id if id == UIA_ComboBoxControlTypeId.0 => "ComboBox",
            id if id == UIA_EditControlTypeId.0 => "Edit",
            id if id == UIA_SliderControlTypeId.0 => "Slider",
            id if id == UIA_TreeItemControlTypeId.0 => "TreeItem",
            id if id == UIA_DataItemControlTypeId.0 => "DataItem",
            id if id == UIA_CalendarControlTypeId.0 => "Calendar",
            id if id == UIA_SpinnerControlTypeId.0 => "Spinner",
            id if id == UIA_SplitButtonControlTypeId.0 => "SplitButton",
            50025 => "Custom",
            _ => return None,
        })
    }

    /// 单窗口 DFS 的时间上限。个别应用的 UIA 树响应极慢（几十 ms 一个节点），
    /// 没有这个上限一个坏窗口就能把整次枚举拖到分钟级。
    const WINDOW_TIME_LIMIT: Duration = Duration::from_secs(4);
    /// 整次枚举的协作式预算；不能中断卡住的 UIA 提供程序调用。
    const TOTAL_TIME_LIMIT: Duration = Duration::from_secs(8);

    /// 枚举所有可见顶层窗口。返回原始元素（过滤交给 [`super::finalize`]）。
    pub fn collect(
        per_window: usize,
        total: usize,
        all_windows: bool,
        window_filter: Option<&str>,
        query: &Query<'_>,
    ) -> WinResult<Vec<RawElement>> {
        let _com = ComGuard::new()?;
        collect_inner(per_window, total, all_windows, window_filter, query)
    }

    fn collect_inner(
        per_window: usize,
        total: usize,
        all_windows: bool,
        window_filter: Option<&str>,
        query: &Query<'_>,
    ) -> WinResult<Vec<RawElement>> {
        unsafe {
            let uia: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)?;
            let walker: IUIAutomationTreeWalker = uia.ControlViewWalker()?;
            let root = uia.GetRootElement()?;
            let monitors = monitors()?;

            let t0 = Instant::now();
            let mut raw: Vec<RawElement> = Vec::new();

            if !all_windows && window_filter.is_none() && query.window_id.is_none() {
                let focused = uia.GetFocusedElement()?;
                let mut top = focused;
                let mut ancestors = 0;
                while let Ok(parent) = walker.GetParentElement(&top) {
                    ancestors += 1;
                    if ancestors > 64 || t0.elapsed() > TOTAL_TIME_LIMIT {
                        return Err(windows::core::Error::from_hresult(windows::core::HRESULT(0x80004005u32 as i32)));
                    }
                    if walker.GetParentElement(&parent).is_err() {
                        break; // parent 是 UIA 根；top 就是当前焦点所属的顶层窗口
                    }
                    top = parent;
                }
                let title = top.CurrentName().map(|s| s.to_string()).unwrap_or_default();
                dfs(&walker, &top, title, per_window.min(total), &mut raw, Instant::now(), t0, query, &monitors);
                return Ok(raw);
            }

            // 全桌面模式：桌面的直接子节点 = 顶层窗口。逐窗口 DFS，
            let mut top = walker.GetFirstChildElement(&root)?;
            loop {
                if raw.len() >= total || t0.elapsed() > TOTAL_TIME_LIMIT {
                    break;
                }
                let title = top.CurrentName().map(|s| s.to_string()).unwrap_or_default();
                // 先按顶层标题定位目标；其他窗口不进入 DFS，不消耗元素预算。
                let hwnd = top.CurrentNativeWindowHandle().map(|h| h.0 as isize).unwrap_or(0);
                if super::window_matches(&title, window_filter)
                    && query.window_id.is_none_or(|id| id == hwnd) && !is_cloaked(&top) {
                    // 桌面（Progman/WorkerW）与任务栏照常枚举：桌面图标是真实可点的。
                    dfs(
                        &walker,
                        &top,
                        title.clone(),
                        per_window.min(total - raw.len()),
                        &mut raw,
                        Instant::now(), t0, query, &monitors,
                    );
                }
                let next = walker.GetNextSiblingElement(&top);
                match next {
                    Ok(n) => top = n,
                    Err(_) => break,
                }
            }
            Ok(raw)
        }
    }

    /// 显式栈的 DFS（不用递归 —— UIA 树深度不可控，防栈溢出）。
    fn dfs(
        walker: &IUIAutomationTreeWalker,
        window_el: &IUIAutomationElement,
        window: String,
        budget: usize,
        out: &mut Vec<RawElement>,
        t0: Instant,
        total_start: Instant,
        query: &Query<'_>,
        monitors: &[[i32; 4]],
    ) {
        unsafe {
            let hwnd = window_el.CurrentNativeWindowHandle().map(|h| h.0 as isize).unwrap_or(0);
            let window_runtime_id = runtime_id(window_el).unwrap_or_default();
            let mut visited = 0usize;
            let mut stack: Vec<(IUIAutomationElement, ControlTypeId, usize)> =
                vec![(window_el.clone(), 0, 0)];
            let mut count = 0usize;
            while let Some((el, parent_type, depth)) = stack.pop() {
                visited += 1;
                if count >= budget || visited > 10_000 || t0.elapsed() > WINDOW_TIME_LIMIT || total_start.elapsed() > TOTAL_TIME_LIMIT {
                    return;
                }
                let ctype = el.CurrentControlType().map(|t| t.0).unwrap_or(0);
                if let Some(role) = role_of(ctype).filter(|role| query.matches_role(role)) {
                    let action = actionable(&el, role);
                    if role != "Custom" || action {
                        if let Some(rect) = el.CurrentBoundingRectangle().and_then(rect_values).ok()
                            .filter(|rect| super::visible_on_monitors(*rect, monitors)) {
                            let password = el.CurrentIsPassword().map(|v| v.as_bool()).unwrap_or(true);
                            let raw_name = super::protected_name(password, || el.CurrentName().map(|s| s.to_string()));
                            let (text, source) = label(&el, walker, raw_name.as_deref().unwrap_or(""), password);
                            if (!text.is_empty() || action) && parent_type != super::SCROLLBAR
                                && !super::is_scrollbar_noise(&text) && query.matches(&text, rect) {
                                let mut point = windows::Win32::Foundation::POINT::default();
                                let clickable_point = el.GetClickablePoint(&mut point).ok().filter(|v| v.as_bool()).map(|_| (point.x, point.y));
                                let ident = if raw_name.is_ok() && !password { identity(&el, hwnd, window_runtime_id.clone()).ok() } else { None };
                                out.push(RawElement {
                                    identity: ident.unwrap_or_else(|| ElementIdentity { hwnd, ..Default::default() }),
                                    role, name: raw_name.unwrap_or_default(), label: text, label_source: source,
                                    enabled: el.CurrentIsEnabled().map(|v| v.as_bool()).unwrap_or(false),
                                    foreground: hwnd != 0 && GetForegroundWindow() as isize == hwnd,
                                    offscreen: el.CurrentIsOffscreen().map(|v| v.as_bool()).unwrap_or(true),
                                    actionable: action, clickable_point,
                                    window: window.clone(), rect,
                                    parent_role: if parent_type == 0 { None } else { Some(parent_type) },
                                });
                                count += 1;
                            }
                        }
                    }
                }
                if el.CurrentIsPassword().map(|v| v.as_bool()).unwrap_or(true) { continue; }
                if depth < 14 {
                    // 栈是 LIFO，孩子的入栈顺序无所谓 —— finalize 会按空间重排。
                    // guard 防御个别控件 GetNextSibling 自环（实测见过这类坏控件）。
                    let mut child = walker.GetFirstChildElement(&el).ok();
                    let mut guard = 0usize;
                    while let Some(c) = child {
                        guard += 1;
                        if guard > 2000 || stack.len() >= 10_000 || t0.elapsed() > WINDOW_TIME_LIMIT || total_start.elapsed() > TOTAL_TIME_LIMIT {
                            break;
                        }
                        stack.push((c.clone(), ctype, depth + 1));
                        match walker.GetNextSiblingElement(&c) {
                            Ok(n) => child = Some(n),
                            Err(_) => break,
                        }
                    }
                }
            }
        }
    }

    /// DWM `DWMWA_CLOAKED`：被虚拟桌面收起 / UWP 挂起的窗口。
    /// 这些窗口的 UIA 树仍在，但元素不可点 —— 剔除，免得给模型一张"幽灵清单"。
    fn is_cloaked(el: &IUIAutomationElement) -> bool {
        unsafe {
            let Ok(hwnd) = el.CurrentNativeWindowHandle() else {
                return false; // 拿不到 hwnd 就不猜 —— 交给叶子级 bbox 过滤兜底
            };
            let mut cloaked: u32 = 0;
            let hr = DwmGetWindowAttribute(
                HWND(hwnd.0),
                DWMWA_CLOAKED,
                &mut cloaked as *mut u32 as *mut core::ffi::c_void,
                std::mem::size_of::<u32>() as u32,
            );
            hr.is_ok() && cloaked != 0
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::RawElement;
    use crate::result::{ErrorKind, ToolError};

    pub fn validate(_element: &super::ScreenElement) -> Result<(i32, i32), ToolError> {
        Err(ToolError::new(ErrorKind::Unsupported, "元素核验只支持 Windows"))
    }

    pub fn overview() -> Result<(Vec<super::WindowSummary>, bool), ToolError> {
        Err(ToolError::new(ErrorKind::Unsupported, "窗口概览只支持 Windows"))
    }

    /// 非 Windows 平台：屏幕交互工具全部返回 unsupported（与 screen.rs 一致）。
    pub fn collect(
        _per_window: usize,
        _total: usize,
        _all_windows: bool,
        _window_filter: Option<&str>,
        _query: &super::Query<'_>,
    ) -> Result<Vec<RawElement>, ToolError> {
        Err(ToolError::new(
            ErrorKind::Unsupported,
            "screen_elements 只在 Windows 上可用",
        )
        .with_hint("Neo 的屏幕感知依赖 Windows UI Automation"))
    }
}

// ==== 测试 ================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::screen::Rect;

    fn vs() -> Rect {
        Rect {
            x: 0,
            y: 0,
            width: 3200,
            height: 2000,
        }
    }

    fn raw(name: &str, rect: [i32; 4]) -> RawElement {
        RawElement {
            role: "Button",
            name: name.to_owned(), label: display_label(name), label_source: "name",
            enabled: true, foreground: true, offscreen: false, actionable: false, clickable_point: None,
            window: "窗口".to_owned(),
            rect,
            parent_role: None,
            identity: ElementIdentity::default(),
        }
    }

    #[test]
    fn cancelled_and_late_publishers_cannot_replace_new_snapshot() {
        let _interaction = INTERACTION.lock().unwrap();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let scope = crate::Scope::new(std::env::temp_dir()).with_cancel(cancel.clone());
        let old_generation = cache_generation();
        cache_invalidate();
        let newer = cache_store(&[]);
        let late = cache_store_checked(&[], &scope, old_generation);
        let unchanged = cache_snapshot().unwrap().0 == newer;
        cancel.store(true, std::sync::atomic::Ordering::Release);
        let cancelled = cache_store_checked(&[], &scope, cache_generation());
        let still_unchanged = cache_snapshot().unwrap().0 == newer;
        let stale_invalidation = cache_invalidate_checked(&crate::Scope::new(std::env::temp_dir()), old_generation);
        let not_cleared = cache_snapshot().is_some();
        cache_invalidate();
        drop(_interaction);
        assert!(late.is_err(), "失效前启动的发布者必须拒绝");
        assert!(unchanged && still_unchanged, "旧代或已取消发布者不能覆盖新快照");
        assert!(cancelled.is_err(), "取消的 Scope 必须拒绝发布");
        assert!(stale_invalidation.is_err() && not_cleared, "旧观察失败也不能清空新缓存");
    }

    #[test]
    fn checked_publish_survives_job_drop_but_not_explicit_invalidation() {
        let _interaction = INTERACTION.lock().unwrap();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let scope = crate::Scope::new(std::env::temp_dir()).with_cancel(cancel.clone());
        let id = cache_store_checked(&[], &scope, cache_generation()).unwrap();
        let at = CACHE.lock().unwrap().as_ref().unwrap().at;
        let generation = cache_generation();
        assert_eq!(cache_publish_checked(&[], &scope, generation, Some(&id)).unwrap(), id);
        assert_eq!(CACHE.lock().unwrap().as_ref().unwrap().at, at);
        assert_eq!(cache_generation(), generation);
        cancel.store(true, std::sync::atomic::Ordering::Release);
        drop(scope);
        assert_eq!(cache_snapshot().unwrap().0, id, "正常任务结束不使成功快照失效");
        cache_invalidate();
        assert!(cache_snapshot().is_none());
        assert_ne!(cache_generation(), generation);
        let empty_generation = cache_generation();
        cache_invalidate();
        assert_ne!(cache_generation(), empty_generation);
    }

    #[test]
    fn publication_waiting_on_cache_lock_rechecks_cancellation_inside_lock() {
        let _interaction = INTERACTION.lock().unwrap();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let scope = crate::Scope::new(std::env::temp_dir()).with_cancel(cancel.clone());
        let newer = cache_store(&[]);
        let generation = cache_generation();
        let lock = CACHE.lock().unwrap();
        let (started, ready) = std::sync::mpsc::channel();
        let result = std::thread::scope(|threads| {
            let job = threads.spawn(|| {
                started.send(()).unwrap();
                cache_store_checked(&[], &scope, generation)
            });
            ready.recv().unwrap();
            cancel.store(true, std::sync::atomic::Ordering::Release);
            drop(lock);
            job.join().unwrap()
        });
        assert!(result.is_err());
        assert_eq!(cache_snapshot().unwrap().0, newer);
        cache_invalidate();
    }

    #[test]
    fn window_filter_selects_targets_before_spending_element_budget() {
        let windows = [("其他窗口", 800), ("目标 EDITOR", 2), ("Editor 第二窗口", 3)];
        let mut remaining = 4;
        let mut visited = Vec::new();
        for (title, count) in windows {
            if window_matches(title, Some("editor")) {
                let taken = count.min(remaining);
                visited.push((title, taken));
                remaining -= taken;
            }
        }
        assert_eq!(visited, vec![("目标 EDITOR", 2), ("Editor 第二窗口", 2)]);
        assert!(window_matches("任意窗口", None));
        assert!(window_matches("ÄBC", Some("äb")));
        assert!(!window_matches("其他窗口", Some("目标")));
    }

    /// 里程碑式的 happy path：过滤、排序、编号一条龙。
    #[test]
    fn finalize_sorts_by_reading_order_and_numbers() {
        let els = finalize(
            vec![
                raw("右下", [500, 400, 100, 40]),
                raw("左上", [10, 10, 100, 40]),
                raw("同行右", [200, 10, 100, 40]),
            ],
            vs(),
            800,
        );
        let ids: Vec<(usize, &str)> = els.iter().map(|e| (e.id, e.name.as_str())).collect();
        assert_eq!(ids, vec![(1, "左上"), (2, "同行右"), (3, "右下")]);
    }

    /// 本机实测的两大噪音源：无名元素与滚动条步进按钮。
    #[test]
    fn noise_is_filtered() {
        let els = finalize(
            vec![
                raw("", [10, 10, 100, 40]),             // 无名
                raw("垂直小幅下降", [10, 60, 24, 100]), // 滚动条步进
                raw("真按钮", [10, 200, 100, 40]),
            ],
            vs(),
            800,
        );
        assert_eq!(els.len(), 1);
        assert_eq!(els[0].name, "真按钮");
    }

    /// 父元素是 ScrollBar 的子按钮一律丢（比名字特征更可靠的信号）。
    #[test]
    fn scrollbar_children_are_dropped_by_parent_type() {
        let mut sb = raw("拖我", [10, 60, 24, 100]);
        sb.parent_role = Some(SCROLLBAR);
        let els = finalize(vec![sb, raw("真按钮", [10, 200, 100, 40])], vs(), 800);
        assert_eq!(els.len(), 1);
    }

    /// 叶子级可见性：部分露出窗口的按钮仍然保留（bbox 与桌面相交即可），
    /// 但完全在桌面外的不留 —— 这就是"不能用 IsOffscreen 剪枝"的正确替代。
    #[test]
    fn visibility_is_decided_by_bbox_intersection() {
        let els = finalize(
            vec![
                raw("半露出", [-50, 10, 100, 40]),  // 左半在屏幕外
                raw("全在外", [-500, 10, 100, 40]), // 完全在桌面左侧之外
                raw("在桌面内", [10, 10, 100, 40]),
            ],
            vs(),
            800,
        );
        let names: Vec<&str> = els.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["半露出", "在桌面内"]);
    }

    #[test]
    fn duplicates_are_collapsed() {
        let els = finalize(
            vec![
                raw("同一个", [10, 10, 100, 40]),
                raw("同一个", [10, 10, 100, 40]),
            ],
            vs(),
            800,
        );
        assert_eq!(els.len(), 1);
    }

    #[test]
    fn budget_truncates_after_numbering() {
        let raws: Vec<RawElement> = (0..50)
            .map(|i| raw(&format!("b{i}"), [0, i * 50, 100, 40]))
            .collect();
        let els = finalize(raws, vs(), 10);
        assert_eq!(els.len(), 10);
        assert_eq!(els.last().unwrap().id, 10);
    }

    #[test]
    fn cache_round_trip_and_expiry_semantics() {
        let els = vec![ScreenElement {
            id: 1,
            role: "Button",
            name: "开始".into(),
            window: "任务栏".into(),
            rect: [1061, 1904, 90, 96],
            identity: ElementIdentity::default(),
            ..Default::default()
        }];
        let _interaction = INTERACTION.lock().unwrap();
        let snapshot = cache_store(&els);
        let got = cache_lookup(&snapshot, 1).expect("刚存过就应当查得到");
        assert_eq!(got.name, "开始");
        assert_eq!(got.center(), (1106, 1952));
        assert!(cache_lookup(&snapshot, 99).is_none(), "没存过的编号查不到");
        let newer = cache_store(&els);
        assert!(cache_lookup(&snapshot, 1).is_none());
        assert!(cache_lookup(&newer, 1).is_some());
        CACHE.lock().unwrap().as_mut().unwrap().at = Instant::now() - Duration::from_secs(59);
        assert!(cache_snapshot().is_some());
        assert!(cache_remaining_seconds() <= 1);
        let at = CACHE.lock().unwrap().as_ref().unwrap().at;
        assert!(cache_snapshot().is_some());
        assert_eq!(CACHE.lock().unwrap().as_ref().unwrap().at, at);
        cache_invalidate();
        assert!(cache_snapshot().is_none());
        assert!(cache_lookup(&newer, 1).is_none());
    }

    #[test]
    fn snapshot_is_consumed_once_across_threads_without_desktop_access() {
        let _interaction = INTERACTION.lock().unwrap();
        let els = finalize(vec![raw("保存", [0, 0, 100, 40])], vs(), 1);
        let snapshot = cache_store(&els);
        let barrier = std::sync::Barrier::new(8);
        let successes = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8).map(|_| scope.spawn(|| {
                barrier.wait();
                usize::from(cache_consume(&snapshot, 1).is_some())
            })).collect();
            handles.into_iter().map(|h| h.join().unwrap()).sum::<usize>()
        });
        assert_eq!(successes, 1);
        assert!(cache_snapshot().is_none());
        assert!(validate_target(&els[0]).is_err());
    }

    #[test]
    fn stale_ambiguous_and_changed_targets_fail_closed() {
        let mut el = finalize(vec![raw("保存", [10, 10, 100, 40])], vs(), 10).remove(0);
        el.identity = ElementIdentity { hwnd: 42, process_id: 7, process_started: 100,
            window_process_id: 7, window_process_started: 100, window_runtime_id: vec![4, 5], runtime_id: vec![1, 2, 3] };
        let mut cached = Cached { at: Instant::now(), id: "snapshot".into(), els: vec![el.clone()] };
        assert!(lookup(&cached, "snapshot", 1).is_some());
        assert!(lookup(&cached, "", 1).is_none());
        assert!(lookup(&cached, "old", 1).is_none());
        cached.at = Instant::now() - CACHE_TTL - Duration::from_secs(1);
        assert!(lookup(&cached, "snapshot", 1).is_none());
        cached.at = Instant::now();
        cached.els.push(el.clone());
        assert!(lookup(&cached, "snapshot", 1).is_none());
        assert!(same_target(&el, &el, true, false));
        assert!(!same_target(&el, &el, false, false));
        assert!(!same_target(&el, &el, true, true));
        let mut changed = el.clone();
        changed.rect[0] += 1;
        assert!(!same_target(&el, &changed, true, false));
        changed = el.clone(); changed.identity.hwnd += 1;
        assert!(!same_target(&el, &changed, true, false));
        changed = el.clone(); changed.identity.window_runtime_id.push(9);
        assert!(!same_target(&el, &changed, true, false));
        changed = el.clone(); changed.identity.process_started += 1;
        assert!(!same_target(&el, &changed, true, false));
        changed = el.clone(); changed.identity.window_process_started += 1;
        assert!(!same_target(&el, &changed, true, false));
        changed = el.clone(); changed.identity.runtime_id.push(9);
        assert!(!same_target(&el, &changed, true, false));
        changed = el.clone(); changed.name = "删除".into();
        assert!(!same_target(&el, &changed, true, false));
        changed = el.clone(); changed.identity.runtime_id.clear();
        assert!(!same_target(&changed, &changed, true, false));
    }

    #[test]
    fn local_query_filters_before_result_budget_and_handles_negative_coordinates() {
        let query = Query { keyword: "ä保", window_id: Some(42), region: Some([-1920, 0, 200, 200]), ..Default::default() };
        assert!(query.matches("Ä保存", [-1900, 10, 100, 40]));
        assert!(!query.matches("Ä保存", [100, 10, 100, 40]));
        assert!(!query.matches("删除", [-1900, 10, 100, 40]));
        assert!(intersects([i32::MAX - 1, 0, 100, 1], [i32::MAX, 0, 1, 1]));
        assert!(!intersects([0, 0, 10, 10], [10, 0, 10, 10]));
    }

    #[test]
    fn labels_do_not_change_raw_identity_and_unlabeled_actions_survive() {
        let full = format!("  {}尾部", "中".repeat(120));
        let mut source = raw(&full, [0, 0, 100, 40]);
        source.identity = ElementIdentity { hwnd: 42, process_id: 7, process_started: 100,
            window_process_id: 7, window_process_started: 100, window_runtime_id: vec![4], runtime_id: vec![1] };
        let expected = finalize(vec![source], vs(), 1).remove(0);
        assert_eq!(expected.name, full);
        assert_eq!(expected.label.chars().count(), LABEL_CHARS);
        let mut current = expected.clone();
        current.label = "不同显示文字".into(); current.label_source = "child_text";
        assert!(same_target(&expected, &current, true, false));
        current.name.push('变');
        assert!(!same_target(&expected, &current, true, false));
        let mut unnamed = raw("", [0, 0, 10, 10]);
        unnamed.actionable = true; unnamed.label_source = "unlabeled";
        let result = finalize(vec![unnamed], vs(), 1);
        assert_eq!(result.len(), 1);
        assert!(result[0].label.is_empty());
        let mut executable = expected.clone(); executable.actionable = true;
        assert!(executable.non_executable_reason().is_none());
        executable.foreground = false;
        assert_eq!(executable.non_executable_reason(), Some("background_window"));
        assert!(validate_target(&executable).is_err());
    }

    #[test]
    fn label_limits_exclude_password_input_and_nested_actions() {
        assert!(label_node_allowed("Text", false, false, false, 2, 16));
        for (role, action, focus, password, depth, visits) in [
            ("Text", false, false, true, 1, 1), ("Edit", false, false, false, 1, 1),
            ("Button", false, false, false, 1, 1), ("Text", true, false, false, 1, 1),
            ("Text", false, true, false, 1, 1), ("Text", false, false, false, 3, 1),
            ("Text", false, false, false, 1, 17),
        ] { assert!(!label_node_allowed(role, action, focus, password, depth, visits)); }
        let mut pieces = Vec::new();
        for text in ["甲", "乙", "丙", "丁"] { add_label_piece(&mut pieces, text); }
        assert_eq!(pieces.len(), 3);
        assert_eq!(display_label(&"中".repeat(200)).chars().count(), 96);
        assert!(!passive_content("Text", false, false, true));
        assert!(passive_content("Image", false, false, false));
        assert!(!passive_content("Unknown", false, false, false));
        assert!(!passive_content("Text", true, false, false));
        assert!(!passive_content("Image", false, true, false));
        assert!(!label_node_allowed("Unknown", false, false, false, 1, 1));
        assert!(!label_node_allowed("Pane", true, false, false, 1, 1));
    }

    #[test]
    fn partial_offscreen_candidates_use_visible_pixels_not_original_center_or_gaps() {
        let rect = [-90, 10, 100, 40];
        let monitors = [[0, 0, 100, 100], [200, 0, 100, 100]];
        let points = candidate_points(rect, Some((-40, 30)), &monitors);
        assert!(!points.is_empty());
        assert!(points.iter().all(|p| point_in(rect, *p) && point_in(monitors[0], *p)));
        assert!(candidate_points([110, 10, 50, 40], Some((120, 20)), &monitors).is_empty());
        let points = candidate_points([0, 0, 300, 100], Some((150, 50)), &monitors);
        assert!(points.iter().all(|p| monitors.iter().any(|m| point_in(*m, *p))));
        assert_eq!(rect, [-90, 10, 100, 40]);
    }

    #[test]
    fn text_ownership_requires_exact_ancestor_and_rejects_nested_button() {
        let mut expected = finalize(vec![raw("保存", [0, 0, 100, 40])], vs(), 1).remove(0);
        expected.identity = ElementIdentity { hwnd: 42, process_id: 7, process_started: 100,
            window_process_id: 7, window_process_started: 100, window_runtime_id: vec![4], runtime_id: vec![1] };
        let mut text = expected.clone(); text.role = "Text"; text.identity.runtime_id = vec![2];
        assert!(!same_target(&expected, &text, true, false));
        assert!(may_ascend_hit(&expected, &text, passive_content("Text", false, false, false), 0, true, false));
        assert!(same_target(&expected, &expected, true, false));
        assert!(!may_ascend_hit(&expected, &text, passive_content("Button", true, false, false), 0, true, false));
        assert!(!may_ascend_hit(&expected, &text, true, HIT_ANCESTORS, true, false));
        assert!(!may_ascend_hit(&expected, &text, true, 0, false, false));
        assert!(!may_ascend_hit(&expected, &text, true, 0, true, true));
        let mut sibling = expected.clone(); sibling.identity.runtime_id = vec![9];
        assert!(!same_target(&expected, &sibling, true, false));
        text.identity.hwnd += 1;
        assert!(!may_ascend_hit(&expected, &text, true, 0, true, false));
    }

    #[test]
    fn passwords_never_read_their_own_name() {
        let name = protected_name::<()>(true, || panic!("password Name must not be read"));
        assert_eq!(name.unwrap(), "");
        assert_eq!(protected_name::<()>(false, || Ok("  原始名称  ".into())).unwrap(), "  原始名称  ");
        assert!(protected_name(false, || Err::<String, _>("provider failure")).is_err());
    }

    #[test]
    fn candidate_failures_do_not_bypass_occlusion_or_abort_other_candidates() {
        let monitors = [[0, 0, 100, 100]];
        let points = candidate_points([0, 0, 100, 100], Some((10, 10)), &monitors);
        let mut visited = Vec::new();
        let selected = verified_candidate(points.clone(), |point| {
            visited.push(point);
            match visited.len() {
                1 => Err("provider failure"),
                2 => Ok(false), // 遮挡不能退回几何坐标。
                _ => Ok(true),
            }
        });
        assert_eq!(visited, points[..3]);
        assert_eq!(selected, Some(points[2]));
        assert_eq!(verified_candidate(points, |_| Ok::<_, ()>(false)), None);
        let fallback = candidate_points([0, 0, 100, 100], None, &monitors);
        assert!(!fallback.is_empty());
        assert_eq!(verified_candidate(fallback, |_| Err::<bool, _>("unknown")), None);
    }

    #[test]
    fn monitor_gaps_do_not_consume_visible_element_budget() {
        let monitors = [[-100, 0, 100, 100], [100, 0, 100, 100]];
        let found: Vec<_> = [[10, 10, 50, 50], [-10, 10, 30, 30], [110, 10, 30, 30]].into_iter()
            .filter(|rect| visible_on_monitors(*rect, &monitors)).take(1).collect();
        assert_eq!(found, vec![[-10, 10, 30, 30]]);
        assert!(!visible_on_monitors([0, 0, 100, 100], &monitors));
        assert!(!visible_on_monitors([0, 0, 1, 1], &[]));
    }

    #[test]
    fn role_prefilter_spends_budget_only_on_normalized_roles() {
        assert_eq!(parse_role(" button ").unwrap(), Some("Button"));
        assert_eq!(parse_role(" custom ").unwrap(), Some("Custom"));
        assert!(parse_role("Buttons").is_err());
        let query = Query { role: parse_role("button").unwrap(), ..Default::default() };
        let found: Vec<_> = ["Edit", "Edit", "Button", "Button"].into_iter()
            .filter(|r| query.matches_role(r)).take(1).collect();
        assert_eq!(found, vec!["Button"]);
    }

    /// 只读的 Windows UIA 烟雾探针；默认忽略，避免无头 CI 被桌面会话影响。
    #[test]
    #[ignore = "诊断用：枚举当前桌面的 UIA 元素"]
    fn enumerate_live_desktop_without_panicking() {
        let els = enumerate_mode(None, true, 600, 800).expect("UIA 全桌面枚举应成功");
        assert!(!els.is_empty(), "真实桌面应该至少有一个可交互元素");
        assert!(els.len() <= 800);
        assert!(els.iter().enumerate().all(|(i, e)| e.id == i + 1));
        println!(
            "UIA 全桌面枚举到 {} 个元素，首项：{:?}",
            els.len(),
            els.first()
        );

        let focused = enumerate_mode(None, false, 600, 800).expect("UIA 焦点窗口枚举应成功");
        assert!(!focused.is_empty(), "焦点窗口应该至少有一个可交互元素");
        let title = &focused[0].window;
        assert!(
            focused.iter().all(|e| &e.window == title),
            "焦点模式不应混入其他顶层窗口"
        );
        println!("焦点窗口「{title}」枚举到 {} 个元素", focused.len());
    }

    /// 编号必须从 1 连续递增 —— 模型把它当一个列表引用，断号会让人怀疑丢数据。
    #[test]
    fn ids_are_contiguous_from_one() {
        let raws: Vec<RawElement> = (0..7)
            .map(|i| raw(&format!("x{i}"), [0, i * 50, 100, 40]))
            .collect();
        let els = finalize(raws, vs(), 800);
        for (i, e) in els.iter().enumerate() {
            assert_eq!(e.id, i + 1);
        }
    }
}
