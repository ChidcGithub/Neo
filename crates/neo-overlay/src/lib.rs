//! neo-overlay：统一渲染层 —— 全屏流光跑马灯 + 所有辅助卡片窗口。
//!
//! 架构：独立窗口线程 + 专属 wgpu/Dx12 渲染 + 内嵌独立 egui 上下文，
//! 与 egui 主界面（eframe 占着进程级 winit EventLoop 单例）完全解耦。
//!
//! **一个窗口画所有东西**：迷你窗、确认窗、课堂总结窗不再各自创建原生
//! 窗口（新建窗口在首个 wgpu 帧落地前会闪黑 1~3 帧），而是作为「卡片」
//! 画进这个常驻层 —— 没有新建窗口就没有闪黑，且与波纹同一条渲染管线。
//!
//! 关键点：
//! - DX12 透明交换链必须用 `Dx12SwapchainKind::DxgiFromVisual`（默认 DxgiFromHwnd 只有 Opaque alpha）
//! - `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)` 让抓屏拍不到自己，从而能持续抓桌面做实时折射
//! - 输入路由：无卡时 `WS_EX_TRANSPARENT` 整窗穿透；有卡时清掉它、
//!   靠 `WM_NCHITTEST` 命中卡矩形外返回 `HTTRANSPARENT` —— 卡片收点击、
//!   桌面其余区域照常穿透（Rainmeter 式做法）
//! - `WS_EX_NOACTIVATE` + `SW_SHOWNA` 永不抢焦点；卡片全是纯鼠标交互，无需键盘
//! - 窗口每次露面（含卡片首次出现）都先离屏渲好一帧再 `SW_SHOWNA` ——
//!   用户看到的第一个画面就是成品，没有黑帧
//! - 抓屏线程最多 20fps 把桌面帧喂给渲染线程，光带动画仍随交换链 vsync
//! - 控制命令走 mpsc + `PostThreadMessageW` 唤醒（消息循环在隐藏时整块睡眠）
//! - 视觉为 VCC edgeglow v2 移植：0.6x 离屏渲染光环 + 线性放大 blit 上屏（低频内容几乎无损，省 ~65% GPU）

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use neo_tools::tools::screen::{self, Shot};
use windows_sys::Win32::Foundation::{
    GetLastError, ERROR_CLASS_ALREADY_EXISTS, HWND, LPARAM, LRESULT, WPARAM,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::System::SystemInformation::OSVERSIONINFOW;
use windows_sys::Wdk::System::SystemServices::RtlGetVersion;
use windows_sys::Win32::UI::HiDpi::GetDpiForSystem;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetWindowLongPtrW, LoadCursorW, PeekMessageW, PostThreadMessageW, RegisterClassExW,
    SetCursor, SetWindowDisplayAffinity, SetWindowLongPtrW, SetWindowPos, ShowWindow,
    TranslateMessage, CS_HREDRAW, CS_VREDRAW, GWL_EXSTYLE, GWLP_USERDATA, HCURSOR,
    HTCLIENT, HTTRANSPARENT, HWND_TOPMOST, IDC_ARROW, MSG, PM_REMOVE, SWP_NOACTIVATE,
    SWP_FRAMECHANGED, SWP_NOMOVE, SWP_NOSIZE, SW_HIDE, SW_SHOWNA, WDA_EXCLUDEFROMCAPTURE, WM_APP,
    WM_DISPLAYCHANGE, WM_DPICHANGED, WM_SETTINGCHANGE,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCHITTEST, WM_QUIT,
    WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SETCURSOR, WNDCLASSEXW, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

/// 相位目标表（VCC TARGETS 照搬）：(强度, 速度, 环流)。
const PH_IDLE: (f32, f32, f32) = (0.00, 0.50, 0.015);
const PH_LISTEN: (f32, f32, f32) = (0.76, 0.85, 0.07);
/// 离屏渲染比例（VCC RENDER_SCALE=0.6：光环是低频内容，线性放大几乎无损）。
const RENDER_SCALE: f32 = 0.6;
/// 线程消息：命令队列里有货，唤醒睡眠中的消息循环。
const WM_NEO_CMD: u32 = WM_APP + 1;
const START_TIMEOUT: Duration = Duration::from_secs(5);
/// 桌面纹理独立限频；不降低光带、淡出或卡片输入的显示帧率。
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
    unsafe { ShowWindow(hwnd, SW_HIDE); }
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
            unsafe { PostThreadMessageW(self.tid, WM_NEO_CMD, 0, 0); }
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
        self.tx.send(Cmd::Suspend(tx, deadline, guard.1.clone())).map_err(|_| "桌面屏障窗口线程已退出")?;
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
                    if cancel.load(Ordering::Acquire) || self.stop.load(Ordering::Acquire) || Instant::now() >= deadline {
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

    fn client_point(self, screen: (i32, i32)) -> (i32, i32) {
        (screen.0 - self.origin.0, screen.1 - self.origin.1)
    }
}

fn validate_dimensions(size: (u32, u32), max_dimension: u32) -> Result<(), String> {
    let (width, height) = size;
    if width == 0 || height == 0 || width > max_dimension || height > max_dimension {
        return Err(format!("overlay 尺寸 {width}x{height} 超出有效范围 1..={max_dimension}"));
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

fn transparent_alpha(modes: &[wgpu::CompositeAlphaMode]) -> Result<wgpu::CompositeAlphaMode, String> {
    // shader 和 egui 都输出预乘 alpha；PostMultiplied 不能直接替代。
    modes.contains(&wgpu::CompositeAlphaMode::PreMultiplied)
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
    (unsafe { RtlGetVersion(&mut info) } >= 0)
        .then_some((info.dwMajorVersion, info.dwMinorVersion, info.dwBuildNumber))
}

fn valid_upload(size: (u32, u32), len: usize, max_dimension: u32) -> bool {
    validate_dimensions(size, max_dimension).is_ok()
        && size.0.checked_mul(4).is_some()
        && (size.0 as usize).checked_mul(size.1 as usize)
            .and_then(|pixels| pixels.checked_mul(4)) == Some(len)
}

fn capture_rest(elapsed: Duration) -> Duration {
    // 慢采集也留出原来的休息时间，不追赶欠帧、不因限频优化反而增加工作量。
    CAPTURE_INTERVAL.saturating_sub(elapsed).max(Duration::from_millis(33))
}

fn fade_complete(fading: bool, intensity: f32) -> bool {
    fading && intensity < 0.02
}

/// 一张画进渲染层的卡片（替代过去的独立原生子窗口）。
///
/// - `rect`：主显示器坐标系的 egui 点（主屏原点 = 虚拟屏原点 (0,0)，
///   层内会换算成窗口客户区像素）。**共享槽**：层的 Area 定位与命中测试
///   每帧都重读它 —— 应用线程（10fps 内容节拍）与绘制闭包（层内 vsync
///   自驱动画，如迷你窗的避让滑动）都可以随时改写；
/// - `interactive`：false 的卡（截屏闪光）只画不收点击，也不进命中矩形；
/// - `draw`：每帧在**窗口线程**的 egui Ui 里执行（Ui 已钉在卡片矩形内，
///   按「填满整个矩形」画即可，与旧视口回调的体感一致）。只准画，不准
///   阻塞；需要的数据捕获进闭包（沿用旧视口的 `Arc<AtomicU8>` 回答案
///   的模式）。**闭包内不得调用 `set_card`（卡片锁正被持着，会死锁）**。
pub struct Card {
    pub rect: Arc<Mutex<[f32; 4]>>,
    pub interactive: bool,
    pub draw: Box<dyn FnMut(&mut egui::Ui) + Send>,
}

impl Card {
    /// 交互卡的便捷构造。
    pub fn interactive(rect: [f32; 4], draw: impl FnMut(&mut egui::Ui) + Send + 'static) -> Self {
        Self {
            rect: Arc::new(Mutex::new(rect)),
            interactive: true,
            draw: Box::new(draw),
        }
    }

    /// 只画不收点击的卡（截屏闪光这类纯视觉反馈）。
    pub fn passive(rect: [f32; 4], draw: impl FnMut(&mut egui::Ui) + Send + 'static) -> Self {
        Self {
            rect: Arc::new(Mutex::new(rect)),
            interactive: false,
            draw: Box::new(draw),
        }
    }
}

fn card_area(id: u8, pos: egui::Pos2, interactive: bool) -> egui::Area {
    egui::Area::new(egui::Id::new(("neo-layer-card", id)))
        .fixed_pos(pos)
        .movable(false)
        .interactable(interactive)
        .sense(if interactive { egui::Sense::click() } else { egui::Sense::hover() })
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
            tx: self.tx.clone(), tid: self.tid, count: self.suspended.clone(), stop: self.stop.clone(),
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
        self.level.store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    pub fn is_alive(&self) -> bool {
        !self.stop.load(Ordering::Acquire)
            && self.thread.as_ref().is_some_and(|thread| !thread.is_finished())
    }

    pub fn is_visible(&self) -> bool {
        self.is_alive() && self.visible.load(Ordering::Relaxed)
    }

    /// 注册/更新/撤下一张卡片（`None` = 撤下）。窗口线程被唤醒后
    /// 下一帧生效：卡片出现会露面窗口、打开输入路由；最后一张撤下且
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
        // 锁已释放；最后一张撤下也要唤醒，让窗口隐藏并恢复穿透。
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
            unsafe { PostThreadMessageW(self.tid, WM_NEO_CMD, 0, 0); }
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

    fn await_ready(mut self, ready: Receiver<Result<u32, String>>, timeout: Duration) -> Result<Self, String> {
        self.tid = ready.recv_timeout(timeout)
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
                run(ready_tx, cmd_rx, level, visible, stop, capture, cards, suspended);
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
    }.await_ready(ready_rx, START_TIMEOUT)
}

/// 窗口线程主函数。
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
            self.1.lock().unwrap_or_else(|p| p.into_inner()).set_enabled(false);
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
                    let generation = slot.lock().ok().and_then(|s| s.begin());
                    let Some(generation) = generation else {
                        std::thread::sleep(Duration::from_millis(80));
                        continue;
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
    let mut shown = false; // 窗口当前是否可见
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
                    // 不因窗口仍在渲染或卡片可交互而重新采集。
                    ripple.hide();
                    gfx.tgt = PH_IDLE;
                }
                Cmd::Suspend(ack, deadline, live) => {
                    if !desktop_request_live(deadline, &live) {
                        let _ = ack.send(Err("过期桌面请求，未隐藏浮层".into()));
                        continue;
                    }
                    let result = hide_for_desktop(hwnd);
                    shown = false;
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
        // 输入路由随卡片有无切换（改 WS_EX_TRANSPARENT 必须 FRAMECHANGED 才生效）。
        let interactive = gfx.cards.lock()
            .map(|cards| cards.values().any(|card| card.interactive))
            .unwrap_or(false);
        gfx.set_hit_testable(hwnd, interactive);

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
            if !shown {
                gfx.wnd.display_dirty.store(true, Ordering::Relaxed);
            }
            if let Err(error) = gfx.refresh_display(false).and_then(|()| gfx.render(&level)) {
                eprintln!("neo-overlay 已停止，交回辅助窗口 fallback: {error}");
                break;
            }
            // 露面时机：先离屏渲好一帧再 SW_SHOWNA —— 用户看到的第一个
            // 画面就是成品，新建窗口的黑帧在这套结构里不存在。
            if !shown {
                shown = true;
                visible.store(true, Ordering::Relaxed);
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
                gfx.desktop = Gfx::placeholder_desktop(
                    &gfx.device,
                    &gfx.queue,
                    &gfx.bind_layout,
                    &gfx.uniform_buf,
                    &gfx.sampler,
                );
                if !have_cards {
                    unsafe {
                        ShowWindow(hwnd, SW_HIDE);
                    }
                    shown = false;
                    visible.store(false, Ordering::Relaxed);
                }
            }
        } else {
            if shown {
                // 防御：正常路径在淡出分支里已隐藏；命令乱序时这里兜底。
                unsafe {
                    ShowWindow(hwnd, SW_HIDE);
                }
                shown = false;
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
    gfx.slot.lock().unwrap_or_else(|p| p.into_inner()).set_enabled(false);
}

/// 窗口线程与窗口过程共享的状态（命中矩形 + 输入事件队列）。
struct WndState {
    /// 虚拟桌面物理边界；显示拓扑变化时与窗口一起更新。
    bounds: Mutex<DesktopBounds>,
    display_dirty: AtomicBool,
    /// 像素/点（f32 位模式）。物理像素 ↔ egui 点的换算系数。
    ppp: AtomicU32,
    /// 命中矩形（客户区物理像素 x,y,w,h）。
    hit_rects: Mutex<Vec<[i32; 4]>>,
    /// 排队给 egui 的输入事件。
    events: Mutex<Vec<egui::Event>>,
    /// 最近的指针位置（egui 需要持续 hover 态；wheel 事件也要带位置）。
    pointer: Mutex<egui::Pos2>,
    /// 指针上一帧是否在任一卡片内（离开沿补 PointerGone，否则 hover 卡住）。
    inside: AtomicBool,
}

/// 客户区坐标拆包（WM_MOUSE* 的 lparam 是符号 16 位对）。
fn unpack_client(lparam: LPARAM) -> (i32, i32) {
    let v = lparam as usize;
    (
        (v & 0xFFFF) as u16 as i16 as i32,
        ((v >> 16) & 0xFFFF) as u16 as i16 as i32,
    )
}

/// 窗口过程：命中路由（卡矩形外 HTTRANSPARENT 穿透）+ 输入事件入队。
unsafe extern "system" fn overlay_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const WndState;
    // SAFETY: userdata 在 CreateWindowExW 成功后立即设置、DestroyWindow 时才回收。
    let state = unsafe { ptr.as_ref() };
    let Some(st) = state else {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    };
    let ppp = f32::from_bits(st.ppp.load(Ordering::Relaxed));
    match msg {
        WM_DISPLAYCHANGE | WM_DPICHANGED | WM_SETTINGCHANGE => {
            st.display_dirty.store(true, Ordering::Relaxed);
            return 0;
        }
        WM_NCHITTEST => {
            // 屏幕坐标（符号 16 位对）→ 客户区
            let (sx, sy) = unpack_client(lparam);
            let (cx, cy) = st.bounds.lock().unwrap().client_point((sx, sy));
            let hit = st
                .hit_rects
                .lock()
                .map(|rects| {
                    rects
                        .iter()
                        .any(|[x, y, w, h]| cx >= *x && cx < x + w && cy >= *y && cy < y + h)
                })
                .unwrap_or(false);
            // 离开沿：通知 egui 指针消失，卡片上的 hover 高亮才不会卡死。
            if st.inside.swap(hit, Ordering::Relaxed) && !hit {
                if let Ok(mut evs) = st.events.lock() {
                    evs.push(egui::Event::PointerGone);
                }
            }
            return if hit {
                HTCLIENT as LRESULT
            } else {
                HTTRANSPARENT as LRESULT
            };
        }
        WM_MOUSEMOVE | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN | WM_RBUTTONUP => {
            let (x, y) = unpack_client(lparam);
            let pos = egui::pos2(x as f32 / ppp, y as f32 / ppp);
            if let Ok(mut p) = st.pointer.lock() {
                *p = pos;
            }
            let ev = match msg {
                WM_MOUSEMOVE => egui::Event::PointerMoved(pos),
                _ => egui::Event::PointerButton {
                    pos,
                    button: match msg {
                        WM_LBUTTONDOWN | WM_LBUTTONUP => egui::PointerButton::Primary,
                        _ => egui::PointerButton::Secondary,
                    },
                    pressed: matches!(msg, WM_LBUTTONDOWN | WM_RBUTTONDOWN),
                    modifiers: egui::Modifiers::default(),
                },
            };
            if let Ok(mut evs) = st.events.lock() {
                evs.push(ev);
            }
            return 0;
        }
        WM_MOUSEWHEEL => {
            // 高位是有符号的 120 分度增量；本层只有纵向滚动（课堂总结）。
            let delta = ((wparam >> 16) as u16 as i16) as f32 / 120.0;
            if let Ok(mut evs) = st.events.lock() {
                evs.push(egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Line,
                    delta: egui::vec2(0.0, delta),
                    modifiers: egui::Modifiers::default(),
                    phase: egui::TouchPhase::Move,
                });
            }
            return 0;
        }
        WM_SETCURSOR => {
            // 只有命中区内才轮到我们设光标（区外 HTTRANSPARENT 早穿透了）；
            // 类没注册 hCursor，不设就是「无光标」闪烁。
            if (lparam & 0xFFFF) as i32 == HTCLIENT as i32 {
                unsafe {
                    let arrow: HCURSOR = LoadCursorW(std::ptr::null_mut(), IDC_ARROW);
                    SetCursor(arrow);
                }
                return 1;
            }
        }
        _ => {}
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

/// 0.6x 离屏光环纹理（blit 放大上屏）。
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
    /// 窗口过程共享状态（命中矩形 / 输入队列 / ppp / 原点）。
    wnd: Arc<WndState>,
    /// 当前是否收鼠标（WS_EX_TRANSPARENT 是否已清掉）。
    hit_testable: bool,
    display_checked: Instant,
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
    /// 透镜渲染管线与绑定布局（输出 0.6x 离屏 Rgba8Unorm）。
    /// 抽成独立函数：离屏测试不建窗口/surface，只验证 shader 的折射行为。
    fn lens_pipeline(device: &wgpu::Device) -> (wgpu::RenderPipeline, wgpu::BindGroupLayout) {
        Self::lens_pipeline_source(device, include_str!("shader.wgsl"))
    }

    fn lens_pipeline_source(device: &wgpu::Device, source: &str) -> (wgpu::RenderPipeline, wgpu::BindGroupLayout) {
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
        // WS_EX_TRANSPARENT = 鼠标穿透；WS_EX_NOACTIVATE = 永不抢焦点；
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
                WS_EX_TOPMOST | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
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
            hit_rects: Mutex::new(Vec::new()),
            events: Mutex::new(Vec::new()),
            pointer: Mutex::new(egui::pos2(f32::NEG_INFINITY, f32::NEG_INFINITY)),
            inside: AtomicBool::new(false),
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
        // 让窗口对截屏不可见（抓屏线程才能拍到干净的桌面做折射）。
        // 旧系统可能成功返回却仅按 WDA_MONITOR 处理（抓屏黑块），不能只看 BOOL。
        // 版本未知/低于 19041 或 API 失败时，只画光环，不给抓屏线程许可。
        let exclude_ok = supports_capture_exclusion(windows_version())
            && unsafe { SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) } != 0;
        slot.lock().unwrap().set_exclude_ok(exclude_ok);

        // 强制 DX12 + DirectComposition 交换链（否则窗口不支持透明）
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
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                required_limits,
                ..Default::default()
            })).map_err(|e| e.to_string())?;
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
            *caps.present_modes.first().ok_or("overlay surface 没有可用呈现模式")?
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

        // 透镜管线输出到 0.6x 离屏纹理（Rgba8Unorm）
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

        let offscreen = Self::make_offscreen(
            &device,
            &blit_layout,
            &sampler,
            ((config.width as f32) * RENDER_SCALE).max(1.0) as u32,
            ((config.height as f32) * RENDER_SCALE).max(1.0) as u32,
        )?;

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
            hit_testable: false,
            display_checked: Instant::now(),
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

    /// 输入路由切换：有卡片时清掉 WS_EX_TRANSPARENT（改样式必须
    /// SWP_FRAMECHANGED 才生效），靠 WM_NCHITTEST 穿透卡片外区域；
    /// 无卡片时恢复整窗穿透（连 NCHITTEST 都不来，零开销）。
    fn set_hit_testable(&mut self, hwnd: HWND, on: bool) {
        if self.hit_testable == on {
            return;
        }
        self.hit_testable = on;
        unsafe {
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            let ex = if on {
                ex & !(WS_EX_TRANSPARENT as isize)
            } else {
                ex | (WS_EX_TRANSPARENT as isize)
            };
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex);
            SetWindowPos(
                hwnd,
                std::ptr::null_mut(),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            );
        }
        if !on {
            // 回穿透模式：告知 egui 指针已走，hover 不残留。
            if let Ok(mut evs) = self.wnd.events.lock() {
                evs.push(egui::Event::PointerGone);
            }
        }
    }

    /// 跑一帧卡片 UI：排水输入事件 → egui 步进 → 命中矩形刷新。
    /// 返回 tessellation 结果与纹理增量（None = 没有卡片）。
    fn egui_frame(
        &mut self,
    ) -> Option<(
        Vec<egui::ClippedPrimitive>,
        egui::TexturesDelta,
        egui_wgpu::ScreenDescriptor,
    )> {
        let ppp = f32::from_bits(self.wnd.ppp.load(Ordering::Relaxed));
        let (vx, vy) = self.wnd.bounds.lock().unwrap().origin;
        let (ox, oy) = (vx as f32 / ppp, vy as f32 / ppp);

        let events = self
            .wnd
            .events
            .lock()
            .map(|mut e| std::mem::take(&mut *e))
            .unwrap_or_default();
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
            events,
            // 本层永不抢键盘焦点（WS_EX_NOACTIVATE），但有焦点标记的控件
            // （如按钮 hover/按下）需要它才正常响应指针事件。
            focused: true,
            ..Default::default()
        };

        let mut cards = self.cards.lock().ok()?;
        if cards.is_empty() {
            if let Ok(mut rects) = self.wnd.hit_rects.lock() {
                rects.clear();
            }
            return None;
        }

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
                card_area(*id, pos, card.interactive)
                    .order(egui::Order::Foreground)
                    .show(ctx, |ui| {
                        ui.set_width(w);
                        ui.set_height(h);
                        (card.draw)(ui);
                    });
            }
        }
        let full = self.egui_ctx.end_pass();

        // 命中矩形（客户区物理像素）：层内点 × ppp；只收交互卡。
        if let Ok(mut rects) = self.wnd.hit_rects.lock() {
            *rects = cards
                .values()
                .filter(|c| c.interactive)
                .map(|c| {
                    let r = *c.rect.lock().unwrap_or_else(|p| p.into_inner());
                    [
                        ((r[0] - ox) * ppp) as i32,
                        ((r[1] - oy) * ppp) as i32,
                        (r[2] * ppp) as i32,
                        (r[3] * ppp) as i32,
                    ]
                })
                .collect();
        }

        let jobs = self
            .egui_ctx
            .tessellate(full.shapes, full.pixels_per_point);
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

    /// 上传最新抓屏帧；尺寸变化时重建纹理与 bind group。
    fn upload_desktop(&mut self, shot: Shot) {
        // 防截屏失败的帧是"自拍"，上传只会喂养折射反馈循环；丢弃。
        if !self.exclude_ok {
            return;
        }
        // 跨线程来的数据，长度不符时 write_texture 会直接 panic —— 宁可丢帧。
        if !valid_upload((shot.width, shot.height), shot.rgba.len(),
            self.device.limits().max_texture_dimension_2d) {
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
        if bounds != old || ppp != old_ppp || dirty {
            *self.wnd.bounds.lock().unwrap() = bounds;
            self.wnd.ppp.store(ppp.to_bits(), Ordering::Relaxed);
            self.wnd.hit_rects.lock().unwrap().clear();
            self.wnd.inside.store(false, Ordering::Relaxed);
            *self.wnd.pointer.lock().unwrap() = egui::pos2(f32::NEG_INFINITY, f32::NEG_INFINITY);
            let mut events = self.wnd.events.lock().unwrap();
            events.clear();
            events.push(egui::Event::PointerGone);
            drop(events);
            // SetWindowPos 会重入窗口过程，不能持有任何 WndState 锁。
            unsafe {
                SetWindowPos(self.hwnd, HWND_TOPMOST, bounds.origin.0, bounds.origin.1,
                    bounds.size.0 as i32, bounds.size.1 as i32, SWP_NOACTIVATE);
            }
            self.slot.lock().unwrap().invalidate();
            self.desktop = Self::placeholder_desktop(&self.device, &self.queue,
                &self.bind_layout, &self.uniform_buf, &self.sampler);
        }
        if bounds.size != old.size || force {
            self.config.width = bounds.size.0;
            self.config.height = bounds.size.1;
            self.surface.configure(&self.device, &self.config);
        }
        if bounds.size != old.size {
            self.offscreen = Self::make_offscreen(&self.device,
                &self.blit_pipeline.get_bind_group_layout(0), &self.sampler,
                ((bounds.size.0 as f32) * RENDER_SCALE).max(1.0) as u32,
                ((bounds.size.1 as f32) * RENDER_SCALE).max(1.0) as u32)?;
        }
        Ok(())
    }

    fn render(&mut self, level: &AtomicU32) -> Result<(), String> {
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
        let egui = self.egui_frame();
        if !draw_ripple && egui.is_none() {
            return Ok(()); // 无事可画（正常不会走到：active 前提是波纹或卡片在）
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

        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => {
                f
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                // 锁屏 / UAC 安全桌面期间拿不到 surface：直接 return 会让消息
                // 循环在活跃分支里 100% 空转一个核。睡 50ms 降频等系统回来。
                std::thread::sleep(Duration::from_millis(50));
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                return self.refresh_display(true);
            }
            wgpu::CurrentSurfaceTexture::Validation => return Err("overlay surface 校验失败".into()),
        };
        let view = frame.texture.create_view(&Default::default());
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("overlay-enc"),
            });
        if draw_ripple {
            // pass 1：光环 → 0.6x 离屏纹理
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
                pass.draw(0..3, 0..1);
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
        if let Some((jobs, delta, sd)) = egui {
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
        } else {
            self.queue.submit(std::iter::once(enc.finish()));
        }
        self.queue.present(frame);
        Ok(())
    }
}

#[cfg(test)]
mod regression_tests {
    use super::*;

    // #region debug-point A: opt-in HTTP evidence, test builds only
    fn routing_debug_event(message: &str, data: &str) {
        use std::io::{Read, Write};
        use std::net::TcpStream;
        assert_eq!(std::env::var("DEBUG_SESSION_ID").as_deref(), Ok("agent-click-routing"));
        assert_eq!(std::env::var("DEBUG_SERVER_URL").as_deref(), Ok("http://127.0.0.1:7777/event"));
        let timeout = Duration::from_millis(800);
        let mut stream = TcpStream::connect_timeout(&"127.0.0.1:7777".parse().unwrap(), timeout).unwrap();
        stream.set_read_timeout(Some(timeout)).unwrap();
        stream.set_write_timeout(Some(timeout)).unwrap();
        let body = format!(r#"{{"sessionId":"agent-click-routing","runId":"isolated-post-{}","hypothesisId":"A","location":"neo-overlay/src/lib.rs:regression_tests","message":"{message}","data":{data}}}"#, std::process::id());
        write!(stream, "POST /event HTTP/1.1\r\nHost: 127.0.0.1:7777\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.0 200") || response.starts_with("HTTP/1.1 200"));
    }

    #[test]
    #[ignore = "explicit isolated desktop diagnostic; requires local HTTP collector"]
    fn isolated_click_routing_probe() {
        routing_debug_event("pre_probe", r#"{"business_logic_changed":true,"production_hide_helper":true,"real_input":false,"desktop_switch":false}"#);
        // #region debug-point B: controlled windows on a never-activated desktop
        use windows_sys::Win32::Foundation::POINT;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SendMessageTimeoutW, WindowFromPoint, WS_EX_NOREDIRECTIONBITMAP,
            SMTO_ABORTIFHUNG, UnregisterClassW,
        };
        // These desktop APIs are test-only; avoid adding production dependency features.
        #[link(name = "user32")]
        extern "system" {
            fn CreateDesktopW(name: *const u16, device: *const u16, mode: *const std::ffi::c_void,
                flags: u32, access: u32, security: *const std::ffi::c_void) -> *mut std::ffi::c_void;
            fn SetThreadDesktop(desktop: *mut std::ffi::c_void) -> i32;
            fn CloseDesktop(desktop: *mut std::ffi::c_void) -> i32;
        }
        const QUERY_POINT: u32 = WM_APP + 91;
        static OVERLAY_HITS: AtomicU32 = AtomicU32::new(0);
        static BASE_BUTTONS: AtomicU32 = AtomicU32::new(0);
        unsafe extern "system" fn base_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
            match msg {
                QUERY_POINT => {
                    let (x, y) = unpack_client(lp);
                    unsafe { WindowFromPoint(POINT { x, y }) as LRESULT }
                }
                WM_NCHITTEST => HTCLIENT as LRESULT,
                WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN | WM_RBUTTONUP => {
                    BASE_BUTTONS.fetch_add(1, Ordering::Relaxed);
                    0
                }
                _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
            }
        }
        unsafe extern "system" fn probe_overlay_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
            if msg == WM_NCHITTEST {
                OVERLAY_HITS.fetch_add(1, Ordering::Relaxed);
            }
            unsafe { overlay_wnd_proc(hwnd, msg, wp, lp) }
        }
        struct Desktop(*mut std::ffi::c_void);
        impl Drop for Desktop {
            fn drop(&mut self) { unsafe { CloseDesktop(self.0); } }
        }
        struct Window(HWND);
        impl Drop for Window {
            fn drop(&mut self) {
                unsafe {
                    SetWindowLongPtrW(self.0, GWLP_USERDATA, 0);
                    DestroyWindow(self.0);
                }
            }
        }
        struct Worker(Arc<AtomicBool>, Option<JoinHandle<()>>);
        impl Drop for Worker {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
                if let Some(thread) = self.1.take() { let _ = thread.join(); }
            }
        }
        fn send(hwnd: HWND, msg: u32, lp: LPARAM) -> LRESULT {
            let mut result = 0;
            assert_ne!(unsafe { SendMessageTimeoutW(hwnd, msg, 0, lp, SMTO_ABORTIFHUNG, 1000, &mut result) }, 0,
                "controlled SendMessageTimeoutW failed: {}", unsafe { GetLastError() });
            result as LRESULT
        }
        let name: Vec<u16> = format!("neo-routing-probe-{}\0", std::process::id()).encode_utf16().collect();
        // Access deliberately excludes DESKTOP_SWITCHDESKTOP. No input APIs are used.
        let desktop = Desktop(unsafe { CreateDesktopW(name.as_ptr(), std::ptr::null(),
            std::ptr::null(), 0, 0x0083, std::ptr::null()) });
        routing_debug_event("desktop_created", &format!(r#"{{"ok":{},"error":{},"switch_access":false}}"#,
            !desktop.0.is_null(), unsafe { GetLastError() }));
        assert!(!desktop.0.is_null());
        let desk = desktop.0 as usize;
        let result = std::thread::spawn(move || {
            assert_ne!(unsafe { SetThreadDesktop(desk as _) }, 0);
            let base_name: Vec<u16> = "neo-routing-probe-base\0".encode_utf16().collect();
            let overlay_name: Vec<u16> = "neo-routing-probe-overlay\0".encode_utf16().collect();
            let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
            for (name, proc) in [(&base_name, base_proc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT),
                (&overlay_name, probe_overlay_proc)] {
                let wc = WNDCLASSEXW { cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                    lpfnWndProc: Some(proc), hInstance: instance, lpszClassName: name.as_ptr(), ..Default::default() };
                assert_ne!(unsafe { RegisterClassExW(&wc) }, 0);
            }
            let create = |name: &[u16], ex| {
                let hwnd = unsafe { CreateWindowExW(ex, name.as_ptr(), std::ptr::null(), WS_POPUP,
                    0, 0, 640, 480, std::ptr::null_mut(), std::ptr::null_mut(),
                    GetModuleHandleW(std::ptr::null()), std::ptr::null()) };
                assert!(!hwnd.is_null());
                Window(hwnd)
            };
            let stop = Arc::new(AtomicBool::new(false));
            let worker_stop = stop.clone();
            let (tx, rx) = channel();
            let worker_name = base_name.clone();
            let worker = Worker(stop, Some(std::thread::spawn(move || {
                assert_ne!(unsafe { SetThreadDesktop(desk as _) }, 0);
                let base = create(&worker_name, WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW);
                unsafe { ShowWindow(base.0, SW_SHOWNA); }
                tx.send((base.0 as usize, unsafe { GetCurrentThreadId() })).unwrap();
                while !worker_stop.load(Ordering::Acquire) {
                    let mut msg = MSG::default();
                    unsafe {
                        while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                            TranslateMessage(&msg);
                            DispatchMessageW(&msg);
                        }
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
            })));
            let (foreign, foreign_tid) = rx.recv_timeout(Duration::from_secs(3)).unwrap();
            let foreign = foreign as HWND;
            let same = create(&base_name, WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW);
            let state = Box::new(WndState {
                bounds: Mutex::new(DesktopBounds { origin: (0, 0), size: (640, 480) }),
                display_dirty: AtomicBool::new(false), ppp: AtomicU32::new(1.0f32.to_bits()),
                hit_rects: Mutex::new(vec![[400, 300, 100, 100]]), events: Mutex::new(Vec::new()),
                pointer: Mutex::new(egui::Pos2::ZERO), inside: AtomicBool::new(false),
            });
            let ex = WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_NOREDIRECTIONBITMAP;
            let overlay = create(&overlay_name, ex);
            unsafe {
                SetWindowLongPtrW(overlay.0, GWLP_USERDATA, &*state as *const WndState as isize);
                ShowWindow(overlay.0, SW_SHOWNA);
                SetWindowPos(overlay.0, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            }
            routing_debug_event("windows_ready", &format!(r#"{{"owner_tid":{},"target_tid":{},"overlay":{},"foreign_base":{},"same_base":{},"exstyle":{},"card":[400,300,100,100]}}"#,
                unsafe { GetCurrentThreadId() }, foreign_tid, overlay.0 as usize, foreign as usize,
                same.0 as usize, unsafe { GetWindowLongPtrW(overlay.0, GWL_EXSTYLE) }));
            let observe = |case: &str, x: i32, y: i32| {
                let lp = ((y as u32) << 16 | x as u32) as LPARAM;
                let before = OVERLAY_HITS.load(Ordering::Relaxed);
                let owner_found = unsafe { WindowFromPoint(POINT { x, y }) };
                let owner_calls = OVERLAY_HITS.load(Ordering::Relaxed) - before;
                let before = OVERLAY_HITS.load(Ordering::Relaxed);
                let foreign_found = send(foreign, QUERY_POINT, lp) as HWND;
                let foreign_calls = OVERLAY_HITS.load(Ordering::Relaxed) - before;
                let direct_hit = send(overlay.0, WM_NCHITTEST, lp);
                routing_debug_event("hit_observation", &format!(r#"{{"case":"{case}","point":[{x},{y}],"exstyle":{},"owner_found":{},"foreign_found":{},"owner_hit_calls":{owner_calls},"foreign_hit_calls":{foreign_calls},"explicit_overlay_hit":{direct_hit}}}"#,
                    unsafe { GetWindowLongPtrW(overlay.0, GWL_EXSTYLE) }, owner_found as usize, foreign_found as usize));
                foreign_found
            };
            observe("cross_thread_card_inside", 450, 350);
            let found = observe("cross_thread_card_outside", 100, 100);
            // Explicit delivery to the queried controlled HWND is not hardware-input routing.
            assert!(found == overlay.0 || found == foreign || found == same.0);
            state.events.lock().unwrap().clear();
            BASE_BUTTONS.store(0, Ordering::Relaxed);
            for msg in [WM_LBUTTONDOWN, WM_LBUTTONUP, WM_RBUTTONDOWN, WM_RBUTTONUP] {
                send(found, msg, (100 << 16) | 100);
            }
            routing_debug_event("controlled_delivery_only", &format!(r#"{{"selected":{},"overlay_events":{},"base_buttons":{},"hardware_routing_proven":false}}"#,
                found as usize, state.events.lock().unwrap().len(), BASE_BUTTONS.load(Ordering::Relaxed)));
            unsafe { ShowWindow(foreign, SW_HIDE); ShowWindow(same.0, SW_SHOWNA); }
            observe("same_thread_card_outside", 100, 100);
            unsafe {
                ShowWindow(same.0, SW_HIDE); ShowWindow(foreign, SW_SHOWNA);
                SetWindowLongPtrW(overlay.0, GWL_EXSTYLE, (ex | WS_EX_TRANSPARENT) as isize);
                SetWindowPos(overlay.0, HWND_TOPMOST, 0, 0, 0, 0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED);
            }
            observe("cross_thread_transparent_style", 100, 100);
            hide_for_desktop(overlay.0).unwrap();
            let hidden_hit = observe("overlay_hidden_control", 100, 100);
            assert_eq!(hidden_hit, foreign);
            routing_debug_event("barrier_hide_verified", r#"{"production_hide_helper":true,"root_matches_target":true,"real_input":false}"#);
            drop(overlay);
            drop(same);
            drop(worker);
            unsafe { UnregisterClassW(overlay_name.as_ptr(), instance); UnregisterClassW(base_name.as_ptr(), instance); }
        }).join();
        drop(desktop);
        routing_debug_event("post_probe", &format!(r#"{{"completed":{},"real_input":false,"desktop_switch":false}}"#, result.is_ok()));
        result.unwrap();
        // #endregion debug-point B
    }
    // #endregion debug-point A

    #[test]
    fn dimensions_request_adapter_capacity_and_reject_insufficient_limits() {
        let supported = wgpu::Limits { max_texture_dimension_2d: 16384, ..wgpu::Limits::default() };
        let requested = overlay_limits(&supported, (11520, 2160)).unwrap();
        assert_eq!(requested.max_texture_dimension_2d, 16384);
        assert!(requested.check_limits(&supported));
        let limited = wgpu::Limits { max_texture_dimension_2d: 4096, ..supported.clone() };
        assert!(overlay_limits(&limited, (3840, 2160)).is_ok());
        assert!(overlay_limits(&limited, (11520, 2160)).is_err());
        let insufficient = wgpu::Limits { max_bind_groups: 0, ..supported };
        assert!(overlay_limits(&insufficient, (1920, 1080)).is_err());
    }

    #[test]
    fn dimensions_hotplug_checks_device_limit_before_reconfiguration() {
        let supported = wgpu::Limits { max_texture_dimension_2d: 16384, ..wgpu::Limits::default() };
        let device = overlay_limits(&supported, (1920, 1080)).unwrap();
        for size in [(11520, 2160), (16384, 2160), (2160, 16384)] {
            assert!(validate_dimensions(size, device.max_texture_dimension_2d).is_ok());
        }
        for size in [(16385, 2160), (2160, 16385), (0, 2160), (1920, 0)] {
            assert!(validate_dimensions(size, device.max_texture_dimension_2d).is_err());
        }
        for (width, height) in [(0, 1080), (1920, 0), (-1, 1080), (1920, -1)] {
            assert!(DesktopBounds::from_rect(screen::Rect { x: -1920, y: 0, width, height }).is_err());
        }
    }

    #[test]
    fn dimensions_upload_rejects_zero_oversize_and_malformed_frames() {
        assert!(valid_upload((11520, 2160), 11520 * 2160 * 4, 16384));
        assert!(!valid_upload((11520, 2160), 11520 * 2160 * 4, 8192));
        assert!(!valid_upload((0, 0), 0, 16384));
        assert!(!valid_upload((2, 2), 15, 16384));
        assert!(!valid_upload((u32::MAX, u32::MAX), 0, u32::MAX));
    }

    #[test]
    fn capture_exclusion_requires_19041_and_successful_affinity() {
        for (version, supported) in [
            (None, false), (Some((6, 3, 9600)), false),
            (Some((10, 0, 19040)), false), (Some((10, 0, 19041)), true),
            (Some((10, 0, 19045)), true), (Some((10, 0, 22000)), true),
        ] {
            assert_eq!(supports_capture_exclusion(version), supported);
            for affinity_ok in [false, true] {
                let mut state = CaptureState::default();
                state.set_enabled(true);
                state.set_exclude_ok(supports_capture_exclusion(version) && affinity_ok);
                assert_eq!(state.begin().is_some(), supported && affinity_ok);
            }
        }
    }

    #[test]
    fn transparency_never_falls_back_to_opaque_or_wrong_alpha_convention() {
        use wgpu::CompositeAlphaMode::*;
        for modes in [vec![], vec![Opaque], vec![Auto, Inherit], vec![PostMultiplied]] {
            assert!(transparent_alpha(&modes).is_err());
        }
        assert_eq!(transparent_alpha(&[Opaque, PreMultiplied]).unwrap(), PreMultiplied);
    }

    fn safe_capture() -> CaptureState {
        let mut state = CaptureState::default();
        state.set_exclude_ok(true);
        state
    }

    #[test]
    fn capture_safety_blocks_work_even_after_show_and_topology_changes() {
        let mut state = CaptureState::default();
        for _ in 0..100 {
            state.set_enabled(true);
            state.invalidate();
            assert!(state.begin().is_none());
        }
        state.set_exclude_ok(true);
        let old = state.begin().unwrap();
        state.publish(old, bounds(), shot(1));
        state.set_exclude_ok(false);
        state.publish(old, bounds(), shot(2));
        assert!(state.take(bounds()).is_none());
        state.set_enabled(false);
        state.set_exclude_ok(true);
        assert!(state.begin().is_none(), "安全许可不能覆盖 Hide");
        state.set_enabled(true);
        state.publish(old, bounds(), shot(3));
        assert!(state.take(bounds()).is_none());
    }

    #[test]
    fn capture_cadence_is_at_most_twenty_hz_without_catch_up() {
        for millis in [0, 5, 17, 33, 50, 100, 1000] {
            let elapsed = Duration::from_millis(millis);
            let cycle = elapsed + capture_rest(elapsed);
            assert!(cycle >= CAPTURE_INTERVAL);
            assert!(cycle >= elapsed + Duration::from_millis(33));
        }
        let cycles = |rest: Duration| (0..1000).step_by(rest.as_millis() as usize).count();
        assert_eq!(cycles(Duration::from_millis(33)), 31);
        assert_eq!(cycles(capture_rest(Duration::ZERO)), 20);
        eprintln!("合成 1s 零耗时采集：旧 31 次，新 20 次；慢采集不加速");
    }

    fn fake_handle() -> (OverlayHandle, Receiver<Cmd>, Sender<()>, Receiver<()>) {
        let (tx, rx) = channel();
        let (release, wait) = channel();
        let (done, finished) = channel();
        let thread = std::thread::spawn(move || {
            let _ = wait.recv_timeout(Duration::from_secs(2));
            let _ = done.send(());
        });
        (OverlayHandle {
            tx, tid: 0,
            level: Arc::new(AtomicU32::new(0)),
            visible: Arc::new(AtomicBool::new(true)),
            stop: Arc::new(AtomicBool::new(false)),
            capture: Arc::new(Mutex::new(safe_capture())),
            cards: Arc::new(Mutex::new(BTreeMap::new())),
            suspended: Arc::new(AtomicU32::new(0)),
            thread: Some(thread),
            wake_count: Some(AtomicU32::new(0)),
        }, rx, release, finished)
    }

    fn wake_count(handle: &OverlayHandle) -> u32 {
        handle.wake_count.as_ref().unwrap().load(Ordering::Relaxed)
    }

    #[test]
    fn desktop_barrier_ack_then_thread_exit_and_cancel_fail_closed() {
        let (handle, commands, release, finished) = fake_handle();
        let gate = handle.desktop_gate();
        let cancel = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let waiting = scope.spawn(|| gate.acquire(&cancel, Duration::from_secs(1)));
            let Cmd::Suspend(ack, deadline, live) = commands.recv_timeout(Duration::from_secs(1)).unwrap() else { panic!("必须先申请隐藏") };
            assert!(desktop_request_live(deadline, &live));
            assert!(!desktop_can_show(&gate.count));
            ack.send(Ok(())).unwrap();
            let guard = waiting.join().unwrap().unwrap();
            assert!(!desktop_can_show(&gate.count));
            drop(guard);
            assert!(desktop_can_show(&gate.count));
        });
        while commands.try_recv().is_ok() {}
        std::thread::scope(|scope| {
            let waiting = scope.spawn(|| gate.acquire(&cancel, Duration::from_secs(1)));
            let Cmd::Suspend(ack, _, _) = commands.recv_timeout(Duration::from_secs(1)).unwrap() else { panic!("必须先申请隐藏") };
            gate.stop.store(true, Ordering::Release);
            let _ = ack.send(Ok(()));
            assert!(waiting.join().unwrap().is_err());
        });
        assert!(desktop_can_show(&gate.count));
        release.send(()).unwrap();
        finished.recv_timeout(Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn desktop_barrier_timeout_and_last_guard_restore() {
        let (handle, commands, release, finished) = fake_handle();
        let gate = handle.desktop_gate();
        let cancel = AtomicBool::new(false);
        let started = Instant::now();
        assert!(gate.acquire(&cancel, Duration::from_millis(15)).is_err());
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(gate.count.load(Ordering::Acquire), 0);
        let Cmd::Suspend(_, deadline, live) = commands.recv().unwrap() else { panic!("缺少过期请求") };
        assert!(!desktop_request_live(deadline, &live), "迟到请求不能隐藏窗口");
        assert!(!desktop_request_live(Instant::now(), &AtomicBool::new(true)));
        while commands.try_recv().is_ok() {}
        let first = gate.reserve();
        let second = gate.reserve();
        handle.show();
        handle.set_card(card_id::MINI, Some(Card::interactive([0.0; 4], |_| {})));
        assert!(!desktop_can_show(&gate.count));
        drop(first);
        assert!(!desktop_can_show(&gate.count));
        drop(second);
        assert!(desktop_can_show(&gate.count));
        cancel.store(true, Ordering::Release);
        assert!(gate.acquire(&cancel, Duration::from_secs(1)).is_err());
        assert_eq!(gate.count.load(Ordering::Acquire), 0);
        release.send(()).unwrap();
        finished.recv_timeout(Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn toast_passive_card_never_opens_capture_or_posts_show_hide() {
        let (handle, commands, release, finished) = fake_handle();
        handle.set_card(card_id::TOAST, Some(Card::passive([10.0, 20.0, 300.0, 100.0], |_| {})));
        assert!(!handle.cards.lock().unwrap().values().any(|card| card.interactive));
        assert!(handle.capture.lock().unwrap().begin().is_none());
        assert!(commands.try_recv().is_err());
        assert_eq!(wake_count(&handle), 1);
        handle.set_card(card_id::TOAST, None);
        handle.set_card(card_id::TOAST, None);
        assert!(handle.cards.lock().unwrap().is_empty());
        assert_eq!(wake_count(&handle), 2);
        assert!(commands.try_recv().is_err());
        release.send(()).unwrap();
        finished.recv_timeout(Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn toast_passive_area_allows_click_through_to_underlying_card() {
        let ctx = egui::Context::default();
        let target = egui::Rect::from_min_size(egui::pos2(100.0, 100.0), egui::vec2(300.0, 100.0));
        let point = target.center();
        let mut clicked = false;
        for frame in 0..5 {
            let events = if frame >= 3 {
                vec![egui::Event::PointerMoved(point), egui::Event::PointerButton {
                    pos: point, button: egui::PointerButton::Primary,
                    pressed: frame == 3, modifiers: egui::Modifiers::default(),
                }]
            } else { vec![] };
            ctx.begin_pass(egui::RawInput { events, ..Default::default() });
            card_area(card_id::CONFIRM, target.min, true).order(egui::Order::Foreground).show(&ctx, |ui| {
                clicked |= ui.allocate_exact_size(target.size(), egui::Sense::click()).1.clicked();
            });
            card_area(card_id::TOAST, target.min, false).order(egui::Order::Foreground).show(&ctx, |ui| {
                ui.allocate_exact_size(target.size(), egui::Sense::hover());
            });
            let mut output = ctx.end_pass();
            output.textures_delta.clear();
        }
        assert!(clicked, "被动 toast Area 不得挡住底层确认卡");
    }

    #[test]
    fn card_repeated_empty_removals_do_not_notify() {
        let (handle, _commands, release, finished) = fake_handle();
        for _ in 0..5 {
            for id in [card_id::CONFIRM, card_id::MINI, card_id::CLASS, card_id::FLASH, card_id::DOT, card_id::TOAST] {
                handle.set_card(id, None);
            }
        }
        assert!(handle.cards.lock().unwrap().is_empty());
        assert_eq!(wake_count(&handle), 0, "5Hz × 6 张空卡不应产生 30 次通知");
        release.send(()).unwrap();
        finished.recv_timeout(Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn card_updates_and_last_removal_notify_without_changing_ripple() {
        for showing in [false, true] {
            let (handle, _commands, release, finished) = fake_handle();
            let mut ripple = RippleState::default();
            if showing {
                ripple.show();
            }
            handle.set_card(card_id::MINI, Some(Card::interactive([0.0; 4], |_| {})));
            assert_eq!(wake_count(&handle), 1);
            let replacement = Card::passive([0.0; 4], |_| {});
            let rect = replacement.rect.clone();
            handle.set_card(card_id::MINI, Some(replacement));
            assert_eq!(wake_count(&handle), 2);
            {
                let cards = handle.cards.lock().unwrap();
                assert_eq!(cards.len(), 1);
                assert!(Arc::ptr_eq(&cards[&card_id::MINI].rect, &rect));
                assert!(!cards[&card_id::MINI].interactive);
            }
            handle.set_card(card_id::DOT, Some(Card::passive([0.0; 4], |_| {})));
            assert_eq!(wake_count(&handle), 3);
            handle.set_card(card_id::FLASH, None);
            assert_eq!(wake_count(&handle), 3);
            handle.set_card(card_id::MINI, None);
            assert_eq!(wake_count(&handle), 4);
            assert!(ripple.active(!handle.cards.lock().unwrap().is_empty()));
            handle.set_card(card_id::DOT, None);
            assert_eq!(wake_count(&handle), 5);
            assert!(handle.cards.lock().unwrap().is_empty());
            assert_eq!(ripple.active(false), showing);
            handle.set_card(card_id::DOT, None);
            assert_eq!(wake_count(&handle), 5);
            release.send(()).unwrap();
            finished.recv_timeout(Duration::from_secs(1)).unwrap();
        }
    }

    #[test]
    fn card_poisoned_lock_does_not_notify_or_mutate() {
        let (handle, _commands, release, finished) = fake_handle();
        handle.set_card(card_id::MINI, Some(Card::interactive([0.0; 4], |_| {})));
        let cards = handle.cards.clone();
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = cards.lock().unwrap();
            panic!("fake poisoned card slot");
        })).is_err());
        handle.set_card(card_id::MINI, None);
        handle.set_card(card_id::DOT, Some(Card::passive([0.0; 4], |_| {})));
        assert_eq!(wake_count(&handle), 1);
        let cards = handle.cards.lock().unwrap_or_else(|p| p.into_inner());
        assert_eq!(cards.len(), 1);
        assert!(cards.contains_key(&card_id::MINI));
        drop(cards);
        release.send(()).unwrap();
        finished.recv_timeout(Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn card_shutdown_handle_does_not_notify_or_mutate() {
        let (mut handle, _commands, release, finished) = fake_handle();
        handle.set_card(card_id::MINI, Some(Card::interactive([0.0; 4], |_| {})));
        handle.shutdown();
        let before = wake_count(&handle);
        handle.set_card(card_id::MINI, None);
        handle.set_card(card_id::DOT, Some(Card::passive([0.0; 4], |_| {})));
        assert_eq!(wake_count(&handle), before);
        let cards = handle.cards.lock().unwrap();
        assert_eq!(cards.len(), 1);
        assert!(cards.contains_key(&card_id::MINI));
        drop(cards);
        release.send(()).unwrap();
        finished.recv_timeout(Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn lifecycle_shutdown_and_drop_do_not_wait_for_stalled_thread() {
        let (mut handle, commands, release, finished) = fake_handle();
        assert!(handle.is_alive());
        handle.show();
        assert!(matches!(commands.try_recv(), Ok(Cmd::Show)));
        let capture = handle.capture.clone();
        let generation = capture.lock().unwrap().begin().unwrap();
        let start = Instant::now();
        handle.shutdown();
        assert!(start.elapsed() < Duration::from_millis(500));
        assert!(!handle.is_alive());
        assert!(!handle.is_visible());
        assert!(matches!(commands.try_recv(), Ok(Cmd::Shutdown)));
        handle.show();
        let mut state = capture.lock().unwrap();
        state.publish(generation, bounds(), shot(1));
        assert!(state.begin().is_none());
        assert!(state.take(bounds()).is_none());
        drop(state);
        drop(handle);
        assert!(finished.try_recv().is_err());
        release.send(()).unwrap();
        finished.recv_timeout(Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn lifecycle_drop_closes_gate_without_waiting_for_worker() {
        let (handle, _commands, release, finished) = fake_handle();
        handle.show();
        let capture = handle.capture.clone();
        let stop = handle.stop.clone();
        let start = Instant::now();
        drop(handle);
        assert!(start.elapsed() < Duration::from_millis(500));
        assert!(stop.load(Ordering::Acquire));
        assert!(capture.lock().unwrap().begin().is_none());
        assert!(finished.try_recv().is_err());
        release.send(()).unwrap();
        finished.recv_timeout(Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn lifecycle_ready_timeout_error_disconnect_and_success() {
        for outcome in 0..4 {
            let (handle, _commands, release, finished) = fake_handle();
            let stop = handle.stop.clone();
            let capture = handle.capture.clone();
            capture.lock().unwrap().set_enabled(true);
            let (ready, rx) = channel();
            match outcome {
                0 => {},
                1 => ready.send(Err("fake initialization failure".into())).unwrap(),
                2 => {},
                _ => ready.send(Ok(0)).unwrap(),
            }
            let ready = if outcome == 2 { drop(ready); None } else { Some(ready) };
            let start = Instant::now();
            let result = handle.await_ready(rx, Duration::from_millis(10));
            assert!(start.elapsed() < Duration::from_millis(500));
            assert_eq!(result.is_ok(), outcome == 3);
            if outcome != 3 {
                assert!(stop.load(Ordering::Acquire));
                assert!(capture.lock().unwrap().begin().is_none());
                if let Some(ready) = ready {
                    assert!(ready.send(Ok(0)).is_err());
                }
            }
            drop(result);
            release.send(()).unwrap();
            finished.recv_timeout(Duration::from_secs(1)).unwrap();
        }
    }

    #[test]
    fn lifecycle_stalled_initialization_keeps_single_engine_lease() {
        static RUNNING: AtomicBool = AtomicBool::new(false);
        let lease = EngineLease::acquire(&RUNNING).unwrap();
        let (release, wait) = channel();
        let worker = std::thread::spawn(move || {
            let _lease = lease;
            let _ = wait.recv_timeout(Duration::from_secs(2));
        });
        for _ in 0..100 {
            assert!(EngineLease::acquire(&RUNNING).is_err());
        }
        release.send(()).unwrap();
        worker.join().unwrap();
        assert!(EngineLease::acquire(&RUNNING).is_ok());
    }

    #[test]
    fn lifecycle_finished_thread_is_not_alive_even_without_stop_signal() {
        let (mut handle, _commands, release, finished) = fake_handle();
        handle.set_card(card_id::MINI, Some(Card::interactive([0.0; 4], |_| {})));
        release.send(()).unwrap();
        finished.recv_timeout(Duration::from_secs(1)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while !handle.thread.as_ref().unwrap().is_finished() && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(!handle.stop.load(Ordering::Acquire));
        assert!(!handle.is_alive());
        assert!(!handle.is_visible());
        let before = wake_count(&handle);
        handle.set_card(card_id::MINI, None);
        handle.set_card(card_id::DOT, Some(Card::passive([0.0; 4], |_| {})));
        assert_eq!(wake_count(&handle), before);
        let cards = handle.cards.lock().unwrap();
        assert_eq!(cards.len(), 1);
        assert!(cards.contains_key(&card_id::MINI));
        drop(cards);
        handle.show();
        assert!(handle.capture.lock().unwrap().begin().is_none());
        handle.shutdown();
    }

    fn bounds() -> DesktopBounds {
        DesktopBounds { origin: (0, 0), size: (2, 2) }
    }

    fn shot(value: u8) -> Shot {
        Shot { width: 2, height: 2, rgba: vec![value; 16] }
    }

    #[test]
    fn hide_stops_capture_before_fade_with_or_without_cards() {
        for have_cards in [false, true] {
            let mut capture = safe_capture();
            let mut ripple = RippleState::default();
            assert!(capture.begin().is_none());
            assert_eq!(ripple.active(have_cards), have_cards);
            capture.set_enabled(true);
            ripple.show();
            let in_flight = capture.begin().unwrap();
            capture.publish(in_flight, bounds(), shot(1));
            let frozen = capture.take(bounds()).unwrap();
            capture.publish(in_flight, bounds(), shot(2));

            capture.set_enabled(false);
            ripple.hide();
            assert!(ripple.active(have_cards));
            assert!(!ripple.finish_fade(0.5));
            assert!(capture.begin().is_none());
            assert!(capture.take(bounds()).is_none());
            // 在途系统调用不能撤销，但返回后不能再更新淡出画面。
            capture.publish(in_flight, bounds(), shot(3));
            assert!(capture.take(bounds()).is_none());
            assert_eq!(frozen.rgba, vec![1; 16]);
            for _ in 0..40 {
                assert!(capture.begin().is_none());
            }
            assert!(ripple.finish_fade(0.01));
            assert_eq!(ripple.active(have_cards), have_cards);
            assert!(!ripple.finish_fade(0.0));
            assert!(capture.begin().is_none());
        }
    }

    #[test]
    fn reshow_rejects_old_in_flight_and_queued_frames() {
        let mut capture = safe_capture();
        let mut ripple = RippleState::default();
        capture.set_enabled(true);
        ripple.show();
        let old = capture.begin().unwrap();
        capture.publish(old, bounds(), shot(1));
        capture.set_enabled(false);
        ripple.hide();
        capture.set_enabled(true);
        ripple.show();
        let new = capture.begin().unwrap();
        assert_ne!(old, new);
        assert!(capture.take(bounds()).is_none());
        capture.publish(old, bounds(), shot(2));
        assert!(capture.take(bounds()).is_none());
        capture.publish(new, bounds(), shot(3));
        // 迟到旧帧也不能覆盖本轮已发布的新帧。
        capture.publish(old, bounds(), shot(4));
        assert_eq!(capture.take(bounds()).unwrap().rgba, vec![3; 16]);
        assert!(!ripple.finish_fade(0.0));
        assert!(ripple.active(false));
    }

    #[test]
    fn queued_show_cannot_reopen_gate_after_hide() {
        let mut capture = safe_capture();
        let mut ripple = RippleState::default();
        // 模拟窗口线程尚未排空命令时调用线程已连续 Show/Hide。
        capture.set_enabled(true);
        capture.set_enabled(false);
        ripple.show();
        assert!(capture.begin().is_none());
        ripple.hide();
        assert!(capture.begin().is_none());
        // 淡出收尾也不得关掉已经投递的下一轮 Show 许可。
        capture.set_enabled(true);
        let new = capture.begin().unwrap();
        assert!(ripple.finish_fade(0.0));
        assert_eq!(capture.begin(), Some(new));
        ripple.show();
        assert!(ripple.active(false));
    }

    #[test]
    fn repeated_hide_and_show_invalidate_previous_work() {
        let mut capture = safe_capture();
        let mut ripple = RippleState::default();
        for _ in 0..2 {
            capture.set_enabled(false);
            ripple.hide();
            assert!(!ripple.active(false));
            assert!(!ripple.finish_fade(0.0));
            assert!(capture.begin().is_none());
        }
        capture.set_enabled(true);
        let old = capture.begin().unwrap();
        capture.set_enabled(true);
        capture.publish(old, bounds(), shot(1));
        assert!(capture.take(bounds()).is_none());
        let current = capture.begin().unwrap();
        capture.publish(current, bounds(), shot(2));
        assert!(capture.take(bounds()).is_some());
    }

    #[test]
    fn topology_invalidation_rejects_in_flight_even_if_bounds_return() {
        let mut capture = safe_capture();
        capture.set_enabled(true);
        let old = capture.begin().unwrap();
        capture.invalidate();
        capture.publish(old, bounds(), shot(1));
        assert!(capture.take(bounds()).is_none());
        let current = capture.begin().unwrap();
        let shifted = DesktopBounds { origin: (-2, 0), ..bounds() };
        capture.publish(current, shifted, shot(2));
        assert!(capture.take(bounds()).is_none());
        let mut wrong_size = shot(3);
        wrong_size.width = 1;
        capture.publish(current, bounds(), wrong_size);
        assert!(capture.take(bounds()).is_none());
        capture.publish(current, bounds(), shot(4));
        assert!(capture.take(bounds()).is_some());
        capture.set_enabled(false);
        capture.invalidate();
        assert!(capture.begin().is_none());
        capture.publish(current, bounds(), shot(5));
        assert!(capture.take(bounds()).is_none());
    }

    #[test]
    fn topology_change_updates_negative_origin_and_size() {
        let old = DesktopBounds::from_rect(screen::Rect { x: 0, y: 0, width: 1920, height: 1080 }).unwrap();
        let new = DesktopBounds::from_rect(screen::Rect { x: -1280, y: -200, width: 3200, height: 1440 }).unwrap();
        assert_ne!(old, new);
        assert_eq!(new.size, (3200, 1440));
        assert_eq!(new.client_point((50, 60)), (1330, 260));
        let shifted = DesktopBounds { origin: (0, 0), ..new };
        assert_ne!(shifted, new);
        assert_eq!(shifted.size, new.size);
        assert!(DesktopBounds::from_rect(screen::Rect { x: 0, y: 0, width: 0, height: 0 }).is_err());
    }
}

#[cfg(test)]
mod tests {
    /// shader.wgsl 的任何语法/类型错误都会在窗口线程启动时才炸（wgpu 在
    /// create_shader_module 报 naga 错）。把它前移到单测：解析 + 校验。
    #[test]
    fn shader_parses_and_validates() {
        let src = include_str!("shader.wgsl");
        let module = naga::front::wgsl::parse_str(src).expect("shader.wgsl 解析失败");
        let mut validator = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        validator
            .validate(&module)
            .expect("shader.wgsl 校验失败");
    }

    /// 用透镜管线离屏渲一帧：「桌面」RGBA 图 (tex_w×tex_h)，渲染到 out_w×out_h。
    /// 返回 premultiplied 像素。无 GPU 的环境返回 None（调用方自行决定跳过还是失败）。
    fn render_lens_frame(
        img: &[u8],
        tex_w: u32,
        tex_h: u32,
        out_w: u32,
        out_h: u32,
    ) -> Option<Vec<u8>> {
        render_lens_source(img, tex_w, tex_h, out_w, out_h, include_str!("shader.wgsl"))
    }

    fn render_lens_source(
        img: &[u8], tex_w: u32, tex_h: u32, out_w: u32, out_h: u32, source: &str,
    ) -> Option<Vec<u8>> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&Default::default())).ok()?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&Default::default())).ok()?;

        let desktop_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("test-desktop"),
            size: wgpu::Extent3d { width: tex_w, height: tex_h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &desktop_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            img,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * tex_w),
                rows_per_image: Some(tex_h),
            },
            wgpu::Extent3d { width: tex_w, height: tex_h, depth_or_array_layers: 1 },
        );

        let (pipeline, bind_layout) = super::Gfx::lens_pipeline_source(&device, source);

        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("test-uniform"),
            size: 48,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        // time=1.7, level=0, intensity=1, spin=0.05, refr=1, pad×3, res（渲染）, tex（桌面）
        let uni: [f32; 12] = [
            1.7,
            0.0,
            1.0,
            0.05,
            1.0,
            0.0,
            0.0,
            0.0,
            out_w as f32,
            out_h as f32,
            tex_w as f32,
            tex_h as f32,
        ];
        queue.write_buffer(&uniform_buf, 0, bytemuck::cast_slice(&uni));

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let desktop_view = desktop_tex.create_view(&Default::default());
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("test-bind"),
            layout: &bind_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&desktop_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("test-target"),
            size: wgpu::Extent3d { width: out_w, height: out_h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let target_view = target.create_view(&Default::default());

        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("test-enc"),
        });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("test-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target_view,
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
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..3, 0..1);
        }

        // 读回（out_w*4 必须是 256 的倍数：调用方选尺寸时注意）
        assert_eq!((out_w * 4) % 256, 0, "读回行宽需 256 对齐");
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("test-readback"),
            size: (out_w * out_h * 4) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(4 * out_w),
                    rows_per_image: Some(out_h),
                },
            },
            wgpu::Extent3d { width: out_w, height: out_h, depth_or_array_layers: 1 },
        );
        queue.submit(std::iter::once(enc.finish()));

        let slice = readback.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| ());
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        let data = slice.get_mapped_range().unwrap();
        Some(data.to_vec())
    }

    /// 离屏渲染验证透镜真的在扭曲「桌面」：灰度棋盘纹理进管线、读回像素断言 ——
    /// 1. 屏幕中心完全透明（早退区不画）；
    /// 2. 透镜带内 alpha 接近不透明（折射区要显示扭曲桌面）；
    /// 3. 带内存在与原图错位的像素（折射确实发生了位移）；
    /// 4. 色度有界：彩色流光是设计（颜色是「光」），但必须是粉彩渐变
    ///    级别，不能出现通道错接式的爆色。
    #[test]
    fn lens_refracts_desktop_offscreen() {
        const W: u32 = 960;
        const H: u32 = 540; // 假想 1600x900 的 0.6x 离屏

        // 合成桌面：16px 灰度棋盘（高对比，折射错位一眼可辨；灰度便于查「无色」）
        let mut img = vec![0u8; (W * H * 4) as usize];
        for y in 0..H {
            for x in 0..W {
                let v = if ((x / 16) + (y / 16)) % 2 == 0 { 40u8 } else { 215u8 };
                let i = ((y * W + x) * 4) as usize;
                img[i] = v;
                img[i + 1] = v;
                img[i + 2] = v;
                img[i + 3] = 255;
            }
        }

        let Some(data) = render_lens_frame(&img, W, H, W, H) else {
            eprintln!("无 GPU adapter，跳过离屏渲染测试");
            return;
        };

        // 恢复优化前的无条件采样，逐像素确认薄裙裁剪不损失画质。
        let source = include_str!("shader.wgsl");
        assert_eq!(source.matches("if (vis > 0.0)").count(), 1);
        let reference_source = source.replace("if (vis > 0.0)", "if (true)");
        let reference = render_lens_source(&img, W, H, W, H, &reference_source)
            .expect("首次离屏渲染已成功，参考渲染不应失败");
        let max_delta = data.iter().zip(&reference).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
        assert!(max_delta <= 1, "优化前后像素最大差 {max_delta} 超过 1 LSB");
        eprintln!("合成棋盘 GPU 验收：{} 像素，优化前后最大通道差 {max_delta} LSB", W * H);

        let px = |x: u32, y: u32| -> [u8; 4] {
            let i = ((y * W + x) * 4) as usize;
            [data[i], data[i + 1], data[i + 2], data[i + 3]]
        };

        // 1. 屏幕中心完全透明（早退区）
        let center = px(W / 2, H / 2);
        assert_eq!(center[3], 0, "屏幕中心应完全透明, got {center:?}");

        // 2. 左缘透镜带中点 alpha 接近不透明
        //    （inset 12 + 透镜脊 d≈-22 → x≈34，y 取垂直中点避开圆角）
        let band = px(34, H / 2);
        assert!(
            band[3] >= 180,
            "透镜带内应接近不透明, got alpha={} at (34, {})",
            band[3],
            H / 2
        );

        // 3 & 4. 左缘竖带扫描：折射错位存在性 + 平均色度
        let mut shifted = 0u64;
        let mut chroma_sum = 0u64;
        let mut count = 0u64;
        for y in (H / 4)..(H * 3 / 4) {
            for x in 12..64 {
                let p = px(x, y);
                if p[3] < 128 {
                    continue; // 只看透镜带内不透明像素
                }
                count += 1;
                let si = ((y * W + x) * 4) as usize;
                if p[0] != img[si] {
                    shifted += 1;
                }
                let (r, g, b) = (p[0] as i32, p[1] as i32, p[2] as i32);
                chroma_sum += (r - g).unsigned_abs() as u64 + (g - b).unsigned_abs() as u64;
            }
        }
        assert!(count > 1000, "透镜带覆盖像素太少: {count}");
        assert!(
            shifted > count / 4,
            "折射错位像素过少（{shifted}/{count}），透镜没在扭曲桌面"
        );
        let mean_chroma = chroma_sum as f64 / count as f64;
        // 灰度棋盘 + 粉彩流光：每像素 |r-g|+|g-b| 的量级应在「彩色但温和」
        // 区间；超过 150 意味着通道错接 / 爆色之类的管线事故。
        assert!(
            mean_chroma > 1.0 && mean_chroma < 150.0,
            "平均色度 {mean_chroma:.1} 异常：流光滑失（≈0）或爆色（>150）"
        );
    }

    /// 手动预览：拟真桌面（壁纸渐变 + 窗口块 + 文字行 + 任务栏），按真实管线
    /// 0.6x 离屏渲一帧再线性放大回全尺寸，落盘 target/lens-preview.png 供人工调参。
    /// `cargo test -p neo-overlay -- --ignored`
    #[test]
    #[ignore]
    fn lens_preview() {
        const W: u32 = 1280;
        const H: u32 = 720;
        const RW: u32 = 768; // 0.6x 离屏（与生产 RENDER_SCALE 一致）
        const RH: u32 = 432;

        // 拟真桌面：竖向**亮**色渐变壁纸（教室场景多是亮底 PPT，暗底会
        // 掩盖雾感误判）+ 左侧一个白「窗口」（含横线文字带）+ 底部任务栏
        let mut img = vec![0u8; (W * H * 4) as usize];
        for y in 0..H {
            for x in 0..W {
                let g = 150.0 + 60.0 * (y as f32 / H as f32);
                let (mut r, mut gg, mut b) = (g * 0.96, g * 0.98, g * 1.04);
                // 白窗口：x 160..760, y 120..500
                if (160..760).contains(&x) && (120..500).contains(&y) {
                    r = 248.0;
                    gg = 250.0;
                    b = 252.0;
                    // 文字行：每 22px 一条 6px 灰带
                    if y > 150 && (y % 22) < 6 && x > 190 && x < 730 {
                        r = 150.0;
                        gg = 152.0;
                        b = 156.0;
                    }
                }
                // 任务栏
                if y >= H - 44 {
                    r = 226.0;
                    gg = 229.0;
                    b = 234.0;
                    // 任务栏图标格
                    if x > 60 && x < 600 && (x % 52) < 36 && y > H - 38 && y < H - 8 {
                        r = 90.0;
                        gg = 140.0;
                        b = 200.0;
                    }
                }
                let i = ((y * W + x) * 4) as usize;
                img[i] = r as u8;
                img[i + 1] = gg as u8;
                img[i + 2] = b as u8;
                img[i + 3] = 255;
            }
        }

        let Some(data) = render_lens_frame(&img, W, H, RW, RH) else {
            eprintln!("无 GPU adapter，无法生成预览");
            return;
        };
        // 模拟生产 blit：0.6x 离屏线性放大回全尺寸，再按 premultiplied
        // alpha over 合成回原桌面 —— 这才是用户视角的最终效果
        // （直接存 RGBA 的话，查看器会把透明区显示成白，看不清透镜扭曲）。
        let frame = image::RgbaImage::from_vec(RW, RH, data).expect("帧尺寸不符");
        let up = image::imageops::resize(&frame, W, H, image::imageops::FilterType::Triangle);
        let mut composed = img.clone();
        for (dst, src) in composed.chunks_exact_mut(4).zip(up.chunks_exact(4)) {
            let a = src[3] as u16;
            let inv = 255 - a;
            for c in 0..3 {
                // premultiplied over：out = src.rgb + dst.rgb × (1 - src.a)
                dst[c] = (src[c] as u16 + dst[c] as u16 * inv / 255).min(255) as u8;
            }
            dst[3] = 255;
        }
        let out =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/lens-preview.png");
        image::save_buffer(&out, &composed, W, H, image::ColorType::Rgba8).expect("预览图落盘失败");
        eprintln!("预览图 → {}", out.display());
    }
}
