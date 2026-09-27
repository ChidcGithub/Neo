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
//! - 抓屏线程 ~30fps 把桌面帧喂给渲染线程，shader 做边缘折射采样
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
use windows_sys::Win32::UI::HiDpi::GetDpiForSystem;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetWindowLongPtrW, LoadCursorW, PeekMessageW, PostThreadMessageW, RegisterClassExW,
    SetCursor, SetWindowDisplayAffinity, SetWindowLongPtrW, SetWindowPos, ShowWindow,
    TranslateMessage, CS_HREDRAW, CS_VREDRAW, GWL_EXSTYLE, GWLP_USERDATA, HCURSOR,
    HTCLIENT, HTTRANSPARENT, HWND_TOPMOST, IDC_ARROW, MSG, PM_REMOVE, SWP_NOACTIVATE,
    SWP_FRAMECHANGED, SWP_NOMOVE, SWP_NOSIZE, SW_HIDE, SW_SHOWNA, WDA_EXCLUDEFROMCAPTURE, WM_APP,
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

/// 发给窗口线程的命令。
#[derive(Debug)]
enum Cmd {
    Show,
    Hide,
    Shutdown,
}

/// 抓屏帧槽：抓屏线程放最新一帧，渲染线程每帧取走。
type FrameSlot = Arc<Mutex<Option<Shot>>>;

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
    /// 截屏闪光（shotflash，passive）。
    pub const FLASH: u8 = 4;
}

/// 跑马灯控制柄，跨线程安全。Drop 时自动关停窗口线程。
pub struct OverlayHandle {
    tx: Sender<Cmd>,
    /// 窗口线程的 Win32 线程 id（PostThreadMessage 唤醒用）。
    tid: u32,
    level: Arc<AtomicU32>,
    visible: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    cards: CardSlot,
    thread: Option<JoinHandle<()>>,
}

impl OverlayHandle {
    /// 显示全屏跑马灯并开始渲染。
    pub fn show(&self) {
        self.post(Cmd::Show);
    }

    /// 隐藏跑马灯（淡出完成后暂停渲染，省电）。
    pub fn hide(&self) {
        self.post(Cmd::Hide);
    }

    /// 设置音频电平（0.0..=1.0，通常取 RMS），驱动光带起伏。
    pub fn set_level(&self, v: f32) {
        self.level.store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    pub fn is_visible(&self) -> bool {
        self.visible.load(Ordering::Relaxed)
    }

    /// 注册/更新/撤下一张卡片（`None` = 撤下）。窗口线程被唤醒后
    /// 下一帧生效：卡片出现会露面窗口、打开输入路由；最后一张撤下且
    /// 波纹已停时窗口回到隐藏睡眠。
    pub fn set_card(&self, id: u8, card: Option<Card>) {
        if let Ok(mut cards) = self.cards.lock() {
            match card {
                Some(c) => {
                    cards.insert(id, c);
                }
                None => {
                    cards.remove(&id);
                }
            }
        }
        // 唤醒隐藏中的消息循环（队列为空时 WM_NEO_CMD 也无妨：兜底排空而已）。
        unsafe {
            PostThreadMessageW(self.tid, WM_NEO_CMD, 0, 0);
        }
    }

    /// 关停窗口线程并等待退出。
    pub fn shutdown(&mut self) {
        self.post(Cmd::Shutdown);
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }

    /// 投递命令并唤醒窗口线程（线程已退出时安静地丢弃）。
    fn post(&self, cmd: Cmd) {
        if self.tx.send(cmd).is_ok() {
            unsafe {
                PostThreadMessageW(self.tid, WM_NEO_CMD, 0, 0);
            }
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
    let level = Arc::new(AtomicU32::new(0.0f32.to_bits()));
    let visible = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));
    let cards: CardSlot = Arc::new(Mutex::new(BTreeMap::new()));
    let (cmd_tx, cmd_rx) = channel::<Cmd>();
    // 窗口线程初始化完成后回传线程 id（命令唤醒要靠它）。
    let (ready_tx, ready_rx) = channel::<Result<u32, String>>();
    let thread = {
        let level = level.clone();
        let visible = visible.clone();
        let stop = stop.clone();
        let cards = cards.clone();
        std::thread::Builder::new()
            .name("neo-overlay".into())
            .spawn(move || run(ready_tx, cmd_rx, level, visible, stop, cards))
            .map_err(|e| format!("创建 neo-overlay 线程失败: {e}"))?
    };
    let tid = ready_rx
        .recv()
        .map_err(|_| "neo-overlay 线程意外退出".to_string())??;
    Ok(OverlayHandle {
        tx: cmd_tx,
        tid,
        level,
        visible,
        stop,
        cards,
        thread: Some(thread),
    })
}

/// 窗口线程主函数。
fn run(
    ready: Sender<Result<u32, String>>,
    cmd_rx: Receiver<Cmd>,
    level: Arc<AtomicU32>,
    visible: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    cards: CardSlot,
) {
    // 无论正常退出还是 panic 展开（wgpu 的校验错误默认就是 panic），都要放
    // stop —— 否则抓屏线程跑到进程退出：迷你窗被永久压制、冻结的跑马灯残留
    // 在屏上、show()/hide() 全部落空且无人上报。
    struct StopGuard(Arc<AtomicBool>);
    impl Drop for StopGuard {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }
    let _stop_guard = StopGuard(stop.clone());

    // 波纹是否在跑（决定抓屏线程是否工作；卡片单独露面时不抓屏）。
    let ripple_on = Arc::new(AtomicBool::new(false));
    // 抓屏线程：波纹可见时 ~30fps 抓虚拟桌面，供 shader 折射采样。
    let slot: FrameSlot = Arc::new(Mutex::new(None));
    {
        let slot = slot.clone();
        let ripple_on = ripple_on.clone();
        let stop = stop.clone();
        // spawn 失败（系统线程资源枯竭等）不必带崩窗口：slot 永远为空 →
        // desktop 保持 1x1 占位纹理 → render() 里 refr 自动为 0，
        // 退回纯光环模式继续跑。
        let _ = std::thread::Builder::new()
            .name("neo-overlay-cap".into())
            .spawn(move || {
                screen::ensure_dpi_aware();
                while !stop.load(Ordering::Relaxed) {
                    if !ripple_on.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(80));
                        continue;
                    }
                    let rect = screen::virtual_screen();
                    // 静默抓帧：`capture()` 带「广播截屏信号 + 等迷你窗躲开 300ms」
                    // 的副作用，那是给「AI 应用户要求截屏」的；折射抓帧是 ~30fps 的
                    // 后台循环，走它会压死迷你窗、把折射帧率拖到 ~3fps、还会在听写
                    // 结束后凭空闪一次全屏白框（信号被反复续期后的假反馈）。
                    // 本窗自带 WDA_EXCLUDEFROMCAPTURE，画面里本来就没有自己。
                    if let Ok(shot) = screen::capture_silent(rect) {
                        if let Ok(mut g) = slot.lock() {
                            *g = Some(shot);
                        }
                    }
                    std::thread::sleep(Duration::from_millis(33));
                }
            });
    }

    let tid = unsafe { GetCurrentThreadId() };
    match Gfx::new(slot, cards) {
        Ok(gfx) => {
            if ready.send(Ok(tid)).is_err() {
                stop.store(true, Ordering::Relaxed);
                return;
            }
            msg_loop(gfx, cmd_rx, level, visible, ripple_on, stop);
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
    ripple_on: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
) {
    let hwnd = gfx.hwnd;
    let mut ripple_showing = false; // 波纹相位目标是否亮着
    let mut fading = false; // 波纹淡出中
    let mut shown = false; // 窗口当前是否可见
    let mut shutdown = false;
    while !shutdown {
        // 先排空命令队列（PostThreadMessage 只负责唤醒，命令本体在 channel 里）
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                Cmd::Show => {
                    fading = false;
                    ripple_showing = true;
                    ripple_on.store(true, Ordering::Relaxed);
                    gfx.tgt = PH_LISTEN;
                }
                Cmd::Hide => {
                    if ripple_showing {
                        // 目标相位归零，淡出完成后在渲染循环里真正收尾
                        fading = true;
                        gfx.tgt = PH_IDLE;
                    } else {
                        ripple_on.store(false, Ordering::Relaxed);
                    }
                }
                Cmd::Shutdown => shutdown = true,
            }
        }
        if shutdown {
            break;
        }

        let have_cards = gfx.has_cards();
        // 输入路由随卡片有无切换（改 WS_EX_TRANSPARENT 必须 FRAMECHANGED 才生效）。
        gfx.set_hit_testable(hwnd, have_cards);

        if ripple_showing || have_cards {
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
            gfx.render(&level);
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
            // 波纹淡出完成且没有卡片：真正隐藏窗口、停渲染、放掉陈旧桌面纹理
            if fading && gfx.cur_i < 0.02 && !have_cards {
                fading = false;
                ripple_showing = false;
                ripple_on.store(false, Ordering::Relaxed);
                gfx.desktop = Gfx::placeholder_desktop(
                    &gfx.device,
                    &gfx.queue,
                    &gfx.bind_layout,
                    &gfx.uniform_buf,
                    &gfx.sampler,
                );
                unsafe {
                    ShowWindow(hwnd, SW_HIDE);
                }
                shown = false;
                visible.store(false, Ordering::Relaxed);
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
    stop.store(true, Ordering::Relaxed);
    unsafe {
        // 取回 userdata 里的 Arc 所有权，随窗口一起释放。
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const WndState;
        if !ptr.is_null() {
            drop(Arc::from_raw(ptr));
        }
        DestroyWindow(hwnd);
    }
}

/// 窗口线程与窗口过程共享的状态（命中矩形 + 输入事件队列）。
struct WndState {
    /// 窗口左上角的虚拟屏坐标（可为负；本层窗口不移动不缩放）。
    origin: (i32, i32),
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
        WM_NCHITTEST => {
            // 屏幕坐标（符号 16 位对）→ 客户区
            let (sx, sy) = unpack_client(lparam);
            let (cx, cy) = (sx - st.origin.0, sy - st.origin.1);
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
}

impl Gfx {
    /// 透镜渲染管线与绑定布局（输出 0.6x 离屏 Rgba8Unorm）。
    /// 抽成独立函数：离屏测试不建窗口/surface，只验证 shader 的折射行为。
    fn lens_pipeline(device: &wgpu::Device) -> (wgpu::RenderPipeline, wgpu::BindGroupLayout) {
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
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
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
        let width = rect.width.max(1) as u32;
        let height = rect.height.max(1) as u32;

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
            origin: (rect.x, rect.y),
            ppp: AtomicU32::new(((unsafe { GetDpiForSystem() } as f32) / 96.0).to_bits()),
            hit_rects: Mutex::new(Vec::new()),
            events: Mutex::new(Vec::new()),
            pointer: Mutex::new(egui::pos2(f32::NEG_INFINITY, f32::NEG_INFINITY)),
            inside: AtomicBool::new(false),
        });
        unsafe {
            // 所有权随窗口生命周期；msg_loop 在 DestroyWindow 前 from_raw 回收。
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
        // WDA_EXCLUDEFROMCAPTURE 需 Win10 2004+；低版本或远程会话等场景下
        // 返回 FALSE。失败后抓屏会拍到跑马灯自己，折射形成反馈循环 ——
        // 记下来，整轮禁用折射（只画光环），比"越转越亮"体面得多。
        let exclude_ok = unsafe { SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) } != 0;

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
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
                .map_err(|e| e.to_string())?;

        // 交换链配置：BGRA + 预乘 alpha
        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| *f == wgpu::TextureFormat::Bgra8Unorm)
            .unwrap_or(caps.formats[0]);
        let alpha_mode = if caps
            .alpha_modes
            .contains(&wgpu::CompositeAlphaMode::PreMultiplied)
        {
            wgpu::CompositeAlphaMode::PreMultiplied
        } else {
            caps.alpha_modes[0]
        };
        let present_mode = if caps.present_modes.contains(&wgpu::PresentMode::Fifo) {
            wgpu::PresentMode::Fifo
        } else {
            caps.present_modes[0]
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
        );

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
        })
    }

    fn make_offscreen(
        device: &wgpu::Device,
        blit_layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        w: u32,
        h: u32,
    ) -> Offscreen {
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
        Offscreen {
            _tex: tex,
            view,
            blit_bind,
            w,
            h,
        }
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
        let (vx, vy) = self.wnd.origin;
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
                egui::Area::new(egui::Id::new(("neo-layer-card", *id)))
                    .fixed_pos(pos)
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
        if shot.rgba.len() != shot.width as usize * shot.height as usize * 4 {
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

    fn render(&mut self, level: &AtomicU32) {
        // 取最新抓屏帧
        let shot = self.slot.lock().ok().and_then(|mut g| g.take());
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
            return; // 无事可画（正常不会走到：active 前提是波纹或卡片在）
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
                return;
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
            wgpu::CurrentSurfaceTexture::Validation => return,
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

        let (pipeline, bind_layout) = super::Gfx::lens_pipeline(&device);

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
    /// 4. 无色：带内平均色度接近中性（物理色散只允许细微彩边）。
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
        assert!(
            mean_chroma < 14.0,
            "平均色度 {mean_chroma:.1} 超标：波纹应是无色的（物理色散只允许细边）"
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

        // 拟真桌面：竖向暗色渐变壁纸 + 左侧一个亮「窗口」（含横线文字带）+ 底部任务栏
        let mut img = vec![0u8; (W * H * 4) as usize];
        for y in 0..H {
            for x in 0..W {
                let g = 30.0 + 50.0 * (y as f32 / H as f32);
                let (mut r, mut gg, mut b) = (g * 0.9, g, g * 1.15);
                // 亮窗口：x 160..760, y 120..500
                if (160..760).contains(&x) && (120..500).contains(&y) {
                    r = 205.0;
                    gg = 208.0;
                    b = 214.0;
                    // 文字行：每 22px 一条 6px 灰带
                    if y > 150 && (y % 22) < 6 && x > 190 && x < 730 {
                        r = 120.0;
                        gg = 122.0;
                        b = 128.0;
                    }
                }
                // 任务栏
                if y >= H - 44 {
                    r = 22.0;
                    gg = 24.0;
                    b = 28.0;
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
