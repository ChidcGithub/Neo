//! 屏幕捕获与鼠标输入（Windows）。
//!
//! 三个屏幕工具（`screenshot` / `click` / `drag`）共用这一层，
//! 只做两件事：**把屏幕像素读出来**、**把鼠标事件发出去**。
//!
//! ## 坐标系：虚拟桌面的**物理像素**
//!
//! 一切都以**虚拟桌面**（所有显示器拼起来的那块大画布）的**物理像素**为准：
//!
//! ```text
//!         ┌─────────────┬─────────────┐
//!         │ 显示器 1     │ 显示器 2     │   ← 虚拟桌面 = 两者的并集
//!         │  (0,0)      │  (1920,0)   │
//!         └─────────────┴─────────────┘
//!         ↑ 原点在这里；有显示器排在左边/上边时坐标就是**负数**
//! ```
//!
//! PNG 图内原点始终是 (0,0)，不等于桌面原点；截图引用负责累加区域偏移，不做 DPI 乘除。
//!
//! ## DPI：为什么必须处理
//!
//! 进程如果是 **DPI 不感知**的，Windows 会把整个桌面按缩放比例"虚拟化"：
//! 一块 3840×2160、缩放 200% 的屏在进程眼里是 1920×1080，`BitBlt` 抓回来的是
//! **被拉伸过的糊图**，`SendInput` 也按虚拟坐标落点 —— 于是"照着截图点"必然点偏。
//!
//! 所以进这一层先 `SetProcessDpiAwarenessContext(PER_MONITOR_AWARE_V2)`，
//! 拿到真实物理像素。（`eframe`/`winit` 启动时通常已经设过，重复设置只返回
//! `FALSE`，无害。）顺带用 `GetDpiForMonitor` 把缩放比报给模型，
//! 排查"为什么点偏了"时有用。

use crate::result::{ErrorKind, ToolError};

/// 最近一次截屏开始的时间戳（毫秒，Unix epoch；0 = 还没截过）。
///
/// 上层（neo-app 的迷你窗）每帧读它：值一变就说明 AI 正在截屏，
/// 立刻把自己从屏幕上藏起来，避免被截进画面。置位放在这里而不是 app 侧，
/// 是因为 `capture()` 是 `screenshot` / `screen_elements` 的唯一入口，一处全覆盖。
pub static SCREENSHOT_AT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 最近一次**合成输入**（`SendInput` 注入的鼠标事件）完成的墙钟毫秒
/// （Unix epoch；0 = 还没注入过）。
///
/// `GetAsyncKeyState` 分不清真人点击与 AI 用 click/drag 注入的点击；
/// 上层的「用户点了屏幕 = 打断 AI」判定（neo-app 的 miniwin）读这个戳，
/// 把按下沿落在注入窗口期内的当作 AI 自己的动作忽略 ——
/// 否则 AI 操作鼠标时会自己把自己打断。
pub static SYNTHETIC_INPUT_AT: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// 当前墙钟毫秒（Unix epoch）。
pub(crate) fn epoch_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 最近一次截屏的区域（虚拟桌面物理像素）。
///
/// 与 [`SCREENSHOT_AT`] 的配对约定：**先写区域、再置位时间戳**；
/// 上层读到新时间戳后回来取，拿到的一定是本次的区域。
/// 区域截图的闪光动画靠它定位。
pub static SCREENSHOT_RECT: std::sync::Mutex<Option<Rect>> = std::sync::Mutex::new(None);

/// 置位 [`SCREENSHOT_AT`]（带上本次抓取的区域），返回当前毫秒时间戳。
fn mark_screenshot(rect: Rect) {
    if let Ok(mut slot) = SCREENSHOT_RECT.lock() {
        *slot = Some(rect);
    }
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    SCREENSHOT_AT.store(millis, std::sync::atomic::Ordering::Relaxed);
}

/// 屏幕上的一个矩形（虚拟桌面物理像素）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    /// 半开区间：右/下边界不属于本块（虚拟桌面最右一列是 `x + width - 1`）。
    pub fn contains(&self, x: i32, y: i32) -> bool {
        self.width > 0 && self.height > 0 && x >= self.x && y >= self.y
            && i64::from(x) < i64::from(self.x) + i64::from(self.width)
            && i64::from(y) < i64::from(self.y) + i64::from(self.height)
    }

    /// 越界的说明 —— **带上合法范围**，模型据此自己改，不用再问。
    pub fn outside_error(&self, what: &str, x: i32, y: i32) -> ToolError {
        ToolError::bad_args(format!(
            "{what} ({x}, {y}) 不在屏幕内：虚拟桌面 x {}…{}、y {}…{}",
            self.x,
            i64::from(self.x) + i64::from(self.width) - 1,
            self.y,
            i64::from(self.y) + i64::from(self.height) - 1
        ))
        .with_hint("使用 screenshot 返回的 screenshot_id 加图内 x/y；无引用时必须给桌面物理坐标")
    }
}

/// 显示器矩形的包围盒；不把包围盒中的空洞当作显示器。
pub fn monitor_bounds(monitors: &[Rect]) -> Result<Rect, ToolError> {
    if monitors.is_empty() || monitors.iter().any(|r| r.width <= 0 || r.height <= 0) {
        return Err(ToolError::io("没有有效的显示器拓扑"));
    }
    let x = monitors.iter().map(|r| r.x).min().unwrap();
    let y = monitors.iter().map(|r| r.y).min().unwrap();
    let right = monitors.iter().map(|r| i64::from(r.x) + i64::from(r.width)).max().unwrap();
    let bottom = monitors.iter().map(|r| i64::from(r.y) + i64::from(r.height)).max().unwrap();
    let width = i32::try_from(right - i64::from(x)).map_err(|_| ToolError::bad_args("桌面宽度溢出"))?;
    let height = i32::try_from(bottom - i64::from(y)).map_err(|_| ToolError::bad_args("桌面高度溢出"))?;
    Ok(Rect { x, y, width, height })
}

pub fn require_monitor_point(monitors: &[Rect], what: &str, x: i32, y: i32) -> Result<(), ToolError> {
    if monitors.iter().any(|r| r.contains(x, y)) { return Ok(()); }
    Err(ToolError::bad_args(format!("{what} ({x}, {y}) 不在屏幕内：位置未被任何实际显示器覆盖（可能是虚拟桌面空洞）"))
        .with_hint("使用最新 screenshot_id 和图内坐标，或优先使用 UIA 元素定位"))
}

/// 返回区域中真实显示器覆盖的面积，重叠显示器只算一次。
pub fn monitor_coverage(rect: Rect, monitors: &[Rect]) -> i64 {
    let mut clipped = Vec::new();
    for m in monitors {
        let left = i64::from(rect.x.max(m.x));
        let top = i64::from(rect.y.max(m.y));
        let right = (i64::from(rect.x) + i64::from(rect.width)).min(i64::from(m.x) + i64::from(m.width));
        let bottom = (i64::from(rect.y) + i64::from(rect.height)).min(i64::from(m.y) + i64::from(m.height));
        if right > left && bottom > top { clipped.push((left, top, right, bottom)); }
    }
    let mut xs: Vec<_> = clipped.iter().flat_map(|r| [r.0, r.2]).collect();
    xs.sort_unstable();
    xs.dedup();
    let mut area = 0;
    for band in xs.windows(2) {
        let mut ys: Vec<_> = clipped.iter().filter(|r| r.0 <= band[0] && r.2 >= band[1]).map(|r| (r.1, r.3)).collect();
        ys.sort_unstable();
        let mut end = i64::MIN;
        let mut height = 0;
        for (top, bottom) in ys {
            height += (bottom - top.max(end)).max(0);
            end = end.max(bottom);
        }
        area += (band[1] - band[0]) * height;
    }
    area
}

/// 选择 65536 个输入桶中落在目标像素区间内部的整数点，拒绝不可表达的超大桌面。
pub fn normalize_axis(pixel: i32, origin: i32, span: i32) -> Result<i32, ToolError> {
    let local = i64::from(pixel) - i64::from(origin);
    if !(1..=65536).contains(&span) || local < 0 || local >= i64::from(span) {
        return Err(ToolError::bad_args("输入坐标越界或桌面跨度超过 65536，无法精确表示，已拒绝而非夹取"));
    }
    let span = i64::from(span);
    let first = (local * 65536 + span - 1) / span;
    let last = ((local + 1) * 65536 + span - 1) / span - 1;
    Ok(((first + last) / 2) as i32)
}

fn to_absolute(x: i32, y: i32, vs: Rect) -> Result<(i32, i32), ToolError> {
    Ok((normalize_axis(x, vs.x, vs.width)?, normalize_axis(y, vs.y, vs.height)?))
}

/// 鼠标键。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
}

impl Button {
    /// 宽容解析：模型写 `left` / `Left` / `primary` 都认。
    pub fn parse(s: &str) -> Result<Self, ToolError> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "left" | "l" | "primary" => Ok(Button::Left),
            "right" | "r" | "secondary" => Ok(Button::Right),
            other => Err(ToolError::bad_args(format!(
                "button 只能是 left 或 right，收到 `{other}`"
            ))
            .with_hint("左键用 left（默认），右键用 right")),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Button::Left => "left",
            Button::Right => "right",
        }
    }
}

/// 一张截下来的位图（RGBA8，行优先，左上角为原点）。
pub struct Shot {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Shot {
    /// 编码成 PNG。
    ///
    /// 给模型看的必须是**压缩后**的字节：一张 1080p 截图的原始位图约 8 MB，
    /// PNG 后 2–4 MB，而官方对单张图有 32 MiB 上限、整包 48 MiB ——
    /// 原始位图一多就顶到天花板了。
    pub fn to_png(&self) -> Result<Vec<u8>, ToolError> {
        use image::codecs::png::PngEncoder;
        use image::{ExtendedColorType, ImageEncoder};

        let mut out = Vec::new();
        PngEncoder::new(&mut out)
            .write_image(
                &self.rgba,
                self.width,
                self.height,
                ExtendedColorType::Rgba8,
            )
            .map_err(|e| ToolError::io(format!("PNG 编码失败：{e}")))?;
        Ok(out)
    }
}

#[cfg(any(windows, test))]
fn capture_bytes(rect: Rect) -> Result<usize, ToolError> {
    if rect.width <= 0 || rect.height <= 0 {
        return Err(ToolError::bad_args("截屏区域必须是正的宽高"));
    }
    (rect.width as usize).checked_mul(rect.height as usize).and_then(|n| n.checked_mul(4))
        .filter(|n| *n <= isize::MAX as usize && *n <= u32::MAX as usize)
        .ok_or_else(|| ToolError::new(ErrorKind::TooLarge, "截图位图超出 GDI/内存切片尺寸限制"))
}

// #region debug-point B/C: bounded evidence, populated only by the opt-in click tool
#[derive(Default)]
pub(crate) struct ClickEvidence {
    before: Option<ClickPoint>,
    after: Option<ClickPoint>,
    primary: Option<SendEvidence>,
    release_cleanup: Option<SendEvidence>,
}

#[derive(serde::Serialize)]
struct ClickPoint {
    cursor: Option<(i32, i32)>,
    target: (i32, i32),
    target_own_pid: Option<bool>,
    target_class: TargetClass,
}

#[derive(serde::Serialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum TargetClass { NoWindow, Unknown, NeoOverlay, Other }

#[cfg(any(windows, test))]
fn classify_click_class(class: &[u16]) -> TargetClass {
    if class.iter().copied().eq("neo-overlay".encode_utf16()) {
        TargetClass::NeoOverlay
    } else {
        TargetClass::Other
    }
}

#[derive(serde::Serialize)]
struct SendEvidence {
    requested: usize,
    sent: u32,
    last_error_immediate: u32,
    elapsed_us: u128,
}

impl ClickEvidence {
    pub(super) fn append_events(self, events: &mut Vec<serde_json::Value>, run_id: &str) {
        use serde_json::json;
        if self.before.is_some() || self.after.is_some() {
            events.push(super::click::debug_envelope("C", "click_target_observations", json!({
                "before": self.before, "after": self.after,
                "window_from_point_is_observation_not_delivery_proof": true,
            }), run_id));
        }
        if self.primary.is_some() {
            events.push(super::click::debug_envelope("B", "send_input_completed", json!({
                "primary": self.primary, "existing_release_cleanup": self.release_cleanup,
                "accepted_does_not_prove_target_response": true,
                "target_response_verified": false,
                "last_error_may_be_stale_on_success": true,
                "last_error_does_not_identify_uipi": true,
            }), run_id));
        }
    }
}
// #endregion debug-point B/C

#[cfg(windows)]
mod imp {
    use super::*;
    use std::ffi::c_void;
    use std::time::Duration;

    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC,
        MonitorFromPoint, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, CAPTUREBLT,
        DIB_RGB_COLORS, MONITOR_DEFAULTTONEAREST, SRCCOPY, GdiFlush,
    };
    use windows_sys::Win32::UI::HiDpi::{
        GetDpiForMonitor, SetProcessDpiAwareness, SetProcessDpiAwarenessContext,
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, MDT_EFFECTIVE_DPI,
        PROCESS_PER_MONITOR_DPI_AWARE,
    };
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN,
        MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
        MOUSEEVENTF_VIRTUALDESK, MOUSEINPUT, MOUSE_EVENT_FLAGS,
    };
    // `GetSystemMetrics` 在 WindowsAndMessaging，不在 Gdi。
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN,
    };

    /// 进这一层之前调用一次即可（重复调用无害）。
    pub fn ensure_dpi_aware() {
        unsafe {
            // PER_MONITOR_AWARE_V2 需 Win10 1703+。返回 FALSE 有两种典型场景：
            // 系统太老（API 不存在时进程根本起不来，所以这里是"有但不认"），
            // 或宿主进程已经设置过 DPI 感知（此时再设必然 ERROR_ACCESS_DENIED，
            // 而已有的感知仍然有效）。两种情况下退回 Win8.1 就有的
            // Per-Monitor V1 都是无害的兜底：成了保住每显示器感知，败了说明早就有了。
            if SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) == 0 {
                SetProcessDpiAwareness(PROCESS_PER_MONITOR_DPI_AWARE);
            }
        }
    }

    pub struct DpiGuard {
        previous: windows_sys::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT,
        _thread: std::marker::PhantomData<std::rc::Rc<()>>,
    }

    impl Drop for DpiGuard {
        fn drop(&mut self) {
            unsafe { windows_sys::Win32::UI::HiDpi::SetThreadDpiAwarenessContext(self.previous); }
        }
    }

    /// 宿主可能已锁定为系统感知；使用线程上下文，结束后恢复，不改变宿主线程设置。
    pub fn physical_pixels() -> Result<DpiGuard, ToolError> {
        let previous = unsafe {
            windows_sys::Win32::UI::HiDpi::SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)
        };
        if previous.is_null() { return Err(ToolError::io("无法切换为物理像素 DPI 上下文，已拒绝桌面操作")); }
        Ok(DpiGuard { previous, _thread: std::marker::PhantomData })
    }

    /// 虚拟桌面（所有显示器的并集），物理像素。
    pub fn virtual_screen() -> Rect {
        let Ok(_dpi) = physical_pixels() else {
            return Rect { x: 0, y: 0, width: 0, height: 0 };
        };
        unsafe {
            Rect {
                x: GetSystemMetrics(SM_XVIRTUALSCREEN),
                y: GetSystemMetrics(SM_YVIRTUALSCREEN),
                width: GetSystemMetrics(SM_CXVIRTUALSCREEN),
                height: GetSystemMetrics(SM_CYVIRTUALSCREEN),
            }
        }
    }

    /// 枚举实际显示器，在线程物理像素上下文中读取并稳定排序。
    pub fn monitors() -> Result<Vec<Rect>, ToolError> {
        use windows_sys::Win32::Foundation::{LPARAM, RECT};
        use windows_sys::Win32::Graphics::Gdi::{EnumDisplayMonitors, HDC, HMONITOR};
        unsafe extern "system" fn collect(_monitor: HMONITOR, _dc: HDC, rect: *mut RECT, data: LPARAM) -> i32 {
            if rect.is_null() { return 0; }
            let r = unsafe { *rect };
            let (Ok(width), Ok(height)) = (i32::try_from(i64::from(r.right) - i64::from(r.left)), i32::try_from(i64::from(r.bottom) - i64::from(r.top))) else { return 0; };
            if width <= 0 || height <= 0 { return 0; }
            unsafe { &mut *(data as *mut Vec<Rect>) }.push(Rect { x: r.left, y: r.top, width, height });
            1
        }
        let _dpi = physical_pixels()?;
        let mut result = Vec::<Rect>::new();
        let ok = unsafe { EnumDisplayMonitors(std::ptr::null_mut(), std::ptr::null(), Some(collect), &mut result as *mut _ as LPARAM) };
        if ok == 0 || result.is_empty() { return Err(ToolError::io("无法枚举实际显示器，已拒绝桌面操作")); }
        result.sort_unstable_by_key(|r| (r.x, r.y, r.width, r.height));
        result.dedup();
        Ok(result)
    }

    /// 某个点所在显示器的缩放比（1.0 = 96 DPI）。取不到返回 `None`。
    pub fn dpi_scale_at(x: i32, y: i32) -> Option<f64> {
        unsafe {
            let monitor = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST);
            if monitor.is_null() {
                return None;
            }
            let mut dx: u32 = 96;
            let mut dy: u32 = 96;
            if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) != 0 {
                return None;
            }
            Some(f64::from(dx) / 96.0)
        }
    }

    /// 抓一块屏幕区域。
    pub fn capture(rect: Rect) -> Result<Shot, ToolError> {
        if rect.width <= 0 || rect.height <= 0 {
            return Err(ToolError::bad_args("截屏区域必须是正的宽高")
                .with_hint("width / height 是整数像素"));
        }
        // 先广播「要截屏了」再抓帧：迷你窗靠这个信号把自己藏起来，
        // 等它真正从屏幕上消失，画面里才不会带上 Neo 自己的窗口。
        // 迷你窗的视口回调以 ~16ms 自驱轮询这个信号，300ms 的余量够它
        // 走完「检测 → 隐藏 → DWM 重合成」一整圈。
        mark_screenshot(rect);
        std::thread::sleep(Duration::from_millis(300));
        capture_impl(rect)
    }

    /// 抓一块屏幕区域，但**不广播截屏信号、不等迷你窗躲开**。
    /// 课堂记录等静默后台截图用它：屏幕闪光与回避是给「AI 应用户要求截屏」
    /// 的反馈，后台监听截屏不该惊扰正在上课的屏幕。
    pub fn capture_silent(rect: Rect) -> Result<Shot, ToolError> {
        if rect.width <= 0 || rect.height <= 0 {
            return Err(ToolError::bad_args("截屏区域必须是正的宽高")
                .with_hint("width / height 是整数像素"));
        }
        capture_impl(rect)
    }

    /// 实际的 GDI 抓帧（`capture` / `capture_silent` 共用）。
    fn capture_impl(rect: Rect) -> Result<Shot, ToolError> {
        let _dpi = physical_pixels()?;
        let bytes = capture_bytes(rect)?;
        let displays = monitors()?;
        if monitor_coverage(rect, &displays) == 0 {
            return Err(ToolError::bad_args("截图区域没有任何实际显示器覆盖，已拒绝捕获空洞"));
        }
        // 分配失败必须在取得 GDI 资源之前返回，避免 Rust 提前退出泄漏句柄。
        let mut rgba = Vec::new();
        rgba.try_reserve_exact(bytes).map_err(|_| ToolError::io("截图缓冲区分配失败"))?;
        unsafe {
            let screen = GetDC(std::ptr::null_mut());
            if screen.is_null() {
                return Err(ToolError::new(
                    ErrorKind::Unsupported,
                    "拿不到屏幕设备上下文（GetDC 失败）—— 多半是没有可交互的桌面会话",
                )
                .with_hint("确认 Neo 跑在真实登录的桌面会话里（不是服务或无头会话）"));
            }
            let mem = CreateCompatibleDC(screen);
            if mem.is_null() {
                ReleaseDC(std::ptr::null_mut(), screen);
                return Err(ToolError::io("CreateCompatibleDC 失败"));
            }

            // 负高度 = 自上而下存行，读出来就是屏幕顺序，不用再翻转。
            let mut bi: BITMAPINFO = std::mem::zeroed();
            bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            bi.bmiHeader.biWidth = rect.width;
            bi.bmiHeader.biHeight = -rect.height;
            bi.bmiHeader.biPlanes = 1;
            bi.bmiHeader.biBitCount = 32;
            bi.bmiHeader.biCompression = 0; // BI_RGB

            let mut bits: *mut c_void = std::ptr::null_mut();
            let dib = CreateDIBSection(
                screen,
                &bi,
                DIB_RGB_COLORS,
                &mut bits,
                std::ptr::null_mut(),
                0,
            );
            if dib.is_null() || bits.is_null() {
                if !dib.is_null() { DeleteObject(dib); }
                DeleteDC(mem);
                ReleaseDC(std::ptr::null_mut(), screen);
                return Err(ToolError::io("CreateDIBSection 失败（显存不足？）"));
            }
            let old = SelectObject(mem, dib);
            if old.is_null() || old as isize == -1 {
                DeleteObject(dib);
                DeleteDC(mem);
                ReleaseDC(std::ptr::null_mut(), screen);
                return Err(ToolError::io("SelectObject 失败"));
            }

            // `CAPTUREBLT`：把分层窗口也抓进来（否则某些浮层是黑的）。
            // `BitBlt` **不含光标** —— 对"给模型看屏幕"来说正好，光标只会挡住内容。
            let ok = BitBlt(
                mem,
                0,
                0,
                rect.width,
                rect.height,
                screen,
                rect.x,
                rect.y,
                SRCCOPY | CAPTUREBLT,
            );

            // DIB 指针只能在 GDI 批处理完成后读取。
            let flushed = GdiFlush();
            if ok != 0 && flushed != 0 {
                let src = std::slice::from_raw_parts(bits as *const u8, bytes);
                // GDI 给的是 BGRA，且 alpha 通道不可信（常为 0），一律按不透明处理。
                for px in src.chunks_exact(4) {
                    rgba.extend_from_slice(&[px[2], px[1], px[0], 0xff]);
                }
            }

            let restored = SelectObject(mem, old);
            // 先销毁 DC，即使恢复选入对象失败，也不会删除仍被选入的位图。
            let dc_deleted = DeleteDC(mem);
            let bitmap_deleted = DeleteObject(dib);
            let released = ReleaseDC(std::ptr::null_mut(), screen);

            if ok == 0 || flushed == 0 {
                return Err(ToolError::io("BitBlt/GdiFlush 失败：没能读回屏幕像素"));
            }
            if restored.is_null() || restored as isize == -1 || dc_deleted == 0 || bitmap_deleted == 0 || released == 0 {
                return Err(ToolError::io("截图 GDI 资源清理失败"));
            }
            Ok(Shot {
                width: rect.width as u32,
                height: rect.height as u32,
                rgba,
            })
        }
    }

    fn mouse(flags: MOUSE_EVENT_FLAGS, dx: i32, dy: i32) -> INPUT {
        INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx,
                    dy,
                    mouseData: 0,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    const MOVE_ABS: MOUSE_EVENT_FLAGS =
        MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK;

    fn down_up(button: Button) -> (MOUSE_EVENT_FLAGS, MOUSE_EVENT_FLAGS) {
        match button {
            Button::Left => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
            Button::Right => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
        }
    }

    fn send(events: &[INPUT]) -> Result<(), ToolError> {
        send_with_evidence(events, None)
    }

    // #region debug-point B: no environment reads or extra Win32 calls when disabled
    fn send_with_evidence(events: &[INPUT], mut evidence: Option<&mut Option<SendEvidence>>) -> Result<(), ToolError> {
        // #endregion debug-point B
        if events.is_empty() {
            return Ok(());
        }
        // 底层也失效，覆盖绕过 click/drag 工具直接调用本层的输入。
        super::super::screen_uia::cache_invalidate();
        super::super::screenshot_space::invalidate();
        // 注入前先置位：事件进系统队列的瞬间，全局左键就可能被采样到「按下」。
        SYNTHETIC_INPUT_AT.store(epoch_millis(), std::sync::atomic::Ordering::Relaxed);
        // #region debug-point B: clock only for opt-in calls, immediately before input
        let started = evidence.as_ref().map(|_| std::time::Instant::now());
        // #endregion debug-point B
        let sent = unsafe {
            SendInput(
                events.len() as u32,
                events.as_ptr(),
                std::mem::size_of::<INPUT>() as i32,
            )
        };
        // #region debug-point B: capture GetLastError before any clock/Win32/log operation
        let last_error = if evidence.is_some() {
            Some(unsafe { windows_sys::Win32::Foundation::GetLastError() })
        } else { None };
        // #endregion debug-point B
        // 完成后再写一次：drag 的分步移动会持续刷新这个戳，松手后
        // 短暂的窗口期（上层采样间隔）内的按下沿也都算合成的。
        SYNTHETIC_INPUT_AT.store(epoch_millis(), std::sync::atomic::Ordering::Relaxed);
        // #region debug-point B: save numbers only, defer transport until cleanup completes
        if let (Some(slot), Some(started), Some(last_error)) = (evidence.as_mut(), started, last_error) {
            **slot = Some(SendEvidence {
                requested: events.len(), sent, last_error_immediate: last_error,
                elapsed_us: started.elapsed().as_micros(),
            });
        }
        // #endregion debug-point B
        if sent as usize != events.len() {
            // 最典型的成因：目标窗口以**管理员**身份运行而 Neo 不是 ——
            // UIPI 会静默丢掉这些事件。所以这里必须报出来，不能当成功。
            return Err(ToolError::new(
                ErrorKind::NotAllowed,
                format!("鼠标事件没有全部送达（{} / {}）", sent, events.len()),
            )
            .with_hint(
                "通常是因为目标窗口以管理员身份运行、而 Neo 不是：\
                 要么用管理员身份启动 Neo，要么换一个目标",
            ));
        }
        Ok(())
    }

    /// 移动 + 点击。
    ///
    /// `double` 时把"按下—抬起—按下—抬起"**一次发出去**：Windows 按两次点击的
    /// 时间间隔是否小于双击阈值来判定双击，拆成两次 `SendInput` 反而容易被
    /// 线程调度顶出阈值，变成一个单击加一个单击。
    pub fn click(x: i32, y: i32, button: Button, double: bool) -> Result<(), ToolError> {
        click_with_evidence(x, y, button, double, None)
    }

    // #region debug-point C: no title/path/raw class or foreign PID leaves this function
    fn observe_click_point(x: i32, y: i32) -> ClickPoint {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetClassNameW, GetCursorPos, GetWindowThreadProcessId, WindowFromPoint,
        };
        unsafe {
            let mut cursor = POINT { x: 0, y: 0 };
            let cursor = (GetCursorPos(&mut cursor) != 0).then_some((cursor.x, cursor.y));
            let window = WindowFromPoint(POINT { x, y });
            let (target_own_pid, target_class) = if window.is_null() {
                (None, TargetClass::NoWindow)
            } else {
                let mut pid = 0;
                let thread = GetWindowThreadProcessId(window, &mut pid);
                let own = (thread != 0 && pid != 0).then(|| pid == windows_sys::Win32::System::Threading::GetCurrentProcessId());
                let mut class = [0u16; 256];
                let len = GetClassNameW(window, class.as_mut_ptr(), class.len() as i32);
                (own, if len <= 0 { TargetClass::Unknown } else { classify_click_class(&class[..len as usize]) })
            };
            ClickPoint { cursor, target: (x, y), target_own_pid, target_class }
        }
    }
    // #endregion debug-point C

    // #region debug-point B/C: preserve original batch and original failure release
    pub(crate) fn click_with_evidence(x: i32, y: i32, button: Button, double: bool, mut evidence: Option<&mut ClickEvidence>) -> Result<(), ToolError> {
        // #endregion debug-point B/C
        let _dpi = physical_pixels()?;
        let displays = monitors()?;
        let vs = monitor_bounds(&displays)?;
        require_monitor_point(&displays, "点击位置", x, y)?;
        let (nx, ny) = to_absolute(x, y, vs)?;
        let (down, up) = down_up(button);

        let mut events = vec![mouse(MOVE_ABS, nx, ny), mouse(down, 0, 0), mouse(up, 0, 0)];
        if double {
            events.push(mouse(down, 0, 0));
            events.push(mouse(up, 0, 0));
        }
        // #region debug-point C: local observation only, no transport/sleep before input
        if let Some(evidence) = evidence.as_mut() {
            evidence.before = Some(observe_click_point(x, y));
        }
        // #endregion debug-point C
        let result = send_with_evidence(&events, evidence.as_mut().map(|e| &mut e.primary));
        if result.is_err() {
            // 批次可能只送达按下而未送达抬起；只释放，不再移动到潜在危险位置。
            let _ = send_with_evidence(&[mouse(up, 0, 0)], evidence.as_mut().map(|e| &mut e.release_cleanup));
        }
        // #region debug-point C: no wait for the target; acceptance is not a response
        if let Some(evidence) = evidence.as_mut() {
            evidence.after = Some(observe_click_point(x, y));
        }
        // #endregion debug-point C
        result
    }

    /// 拖动：移过去 → 按下 → 分步移动到终点 → 抬起。
    ///
    /// 分步是必需的：直接"按下 → 跳到终点 → 抬起"在多数应用里会被当成单击
    /// （中间没有鼠标移动消息）。步数按 `duration_ms` 切，每步之间小睡。
    #[cfg(test)]
    pub fn drag(from: (i32, i32), to: (i32, i32), button: Button, duration_ms: u64) -> Result<(), ToolError> {
        drag_cancellable(from, to, button, duration_ms, || false)
    }

    pub fn drag_cancellable(
        from: (i32, i32), to: (i32, i32), button: Button, duration_ms: u64,
        cancelled: impl Fn() -> bool,
    ) -> Result<(), ToolError> {
        if cancelled() { return Err(crate::cancelled_error()); }
        let _dpi = physical_pixels()?;
        let displays = monitors()?;
        let vs = monitor_bounds(&displays)?;
        require_monitor_point(&displays, "拖动起点", from.0, from.1)?;
        require_monitor_point(&displays, "拖动终点", to.0, to.1)?;
        // 端点和归一化可表达性全部在按下前检查。
        to_absolute(from.0, from.1, vs)?;
        to_absolute(to.0, to.1, vs)?;
        let (down, up) = down_up(button);
        drag_with(from, to, duration_ms, |input| match input {
            DragInput::Start((x, y)) => {
                let (nx, ny) = to_absolute(x, y, vs)?;
                if cancelled() { return Err(crate::cancelled_error()); }
                send(&[mouse(MOVE_ABS, nx, ny), mouse(down, 0, 0)])
            }
            DragInput::Move((x, y)) => {
                let (nx, ny) = to_absolute(x, y, vs)?;
                if cancelled() { return Err(crate::cancelled_error()); }
                send(&[mouse(MOVE_ABS, nx, ny)])
            }
            DragInput::Release => send(&[mouse(up, 0, 0)]),
        }, std::thread::sleep, &cancelled)
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    fn unsupported() -> ToolError {
        ToolError::new(ErrorKind::Unsupported, "屏幕交互目前只在 Windows 上实现")
            .with_hint("其他平台可以先用 `bash` 的 screencapture / import 等命令截屏")
    }

    pub fn ensure_dpi_aware() {}

    pub fn physical_pixels() -> Result<(), ToolError> { Err(unsupported()) }

    pub fn monitors() -> Result<Vec<Rect>, ToolError> { Err(unsupported()) }

    pub fn virtual_screen() -> Rect {
        Rect {
            x: 0,
            y: 0,
            width: 0,
            height: 0,
        }
    }

    pub fn dpi_scale_at(_x: i32, _y: i32) -> Option<f64> {
        None
    }

    pub fn capture(_rect: Rect) -> Result<Shot, ToolError> {
        Err(unsupported())
    }

    pub fn capture_silent(_rect: Rect) -> Result<Shot, ToolError> {
        Err(unsupported())
    }

    pub fn click(_x: i32, _y: i32, _button: Button, _double: bool) -> Result<(), ToolError> {
        Err(unsupported())
    }

    // #region debug-point B/C: unsupported platforms retain the same error
    pub(crate) fn click_with_evidence(x: i32, y: i32, button: Button, double: bool, _evidence: Option<&mut ClickEvidence>) -> Result<(), ToolError> {
        click(x, y, button, double)
    }
    // #endregion debug-point B/C

    #[cfg(test)]
    pub fn drag(
        _from: (i32, i32),
        _to: (i32, i32),
        _button: Button,
        _duration_ms: u64,
    ) -> Result<(), ToolError> {
        Err(unsupported())
    }

    pub fn drag_cancellable(
        _from: (i32, i32), _to: (i32, i32), _button: Button, _duration_ms: u64,
        cancelled: impl Fn() -> bool,
    ) -> Result<(), ToolError> {
        if cancelled() { return Err(crate::cancelled_error()); }
        Err(unsupported())
    }
}

pub use imp::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DragInput {
    Start((i32, i32)),
    Move((i32, i32)),
    Release,
}

fn drag_with(
    from: (i32, i32), to: (i32, i32), duration_ms: u64,
    mut send: impl FnMut(DragInput) -> Result<(), ToolError>,
    mut sleep: impl FnMut(std::time::Duration),
    cancelled: impl Fn() -> bool,
) -> Result<(), ToolError> {
    if cancelled() { return Err(crate::cancelled_error()); }
    if let Err(error) = send(DragInput::Start(from)) {
        return Err(drag_cleanup(&mut send, error));
    }
    // 每步最多等待 15ms；取消只允许释放，不再移动或按下。
    let steps = (duration_ms / 15).clamp(1, 667) as i32;
    for i in 1..=steps {
        let t = f64::from(i) / f64::from(steps);
        let x = (f64::from(from.0) + (f64::from(to.0) - f64::from(from.0)) * t).round() as i32;
        let y = (f64::from(from.1) + (f64::from(to.1) - f64::from(from.1)) * t).round() as i32;
        if cancelled() { return Err(drag_cleanup(&mut send, crate::cancelled_error())); }
        if let Err(error) = send(DragInput::Move((x, y))) {
            return Err(drag_cleanup(&mut send, error));
        }
        if cancelled() { return Err(drag_cleanup(&mut send, crate::cancelled_error())); }
        if i < steps { sleep(std::time::Duration::from_millis(15)); }
    }
    if let Err(error) = send(DragInput::Release) {
        return Err(drag_cleanup(&mut send, error));
    }
    if cancelled() { return Err(crate::cancelled_error()); }
    Ok(())
}

fn drag_cleanup(send: &mut impl FnMut(DragInput) -> Result<(), ToolError>, mut error: ToolError) -> ToolError {
    if let Err(release) = send(DragInput::Release) {
        // 释放失败不能伪装成普通取消；保留原始错误及无法确认松键的诊断。
        error.message = format!("{}；鼠标释放失败：{}", error.message, release.message);
        error.kind = release.kind;
        error.hint = release.hint;
    }
    error
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drag_cancel_after_sleep_only_releases_without_late_moves() {
        let cancel = std::cell::Cell::new(false);
        let mut inputs = Vec::new();
        let result = drag_with((0, 0), (1000, 1000), 10_000,
            |input| { inputs.push(input); Ok(()) }, |_| cancel.set(true), || cancel.get());
        assert!(result.is_err(), "取消不得报告拖拽成功；实际输入数 {}", inputs.len());
        assert_eq!(inputs.len(), 3);
        assert!(matches!(inputs[0], DragInput::Start(_)));
        assert!(matches!(inputs[1], DragInput::Move(_)));
        assert_eq!(inputs[2], DragInput::Release);
    }

    #[test]
    fn drag_cancel_on_start_and_release_failure_preserves_diagnostics() {
        for release_fails in [false, true] {
            let cancel = std::cell::Cell::new(false);
            let mut inputs = Vec::new();
            let error = drag_with((0, 0), (10, 10), 300, |input| {
                inputs.push(input);
                if matches!(input, DragInput::Start(_)) { cancel.set(true); }
                if input == DragInput::Release && release_fails { Err(ToolError::io("假释放失败")) } else { Ok(()) }
            }, |_| panic!("取消后不得等待"), || cancel.get()).unwrap_err();
            assert_eq!(inputs, vec![DragInput::Start((0, 0)), DragInput::Release]);
            assert_eq!(error.kind, if release_fails { ErrorKind::Io } else { crate::cancelled_error().kind });
            if release_fails { assert!(error.message.contains("假释放失败")); }
        }
    }

    #[test]
    fn drag_failures_only_cleanup_and_never_report_success() {
        for fail_at in [0, 1, 2] {
            let mut inputs = Vec::new();
            let error = drag_with((0, 0), (10, 10), 0, |input| {
                inputs.push(input);
                if inputs.len() == fail_at + 1 { Err(ToolError::io("假输入失败")) } else { Ok(()) }
            }, |_| panic!("单步无需等待"), || false).unwrap_err();
            assert_eq!(inputs.last(), Some(&DragInput::Release));
            assert_eq!(inputs.len(), fail_at + 2);
            assert!(error.message.contains("假输入失败"));
        }
        let mut inputs = Vec::new();
        let mut sleeps = 0;
        drag_with((-10, 20), (100, -30), 300,
            |input| { inputs.push(input); Ok(()) }, |_| sleeps += 1, || false).unwrap();
        assert_eq!(inputs.len(), 22);
        assert_eq!(sleeps, 19);
        assert_eq!(inputs[20], DragInput::Move((100, -30)));
        assert_eq!(inputs[21], DragInput::Release);
    }

    #[test]
    fn drag_precancel_has_no_input_or_sleep() {
        let result = drag_with((0, 0), (10, 10), 300,
            |_| panic!("取消后不得输入"), |_| panic!("取消后不得等待"), || true);
        assert_eq!(result.unwrap_err().kind, crate::cancelled_error().kind);
    }

    #[test]
    fn input_normalization_exhaustive_pixel_buckets() {
        for span in [1, 1920, 3840, 7680, 65536] {
            for origin in [0, -1920, -7680] {
                for pixel in 0..span {
                    let n = normalize_axis(origin + pixel, origin, span).unwrap();
                    assert!((0..65536).contains(&n));
                    assert_eq!(i64::from(n) * i64::from(span) / 65536, i64::from(pixel));
                }
            }
        }
        for (pixel, origin, span) in [(0, 0, 65537), (0, 0, 0), (-1, 0, 1920), (1920, 0, 1920)] {
            assert!(normalize_axis(pixel, origin, span).is_err());
        }
    }

    #[test]
    fn display_coverage_rejects_holes_and_unions_overlap() {
        let displays = [Rect { x: -100, y: -100, width: 100, height: 100 }, Rect { x: 0, y: 0, width: 200, height: 100 }];
        let bounds = monitor_bounds(&displays).unwrap();
        assert_eq!(bounds, Rect { x: -100, y: -100, width: 300, height: 200 });
        assert_eq!(monitor_coverage(bounds, &displays), 30000);
        assert!(require_monitor_point(&displays, "点击位置", -1, -1).is_ok());
        assert!(require_monitor_point(&displays, "拖动终点", 0, -1).is_err());
        assert_eq!(monitor_coverage(Rect { x: 0, y: -100, width: 200, height: 100 }, &displays), 0);
        assert_eq!(monitor_coverage(displays[0], &[displays[0], displays[0]]), 10000);
    }

    #[test]
    fn click_debug_evidence_is_bounded_and_does_not_claim_target_response() {
        let mut events = Vec::new();
        ClickEvidence::default().append_events(&mut events, "pure-test");
        assert!(events.is_empty());
        let evidence = ClickEvidence {
            before: Some(ClickPoint {
                cursor: Some((-1, 2)), target: (3, 4), target_own_pid: Some(true),
                target_class: classify_click_class(&"neo-overlay".encode_utf16().collect::<Vec<_>>()),
            }),
            primary: Some(SendEvidence { requested: 3, sent: 3, last_error_immediate: 5, elapsed_us: 1 }),
            ..Default::default()
        };
        evidence.append_events(&mut events, "pure-test");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["data"]["before"]["target_class"], "neo_overlay");
        assert_eq!(events[0]["data"]["before"]["target_own_pid"], true);
        assert_eq!(events[1]["data"]["primary"]["sent"], 3);
        assert_eq!(events[1]["data"]["target_response_verified"], false);
        assert_eq!(events[1]["data"]["accepted_does_not_prove_target_response"], true);
        assert_eq!(events[1]["data"]["last_error_may_be_stale_on_success"], true);
        assert_eq!(classify_click_class(&"private-window-class".encode_utf16().collect::<Vec<_>>()), TargetClass::Other);
        assert_eq!(serde_json::to_string(&TargetClass::Other).unwrap(), "\"other\"");
    }

    #[test]
    fn geometry_and_capture_size_reject_overflow_without_desktop_access() {
        let extreme = Rect { x: i32::MAX - 1, y: i32::MIN, width: 100, height: i32::MAX };
        assert!(extreme.contains(i32::MAX, -2));
        assert!(!extreme.contains(i32::MAX, 0));
        assert!(extreme.outside_error("位置", 0, 0).message.contains("2147483745"));
        assert!(capture_bytes(Rect { x: 0, y: 0, width: i32::MAX, height: i32::MAX }).is_err());
        assert!(capture_bytes(Rect { x: 0, y: 0, width: 0, height: 10 }).is_err());
        assert_eq!(capture_bytes(Rect { x: -1920, y: 0, width: 32, height: 32 }).unwrap(), 4096);
    }

    #[test]
    fn button_parsing_is_forgiving_about_case_and_aliases() {
        for s in ["", "left", "LEFT", " l ", "primary"] {
            assert_eq!(Button::parse(s).unwrap(), Button::Left, "{s}");
        }
        for s in ["right", "Right", "r", "secondary"] {
            assert_eq!(Button::parse(s).unwrap(), Button::Right, "{s}");
        }
        let err = Button::parse("middle").unwrap_err();
        assert_eq!(err.kind, ErrorKind::BadArguments);
        assert!(err.hint.is_some(), "要给可执行的出路");
    }

    /// 边界是**半开**的：副屏排在主屏左边时坐标是负数，也属于桌面。
    #[test]
    fn rect_containment_is_half_open_and_allows_negative() {
        let r = Rect {
            x: -1920,
            y: 0,
            width: 1920,
            height: 1080,
        };
        assert!(r.contains(-1920, 0));
        assert!(r.contains(-1, 1079));
        assert!(!r.contains(0, 0), "右边界不含");
        assert!(!r.contains(-1920, 1080));
        assert!(!r.contains(-1921, 0));
    }

    /// 越界时必须把**合法范围**说出来 —— 模型据此自己改，不用再问一轮。
    #[test]
    fn outside_error_states_the_legal_range() {
        let r = Rect {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let e = r.outside_error("点击位置", 5000, 5000);
        assert_eq!(e.kind, ErrorKind::BadArguments);
        assert!(
            e.message.contains("1919") && e.message.contains("1079"),
            "要含合法范围：{}",
            e.message
        );
        assert!(e.hint.is_some());
    }

    /// 把屏幕层的读数打出来（默认忽略，`--ignored --nocapture` 才跑）。
    #[test]
    #[ignore = "诊断用：打印虚拟桌面与缩放比"]
    fn dump_screen_info() {
        ensure_dpi_aware();
        let vs = virtual_screen();
        println!(
            "虚拟桌面: x={} y={} width={} height={}",
            vs.x, vs.y, vs.width, vs.height
        );
        println!(
            "中心点缩放比: {:?}",
            dpi_scale_at(vs.x + vs.width / 2, vs.y + vs.height / 2)
        );
        assert!(vs.width > 0, "没有桌面会话");
    }

    /// 只读的一条：抓 32×32、编码 PNG，并确认 DPI 查询不炸。
    /// 无头会话里允许失败，但必须是**可读的错误**。
    #[test]
    fn screen_layer_never_panics() {
        ensure_dpi_aware();
        let vs = virtual_screen();
        if vs.width <= 0 {
            eprintln!("跳过：没有可用的虚拟桌面（无头会话）");
            return;
        }
        let probe = Rect {
            x: vs.x,
            y: vs.y,
            width: 32,
            height: 32,
        };
        match capture(probe) {
            Ok(shot) => {
                assert_eq!(shot.width, 32);
                assert_eq!(shot.rgba.len(), 32 * 32 * 4);
                let png = shot.to_png().expect("PNG 编码");
                assert!(png.starts_with(&[0x89, b'P', b'N', b'G']), "PNG 魔数");
                assert!(png.len() > 50, "不该是空图");
            }
            Err(e) => {
                assert!(!e.message.is_empty());
                assert!(e.hint.is_some(), "失败也要给出路");
            }
        }
        // 缩放比：要么给个数，要么给 None —— 但不能 panic
        if let Some(scale) = dpi_scale_at(vs.x + 1, vs.y + 1) {
            assert!(scale > 0.0 && scale < 10.0, "缩放比看起来不对：{scale}");
        }
    }

    /// 越界点击必须**在发出任何事件之前**就被拒 —— 不能让鼠标先跑过去。
    #[test]
    fn out_of_range_click_is_rejected_without_touching_the_mouse() {
        let vs = virtual_screen();
        if vs.width <= 0 {
            return;
        }
        let err = click(vs.x + vs.width + 5000, vs.y + 10, Button::Left, false).unwrap_err();
        assert_eq!(err.kind, ErrorKind::BadArguments);
        assert!(err.message.contains("不在屏幕内"), "{}", err.message);
        // 拖动两端都要查
        let err = drag(
            (vs.x + 1, vs.y + 1),
            (vs.x - 5000, vs.y + 1),
            Button::Left,
            100,
        )
        .unwrap_err();
        assert_eq!(err.kind, ErrorKind::BadArguments);
        assert!(err.message.contains("终点"), "{}", err.message);
    }
}
