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
//!    所以有预算制（每窗 ≤600、总 ≤800、8 秒硬上限）。
//!
//! 顶层窗口的"隐藏"判定用 DWM `DWMWA_CLOAKED`（被虚拟桌面收起的窗口），
//! 比 UIA 自己的 `IsOffscreen` 可靠。

use std::time::{Duration, Instant};

use crate::result::ToolError;

/// 枚举出的一个可交互元素。
///
/// `id` 是给模型引用的编号（SoM：模型答「点击 7」，不用算坐标）；
/// `rect` 是 `[x, y, w, h]`，虚拟桌面物理像素。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScreenElement {
    pub id: usize,
    pub role: &'static str,
    pub name: String,
    /// 元素属于哪个顶层窗口（标题）。
    pub window: String,
    pub rect: [i32; 4],
}

impl ScreenElement {
    /// 中心点（`click` 的 `element_id` 就点这里）。
    pub fn center(&self) -> (i32, i32) {
        (
            self.rect[0] + self.rect[2] / 2,
            self.rect[1] + self.rect[3] / 2,
        )
    }
}

/// 过滤/去重前的原始元素（枚举线程收集）。
pub struct RawElement {
    pub role: &'static str,
    pub name: String,
    pub window: String,
    pub rect: [i32; 4],
    /// 父元素的 ControlType id（滚动条子按钮靠它过滤；`None` = 顶层）。
    pub parent_role: Option<ControlTypeId>,
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
/// - 丢无名元素（实测 96% 的可交互元素有名字，无名的几乎全是噪音）
/// - 丢滚动条步进按钮（父元素是 ScrollBar，或名字特征命中）
/// - 去重（同窗口同名同 bbox 的只留一个）
/// - 按 **上 → 下、左 → 右** 排序后编号 —— 与人的阅读顺序一致，
///   模型看截图时能对上「编号 7 在编号 6 右边」这类空间关系
pub fn finalize(
    raw: Vec<RawElement>,
    vs: super::screen::Rect,
    budget: usize,
) -> Vec<ScreenElement> {
    let mut seen: Vec<(String, String, [i32; 4])> = Vec::new();
    let mut els: Vec<ScreenElement> = Vec::new();
    for e in raw {
        let [x, y, w, h] = e.rect;
        // 与虚拟桌面相交（部分露出的也算 —— 被裁掉一角的按钮仍然可点）
        let intersects =
            x + w > vs.x && y + h > vs.y && x < vs.x + vs.width && y < vs.y + vs.height;
        if !intersects || w <= 0 || h <= 0 {
            continue;
        }
        let name = e.name.trim().to_string();
        if name.is_empty() || is_scrollbar_noise(&name) {
            continue;
        }
        if e.parent_role == Some(SCROLLBAR) {
            continue;
        }
        // 去重
        let key = (e.window.clone(), name.clone(), e.rect);
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        els.push(ScreenElement {
            id: 0, // 排序后统一编
            role: e.role,
            name,
            window: e.window,
            rect: e.rect,
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
    els: std::sync::Arc<Vec<ScreenElement>>,
}

/// 枚举结果的保鲜期。模型通常在枚举后的几秒内就会点，
/// 5 分钟足够覆盖「看结果 → 思考 → 行动」的一轮；过期就要求重新枚举。
const CACHE_TTL: Duration = Duration::from_secs(5 * 60);

/// 存下这次枚举的结果（`screen_elements` 每次跑完都调）。
pub fn cache_store(els: &[ScreenElement]) {
    let arc: std::sync::Arc<Vec<ScreenElement>> = std::sync::Arc::new(els.to_vec());
    let mut cell = CACHE.lock().unwrap();
    *cell = Some(Cached {
        at: Instant::now(),
        els: arc,
    });
}

/// 返回最近一次枚举的完整缓存快照，供搜索工具只读查询。
pub fn cache_snapshot() -> Option<Vec<ScreenElement>> {
    let cell = CACHE.lock().unwrap();
    let cached = cell.as_ref()?;
    if cached.at.elapsed() > CACHE_TTL {
        return None;
    }
    Some((*cached.els).clone())
}

/// 按编号查上次枚举的元素。过期（> [`CACHE_TTL`]）或没枚举过返回 `None`。
pub fn cache_lookup(id: usize) -> Option<ScreenElement> {
    let cell = CACHE.lock().unwrap();
    let cached = cell.as_ref()?;
    if cached.at.elapsed() > CACHE_TTL {
        return None;
    }
    cached.els.iter().find(|e| e.id == id).cloned()
}

// ==== 真正的枚举 ==========================================================

/// 枚举元素；默认可限定到当前焦点窗口，`all_windows=true` 时遍历整个桌面。
pub fn enumerate_mode(
    window_filter: Option<&str>,
    all_windows: bool,
    per_window_budget: usize,
    total_budget: usize,
) -> Result<Vec<ScreenElement>, ToolError> {
    super::screen::ensure_dpi_aware();
    let raw = imp::collect(per_window_budget, total_budget, all_windows).map_err(|e| {
        ToolError::io(format!("Windows UI Automation 枚举失败：{e}"))
            .with_hint("刚切换窗口时 UIA 树可能正在重建；等一秒后重试")
    })?;
    let mut els = finalize(raw, super::screen::virtual_screen(), total_budget);
    if let Some(f) = window_filter {
        let f = f.to_ascii_lowercase();
        els.retain(|e| e.window.to_ascii_lowercase().contains(&f));
        for (i, e) in els.iter_mut().enumerate() {
            e.id = i + 1;
        }
    }
    Ok(els)
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

    use super::{ControlTypeId, RawElement};

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
            _ => return None,
        })
    }

    /// 单窗口 DFS 的时间上限。个别应用的 UIA 树响应极慢（几十 ms 一个节点），
    /// 没有这个上限一个坏窗口就能把整次枚举拖到分钟级。
    const WINDOW_TIME_LIMIT: Duration = Duration::from_secs(4);
    /// 整次枚举的硬上限（`screen_elements` 是同步工具，最坏情况要兜住）。
    const TOTAL_TIME_LIMIT: Duration = Duration::from_secs(8);

    /// 枚举所有可见顶层窗口。返回原始元素（过滤交给 [`super::finalize`]）。
    pub fn collect(
        per_window: usize,
        total: usize,
        all_windows: bool,
    ) -> WinResult<Vec<RawElement>> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
            let result = collect_inner(per_window, total, all_windows);
            CoUninitialize();
            result
        }
    }

    fn collect_inner(
        per_window: usize,
        total: usize,
        all_windows: bool,
    ) -> WinResult<Vec<RawElement>> {
        unsafe {
            let uia: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)?;
            let walker: IUIAutomationTreeWalker = uia.ControlViewWalker()?;
            let root = uia.GetRootElement()?;

            let t0 = Instant::now();
            let mut raw: Vec<RawElement> = Vec::new();

            if !all_windows {
                let focused = uia.GetFocusedElement()?;
                let mut top = focused;
                while let Ok(parent) = walker.GetParentElement(&top) {
                    if walker.GetParentElement(&parent).is_err() {
                        break; // parent 是 UIA 根；top 就是当前焦点所属的顶层窗口
                    }
                    top = parent;
                }
                let title = top.CurrentName().map(|s| s.to_string()).unwrap_or_default();
                dfs(&walker, &top, title, per_window, &mut raw, Instant::now());
                return Ok(raw);
            }

            // 全桌面模式：桌面的直接子节点 = 顶层窗口。逐窗口 DFS，
            let mut top = walker.GetFirstChildElement(&root)?;
            loop {
                if raw.len() >= total || t0.elapsed() > TOTAL_TIME_LIMIT {
                    break;
                }
                let title = top.CurrentName().map(|s| s.to_string()).unwrap_or_default();
                if !is_cloaked(&top) {
                    // 桌面（Progman/WorkerW）与任务栏照常枚举：桌面图标是真实可点的。
                    dfs(
                        &walker,
                        &top,
                        title.clone(),
                        per_window,
                        &mut raw,
                        Instant::now(),
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
    ) {
        unsafe {
            let mut stack: Vec<(IUIAutomationElement, ControlTypeId, usize)> =
                vec![(window_el.clone(), 0, 0)];
            let mut count = 0usize;
            while let Some((el, parent_type, depth)) = stack.pop() {
                if count >= budget || out.len() >= 10_000 || t0.elapsed() > WINDOW_TIME_LIMIT {
                    return;
                }
                let ctype = el.CurrentControlType().map(|t| t.0).unwrap_or(0);
                if let Some(role) = role_of(ctype) {
                    if let Ok(r) = el.CurrentBoundingRectangle() {
                        let name = el.CurrentName().map(|s| s.to_string()).unwrap_or_default();
                        out.push(RawElement {
                            role,
                            name,
                            window: window.clone(),
                            rect: [r.left, r.top, r.right - r.left, r.bottom - r.top],
                            parent_role: if parent_type == 0 {
                                None
                            } else {
                                Some(parent_type)
                            },
                        });
                        count += 1;
                    }
                }
                if depth < 14 {
                    // 栈是 LIFO，孩子的入栈顺序无所谓 —— finalize 会按空间重排。
                    // guard 防御个别控件 GetNextSibling 自环（实测见过这类坏控件）。
                    let mut child = walker.GetFirstChildElement(&el).ok();
                    let mut guard = 0usize;
                    while let Some(c) = child {
                        guard += 1;
                        if guard > 2000 {
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

    /// 非 Windows 平台：屏幕交互工具全部返回 unsupported（与 screen.rs 一致）。
    pub fn collect(
        _per_window: usize,
        _total: usize,
        _all_windows: bool,
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
            name: name.to_owned(),
            window: "窗口".to_owned(),
            rect,
            parent_role: None,
        }
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
        }];
        cache_store(&els);
        let got = cache_lookup(1).expect("刚存过就应当查得到");
        assert_eq!(got.name, "开始");
        assert_eq!(got.center(), (1106, 1952));
        assert!(cache_lookup(99).is_none(), "没存过的编号查不到");
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
