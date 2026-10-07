//! UIA 树枚举 —— 屏幕上所有可交互元素（`screen_elements` 工具的实现，研究见
//! `docs-pri/screen-elements-research.md`）。
//!
//! 三条来之不易的约定（都是本机实测踩出来的）：
//!
//! 1. **不在祖先节点上用 `IsOffscreen` 剪枝**。Chromium 对这个属性的报告不可靠
//!    （窗口非激活时整棵树被判 offscreen），第一次原型就把 WorkBuddy 整个窗口弄丢了。
//!    可见性过滤放到**叶子级**：用 bbox 与虚拟桌面求交判断（见 [`finalize`]）。
//! 2. **坐标是物理像素**（与 `screenshot`/`click`/`drag` 同一坐标系）——
//!    枚举和核验入口用线程级 `PER_MONITOR_AWARE_V2` guard（见 [`super::screen`]）。
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
    let result = imp::validate(element)?;
    if started.elapsed() > Duration::from_secs(2) {
        return Err(ToolError::new(crate::result::ErrorKind::Timeout, "[validation_timeout] 即时核验耗时过长，已拒绝输入")
            .with_hint("重新运行 screen_elements；不要使用旧坐标"));
    }
    Ok(result)
}

fn target_mismatch(expected: &ScreenElement, current: &ScreenElement, enabled: bool, offscreen: bool) -> Option<&'static str> {
    let (a, b) = (&expected.identity, &current.identity);
    if !expected.identity_verifiable() || !current.identity_verifiable() { Some("unverifiable_identity") }
    else if !enabled { Some("element_disabled") }
    else if offscreen { Some("element_offscreen") }
    else if a.hwnd != b.hwnd { Some("window_handle_changed") }
    else if a.process_id != b.process_id || a.process_started != b.process_started { Some("element_process_changed") }
    else if a.window_process_id != b.window_process_id || a.window_process_started != b.window_process_started { Some("window_process_changed") }
    else if a.window_runtime_id != b.window_runtime_id { Some("window_runtime_id_changed") }
    else if a.runtime_id != b.runtime_id { Some("element_runtime_id_mismatch") }
    else if expected.rect != current.rect { Some("element_rect_changed") }
    else if expected.role != current.role { Some("element_role_changed") }
    else if expected.name != current.name { Some("element_name_changed") }
    else if expected.window != current.window { Some("window_title_changed") }
    else { None }
}

fn same_target(expected: &ScreenElement, current: &ScreenElement, enabled: bool, offscreen: bool) -> bool {
    target_mismatch(expected, current, enabled, offscreen).is_none()
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

// 保留失败证据，但单点 provider 故障仍允许尝试其它经过完整核验的候选点。
fn verified_candidate<E>(points: Vec<(i32, i32)>, mut verify: impl FnMut((i32, i32)) -> Result<(), E>) -> Result<(i32, i32), Vec<E>> {
    let mut failures = Vec::new();
    for point in points {
        match verify(point) {
            Ok(()) => return Ok(point),
            Err(error) => failures.push(error),
        }
    }
    Err(failures)
}

#[cfg(any(windows, test))]
#[derive(Clone, Debug)]
struct ValidationFailure {
    code: &'static str,
    kind: crate::result::ErrorKind,
    detail: String,
}

#[cfg(any(windows, test))]
impl ValidationFailure {
    fn rejected(code: &'static str) -> Self {
        Self { code, kind: crate::result::ErrorKind::Conflict, detail: String::new() }
    }

    fn provider(stage: &'static str, hresult: u32) -> Self {
        use crate::result::ErrorKind;
        let (code, kind) = match hresult {
            0x80070005 => ("access_denied", ErrorKind::NotAllowed),
            0x80040201 => ("element_unavailable", ErrorKind::NotFound),
            0x80131505 | 0x800705B4 => ("provider_timeout", ErrorKind::Timeout),
            _ => ("provider_error", ErrorKind::Io),
        };
        Self { code, kind, detail: format!("stage={stage}, HRESULT=0x{hresult:08X}") }
    }

    fn timeout() -> Self {
        Self { code: "validation_timeout", kind: crate::result::ErrorKind::Timeout, detail: String::new() }
    }
}

#[cfg(any(windows, test))]
fn validation_error(mut failures: Vec<ValidationFailure>) -> ToolError {
    use crate::result::ErrorKind;
    // 高优先级故障先展示，避免诊断条数上限把权限/超时证据挤掉；同级保持原序。
    let priority = |kind| match kind {
        ErrorKind::NotAllowed => 5, ErrorKind::Timeout => 4, ErrorKind::Io => 3,
        ErrorKind::NotFound => 2, _ => 1,
    };
    failures.sort_by_key(|f| std::cmp::Reverse(priority(f.kind)));
    let kind = failures.first().map(|f| f.kind).unwrap_or(ErrorKind::Conflict);
    let mut reasons = Vec::new();
    for failure in &failures {
        if reasons.iter().any(|(prior, _): &(&ValidationFailure, String)|
            prior.code == failure.code && prior.kind == failure.kind && prior.detail == failure.detail) { continue; }
        if reasons.len() == 8 { break; }
        reasons.push((failure, if failure.detail.is_empty() { failure.code.to_owned() }
            else { format!("{} ({})", failure.code, failure.detail) }));
    }
    let details = if reasons.is_empty() { "no_visible_candidate".to_owned() }
        else { reasons.into_iter().map(|(_, text)| text).collect::<Vec<_>>().join("; ") };
    ToolError::new(kind, format!("UIA 即时核验拒绝输入：{details}；失败候选/检查数={}", failures.len()))
        .with_hint("确认前台窗口及遮挡后重新运行 screen_elements；若新快照仍失败，请保留上述分类和 HRESULT 排查 provider/权限。不会自动激活窗口、调用 Invoke 或退回坐标")
}

fn passive_content(role: &str, action: bool, focusable: bool, password: bool) -> bool {
    matches!(role, "Text" | "Image") && !action && !focusable && !password
}

#[cfg(any(windows, test))]
fn passive_content_result<E>(role: &str, action: impl FnOnce() -> Result<bool, E>, properties: impl FnOnce() -> Result<(bool, bool), E>) -> Result<bool, E> {
    if !matches!(role, "Text" | "Image") { return Ok(false); }
    let action = action()?;
    let (focusable, password) = properties()?;
    Ok(passive_content(role, action, focusable, password))
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

    use windows::core::{Interface, IUnknown, Result as WinResult};
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

    fn current_pattern(el: &IUIAutomationElement, id: windows::Win32::UI::Accessibility::UIA_PATTERN_ID) -> WinResult<Option<IUnknown>> {
        // windows 0.62.2 GetCurrentPattern uses Type::from_abi: S_OK + null becomes
        // Error::empty() (code 0), losing the distinction from an actual COM failure.
        // UIA's GetPatternProvider contract allows null for unsupported patterns:
        // https://learn.microsoft.com/windows/win32/api/uiautomationcore/nf-uiautomationcore-irawelementprovidersimple-getpatternprovider
        unsafe {
            let mut result = core::ptr::null_mut();
            let hr = (el.vtable().GetCurrentPattern)(el.as_raw(), id, &mut result);
            // Adopt the returned reference exactly once, including a valid non-null
            // output on failure. RAII releases it on every early return; never invoke it.
            let pattern = if result.is_null() { None } else { Some(IUnknown::from_raw(result)) };
            hr.ok()?;
            Ok(pattern)
        }
    }

    fn action_pattern_result(el: &IUIAutomationElement) -> Result<bool, ValidationFailure> {
        use windows::Win32::UI::Accessibility::{UIA_InvokePatternId, UIA_TogglePatternId, UIA_SelectionItemPatternId, UIA_ExpandCollapsePatternId};
        for (id, stage) in [
            (UIA_InvokePatternId, "GetCurrentPattern(Invoke)"),
            (UIA_TogglePatternId, "GetCurrentPattern(Toggle)"),
            (UIA_SelectionItemPatternId, "GetCurrentPattern(SelectionItem)"),
            (UIA_ExpandCollapsePatternId, "GetCurrentPattern(ExpandCollapse)"),
        ] {
            match current_pattern(el, id) {
                Ok(Some(_)) => return Ok(true), // 仅查询接口，绝不调用 Invoke/Toggle。
                Ok(None) => {},
                Err(e) if matches!(e.code().0 as u32, 0x80004002 | 0x80040204) => {},
                // E_POINTER is a real failure, not evidence of an absent pattern.
                // Do not let a later supported pattern hide permission/timeout errors.
                Err(e) => return Err(ValidationFailure::provider(stage, e.code().0 as u32)),
            }
        }
        Ok(false)
    }

    fn action_pattern_state(el: &IUIAutomationElement) -> Option<bool> { action_pattern_result(el).ok() }

    fn action_pattern(el: &IUIAutomationElement) -> bool { action_pattern_state(el) == Some(true) }

    fn passive(el: &IUIAutomationElement) -> Result<bool, ValidationFailure> {
        unsafe {
            let role = match probe("passive_hit.CurrentControlType", el.CurrentControlType())?.0 { 50020 => "Text", 50006 => "Image", _ => "" };
            super::passive_content_result(role, || action_pattern_result(el), || {
                Ok((probe("passive_hit.CurrentIsKeyboardFocusable", el.CurrentIsKeyboardFocusable())?.as_bool(),
                    probe("passive_hit.CurrentIsPassword", el.CurrentIsPassword())?.as_bool()))
            })
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

    #[cfg(test)]
    mod pattern_tests {
        include!("screen_uia_pattern_tests.rs");
    }

    #[cfg(test)]
    mod titlebar_probe {
        include!("screen_uia_titlebar_probe_tests.rs");
    }

    use super::ValidationFailure;

    fn probe<T>(stage: &'static str, result: WinResult<T>) -> Result<T, ValidationFailure> {
        result.map_err(|e| ValidationFailure::provider(stage, e.code().0 as u32))
    }

    fn resolve_hit(uia: &IUIAutomation, walker: &IUIAutomationTreeWalker, expected: &ScreenElement, point: (i32, i32)) -> Result<IUIAutomationElement, ValidationFailure> {
        unsafe {
            let top = probe("ElementFromHandle", uia.ElementFromHandle(HWND(expected.identity.hwnd as _)))?;
            let top_id = probe("window.GetRuntimeId", runtime_id(&top))?;
            let mut el = probe("ElementFromPoint", uia.ElementFromPoint(windows::Win32::Foundation::POINT { x: point.0, y: point.1 }))?;
            for depth in 0..=super::HIT_ANCESTORS {
                if probe("CurrentIsPassword", el.CurrentIsPassword())?.as_bool() {
                    return Err(ValidationFailure::rejected("password_hit"));
                }
                let control_type = probe("CurrentControlType", el.CurrentControlType())?.0;
                let current = ScreenElement {
                    role: role_of(control_type).unwrap_or(""),
                    name: probe("CurrentName", el.CurrentName())?.to_string(),
                    window: probe("window.CurrentName", top.CurrentName())?.to_string(),
                    rect: probe("CurrentBoundingRectangle", el.CurrentBoundingRectangle().and_then(rect_values))?,
                    identity: probe("hit.identity", identity(&el, expected.identity.hwnd, top_id.clone()))?,
                    ..Default::default()
                };
                if !current.identity_verifiable() { return Err(ValidationFailure::rejected("unverifiable_identity")); }
                let enabled = probe("CurrentIsEnabled", el.CurrentIsEnabled())?.as_bool();
                let offscreen = probe("CurrentIsOffscreen", el.CurrentIsOffscreen())?.as_bool();
                let Some(mismatch) = super::target_mismatch(expected, &current, enabled, offscreen) else {
                    if action_pattern_result(&el)? { return Ok(el); }
                    if current.role == "Edit" && probe("CurrentIsKeyboardFocusable", el.CurrentIsKeyboardFocusable())?.as_bool() { return Ok(el); }
                    return Err(ValidationFailure::rejected("no_click_action"));
                };
                if !super::may_ascend_hit(expected, &current, passive(&el)?, depth, enabled, offscreen) {
                    let mut failure = ValidationFailure::rejected(mismatch);
                    // Control View 枚举不保证 ElementFromPoint 返回同一叶子。只报告 Raw 命中，
                    // 不把标题栏/窗口容器当作目标，也不按名称、矩形或 runtime ID 前缀放行。
                    // hwnd 是核验上下文，不是从命中元素推导出的原生 ancestor 证明。
                    failure.detail = format!("raw_hit_rejected, depth={depth}, control_type={control_type}, point={point:?}, expected_role={}, expected_runtime_id={:?} (len={}), hit_runtime_id={:?} (len={}), expected_rect={:?}, hit_rect={:?}",
                        expected.role,
                        &expected.identity.runtime_id[..expected.identity.runtime_id.len().min(8)], expected.identity.runtime_id.len(),
                        &current.identity.runtime_id[..current.identity.runtime_id.len().min(8)], current.identity.runtime_id.len(),
                        expected.rect, current.rect);
                    return Err(failure);
                }
                el = probe("RawViewWalker.GetParentElement", walker.GetParentElement(&el))?;
            }
            Err(ValidationFailure::rejected("hit_ancestor_limit"))
        }
    }

    fn actionable(el: &IUIAutomationElement, role: &str) -> bool {
        action_pattern(el) || (role == "Edit" && unsafe { el.CurrentIsKeyboardFocusable().map(|v| v.as_bool()).unwrap_or(false) })
    }

    pub fn validate(expected: &ScreenElement) -> Result<(i32, i32), ToolError> {
        let one = |failure| super::validation_error(vec![failure]);
        let started = Instant::now();
        let _com = probe("CoInitializeEx", ComGuard::new()).map_err(one)?;
        let setup = || -> Result<_, ValidationFailure> { unsafe {
            let uia: IUIAutomation = probe("CoCreateInstance", CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER))?;
            let walker = probe("RawViewWalker", uia.RawViewWalker())?;
            let hwnd = expected.identity.hwnd as windows_sys::Win32::Foundation::HWND;
            if hwnd.is_null() { return Err(ValidationFailure::rejected("invalid_window")); }
            if GetForegroundWindow() != hwnd { return Err(ValidationFailure::rejected("background_window")); }
            if IsWindowVisible(hwnd) == 0 { return Err(ValidationFailure::rejected("window_not_visible")); }
            if IsIconic(hwnd) != 0 { return Err(ValidationFailure::rejected("window_minimized")); }
            let mut cloaked = 0u32;
            probe("DwmGetWindowAttribute", DwmGetWindowAttribute(HWND(hwnd), DWMWA_CLOAKED, (&mut cloaked as *mut u32).cast(), 4))?;
            if cloaked != 0 { return Err(ValidationFailure::rejected("window_cloaked")); }
            Ok((uia, walker, hwnd, probe("EnumDisplayMonitors", monitors())?))
        }};
        let (uia, walker, hwnd, monitors) = setup().map_err(one)?;
        super::verified_candidate(super::candidate_points(expected.rect, expected.clickable_point, &monitors), |point| { unsafe {
            if started.elapsed() > Duration::from_secs(2) { return Err(ValidationFailure::timeout()); }
            let window_hit = || {
                if GetForegroundWindow() != hwnd { return Err(ValidationFailure::rejected("background_window")); }
                if GetAncestor(WindowFromPoint(windows_sys::Win32::Foundation::POINT { x: point.0, y: point.1 }), GA_ROOT) != hwnd {
                    return Err(ValidationFailure::rejected("native_window_hit_mismatch"));
                }
                Ok(())
            };
            window_hit()?;
            let el = resolve_hit(&uia, &walker, expected, point)?;
            let latest = resolve_hit(&uia, &walker, expected, point)?;
            if !probe("CompareElements", uia.CompareElements(&el, &latest))?.as_bool() {
                return Err(ValidationFailure::rejected("hit_changed_between_checks"));
            }
            window_hit()?;
            if started.elapsed() > Duration::from_secs(2) { return Err(ValidationFailure::timeout()); }
            use windows_sys::Win32::Graphics::Gdi::{MonitorFromPoint, MONITOR_DEFAULTTONULL};
            if MonitorFromPoint(windows_sys::Win32::Foundation::POINT { x: point.0, y: point.1 }, MONITOR_DEFAULTTONULL).is_null() {
                return Err(ValidationFailure::rejected("point_not_on_monitor"));
            }
            Ok(())
        }}).map_err(super::validation_error)
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
#[allow(clippy::too_many_arguments)] // Win32/UIA wrapper; grouping would obscure the API
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
#[path = "screen_uia_validation_tests.rs"]
mod validation_tests;

#[cfg(test)]
#[path = "screen_uia_tests.rs"]
mod tests;
