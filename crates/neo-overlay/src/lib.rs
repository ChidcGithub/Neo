//! neo-overlay：被动渲染层 —— 全屏流光跑马灯 + 非交互辅助卡片。
//!
//! 架构：独立窗口线程 + 专属 wgpu/Dx12 渲染 + 内嵌独立 egui 上下文，
//! 与 egui 主界面（eframe 占着进程级 winit EventLoop 单例）完全解耦。
//!
//! 非交互卡片与波纹共用常驻绘图层；所有交互由应用的独立原生视口负责。
//!
//! 关键点：
//! - DX12 透明交换链必须用 `Dx12SwapchainKind::DxgiFromVisual`（默认 DxgiFromHwnd 只有 Opaque alpha）
//! - `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)` 让抓屏拍不到自己，从而能持续抓桌面做实时折射
//! - 绘制 HWND 永久 `WS_EX_LAYERED | WS_EX_TRANSPARENT`，跨线程穿透；
//!   `HTTRANSPARENT` 仅作同线程兜底，不是跨线程路由保证。
//! - `WS_EX_NOACTIVATE` + `SW_SHOWNA` 永不抢焦点；卡片只画，不收输入
//! - 首次显示前设置 `NonRudeHWND`，退出 Shell 全屏检测，不干预任务栏设置
//! - 窗口每次露面（含卡片首次出现）都先离屏渲好一帧再 `SW_SHOWNA` ——
//!   只在当前 surface 帧提交后露面（不等同于 DWM 已完成合成）
//! - 抓屏线程最多 20fps 把桌面帧喂给渲染线程，光带动画仍随交换链 vsync
//! - 控制命令走 mpsc + `PostThreadMessageW` 唤醒（消息循环在隐藏时整块睡眠）
//! - 视觉为 VCC edgeglow v2 移植：最高 0.6x、像素预算受限的离屏光环 + 线性放大 blit；卡片保持原分辨率

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use neo_tools::tools::screen::{self, Shot};
use windows_sys::Wdk::System::SystemServices::RtlGetVersion;
use windows_sys::Win32::Foundation::{
    GetLastError, ERROR_CLASS_ALREADY_EXISTS, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::SystemInformation::OSVERSIONINFOW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::HiDpi::GetDpiForSystem;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetWindowLongPtrW, PeekMessageW, PostThreadMessageW, RegisterClassExW, RemovePropW, SetPropW,
    SetWindowDisplayAffinity, SetWindowLongPtrW, SetWindowPos, ShowWindow, TranslateMessage,
    UpdateLayeredWindow, CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, HTTRANSPARENT, HWND_TOPMOST, MSG,
    PM_REMOVE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SW_HIDE, SW_SHOWNA, ULW_ALPHA,
    WDA_EXCLUDEFROMCAPTURE, WM_APP, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_NCDESTROY, WM_NCHITTEST,
    WM_QUIT, WM_SETTINGCHANGE, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

/// 相位目标表（VCC TARGETS 照搬）：(强度, 速度, 环流)。
const PH_IDLE: (f32, f32, f32) = (0.00, 0.50, 0.015);
const PH_LISTEN: (f32, f32, f32) = (0.76, 0.85, 0.07);
/// 离屏比例上限；更大桌面可能略柔化，但不改变 shader 的归一化光带宽度。
const RENDER_SCALE: f32 = 0.6;
const OFFSCREEN_PIXEL_BUDGET: u32 = 746_496; // 1920 * 1080 * 0.6²
const OFFSCREEN_MAX_HEIGHT: u32 = 648;

fn offscreen_size((width, height): (u32, u32)) -> (u32, u32) {
    // 无效桌面由调用方验证；helper 对零/极端输入仍保持非零、有界且不溢出。
    let w = f64::from(width.max(1));
    let h = f64::from(height.max(1));
    let budget = f64::from(OFFSCREEN_PIXEL_BUDGET);
    let scale = f64::from(RENDER_SCALE)
        .min(f64::from(OFFSCREEN_MAX_HEIGHT) / h)
        .min((budget / (w * h)).sqrt())
        // 极端长宽比下，短边最少占一个像素，长边也必须遵守总预算。
        .min(budget / w.max(h));
    ((w * scale).max(1.0) as u32, (h * scale).max(1.0) as u32)
}

fn edge_scissors((w, h): (u32, u32)) -> impl Iterator<Item = (u32, u32, u32, u32)> {
    // 与 shader 的 d < -120*px、inset=12*px、corner=14*px 对应。
    // 向外取整并留 2 个离屏像素 AA guard；只裁片元，不改 viewport/几何/UV。
    let edge = ((120.0 + 12.0 + 14.0) * f64::from(h) / 432.0).ceil() as u32 + 2;
    let rects = if u64::from(edge) * 2 >= u64::from(w.min(h)) {
        [(0, 0, w, h), (0, 0, 0, 0), (0, 0, 0, 0), (0, 0, 0, 0)]
    } else {
        [
            (0, 0, w, edge),
            (0, h - edge, w, edge),
            (0, edge, edge, h - 2 * edge),
            (w - edge, edge, edge, h - 2 * edge),
        ]
    };
    rects
        .into_iter()
        .filter(|&(_, _, width, height)| width > 0 && height > 0)
}
/// 线程消息：命令队列里有货，唤醒睡眠中的消息循环。
const WM_NEO_CMD: u32 = WM_APP + 1;
const START_TIMEOUT: Duration = Duration::from_secs(5);
const DRAW_EX_STYLE: u32 =
    WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
const NON_RUDE_HWND: windows_sys::core::PCWSTR = windows_sys::w!("NonRudeHWND");

/// Layered + transparent 忽略窗口形状，把鼠标命中交给底层（不限同线程）。
/// https://learn.microsoft.com/en-us/windows/win32/winmsg/window-features#layered-windows
fn initialize_passive_window(hwnd: HWND, size: (u32, u32)) -> Result<(), String> {
    // MarkFullscreenWindow(false) 仍会回退到 Shell 自动检测；不是「永不全屏」。
    // 官方指定在显示前设置 NonRudeHWND=TRUE，避免全屏 popup 使任务栏降低 Z-order。
    // 属性属于 HWND，跨 hide/resize 保留；WM_NCDESTROY 负责移除。失败不允许露面。
    // https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nf-shobjidl_core-itaskbarlist2-markfullscreenwindow
    if unsafe { SetPropW(hwnd, NON_RUDE_HWND, 1usize as _) } == 0 {
        return Err(format!(
            "SetPropW(NonRudeHWND) 失败，拒绝显示绘图层: {}",
            unsafe { GetLastError() }
        ));
    }
    // DComp 在 HWND 自身内容之上合成；交换链 Clear 不能清掉 HWND 底图。
    // 用逐像素透明 DIB 初始化底层，绝不通过全局 alpha=0 隐藏整棵 visual。
    // 不调用 SetLayeredWindowAttributes：它会使后续 ULW 失败，除非切换 layered 位。
    // 依据与验证边界见本 crate 的 TRANSPARENCY.md。
    layered_backing(hwnd, size, 0)
}

fn layered_backing(hwnd: HWND, size: (u32, u32), pixel: u32) -> Result<(), String> {
    use windows_sys::Win32::Graphics::Gdi::{
        CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, SelectObject, AC_SRC_ALPHA,
        AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS, HBITMAP,
        HDC, HGDIOBJ,
    };
    let (w, h) = size;
    let bytes = (w as usize)
        .checked_mul(h as usize)
        .and_then(|n| n.checked_mul(4))
        .filter(|n| *n <= isize::MAX as usize)
        .ok_or("overlay 底图尺寸溢出")?;
    if w == 0 || h == 0 || w > i32::MAX as u32 || h > i32::MAX as u32 {
        return Err("overlay 底图尺寸无效".into());
    }
    struct BitmapDc {
        dc: HDC,
        bitmap: HBITMAP,
        old: HGDIOBJ,
    }
    impl Drop for BitmapDc {
        fn drop(&mut self) {
            unsafe {
                if !self.old.is_null() {
                    SelectObject(self.dc, self.old);
                }
                if !self.bitmap.is_null() {
                    DeleteObject(self.bitmap);
                }
                DeleteDC(self.dc);
            }
        }
    }
    let dc = unsafe { CreateCompatibleDC(std::ptr::null_mut()) };
    if dc.is_null() {
        return Err(format!("CreateCompatibleDC: {}", unsafe { GetLastError() }));
    }
    let mut backing = BitmapDc {
        dc,
        bitmap: std::ptr::null_mut(),
        old: std::ptr::null_mut(),
    };
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w as i32,
            biHeight: -(h as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits = std::ptr::null_mut();
    backing.bitmap = unsafe {
        CreateDIBSection(
            dc,
            &info,
            DIB_RGB_COLORS,
            &mut bits,
            std::ptr::null_mut(),
            0,
        )
    };
    if backing.bitmap.is_null() {
        return Err(format!("CreateDIBSection: {}", unsafe { GetLastError() }));
    }
    // DIB owns bytes until BitmapDc drops; 32bpp rows need no extra padding.
    unsafe {
        std::slice::from_raw_parts_mut(bits.cast::<u32>(), bytes / 4).fill(pixel);
    }
    backing.old = unsafe { SelectObject(dc, backing.bitmap) };
    if backing.old.is_null() || backing.old as isize == -1 {
        backing.old = std::ptr::null_mut();
        return Err(format!("SelectObject: {}", unsafe { GetLastError() }));
    }
    let size = SIZE {
        cx: w as i32,
        cy: h as i32,
    };
    let source = POINT { x: 0, y: 0 };
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    if unsafe {
        UpdateLayeredWindow(
            hwnd,
            std::ptr::null_mut(),
            std::ptr::null(),
            &size,
            dc,
            &source,
            0,
            &blend,
            ULW_ALPHA,
        )
    } == 0
    {
        return Err(format!(
            "UpdateLayeredWindow 失败，拒绝显示绘图层: {}",
            unsafe { GetLastError() }
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RenderOutcome {
    Skipped,
    Presented,
}

#[derive(Default)]
struct Presentation {
    ready: bool,
    shown: bool,
}

impl Presentation {
    fn invalidate(&mut self) {
        self.ready = false;
        self.shown = false;
    }
    fn can_show(&self, outcome: RenderOutcome) -> bool {
        self.ready && !self.shown && outcome == RenderOutcome::Presented
    }
}
/// 桌面纹理独立限频；不降低光带、淡出或卡片动画的显示帧率。
const CAPTURE_INTERVAL: Duration = Duration::from_millis(50);
static ENGINE_RUNNING: AtomicBool = AtomicBool::new(false);

struct EngineLease(&'static AtomicBool);

impl EngineLease {
    fn acquire(flag: &'static AtomicBool) -> Result<Self, String> {
        flag.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| Self(flag))
            .map_err(|_| "neo-overlay 线程仍在运行，本次不再启动".to_owned())
    }
}

impl Drop for EngineLease {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// 发给窗口线程的命令。
#[derive(Debug)]
enum Cmd {
    Show,
    Hide,
    Suspend(Sender<Result<(), String>>, Instant, Arc<AtomicBool>),
    Resume,
    Shutdown,
}

fn desktop_can_show(count: &AtomicU32) -> bool {
    count.load(Ordering::Acquire) == 0
}

fn desktop_request_live(deadline: Instant, live: &AtomicBool) -> bool {
    live.load(Ordering::Acquire) && Instant::now() < deadline
}

fn hide_for_desktop(hwnd: HWND) -> Result<(), String> {
    if unsafe { windows_sys::Win32::UI::WindowsAndMessaging::IsWindow(hwnd) == 0 } {
        return Err("浮层窗口已失效，拒绝执行桌面工具".into());
    }
    unsafe {
        ShowWindow(hwnd, SW_HIDE);
    }
    if unsafe { windows_sys::Win32::UI::WindowsAndMessaging::IsWindowVisible(hwnd) == 0 } {
        Ok(())
    } else {
        Err("浮层未成功隐藏，拒绝执行桌面工具".into())
    }
}

/// 桌面工具的短期租约；只在后台线程等待窗口线程的隐藏回执。
#[derive(Clone)]
pub struct DesktopGate {
    tx: Sender<Cmd>,
    tid: u32,
    count: Arc<AtomicU32>,
    stop: Arc<AtomicBool>,
}

pub struct DesktopGuard(DesktopGate, Arc<AtomicBool>);

impl DesktopGate {
    fn wake(&self) {
        if self.tid != 0 {
            unsafe {
                PostThreadMessageW(self.tid, WM_NEO_CMD, 0, 0);
            }
        }
    }

    fn reserve(&self) -> DesktopGuard {
        self.count.fetch_add(1, Ordering::AcqRel);
        DesktopGuard(self.clone(), Arc::new(AtomicBool::new(true)))
    }

    pub fn acquire(&self, cancel: &AtomicBool, timeout: Duration) -> Result<DesktopGuard, String> {
        let deadline = Instant::now() + timeout;
        if cancel.load(Ordering::Acquire) || Instant::now() >= deadline {
            return Err("桌面屏障已取消或超时".into());
        }
        let guard = self.reserve();
        let (tx, rx) = channel();
        self.tx
            .send(Cmd::Suspend(tx, deadline, guard.1.clone()))
            .map_err(|_| "桌面屏障窗口线程已退出")?;
        self.wake();
        loop {
            if cancel.load(Ordering::Acquire) || self.stop.load(Ordering::Acquire) {
                return Err("桌面屏障已取消或窗口线程已退出".into());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err("桌面屏障等待隐藏回执超时，未执行工具".into());
            }
            match rx.recv_timeout(remaining.min(Duration::from_millis(10))) {
                Ok(result) => {
                    result?;
                    if cancel.load(Ordering::Acquire)
                        || self.stop.load(Ordering::Acquire)
                        || Instant::now() >= deadline
                    {
                        return Err("桌面屏障已取消、超时或窗口线程已退出".into());
                    }
                    return Ok(guard);
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => return Err("桌面屏障未收到隐藏回执".into()),
            }
        }
    }
}

impl Drop for DesktopGuard {
    fn drop(&mut self) {
        self.1.store(false, Ordering::Release);
        if self.0.count.fetch_sub(1, Ordering::AcqRel) == 1 {
            let _ = self.0.tx.send(Cmd::Resume);
            self.0.wake();
        }
    }
}

/// 采集门和帧槽共用短锁；锁绝不跨越系统抓屏或 GPU 操作。
type FrameSlot = Arc<Mutex<CaptureState>>;

#[derive(Default)]
struct CaptureState {
    enabled: bool,
    exclude_ok: bool,
    generation: u64,
    frame: Option<(DesktopBounds, Shot)>,
    changed: Arc<Condvar>,
}

impl CaptureState {
    fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        self.invalidate();
    }

    fn set_exclude_ok(&mut self, exclude_ok: bool) {
        self.exclude_ok = exclude_ok;
        self.invalidate();
    }

    /// 拓扑刷新也换代，但不能重新打开已关闭的采集门。
    fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.frame = None;
        self.changed.notify_all();
    }

    /// 取得许可即进入在途阶段。随后系统采集不可撤销，Hide 只阻止下一次
    /// 许可并丢弃迟到结果；不能持锁等抓屏结束，否则 hide 会阻塞。
    fn begin(&self) -> Option<u64> {
        (self.enabled && self.exclude_ok).then_some(self.generation)
    }

    fn publish(&mut self, generation: u64, bounds: DesktopBounds, shot: Shot) {
        if self.begin() == Some(generation) {
            self.frame = Some((bounds, shot));
        }
    }

    fn take(&mut self, bounds: DesktopBounds) -> Option<Shot> {
        let (captured, shot) = self.frame.take()?;
        (self.begin().is_some() && captured == bounds && (shot.width, shot.height) == bounds.size)
            .then_some(shot)
    }
}

/// 与许可共用同一把锁，避免 Show / shutdown 落在检查与入睡之间而丢失唤醒。
fn wait_for_capture(slot: &FrameSlot, stop: &AtomicBool) -> Option<u64> {
    let state = slot.lock().ok()?;
    let changed = state.changed.clone();
    let state = changed
        .wait_while(state, |state| {
            !stop.load(Ordering::Acquire) && state.begin().is_none()
        })
        .ok()?;
    if stop.load(Ordering::Acquire) {
        None
    } else {
        state.begin()
    }
}

const DESKTOP_CACHE_BUDGET: usize = 8 * 1024 * 1024;

fn cacheable_desktop(shot: &Shot) -> bool {
    // capacity 也受限，不能通过保留过度分配的 Vec 绕过常驻内存预算。
    shot.rgba.capacity() <= DESKTOP_CACHE_BUDGET
}

/// 精确比较而非哈希；超预算直接上传，避免在呈现线程扫描多屏像素。
fn same_desktop_frame(previous: Option<&Shot>, shot: &Shot) -> bool {
    cacheable_desktop(shot)
        && previous.is_some_and(|previous| {
            cacheable_desktop(previous)
                && previous.width == shot.width
                && previous.height == shot.height
                && previous.rgba == shot.rgba
        })
}

#[derive(Default)]
struct DesktopUploadState {
    last: Option<Shot>,
    // GPU 是否含真实帧与 CPU 是否允许缓存无关（包括真实的 1x1 帧）。
    has_uploaded_desktop: bool,
}

impl DesktopUploadState {
    fn uploaded(&mut self, shot: Shot) {
        self.clear_cache();
        self.has_uploaded_desktop = true;
        if cacheable_desktop(&shot) {
            self.last = Some(shot);
        }
    }

    fn clear_cache(&mut self) {
        // 释放所有权，不承诺堆内存安全擦除；也不额外扫描像素做清零。
        self.last = None;
    }

    fn reset(&mut self) -> bool {
        self.clear_cache();
        std::mem::take(&mut self.has_uploaded_desktop)
    }
}

impl Drop for DesktopUploadState {
    fn drop(&mut self) {
        self.clear_cache();
    }
}

#[derive(Default)]
struct RippleState {
    showing: bool,
    fading: bool,
}

impl RippleState {
    fn show(&mut self) {
        self.showing = true;
        self.fading = false;
    }

    fn hide(&mut self) {
        self.fading = self.showing;
    }

    fn active(&self, have_cards: bool) -> bool {
        self.showing || have_cards
    }

    fn finish_fade(&mut self, intensity: f32) -> bool {
        if !fade_complete(self.fading, intensity) {
            return false;
        }
        self.showing = false;
        self.fading = false;
        true
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DesktopBounds {
    origin: (i32, i32),
    size: (u32, u32),
}

impl DesktopBounds {
    fn from_rect(rect: screen::Rect) -> Result<Self, String> {
        if rect.width <= 0 || rect.height <= 0 {
            return Err(format!("无效虚拟桌面尺寸: {}x{}", rect.width, rect.height));
        }
        Ok(Self {
            origin: (rect.x, rect.y),
            size: (rect.width as u32, rect.height as u32),
        })
    }

    #[cfg(test)]
    fn client_point(self, screen: (i32, i32)) -> (i32, i32) {
        (screen.0 - self.origin.0, screen.1 - self.origin.1)
    }
}

fn validate_dimensions(size: (u32, u32), max_dimension: u32) -> Result<(), String> {
    let (width, height) = size;
    if width == 0 || height == 0 || width > max_dimension || height > max_dimension {
        return Err(format!(
            "overlay 尺寸 {width}x{height} 超出有效范围 1..={max_dimension}"
        ));
    }
    Ok(())
}

fn overlay_limits(supported: &wgpu::Limits, size: (u32, u32)) -> Result<wgpu::Limits, String> {
    validate_dimensions(size, supported.max_texture_dimension_2d)?;
    let mut limits = wgpu::Limits::downlevel_defaults().using_alignment(supported.clone());
    // 预留适配器实际支持的 2D 范围，让正常热插拔不受初始桌面尺寸限制。
    limits.max_texture_dimension_2d = supported.max_texture_dimension_2d;
    if !limits.check_limits(supported) {
        return Err("适配器不满足 overlay 渲染所需 limits".into());
    }
    Ok(limits)
}

fn transparent_alpha(
    modes: &[wgpu::CompositeAlphaMode],
) -> Result<wgpu::CompositeAlphaMode, String> {
    // shader 和 egui 都输出预乘 alpha；PostMultiplied 不能直接替代。
    modes
        .contains(&wgpu::CompositeAlphaMode::PreMultiplied)
        .then_some(wgpu::CompositeAlphaMode::PreMultiplied)
        .ok_or_else(|| "overlay surface 不支持预乘透明合成".into())
}

fn supports_capture_exclusion(version: Option<(u32, u32, u32)>) -> bool {
    version.is_some_and(|version| version >= (10, 0, 19041))
}

fn windows_version() -> Option<(u32, u32, u32)> {
    let mut info = OSVERSIONINFOW {
        dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32,
        ..Default::default()
    };
    // RtlGetVersion 不受应用兼容 manifest 的版本谎报影响；查询失败即禁采。
    (unsafe { RtlGetVersion(&mut info) } >= 0).then_some((
        info.dwMajorVersion,
        info.dwMinorVersion,
        info.dwBuildNumber,
    ))
}

fn valid_upload(size: (u32, u32), len: usize, max_dimension: u32) -> bool {
    validate_dimensions(size, max_dimension).is_ok()
        && size.0.checked_mul(4).is_some()
        && (size.0 as usize)
            .checked_mul(size.1 as usize)
            .and_then(|pixels| pixels.checked_mul(4))
            == Some(len)
}

fn capture_rest(elapsed: Duration) -> Duration {
    // 慢采集也留出原来的休息时间，不追赶欠帧、不因限频优化反而增加工作量。
    CAPTURE_INTERVAL
        .saturating_sub(elapsed)
        .max(Duration::from_millis(33))
}

fn fade_complete(fading: bool, intensity: f32) -> bool {
    fading && intensity < 0.02
}

/// 一张画进渲染层的卡片（替代过去的独立原生子窗口）。
///
/// - `rect`：主显示器坐标系的 egui 点（主屏原点 = 虚拟屏原点 (0,0)，
///   层内会换算成窗口客户区像素）。**共享槽**：层的 Area 定位
///   每帧都重读它 —— 应用线程（10fps 内容节拍）与绘制闭包（层内 vsync
///   自驱动画，如迷你窗的避让滑动）都可以随时改写；
/// - `draw`：每帧在**窗口线程**的 egui Ui 里执行（Ui 已钉在卡片矩形内，
///   按「填满整个矩形」画即可，与旧视口回调的体感一致）。只准画，不准
///   阻塞或处理输入。**闭包内不得调用 `set_card`（卡片锁正被持着，会死锁）**。
pub struct Card {
    pub rect: Arc<Mutex<[f32; 4]>>,
    pub draw: Box<dyn FnMut(&mut egui::Ui) + Send>,
}

impl Card {
    /// 只画不收点击的卡（截屏闪光这类纯视觉反馈）。
    pub fn passive(rect: [f32; 4], draw: impl FnMut(&mut egui::Ui) + Send + 'static) -> Self {
        Self {
            rect: Arc::new(Mutex::new(rect)),
            draw: Box::new(draw),
        }
    }
}

fn card_area(id: u8, pos: egui::Pos2) -> egui::Area {
    egui::Area::new(egui::Id::new(("neo-layer-card", id)))
        .fixed_pos(pos)
        .movable(false)
        .interactable(false)
        .sense(egui::Sense::hover())
}

/// 卡片槽：应用线程每帧按需覆写，渲染线程每帧读最新。
/// BTreeMap 让同 id 覆盖天然幂等、遍历顺序稳定。
type CardSlot = Arc<Mutex<BTreeMap<u8, Card>>>;

/// 卡片 id 分配表（neo-app 各窗口引用这里的常量，不自己造数）。
pub mod card_id {
    /// 工具确认卡（confirmwin）。
    pub const CONFIRM: u8 = 1;
    /// 迷你窗（miniwin）。
    pub const MINI: u8 = 2;
    /// 课堂总结弹窗（classwin）。
    pub const CLASS: u8 = 3;
    /// 截屏闪光（shotflash）。
    pub const FLASH: u8 = 4;
    /// 总结起止红点（classwin::ClassDot）。
    pub const DOT: u8 = 5;
    /// 独立轻提示（toastwin）。
    pub const TOAST: u8 = 6;
}

/// 跑马灯控制柄，跨线程安全。Drop 时自动关停窗口线程。
pub struct OverlayHandle {
    tx: Sender<Cmd>,
    /// 窗口线程的 Win32 线程 id（PostThreadMessage 唤醒用）。
    tid: u32,
    level: Arc<AtomicU32>,
    visible: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    capture: FrameSlot,
    cards: CardSlot,
    suspended: Arc<AtomicU32>,
    thread: Option<JoinHandle<()>>,
    #[cfg(test)]
    wake_count: Option<AtomicU32>,
}

impl OverlayHandle {
    pub fn desktop_gate(&self) -> DesktopGate {
        DesktopGate {
            tx: self.tx.clone(),
            tid: self.tid,
            count: self.suspended.clone(),
            stop: self.stop.clone(),
        }
    }

    /// 显示全屏跑马灯并开始渲染。
    pub fn show(&self) {
        self.post(Cmd::Show);
    }

    /// 立即停止新采集，跑马灯用已有纹理淡出；卡片不受影响。
    /// 已取得许可的在途采集无法回滚，其迟到结果会被丢弃。
    pub fn hide(&self) {
        self.post(Cmd::Hide);
    }

    /// 设置音频电平（0.0..=1.0，通常取 RMS），驱动光带起伏。
    pub fn set_level(&self, v: f32) {
        self.level
            .store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    pub fn is_alive(&self) -> bool {
        !self.stop.load(Ordering::Acquire)
            && self
                .thread
                .as_ref()
                .is_some_and(|thread| !thread.is_finished())
    }

    pub fn is_visible(&self) -> bool {
        self.is_alive() && self.visible.load(Ordering::Relaxed)
    }

    /// 注册/更新/撤下一张卡片（`None` = 撤下）。窗口线程被唤醒后
    /// 下一帧生效：卡片出现会露面窗口，但始终穿透；最后一张撤下且
    /// 波纹已停时窗口回到隐藏睡眠。
    pub fn set_card(&self, id: u8, card: Option<Card>) {
        if !self.is_alive() {
            return;
        }
        let changed = if let Ok(mut cards) = self.cards.lock() {
            match card {
                Some(c) => {
                    cards.insert(id, c);
                    true
                }
                None => cards.remove(&id).is_some(),
            }
        } else {
            false
        };
        // 锁已释放；最后一张撤下也要唤醒，让窗口隐藏。
        if changed {
            self.wake_thread();
        }
    }

    fn wake_thread(&self) {
        #[cfg(test)]
        if let Some(count) = &self.wake_count {
            count.fetch_add(1, Ordering::Relaxed);
            return;
        }
        if self.tid != 0 {
            unsafe {
                PostThreadMessageW(self.tid, WM_NEO_CMD, 0, 0);
            }
        }
    }

    /// 立即关采集门并请求退出；不等待可能卡在驱动里的窗口线程。
    pub fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.visible.store(false, Ordering::Relaxed);
        self.post(Cmd::Shutdown);
        self.tid = 0;
        if let Some(thread) = self.thread.take() {
            if thread.is_finished() {
                let _ = thread.join();
            }
        }
    }

    fn await_ready(
        mut self,
        ready: Receiver<Result<u32, String>>,
        timeout: Duration,
    ) -> Result<Self, String> {
        self.tid = ready
            .recv_timeout(timeout)
            .map_err(|e| format!("neo-overlay 初始化未完成: {e}"))??;
        Ok(self)
    }

    /// 投递命令并唤醒窗口线程（线程已退出时安静地丢弃）。
    fn post(&self, cmd: Cmd) {
        // 许可变更与入队同序；窗口线程只处理动画，不能让积压的 Show
        // 在 hide 返回后重新开门。此锁不等待在途抓屏或渲染。
        let mut capture = self.capture.lock().unwrap_or_else(|p| p.into_inner());
        capture.set_enabled(matches!(cmd, Cmd::Show) && self.is_alive());
        let sent = self.tx.send(cmd).is_ok();
        if !sent {
            capture.set_enabled(false);
        }
        drop(capture);
        if sent {
            self.wake_thread();
        }
    }
}

impl Drop for OverlayHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// 启动渲染层引擎（窗口初始隐藏，等 `show()` / 首张卡片）。
///
/// 窗口与 wgpu 初始化在窗口线程里同步完成，失败（无 DX12 / 建窗失败）
/// 通过 `Err` 上报，调用方降级为无跑马灯/无层即可。
pub fn start() -> Result<OverlayHandle, String> {
    let lease = EngineLease::acquire(&ENGINE_RUNNING)?;
    let level = Arc::new(AtomicU32::new(0.0f32.to_bits()));
    let visible = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));
    let capture = Arc::new(Mutex::new(CaptureState::default()));
    let cards: CardSlot = Arc::new(Mutex::new(BTreeMap::new()));
    let suspended = Arc::new(AtomicU32::new(0));
    let (cmd_tx, cmd_rx) = channel::<Cmd>();
    // 窗口线程初始化完成后回传线程 id（命令唤醒要靠它）。
    let (ready_tx, ready_rx) = channel::<Result<u32, String>>();
    let thread = {
        let level = level.clone();
        let visible = visible.clone();
        let stop = stop.clone();
        let capture = capture.clone();
        let cards = cards.clone();
        let suspended = suspended.clone();
        std::thread::Builder::new()
            .name("neo-overlay".into())
            .spawn(move || {
                let _lease = lease;
                run(
                    ready_tx, cmd_rx, level, visible, stop, capture, cards, suspended,
                );
            })
            .map_err(|e| format!("创建 neo-overlay 线程失败: {e}"))?
    };
    OverlayHandle {
        tx: cmd_tx,
        tid: 0,
        level,
        visible,
        stop,
        capture,
        cards,
        suspended,
        thread: Some(thread),
        #[cfg(test)]
        wake_count: None,
    }
    .await_ready(ready_rx, START_TIMEOUT)
}

/// 窗口线程主函数。
#[allow(clippy::too_many_arguments)] // window-thread entry; grouping would obscure the startup contract
fn run(
    ready: Sender<Result<u32, String>>,
    cmd_rx: Receiver<Cmd>,
    level: Arc<AtomicU32>,
    visible: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    slot: FrameSlot,
    cards: CardSlot,
    suspended: Arc<AtomicU32>,
) {
    // 正常退出/初始化失败都关闭采集；release panic=abort 不会运行析构，
    // 因此尺寸与能力必须在 GPU 调用前校验，不能靠展开或 catch_unwind 保护。
    struct StopGuard(Arc<AtomicBool>, FrameSlot);
    impl Drop for StopGuard {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
            self.1
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .set_enabled(false);
        }
    }
    let _stop_guard = StopGuard(stop.clone(), slot.clone());

    // 抓屏线程：只有采集门打开时才获取新许可；淡出和卡片不打开门。
    {
        let slot = slot.clone();
        let stop = stop.clone();
        // spawn 失败（系统线程资源枯竭等）不必带崩窗口：slot 永远为空 →
        // desktop 保持 1x1 占位纹理 → render() 里 refr 自动为 0，
        // 退回纯光环模式继续跑。
        let _ = std::thread::Builder::new()
            .name("neo-overlay-cap".into())
            .spawn(move || {
                screen::ensure_dpi_aware();
                while !stop.load(Ordering::Relaxed) {
                    let Some(generation) = wait_for_capture(&slot, &stop) else {
                        break;
                    };
                    let started = Instant::now();
                    let rect = screen::virtual_screen();
                    // 静默抓帧：`capture()` 带「广播截屏信号 + 等迷你窗躲开 300ms」
                    // 的副作用，那是给「AI 应用户要求截屏」的；折射抓帧是最多 20fps 的
                    // 后台循环，走它会压死迷你窗、把折射帧率拖到 ~3fps、还会在听写
                    // 结束后凭空闪一次全屏白框（信号被反复续期后的假反馈）。
                    // 本窗自带 WDA_EXCLUDEFROMCAPTURE，画面里本来就没有自己。
                    if let Ok(bounds) = DesktopBounds::from_rect(rect) {
                        if let Ok(shot) = screen::capture_silent(rect) {
                            if let Ok(mut g) = slot.lock() {
                                g.publish(generation, bounds, shot);
                            }
                        }
                    }
                    std::thread::sleep(capture_rest(started.elapsed()));
                }
            });
    }

    let tid = unsafe { GetCurrentThreadId() };
    match Gfx::new(slot, cards) {
        Ok(gfx) => {
            if stop.load(Ordering::Acquire) || ready.send(Ok(tid)).is_err() {
                stop.store(true, Ordering::Relaxed);
                return;
            }
            msg_loop(gfx, cmd_rx, level, visible, stop, suspended);
        }
        Err(e) => {
            let _ = ready.send(Err(e));
            stop.store(true, Ordering::Relaxed);
        }
    }
}

/// 消息循环：活跃（波纹或任一卡片在）时连续渲染（交换链 vsync 限速），
/// 全无时整块睡眠等命令。
fn msg_loop(
    mut gfx: Gfx,
    cmd_rx: Receiver<Cmd>,
    level: Arc<AtomicU32>,
    visible: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    suspended: Arc<AtomicU32>,
) {
    let hwnd = gfx.hwnd;
    let mut ripple = RippleState::default();
    let mut shutdown = false;
    while !shutdown && !stop.load(Ordering::Acquire) {
        // 先排空命令队列（PostThreadMessage 只负责唤醒，命令本体在 channel 里）
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                Cmd::Show => {
                    gfx.wnd.display_dirty.store(true, Ordering::Relaxed);
                    ripple.show();
                    gfx.tgt = PH_LISTEN;
                }
                Cmd::Hide => {
                    // 采集门已由调用线程同步关闭。这里只淡出已有桌面纹理，
                    // 不因窗口仍在渲染或卡片在册而重新采集。
                    ripple.hide();
                    gfx.tgt = PH_IDLE;
                }
                Cmd::Suspend(ack, deadline, live) => {
                    if !desktop_request_live(deadline, &live) {
                        let _ = ack.send(Err("过期桌面请求，未隐藏浮层".into()));
                        continue;
                    }
                    let result = hide_for_desktop(hwnd);
                    gfx.presentation.invalidate();
                    visible.store(false, Ordering::Release);
                    let _ = ack.send(result);
                }
                Cmd::Resume => {}
                Cmd::Shutdown => shutdown = true,
            }
        }
        if shutdown {
            break;
        }

        let have_cards = gfx.has_cards();

        if desktop_can_show(&suspended) && ripple.active(have_cards) {
            // 非阻塞泵消息：线程消息不需要分发，系统消息（含鼠标输入）要处理
            let mut msg = MSG::default();
            unsafe {
                while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                    if msg.message == WM_QUIT {
                        shutdown = true;
                        break;
                    }
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            if shutdown || stop.load(Ordering::Acquire) {
                break;
            }
            if !gfx.presentation.shown {
                gfx.wnd.display_dirty.store(true, Ordering::Relaxed);
            }
            if let Err(error) = gfx.refresh_display(false) {
                eprintln!("neo-overlay 已停止，交回辅助窗口 fallback: {error}");
                break;
            }
            visible.store(gfx.presentation.shown, Ordering::Release);
            let outcome = match gfx.render(&level) {
                Ok(outcome) => outcome,
                Err(error) => {
                    eprintln!("neo-overlay 已停止，交回辅助窗口 fallback: {error}");
                    break;
                }
            };
            visible.store(gfx.presentation.shown, Ordering::Release);
            // 仅本轮真正提交当前 surface 帧才允许露面；成功重配不是 present。
            if gfx.presentation.can_show(outcome)
                && desktop_can_show(&suspended)
                && !stop.load(Ordering::Acquire)
            {
                gfx.presentation.shown = true;
                visible.store(true, Ordering::Release);
                unsafe {
                    ShowWindow(hwnd, SW_SHOWNA);
                    // 每次露面都抢回最前：后到的 topmost 窗口会把它压下去。
                    SetWindowPos(
                        hwnd,
                        HWND_TOPMOST,
                        0,
                        0,
                        0,
                        0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                    );
                }
            }
            // 淡出只清理渲染状态；不能覆盖调用线程刚投递的下一轮 Show 许可。
            if ripple.finish_fade(gfx.cur_i) {
                gfx.cur_i = 0.0;
                gfx.reset_desktop();
                if !have_cards {
                    unsafe {
                        ShowWindow(hwnd, SW_HIDE);
                    }
                    gfx.presentation.invalidate();
                    visible.store(false, Ordering::Relaxed);
                }
            }
        } else {
            if gfx.presentation.shown {
                // 防御：正常路径在淡出分支里已隐藏；命令乱序时这里兜底。
                unsafe {
                    ShowWindow(hwnd, SW_HIDE);
                }
                gfx.presentation.invalidate();
                visible.store(false, Ordering::Relaxed);
            }
            // 隐藏期整块睡眠，WM_NEO_CMD 到达即醒
            let mut msg = MSG::default();
            let r = unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
            if r <= 0 {
                // 0 = WM_QUIT，-1 = 出错
                break;
            }
            unsafe {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
    stop.store(true, Ordering::Release);
    visible.store(false, Ordering::Relaxed);
    gfx.slot
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .set_enabled(false);
}

/// 窗口线程与窗口过程共享的显示状态；绘图层没有输入状态。
struct WndState {
    /// 虚拟桌面物理边界；显示拓扑变化时与窗口一起更新。
    bounds: Mutex<DesktopBounds>,
    display_dirty: AtomicBool,
    /// 像素/点（f32 位模式）。
    ppp: AtomicU32,
}

/// HTTRANSPARENT 只作同线程兜底；跨线程穿透靠永久 layered + transparent。
unsafe extern "system" fn overlay_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_NCDESTROY {
        // SetPropW 合约要求在 WM_NCDESTROY 返回前移除本应用添加的属性。
        unsafe {
            RemovePropW(hwnd, NON_RUDE_HWND);
        }
    }
    if msg == WM_NCHITTEST {
        return HTTRANSPARENT as LRESULT;
    }
    if msg == windows_sys::Win32::UI::WindowsAndMessaging::WM_MOUSEACTIVATE {
        return windows_sys::Win32::UI::WindowsAndMessaging::MA_NOACTIVATE as LRESULT;
    }
    if matches!(msg, WM_DISPLAYCHANGE | WM_DPICHANGED | WM_SETTINGCHANGE) {
        let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const WndState;
        // SAFETY: userdata 的 Arc 由 HWND guard / Gfx::drop 在销毁时回收。
        if let Some(state) = unsafe { ptr.as_ref() } {
            state.display_dirty.store(true, Ordering::Relaxed);
        }
        return 0;
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// 桌面折射纹理。
struct DesktopTex {
    tex: wgpu::Texture,
    bind: wgpu::BindGroup,
    w: u32,
    h: u32,
}

/// 预算受限的离屏光环纹理（blit 放大上屏）。
struct Offscreen {
    _tex: wgpu::Texture,
    view: wgpu::TextureView,
    blit_bind: wgpu::BindGroup,
    w: u32,
    h: u32,
}

/// 离屏 blit shader：全屏三角形线性放大采样（独立模块避免绑定冲突）。
const BLIT_WGSL: &str = r#"
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var smp: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    var p = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0)
    );
    var out: VsOut;
    let xy = p[vi];
    out.pos = vec4<f32>(xy, 0.0, 1.0);
    out.uv = vec2<f32>(xy.x * 0.5 + 0.5, 0.5 - xy.y * 0.5);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return textureSampleLevel(src, smp, in.uv, 0.0);
}
"#;

/// wgpu 渲染上下文。
struct Gfx {
    hwnd: HWND,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    blit_pipeline: wgpu::RenderPipeline,
    bind_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniform_buf: wgpu::Buffer,
    desktop: DesktopTex,
    desktop_upload: DesktopUploadState,
    offscreen: Offscreen,
    slot: FrameSlot,
    /// 防截屏是否设置成功；失败时禁用折射（抓屏拍到的是自己，会反馈循环）。
    exclude_ok: bool,
    // 相位插值状态（VCC curI/curS/curR + tAcc 积分照搬）
    tgt: (f32, f32, f32),
    cur_i: f32,
    cur_s: f32,
    cur_r: f32,
    t_acc: f32,
    last: Instant,
    smooth_level: f32,
    // ---- 统一渲染层（卡片） ----
    /// 卡片槽（应用线程写、本线程读）。
    cards: CardSlot,
    /// 内嵌 egui 上下文（独立字体装配，与主界面互不干扰）。
    egui_ctx: egui::Context,
    egui_renderer: egui_wgpu::Renderer,
    /// egui 时钟起点（RawInput.time）。
    egui_start: Instant,
    /// 窗口过程共享状态（显示拓扑 / ppp / 原点）。
    wnd: Arc<WndState>,
    display_checked: Instant,
    presentation: Presentation,
}

impl Drop for Gfx {
    fn drop(&mut self) {
        unsafe {
            let ptr = GetWindowLongPtrW(self.hwnd, GWLP_USERDATA) as *const WndState;
            SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
            DestroyWindow(self.hwnd);
            if !ptr.is_null() {
                drop(Arc::from_raw(ptr));
            }
        }
    }
}

impl Gfx {
    /// 透镜渲染管线与绑定布局（输出预算受限的离屏 Rgba8Unorm）。
    /// 抽成独立函数：离屏测试不建窗口/surface，只验证 shader 的折射行为。
    fn lens_pipeline(device: &wgpu::Device) -> (wgpu::RenderPipeline, wgpu::BindGroupLayout) {
        Self::lens_pipeline_source(device, include_str!("shader.wgsl"))
    }

    fn lens_pipeline_source(
        device: &wgpu::Device,
        source: &str,
    ) -> (wgpu::RenderPipeline, wgpu::BindGroupLayout) {
        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("overlay-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("overlay-shader"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("overlay-pl"),
            bind_group_layouts: &[Some(&bind_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("overlay-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        (pipeline, bind_layout)
    }

    fn new(slot: FrameSlot, cards: CardSlot) -> Result<Self, String> {
        screen::ensure_dpi_aware();
        let rect = screen::virtual_screen();
        let bounds = DesktopBounds::from_rect(rect)?;
        let (width, height) = bounds.size;

        // 覆盖整个虚拟桌面（可多显示器、原点可为负）的全屏无边框置顶窗口。
        // WS_EX_LAYERED | WS_EX_TRANSPARENT = 跨线程鼠标穿透；NOACTIVATE = 不抢焦点；
        // WS_EX_TOOLWINDOW = 不进任务栏与 Alt+Tab。
        let hinstance = unsafe { GetModuleHandleW(std::ptr::null()) };
        let class_name: Vec<u16> = "neo-overlay\0".encode_utf16().collect();
        let wc = WNDCLASSEXW {
            cbSize: core::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(overlay_wnd_proc),
            hInstance: hinstance,
            lpszClassName: class_name.as_ptr(),
            ..Default::default()
        };
        // 重复注册（同进程重启引擎）返回 0 + ERROR_CLASS_ALREADY_EXISTS，正常放行；
        // 其他失败必须就地拦住，否则 CreateWindowExW 只会报出误导性的"找不到类"。
        let atom = unsafe { RegisterClassExW(&wc) };
        if atom == 0 {
            let err = unsafe { GetLastError() };
            if err != ERROR_CLASS_ALREADY_EXISTS {
                return Err(format!("RegisterClassExW 失败: win32 错误 {err}"));
            }
        }
        let hwnd = unsafe {
            CreateWindowExW(
                DRAW_EX_STYLE,
                class_name.as_ptr(),
                std::ptr::null(),
                WS_POPUP,
                rect.x,
                rect.y,
                width as i32,
                height as i32,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                hinstance,
                std::ptr::null(),
            )
        };
        if hwnd.is_null() {
            return Err("CreateWindowExW 失败".into());
        }
        // 窗口过程共享状态：origin = 窗口在虚拟屏的左上角（可为负），
        // ppp 取系统主屏 DPI（v1：卡片都锚在主屏；副屏异 DPI 备案）。
        let wnd = Arc::new(WndState {
            bounds: Mutex::new(bounds),
            display_dirty: AtomicBool::new(false),
            ppp: AtomicU32::new(((unsafe { GetDpiForSystem() } as f32) / 96.0).to_bits()),
        });
        unsafe {
            // 所有权随窗口生命周期；Gfx::drop 在销毁窗口时回收。
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, Arc::into_raw(wnd.clone()) as _);
        }
        // 之后的初始化（surface/adapter/device…）任一步失败都要拆掉窗口，
        // 否则一个隐藏的全屏 topmost 句柄泄漏到进程退出。
        // （userdata 里的 Arc 一并回收。）
        struct HwndGuard(HWND);
        impl Drop for HwndGuard {
            fn drop(&mut self) {
                unsafe {
                    let ptr = GetWindowLongPtrW(self.0, GWLP_USERDATA) as *const WndState;
                    if !ptr.is_null() {
                        drop(Arc::from_raw(ptr));
                    }
                    DestroyWindow(self.0);
                }
            }
        }
        let hwnd_guard = HwndGuard(hwnd);
        // 必须在 guard 后、surface 前：失败则销毁隐藏窗口，绝不降级为拦截输入的层。
        initialize_passive_window(hwnd, bounds.size)?;
        // 让窗口对截屏不可见（抓屏线程才能拍到干净的桌面做折射）。
        // 旧系统可能成功返回却仅按 WDA_MONITOR 处理（抓屏黑块），不能只看 BOOL。
        // 版本未知/低于 19041 或 API 失败时，只画光环，不给抓屏线程许可。
        let exclude_ok = supports_capture_exclusion(windows_version())
            && unsafe { SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) } != 0;
        slot.lock().unwrap().set_exclude_ok(exclude_ok);

        // 强制 DX12 + DirectComposition 交换链（否则窗口不支持透明）。
        // DComp 明确允许 layered HWND：
        // https://learn.microsoft.com/en-us/windows/win32/api/dcomp/nf-dcomp-idcompositiondevice-createtargetforhwnd
        let desc = wgpu::InstanceDescriptor {
            backends: wgpu::Backends::DX12,
            backend_options: wgpu::BackendOptions {
                dx12: wgpu::Dx12BackendOptions {
                    presentation_system: wgpu::Dx12SwapchainKind::DxgiFromVisual,
                    ..Default::default()
                },
                ..Default::default()
            },
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        };
        let instance = wgpu::Instance::new(desc);
        // 裸 HWND 建 surface：没有 winit 窗口可借，走 raw handle 通道（'static）。
        let win_handle = raw_window_handle::Win32WindowHandle::new(
            core::num::NonZeroIsize::new(hwnd as isize).expect("hwnd 非空"),
        );
        let target = wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_window_handle: raw_window_handle::RawWindowHandle::Win32(win_handle),
            raw_display_handle: Some(raw_window_handle::RawDisplayHandle::Windows(
                raw_window_handle::WindowsDisplayHandle::new(),
            )),
        };
        let surface =
            unsafe { instance.create_surface_unsafe(target) }.map_err(|e| e.to_string())?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: Some(&surface),
            apply_limit_buckets: false,
        }))
        .map_err(|e| e.to_string())?;
        let required_limits = overlay_limits(&adapter.limits(), bounds.size)?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_limits,
            ..Default::default()
        }))
        .map_err(|e| e.to_string())?;
        validate_dimensions(bounds.size, device.limits().max_texture_dimension_2d)?;

        // 交换链配置：BGRA + 预乘 alpha
        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| *f == wgpu::TextureFormat::Bgra8Unorm)
            .or_else(|| caps.formats.first().copied())
            .ok_or("overlay surface 没有可用格式")?;
        let alpha_mode = transparent_alpha(&caps.alpha_modes)?;
        let present_mode = if caps.present_modes.contains(&wgpu::PresentMode::Fifo) {
            wgpu::PresentMode::Fifo
        } else {
            *caps
                .present_modes
                .first()
                .ok_or("overlay surface 没有可用呈现模式")?
        };
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width,
            height,
            present_mode,
            desired_maximum_frame_latency: 2,
            alpha_mode,
            view_formats: vec![],
        };
        surface.configure(&device, &config);

        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("overlay-uniform"),
            size: 48, // 10 个 f32 + 2 个 vec2（见 shader.wgsl Uniforms）
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // blit 绑定布局：离屏纹理 + 采样器
        let blit_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("overlay-blit-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("overlay-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        // 透镜管线输出到预算受限的离屏纹理（Rgba8Unorm）
        let (pipeline, bind_layout) = Self::lens_pipeline(&device);

        // blit 管线：离屏纹理线性放大到交换链
        let blit_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("overlay-blit-shader"),
            source: wgpu::ShaderSource::Wgsl(BLIT_WGSL.into()),
        });
        let blit_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("overlay-blit-pl"),
            bind_group_layouts: &[Some(&blit_layout)],
            immediate_size: 0,
        });
        let blit_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("overlay-blit-pipeline"),
            layout: Some(&blit_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &blit_shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &blit_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        // 初始 1x1 黑色桌面纹理占位（第一帧抓屏到达后重建）
        let desktop =
            Self::placeholder_desktop(&device, &queue, &bind_layout, &uniform_buf, &sampler);

        let (offscreen_w, offscreen_h) = offscreen_size((config.width, config.height));
        let offscreen =
            Self::make_offscreen(&device, &blit_layout, &sampler, offscreen_w, offscreen_h)?;

        // 内嵌 egui：独立上下文 + 中文字体装配（卡片 UI 用它绘制）。
        let egui_ctx = egui::Context::default();
        neo_theme::fonts::install(&egui_ctx);
        let egui_renderer = egui_wgpu::Renderer::new(
            &device,
            config.format,
            egui_wgpu::RendererOptions::default(),
        );

        // 初始化全部完成：句柄的所有权移给返回的 Gfx，守卫解除。
        std::mem::forget(hwnd_guard);
        Ok(Self {
            hwnd,
            surface,
            device,
            queue,
            config,
            pipeline,
            blit_pipeline,
            bind_layout,
            sampler,
            uniform_buf,
            desktop,
            desktop_upload: DesktopUploadState::default(),
            offscreen,
            slot,
            exclude_ok,
            tgt: PH_IDLE,
            cur_i: PH_IDLE.0,
            cur_s: PH_IDLE.1,
            cur_r: PH_IDLE.2,
            t_acc: 0.0,
            last: Instant::now(),
            smooth_level: 0.0,
            cards,
            egui_ctx,
            egui_renderer,
            egui_start: Instant::now(),
            wnd,
            display_checked: Instant::now(),
            presentation: Presentation::default(),
        })
    }

    fn make_offscreen(
        device: &wgpu::Device,
        blit_layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        w: u32,
        h: u32,
    ) -> Result<Offscreen, String> {
        validate_dimensions((w, h), device.limits().max_texture_dimension_2d)?;
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("overlay-offscreen"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = tex.create_view(&Default::default());
        let blit_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("overlay-blit-bg"),
            layout: blit_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        });
        Ok(Offscreen {
            _tex: tex,
            view,
            blit_bind,
            w,
            h,
        })
    }

    fn make_bind_group(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        uniform: &wgpu::Buffer,
        view: &wgpu::TextureView,
        sampler: &wgpu::Sampler,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("overlay-bg"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        })
    }

    /// 任一卡片在册？
    fn has_cards(&self) -> bool {
        self.cards.lock().map(|c| !c.is_empty()).unwrap_or(false)
    }

    /// 跑一帧被动卡片 UI：无指针/键盘输入，不维护命中矩形。
    /// 返回 tessellation 结果与纹理增量（None = 没有卡片）。
    fn egui_frame(
        &mut self,
    ) -> Option<(
        Vec<egui::ClippedPrimitive>,
        egui::TexturesDelta,
        egui_wgpu::ScreenDescriptor,
    )> {
        // RawInput::default 会分配根 viewport map；纯光环帧无需构造它。
        let mut cards = self.cards.lock().ok()?;
        if cards.is_empty() {
            return None;
        }

        let ppp = f32::from_bits(self.wnd.ppp.load(Ordering::Relaxed));
        let (vx, vy) = self.wnd.bounds.lock().unwrap().origin;
        let (ox, oy) = (vx as f32 / ppp, vy as f32 / ppp);

        let screen_pts = egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(
                self.config.width as f32 / ppp,
                self.config.height as f32 / ppp,
            ),
        );
        let input = egui::RawInput {
            screen_rect: Some(screen_pts),
            time: Some(self.egui_start.elapsed().as_secs_f64()),
            focused: false,
            ..Default::default()
        };

        // 主屏点 → 层内点：减窗口原点（虚拟屏原点可为负）。
        // 注意：draw 闭包内不得调用 set_card（锁重入会死锁）。
        // egui 0.36 独立用法：begin_pass/end_pass（Context::run 只留给 runner）。
        self.egui_ctx.set_pixels_per_point(ppp);
        self.egui_ctx.begin_pass(input);
        {
            let ctx = &self.egui_ctx;
            for (id, card) in cards.iter_mut() {
                let [x, y, w, h] = *card.rect.lock().unwrap_or_else(|p| p.into_inner());
                let pos = egui::pos2(x - ox, y - oy);
                card_area(*id, pos)
                    .order(egui::Order::Foreground)
                    .show(ctx, |ui| {
                        ui.set_width(w);
                        ui.set_height(h);
                        (card.draw)(ui);
                    });
            }
        }
        let full = self.egui_ctx.end_pass();

        let jobs = self.egui_ctx.tessellate(full.shapes, full.pixels_per_point);
        let sd = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [self.config.width, self.config.height],
            pixels_per_point: full.pixels_per_point,
        };
        Some((jobs, full.textures_delta, sd))
    }

    /// 1x1 黑色桌面纹理占位（首帧抓屏到达前 / 隐藏后重置：
    /// 折射只对**新鲜**画面开 —— 重新 show 时若还拿着隐藏前的陈旧桌面，
    /// 折射采样到的就是几分钟前的屏幕内容）。
    fn placeholder_desktop(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bind_layout: &wgpu::BindGroupLayout,
        uniform_buf: &wgpu::Buffer,
        sampler: &wgpu::Sampler,
    ) -> DesktopTex {
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("overlay-desktop"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &[0, 0, 0, 255],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        let view = tex.create_view(&Default::default());
        let bind = Self::make_bind_group(device, bind_layout, uniform_buf, &view, sampler);
        DesktopTex {
            tex,
            bind,
            w: 1,
            h: 1,
        }
    }

    fn reset_desktop(&mut self) {
        // Lost / Occluded 重试可能反复刷新拓扑；已有占位时无需再分配/上传。
        if self.desktop_upload.reset() {
            self.desktop = Self::placeholder_desktop(
                &self.device,
                &self.queue,
                &self.bind_layout,
                &self.uniform_buf,
                &self.sampler,
            );
        }
    }

    /// 上传最新抓屏帧；尺寸变化时重建纹理与 bind group。
    fn upload_desktop(&mut self, shot: Shot) {
        // 防截屏失败的帧是"自拍"，上传只会喂养折射反馈循环；丢弃。
        if !self.exclude_ok {
            return;
        }
        // 跨线程来的数据，长度不符时 write_texture 会直接 panic —— 宁可丢帧。
        if !valid_upload(
            (shot.width, shot.height),
            shot.rgba.len(),
            self.device.limits().max_texture_dimension_2d,
        ) {
            return;
        }
        if !cacheable_desktop(&shot) {
            self.desktop_upload.clear_cache();
        }
        if same_desktop_frame(self.desktop_upload.last.as_ref(), &shot) {
            return;
        }
        if shot.width != self.desktop.w || shot.height != self.desktop.h {
            let tex = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("overlay-desktop"),
                size: wgpu::Extent3d {
                    width: shot.width,
                    height: shot.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = tex.create_view(&Default::default());
            let bind = Self::make_bind_group(
                &self.device,
                &self.bind_layout,
                &self.uniform_buf,
                &view,
                &self.sampler,
            );
            self.desktop = DesktopTex {
                tex,
                bind,
                w: shot.width,
                h: shot.height,
            };
        }
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.desktop.tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &shot.rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * self.desktop.w),
                rows_per_image: Some(self.desktop.h),
            },
            wgpu::Extent3d {
                width: self.desktop.w,
                height: self.desktop.h,
                depth_or_array_layers: 1,
            },
        );
        self.desktop_upload.uploaded(shot);
    }

    fn refresh_display(&mut self, force: bool) -> Result<(), String> {
        let dirty = self.wnd.display_dirty.swap(false, Ordering::Relaxed);
        if !force && !dirty && self.display_checked.elapsed() < Duration::from_secs(1) {
            return Ok(());
        }
        self.display_checked = Instant::now();
        // 必须在移动窗口、修改 config 或分配纹理之前拦住无效/超限拓扑。
        let bounds = DesktopBounds::from_rect(screen::virtual_screen())?;
        validate_dimensions(bounds.size, self.device.limits().max_texture_dimension_2d)?;
        let old = *self.wnd.bounds.lock().unwrap();
        let ppp = screen::dpi_scale_at(0, 0).unwrap_or(1.0) as f32;
        let old_ppp = f32::from_bits(self.wnd.ppp.load(Ordering::Relaxed));
        if bounds != old || ppp != old_ppp || force {
            self.invalidate_presentation();
        }
        if bounds != old || ppp != old_ppp || dirty {
            *self.wnd.bounds.lock().unwrap() = bounds;
            self.wnd.ppp.store(ppp.to_bits(), Ordering::Relaxed);
            // SetWindowPos 会重入窗口过程，不能持有任何 WndState 锁。
            unsafe {
                SetWindowPos(
                    self.hwnd,
                    HWND_TOPMOST,
                    bounds.origin.0,
                    bounds.origin.1,
                    bounds.size.0 as i32,
                    bounds.size.1 as i32,
                    SWP_NOACTIVATE,
                );
            }
            self.slot.lock().unwrap().invalidate();
            self.reset_desktop();
        }
        if bounds.size != old.size {
            initialize_passive_window(self.hwnd, bounds.size)?;
        }
        if bounds.size != old.size || force {
            self.config.width = bounds.size.0;
            self.config.height = bounds.size.1;
            self.surface.configure(&self.device, &self.config);
        }
        if bounds.size != old.size {
            let (offscreen_w, offscreen_h) = offscreen_size(bounds.size);
            self.offscreen = Self::make_offscreen(
                &self.device,
                &self.blit_pipeline.get_bind_group_layout(0),
                &self.sampler,
                offscreen_w,
                offscreen_h,
            )?;
        }
        Ok(())
    }

    fn invalidate_presentation(&mut self) {
        // 必须在 resize/configure 前隐藏旧 surface，重配失败也不能露出未定义底图。
        unsafe {
            ShowWindow(self.hwnd, SW_HIDE);
        }
        self.presentation.invalidate();
    }

    fn render(&mut self, level: &AtomicU32) -> Result<RenderOutcome, String> {
        self.render_with_acquire(level, |surface| surface.get_current_texture())
    }

    fn render_with_acquire(
        &mut self,
        level: &AtomicU32,
        acquire: impl FnOnce(&wgpu::Surface<'_>) -> wgpu::CurrentSurfaceTexture,
    ) -> Result<RenderOutcome, String> {
        // 只取当前许可下的帧；Hide 后保留已上传纹理淡出，不再接收迟到帧。
        let current = *self.wnd.bounds.lock().unwrap();
        let shot = self.slot.lock().ok().and_then(|mut g| g.take(current));
        if let Some(shot) = shot {
            self.upload_desktop(shot);
        }

        // 帧率无关步长
        let now = Instant::now();
        let dt = (now - self.last).as_secs_f32().min(0.05);
        self.last = now;

        // 相位插值（VCC：k = 1-exp(-dt*3.2)，tAcc 按速度积分）
        let k = 1.0 - (-dt * 3.2).exp();
        self.cur_i += (self.tgt.0 - self.cur_i) * k;
        self.cur_s += (self.tgt.1 - self.cur_s) * k;
        self.cur_r += (self.tgt.2 - self.cur_r) * k;
        self.t_acc += dt * self.cur_s.max(0.05);

        // 音频电平包络（VCC：攻击 22/s，释放 4.5/s，帧率无关）
        let target = f32::from_bits(level.load(Ordering::Relaxed));
        let kl = if target > self.smooth_level {
            1.0 - (-dt * 22.0).exp()
        } else {
            1.0 - (-dt * 4.5).exp()
        };
        self.smooth_level += (target - self.smooth_level) * kl;

        // 抓屏帧就位后才开折射（否则采样到 1x1 黑占位纹理）
        let refr = if self.desktop.w > 1 { 1.0 } else { 0.0 };

        // 波纹全停（只有卡片在）时跳过整条光环管线，两趟全屏 pass 全省。
        let draw_ripple = self.cur_i > 0.004 || self.tgt.0 > 0.0;
        if !draw_ripple && !self.has_cards() {
            return Ok(RenderOutcome::Skipped);
        }

        if draw_ripple {
            let uni: [f32; 12] = [
                self.t_acc,
                self.smooth_level,
                self.cur_i,
                self.cur_r,
                refr,
                0.0,
                0.0,
                0.0,
                self.offscreen.w as f32,
                self.offscreen.h as f32,
                self.desktop.w as f32,
                self.desktop.h as f32,
            ];
            self.queue
                .write_buffer(&self.uniform_buf, 0, bytemuck::cast_slice(&uni));
        }

        let frame = match acquire(&self.surface) {
            wgpu::CurrentSurfaceTexture::Success(f)
            | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                // 锁屏 / UAC 安全桌面期间拿不到 surface：直接 return 会让消息
                // 循环在活跃分支里 100% 空转一个核。睡 50ms 降频等系统回来。
                std::thread::sleep(Duration::from_millis(50));
                return Ok(RenderOutcome::Skipped);
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.refresh_display(true)?;
                std::thread::sleep(Duration::from_millis(50));
                return Ok(RenderOutcome::Skipped);
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                return Err("overlay surface 校验失败".into())
            }
        };
        // 获取帧之前不跑 egui：end_pass 会取走 textures_delta，失败重试不能丢字体/图片。
        let egui = self.egui_frame();
        if !draw_ripple && egui.is_none() {
            return Ok(RenderOutcome::Skipped); // 卡片可能在获取帧期间被撤销。
        }
        let view = frame.texture.create_view(&Default::default());
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("overlay-enc"),
            });
        if draw_ripple {
            // pass 1：光环 → 预算受限的离屏纹理；整张 clear 清掉未绘制中心的旧像素。
            {
                let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("overlay-pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &self.offscreen.view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &self.desktop.bind, &[]);
                for (x, y, w, h) in edge_scissors((self.offscreen.w, self.offscreen.h)) {
                    pass.set_scissor_rect(x, y, w, h);
                    pass.draw(0..3, 0..1);
                }
            }
            // pass 2：离屏纹理线性放大 → 交换链
            {
                let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("overlay-blit-pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                pass.set_pipeline(&self.blit_pipeline);
                pass.set_bind_group(0, &self.offscreen.blit_bind, &[]);
                pass.draw(0..3, 0..1);
            }
        }
        // pass 3：卡片（egui tessellation → 交换链）。波纹在时 Load 保底层，
        // 只有卡片时自己 Clear 出全透明底。
        if let Some((jobs, mut delta, sd)) = egui {
            for (id, deltas) in &delta.set {
                for image_delta in deltas.iter() {
                    self.egui_renderer
                        .update_texture(&self.device, &self.queue, *id, image_delta);
                }
            }
            let pre =
                self.egui_renderer
                    .update_buffers(&self.device, &self.queue, &mut enc, &jobs, &sd);
            {
                let load = if draw_ripple {
                    wgpu::LoadOp::Load
                } else {
                    wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)
                };
                let mut pass = enc
                    .begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("overlay-cards-pass"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &view,
                            depth_slice: None,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load,
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    })
                    .forget_lifetime();
                self.egui_renderer.render(&mut pass, &jobs, &sd);
            }
            // update_buffers 的拷贝命令必须先于主 encoder 执行。
            self.queue
                .submit(pre.into_iter().chain(std::iter::once(enc.finish())));
            for id in &delta.free {
                self.egui_renderer.free_texture(id);
            }
            // egui 0.36 的 Drop 校验要求显式确认已消费；此处上传/释放均已完成。
            delta.clear();
        } else {
            self.queue.submit(std::iter::once(enc.finish()));
        }
        // wgpu 30 的 present 返回 ()；这里的 ready 表示提交调用已返回，
        // 不是 DXGI HRESULT / GPU fence / DWM 合成完成的证明。
        self.queue.present(frame);
        self.presentation.ready = true;
        Ok(RenderOutcome::Presented)
    }
}

#[cfg(test)]
mod native_tests;

#[cfg(test)]
mod perf_tests;

#[cfg(test)]
#[path = "regression_tests.rs"]
mod regression_tests;

#[cfg(test)]
#[path = "dpi_move_tests.rs"]
mod dpi_move_tests;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
