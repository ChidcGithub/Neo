//! Neo 的应用外壳。
//!
//! 一帧的流程固定为三步，**先摘输入、再绘制、最后落状态**：
//!
//! 1. 决定这一帧的 Enter 归属，并把它从输入流里摘掉（否则 `TextEdit`
//!    会同时插入一个换行）；
//! 2. 按当前主题绘制侧栏、主区（空态 / 对话态）、以及可选的设置面板；
//! 3. 消费各区块回传的动作，改状态、推流式、写库。
//!
//! 主题重算被单独摘出来放进 [`NeoApp::sync_theme`]：它依赖视口尺寸，而拖动
//! 窗口时视口每帧都在变 —— 不设指纹会导致每帧重建 `Style` 并触发整树重排。

use eframe::App;
use egui::{Id, Rect, Vec2};
use neo_store::Store;
use neo_theme::{fonts::LoadedFonts, Distance, Theme, ThemeMode};

use crate::brand::WhaleMark;
use crate::diagnostics::{record, Failure, Level, Phase};
use crate::state::{AppState, Role, Stage, StreamSource};
use crate::ui::{self, Skin};

#[cfg(test)]
fn test_db_path() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    thread_local! {
        static PATH: std::path::PathBuf = {
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("neo-app-test-{}-{id}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            dir.join("neo.db")
        };
    }
    PATH.with(Clone::clone)
}

#[cfg(test)]
thread_local! {
    static STREAM_START: std::cell::Cell<fn(neo_llm::Config, Vec<neo_llm::Msg>, Vec<serde_json::Value>) -> neo_llm::Stream> =
        const { std::cell::Cell::new(neo_llm::start_with_tools) };
}

/// 只记录固定类别，不把驱动/系统返回的路径或原始详情写入日志。
fn overlay_failure_summary(reason: &str) -> &'static str {
    if reason.contains("RegisterClassExW") || reason.contains("CreateWindowExW") {
        "覆盖层初始化失败（原生窗口）；已切换独立窗口，听写不可用"
    } else if reason.contains("线程") || reason.contains("初始化未完成") {
        "覆盖层初始化失败（线程启动或等待）；已切换独立窗口，听写不可用"
    } else {
        "覆盖层初始化失败（渲染设备或其他初始化阶段）；已切换独立窗口，听写不可用"
    }
}

/// 主题重建的输入指纹。
type Fingerprint = (ThemeMode, Distance, i32);

/// 舞台切换 / 模态弹出的入场时长：spec 标准档（0.2s）。
const MODAL_FADE: f32 = 0.2;

/// 唤醒听写的沉默超时：这么久没识别出完整句子就自动收场，回到唤醒检测。
const DICTATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// 全局鼠标左键此刻是否按下（听写打断轮询用）。
/// 主窗托盘 / 跑马灯穿透时 egui 拿不到输入，只能绕到系统 API。
#[cfg(windows)]
fn lmb_down() -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
    // SAFETY: GetAsyncKeyState 是无状态读取，无内存安全问题。
    // 0x8000 = 此刻按着；0x0001 = 自上次调用以来按过 —— 托盘态低频采样下，
    // 比间隔还快的一次点按（听写打断）只有后一个位能留下。
    (unsafe { GetAsyncKeyState(VK_LBUTTON as i32) } as u16 & 0x8001) != 0
}

/// 非 Windows 没有全局输入轮询（点击打断退化为不可用，沉默超时兜底）。
#[cfg(not(windows))]
fn lmb_down() -> bool {
    false
}

/// 发给 STT 线程的命令。音频帧直接投喂；`Reset` 在每次唤醒听写前清掉
/// VAD 缓冲，避免把上一轮的尾巴算进这一轮。
enum SttCmd {
    Audio(u64, Vec<f32>),
    Reset(u64),
}

enum SttResult {
    StartupError(String),
    Transcript(u64, Result<String, String>),
}

fn current_dictation(active: bool, current: u64, received: u64) -> bool {
    active && current == received
}

/// 只决定请求时机，不执行网络操作；关闭自动检查后本会话不再自动排队。
#[derive(Default)]
struct UpdateSchedule {
    ready_at: Option<std::time::Instant>,
    auto_done: bool,
    last_request: Option<std::time::Instant>,
    manual_pending: bool,
    auto_inflight: bool,
    notified: bool,
}

impl UpdateSchedule {
    fn request_due(&mut self, now: std::time::Instant, enabled: bool, manual: bool, checking: bool) -> Option<bool> {
        let ready = *self.ready_at.get_or_insert(now + std::time::Duration::from_secs(3));
        if !enabled {
            self.auto_done = true;
        }
        self.manual_pending |= manual;
        if checking || self.last_request.is_some_and(|last| now.saturating_duration_since(last) < std::time::Duration::from_secs(10)) {
            return None;
        }
        let automatic = !self.manual_pending;
        if automatic && (!enabled || self.auto_done || now < ready) {
            return None;
        }
        self.manual_pending = false;
        self.auto_done = true;
        self.last_request = Some(now);
        self.auto_inflight = automatic;
        Some(automatic)
    }

    fn next_deadline(&self, now: std::time::Instant, checking: bool) -> Option<std::time::Instant> {
        // 在飞的 worker 完成时会唤醒 UI；不为已到期的请求安排忙轮询。
        if checking {
            return None;
        }
        if self.manual_pending {
            return Some(self.last_request.map_or(now, |last| last + std::time::Duration::from_secs(10)));
        }
        if !self.auto_done {
            return self.ready_at;
        }
        None
    }
}

pub(crate) fn desktop_suspended(ctx: &egui::Context) -> bool {
    ctx.data(|data| data.get_temp::<bool>(egui::Id::new("neo-desktop-suspended"))).unwrap_or(false)
}

pub(crate) fn desktop_viewport(ctx: &egui::Context, viewport: egui::ViewportId) -> bool {
    let suspended = desktop_suspended(ctx);
    let key = egui::Id::new(("neo-desktop-viewport", viewport));
    let previous = ctx.data_mut(|data| {
        let previous = data.get_temp::<bool>(key).unwrap_or(false);
        data.insert_temp(key, suspended);
        previous
    });
    if suspended != previous {
        ctx.send_viewport_cmd_to(viewport, egui::ViewportCommand::Visible(!suspended));
    }
    suspended
}

/// 只在拥有窗口的 UI 线程调用。主窗句柄来自 eframe，不按标题猜测窗口。
#[derive(Default)]
struct DesktopWindows {
    main_avoided: bool,
    #[cfg(all(windows, not(test)))]
    main: isize,
    #[cfg(all(windows, not(test)))]
    placement: Option<windows_sys::Win32::UI::WindowsAndMessaging::WINDOWPLACEMENT>,
    #[cfg(test)]
    fail_verify: bool,
}

fn desktop_window_obstructs(visible: bool, minimized: bool) -> bool {
    visible && !minimized
}

impl DesktopWindows {
    #[cfg(all(windows, not(test)))]
    fn bind(&mut self, frame: &eframe::Frame) {
        use eframe::wgpu::rwh::{HasWindowHandle, RawWindowHandle};
        if let Ok(handle) = frame.window_handle() {
            if let RawWindowHandle::Win32(handle) = handle.as_raw() {
                self.main = handle.hwnd.get();
            }
        }
    }

    #[cfg(all(windows, not(test)))]
    fn visible_windows(&self) -> Result<Vec<isize>, String> {
        use windows_sys::Win32::{Foundation::*, UI::WindowsAndMessaging::*, System::Threading::GetCurrentThreadId};
        unsafe extern "system" fn visit(hwnd: HWND, data: LPARAM) -> i32 {
            unsafe {
                if desktop_window_obstructs(IsWindowVisible(hwnd) != 0, IsIconic(hwnd) != 0) {
                    (*(data as *mut Vec<isize>)).push(hwnd as isize);
                }
            }
            1
        }
        let mut windows = Vec::<isize>::new();
        if unsafe { EnumThreadWindows(GetCurrentThreadId(), Some(visit), &mut windows as *mut _ as isize) } == 0 {
            return Err("无法枚举 Neo 窗口，拒绝派发桌面工具".into());
        }
        Ok(windows)
    }

    #[cfg(all(windows, not(test)))]
    fn hide(&mut self, avoid_main: bool) -> Result<(), String> {
        use windows_sys::Win32::{Foundation::HWND, UI::WindowsAndMessaging::*, System::Threading::GetCurrentThreadId};
        let main = self.main as HWND;
        if main.is_null() || unsafe { GetWindowThreadProcessId(main, std::ptr::null_mut()) != GetCurrentThreadId() } {
            return Err("无法确认 Neo 主窗口归属，拒绝派发桌面工具".into());
        }
        let windows = self.visible_windows()?;
        if !avoid_main && windows.contains(&self.main) {
            return Err("Neo 主窗口仍可见，拒绝派发桌面工具".into());
        }
        // 先保存主窗状态，再隐藏任何辅助窗，失败时不留下半完成的暂避。
        if windows.contains(&self.main) && self.placement.is_none() {
            let mut placement: WINDOWPLACEMENT = unsafe { std::mem::zeroed() };
            placement.length = std::mem::size_of::<WINDOWPLACEMENT>() as u32;
            if unsafe { GetWindowPlacement(main, &mut placement) } == 0 {
                return Err("无法保存 Neo 主窗口状态".into());
            }
            self.placement = Some(placement);
            self.main_avoided = true;
        }
        for hwnd in windows {
            if hwnd == self.main {
                unsafe { ShowWindow(main, SW_MINIMIZE); }
            } else {
                unsafe { ShowWindow(hwnd as HWND, SW_HIDE); }
            }
        }
        Ok(())
    }

    #[cfg(any(not(windows), test))]
    fn hide(&mut self, avoid_main: bool) -> Result<(), String> {
        self.main_avoided |= avoid_main;
        Ok(())
    }

    fn manually_restored(&self) -> bool {
        #[cfg(all(windows, not(test)))]
        unsafe {
            use windows_sys::Win32::UI::WindowsAndMessaging::*;
            let hwnd = self.main as windows_sys::Win32::Foundation::HWND;
            return self.main_avoided && IsWindowVisible(hwnd) != 0 && IsIconic(hwnd) == 0;
        }
        #[cfg(any(not(windows), test))]
        false
    }

    fn verify(&self) -> Result<(), String> {
        #[cfg(all(windows, not(test)))]
        if !self.visible_windows()?.is_empty() {
            return Err("Neo 窗口尚未完成暂避，拒绝派发桌面工具".into());
        }
        #[cfg(test)]
        if self.fail_verify { return Err("模拟窗口暂避失败".into()); }
        Ok(())
    }

    fn restore(&mut self) {
        // 辅助窗由各自 viewport 的当前逻辑状态恢复，绝不重放旧 HWND 列表。
        // 原生层只负责同步隐藏/验证，不改写 winit 拥有的窗口样式。
        #[cfg(all(windows, not(test)))]
        if let Some(mut placement) = self.placement.take() {
            unsafe {
                use windows_sys::Win32::UI::WindowsAndMessaging::*;
                let hwnd = self.main as windows_sys::Win32::Foundation::HWND;
                if IsWindow(hwnd) != 0 && GetWindowThreadProcessId(hwnd, std::ptr::null_mut()) == windows_sys::Win32::System::Threading::GetCurrentThreadId() {
                    let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
                    SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style | WS_EX_NOACTIVATE as isize);
                    if placement.showCmd == SW_SHOWNORMAL as u32 {
                        placement.showCmd = SW_SHOWNOACTIVATE as u32;
                    }
                    SetWindowPlacement(hwnd, &placement);
                    SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style);
                }
            }
        }
        self.main_avoided = false;
    }
}

impl Drop for DesktopWindows {
    fn drop(&mut self) { self.restore(); }
}

pub struct NeoApp {
    state: AppState,
    whale: WhaleMark,
    fonts: LoadedFonts,
    theme: Theme,
    fingerprint: Option<Fingerprint>,
    /// 本地持久化。打开失败时为 `None`（界面照常工作，只是不保存）。
    store: Option<Store>,
    /// 上次落库的接口配置，用于把「每帧读输入框」节流成「变化时才写」。
    saved_api: (String, String),
    saved_context_tokens: usize,
    /// 上次落库的活跃会话 id，作用同上。
    saved_active: Option<i64>,
    saved_classroom_safe: bool,
    saved_silent_startup_errors: bool,
    saved_auto_check_updates: bool,
    update_checker: crate::updates::UpdateChecker,
    update_schedule: UpdateSchedule,
    applied_classroom_safe: bool,
    /// 上次落库的面板内设置。直接修改 state 的控件统一在帧末检测变化。
    /// 元素顺序：思考过程 / 思考挡位 / 主题 / 距离 / 最小化到托盘 / 语音唤醒 / 启动即后台 / 课堂总结。
    saved_prefs: (
        bool,
        neo_llm::Thinking,
        ThemeMode,
        Distance,
        bool,
        bool,
        bool,
        bool,
    ),
    /// 原生文件夹选择器在独立线程运行，主线程只接收结果。
    workspace_picker: Option<std::sync::mpsc::Receiver<Option<std::path::PathBuf>>>,
    /// 舞台切换淡入的基准：上一帧画出去的舞台。
    prev_stage: Stage,
    /// 设置面板弹出淡入的基准：上一帧画出去时的开关。
    prev_show_settings: bool,
    /// 语音唤醒引擎（"Hi, Neo"）。模型未训练或麦克风不可用时为 `None`，
    /// 界面照常工作，只是没有唤醒。
    wake: Option<neo_wake::WakeEngine>,
    /// 唤醒事件的接收端（由转发线程喂入，事件到达即唤起一帧）。
    wake_rx: Option<std::sync::mpsc::Receiver<neo_wake::WakeEvent>>,
    /// 当前接收端允许的测试代次；进出测试后持续拒收旧事件，不依赖转发时机。
    wake_epoch: u64,
    /// 引擎线程已报错退出：本次运行不再自动重启（否则帧帧重试）。
    wake_broken: bool,
    /// 系统托盘图标。只在真实客户端装配（`start_tray`），离屏测试没有托盘。
    tray: Option<tray_icon::TrayIcon>,
    /// 托盘菜单「显示主界面」的 id。
    tray_show_id: tray_icon::menu::MenuId,
    /// 托盘菜单「退出」的 id。
    tray_quit_id: tray_icon::menu::MenuId,
    /// 当前是否处于「关窗转后台」的隐藏态。
    hidden_to_tray: bool,
    /// 保存屏障通过或用户确认丢弃后，放行下一次关闭且不再启动任务。
    quitting: bool,
    exit_blocked: bool,
    confirm_discard: bool,
    /// 上一帧是否在忙（生成/工具轮）：忙→闲沿驱动「任务完成」系统通知。
    was_busy: bool,
    /// 上一轮对话完成的时刻（忙→闲沿写入）：5 分钟内再次唤醒续聊用。
    last_round_done: Option<std::time::Instant>,
    /// 独立提示窗在 logic-only 路径也消费队列，截止时间不因切窗续期。
    pending_toasts: Vec<(neo_ui::ToastKind, String, std::time::Instant)>,
    toastwin: ui::toastwin::ToastWin,
    /// 本帧 `persist_ready` 的结果：切会话/新会话的门控（落库失败不切）。
    /// 持久化已挪进 `tick`（托盘隐藏时也照跑），渲染段只读这个结果。
    persistence_ok: bool,
    /// 全屏跑马灯覆盖层（唤醒聆听时亮起）。离屏测试没有 GPU 窗口线程，为 `None`。
    overlay: Option<neo_overlay::OverlayHandle>,
    overlay_attempted: bool,
    desktop_rx: std::sync::mpsc::Receiver<crate::state::DesktopRequest>,
    desktop_pending: Vec<crate::state::DesktopRequest>,
    desktop_windows: DesktopWindows,
    desktop_show_pending: bool,
    desktop_restore_pending: bool,
    desktop_exit_pending: bool,
    desktop_cancels: Vec<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// 后台执行期间的角落迷你窗（对话速览 / 截屏回避 / 打断确认）。
    miniwin: ui::miniwin::MiniWin,
    /// 截屏完成后的区域闪光（整屏截图 = 全屏边框一闪）。
    shotflash: ui::miniwin::ShotFlash,
    /// 课堂总结起止的左上角红点（开始/结束打磨各亮 5s）。
    class_dot: ui::classwin::ClassDot,
    /// 已见到的总结起止计数（差值 = 新的红点事件）。
    class_pings_seen: u64,
    /// 工具权限确认窗（独立弹出，不绑定主窗）。
    confirmwin: ui::confirmwin::ConfirmWin,
    /// 课堂总结的顶部弹窗（打磨完成后从屏幕上方滑入）。
    classwin: ui::classwin::ClassWin,
    /// 已见到的「唤回主窗」信号（`open_app` 工具置位；变了 = AI 要开主界面）。
    show_window_seen: u64,
    /// 记忆文件 mtime 轮询节拍（2s）。
    memory_poll: std::time::Instant,
    /// 课堂总结控制器（最大化监听 / 转写 / 打磨 / 弹窗状态机）。
    class: crate::class::ClassMonitor,
    /// STT 线程的投喂端（音频帧 / 复位命令）。
    stt_tx: Option<std::sync::mpsc::Sender<SttCmd>>,
    /// STT 转写结果的回收端。
    stt_rx: Option<std::sync::mpsc::Receiver<SttResult>>,
    dictation_epoch: u64,
    /// 唤醒后的听写进行中：音频帧正经唤醒引擎流向 STT。
    dictating: bool,
    /// 听写起点，用于沉默超时。
    dictation_since: Option<std::time::Instant>,
    /// 听写期间的左键状态（沿检测）：按下沿 = 打断，丢弃本次识别。
    dictation_lmb_down: bool,
    /// 「启动即后台」只在首帧执行一次。
    start_hidden_done: bool,
}

impl NeoApp {
    /// 在给定的 egui 上下文上装配字体、品牌资源与初始主题。
    ///
    /// 只依赖 `Context`（而不是 `eframe::CreationContext`），
    /// 这样离屏渲染测试也能用同一套装配路径 —— 测试里看到的
    /// 就是真机上的那一套字体与色板。
    ///
    /// # 时序约束
    ///
    /// **必须在第一帧开始之前调用。** `Context::set_fonts` 的效果要到下一帧
    /// 才生效，而 `neo-bold` / `neo-mono` 这两个自定义字族在第一帧就会被用到，
    /// 在帧内装配会直接 panic。`eframe` 的 `app_creator` 满足这一约束。
    pub fn install(ctx: &egui::Context) -> Self {
        let fonts = neo_theme::fonts::install(ctx);
        let whale = WhaleMark::load(ctx);

        // 持久化：打不开就降级为纯内存模式，不挡启动。
        #[cfg(not(test))]
        let db_path = neo_store::default_db_path();
        #[cfg(test)]
        let db_path = test_db_path();
        let (store, store_ok) = match Store::open(&db_path) {
            Ok(s) => (Some(s), true),
            Err(_) => {
                record(Level::Error, "storage", "数据库打开失败，将以纯内存模式运行");
                crate::startup::record_error("database", "数据库打开失败，将以纯内存模式运行");
                (None, false)
            }
        };
        let db_path = db_path.to_string_lossy().into_owned();

        let mut state = AppState::with_store(store_ok, Some(db_path));
        let (requests, desktop_rx) = std::sync::mpsc::channel();
        state.desktop_execution = Some(crate::state::DesktopExecution {
            requests, active: Default::default(), overlay: None,
        });

        let mut app = Self {
            state,
            whale,
            fonts,
            theme: Theme::new(ThemeMode::Dark, 1080.0, Distance::default()),
            fingerprint: None,
            store,
            saved_api: (String::new(), String::new()),
            saved_context_tokens: neo_llm::CONTEXT_TOKENS,
            saved_active: None,
            saved_classroom_safe: true,
            saved_silent_startup_errors: false,
            saved_auto_check_updates: true,
            update_checker: Default::default(),
            update_schedule: Default::default(),
            applied_classroom_safe: true,
            workspace_picker: None,
            prev_stage: Stage::Hero,
            prev_show_settings: false,
            wake: None,
            wake_rx: None,
            wake_epoch: 0,
            wake_broken: false,
            tray: None,
            tray_show_id: tray_icon::menu::MenuId::new("neo-tray-show"),
            tray_quit_id: tray_icon::menu::MenuId::new("neo-tray-quit"),
            hidden_to_tray: false,
            quitting: false,
            exit_blocked: false,
            confirm_discard: false,
            pending_toasts: Vec::new(),
            toastwin: Default::default(),
            was_busy: false,
            last_round_done: None,
            persistence_ok: true,
            overlay: None,
            overlay_attempted: false,
            desktop_rx,
            desktop_pending: Vec::new(),
            desktop_windows: DesktopWindows::default(),
            desktop_show_pending: false,
            desktop_restore_pending: false,
            desktop_exit_pending: false,
            desktop_cancels: Vec::new(),
            miniwin: Default::default(),
            shotflash: Default::default(),
            class_dot: Default::default(),
            class_pings_seen: 0,
            confirmwin: Default::default(),
            classwin: Default::default(),
            show_window_seen: 0,
            memory_poll: std::time::Instant::now(),
            class: Default::default(),
            stt_tx: None,
            stt_rx: None,
            dictation_epoch: 0,
            dictating: false,
            dictation_since: None,
            dictation_lmb_down: false,
            start_hidden_done: false,
            saved_prefs: (
                false,
                neo_llm::Thinking::default(),
                ThemeMode::Dark,
                Distance::default(),
                true,
                true,
                true,
                false,
            ),
        };

        app.load_settings();
        app.saved_api = (app.state.api_base.clone(), app.state.api_key.clone());
        // 模型启动只读本地缓存；更新检查由独立调度器延后执行。
        app.saved_classroom_safe = app.state.classroom_safe;
        app.saved_silent_startup_errors = app.state.silent_startup_errors;
        app.saved_auto_check_updates = app.state.auto_check_updates;
        app.state.sync_startup_policy(crate::startup::set_silent_policy);
        app.applied_classroom_safe = app.state.classroom_safe;
        record(Level::Info, "app", "应用已启动");
        // 装载之后同步一次基准值，否则第一帧会把"从库里读到的"当成"用户刚改的"。
        app.saved_prefs = (
            app.state.show_reasoning,
            app.state.thinking,
            app.state.theme_mode,
            app.state.distance,
            app.state.minimize_to_tray,
            app.state.wake_enabled,
            app.state.start_in_tray,
            app.state.class_enabled,
        );
        if let Some(store) = app.store.as_ref() {
            Self::refresh_sessions(&mut app.state, store);
            // 恢复上次使用的会话。
            Self::restore_last_session(&mut app.state, store);
            app.saved_active = app.state.active_session;
            // 长期记忆入库：启动加载，之后由 tick 里的 mtime 轮询热更新。
            app.state.memories = neo_tools::tools::memory::load_memories();
        }

        // 首帧还不知道视口尺寸，先按 1080p 建一套；`sync_theme` 会在第一帧立刻纠正。
        app.theme = Theme::new(app.state.theme_mode, 1080.0, app.state.distance);
        app.theme.apply(ctx);
        // 动效基准：启动恢复不算「切换」，首帧不播入场。
        app.prev_stage = app.state.stage;
        app.prev_show_settings = app.state.show_settings;

        app
    }

    /// 启动语音唤醒（"Hi, Neo"）。
    ///
    /// **只应在真实客户端里调用**（`main`），离屏测试不碰麦克风。
    /// 模型还没训练好 / 采集失败时引擎线程会回一条 [`neo_wake::WakeEvent::Error`]，
    /// 界面降级为"无唤醒"照常工作。设置里关掉唤醒时是空操作。
    pub fn start_wake(&mut self, ctx: &egui::Context) {
        if !self.state.effective_wake_enabled() || self.wake.is_some() || self.wake_broken {
            return;
        }
        let (engine, rx) = neo_wake::WakeEngine::start(neo_wake::WakeConfig::default());
        // 引擎线程 → UI 之间加一段转发：事件到达时主动唤起一帧，
        // 主循环不用为了它空转轮询。
        let (tx, ui_rx) = std::sync::mpsc::channel();
        let repaint = ctx.clone();
        match std::thread::Builder::new()
            .name("neo-wake-forward".into())
            .spawn(move || {
                while let Ok(event) = rx.recv() {
                    if tx.send(event).is_err() {
                        break;
                    }
                    repaint.request_repaint();
                }
            }) {
            Ok(_) => {
                self.wake_epoch = engine.event_epoch();
                self.wake = Some(engine);
                self.wake_rx = Some(ui_rx);
            }
            Err(_) => self.fail_wake("无法启动唤醒事件转发线程".into()),
        }
    }

    /// 启动全屏跑马灯覆盖层（独立窗口线程，初始隐藏，等唤醒时 `show`）。
    ///
    /// **只应在真实客户端里调用**（`main`），离屏测试没有 GPU 窗口线程。
    pub fn start_overlay(&mut self) {
        if !self.overlay_attempted {
            self.overlay_attempted = true;
            // 失败（无 DX12 / 建窗失败）降级为无跑马灯，不拖垮主程序。
            match neo_overlay::start() {
                Ok(h) => self.overlay = Some(h),
                Err(reason) => {
                    record(Level::Error, "overlay", overlay_failure_summary(&reason));
                    self.pending_toasts.push((
                        neo_ui::ToastKind::Warning,
                        "覆盖层不可用，提示已改用独立窗口；语音唤醒将打开输入框，不启动听写".into(),
                        std::time::Instant::now() + std::time::Duration::from_secs(8),
                    ));
                },
            }
        }
    }

    /// 启动 STT 线程：预载 SenseVoice 模型，之后等唤醒听写的音频帧。
    ///
    /// **只应在真实客户端里调用**（`main`）。模型缺失时线程立刻回一条
    /// `Err`，界面降级为「唤醒只打开主界面」的老行为，不挡启动。
    pub fn start_stt(&mut self, ctx: &egui::Context) {
        if self.stt_tx.is_some() {
            return;
        }
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<SttCmd>();
        let (out_tx, out_rx) = std::sync::mpsc::channel::<SttResult>();
        let repaint = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("neo-stt".into())
            .spawn(move || {
                let engine = match neo_stt::SttEngine::create(&neo_stt::SttConfig::default()) {
                    Ok(engine) => engine,
                    Err(msg) => {
                        let _ = out_tx.send(SttResult::StartupError(msg));
                        repaint.request_repaint();
                        return;
                    }
                };
                // 引擎约定在同一线程内驱动：收帧 → VAD 断句 → 成句即转写。
                let mut epoch = 0;
                while let Ok(cmd) = cmd_rx.recv() {
                    match cmd {
                        SttCmd::Reset(next) => {
                            epoch = next;
                            engine.reset();
                        }
                        SttCmd::Audio(generation, frame) => {
                            if generation != epoch {
                                continue;
                            }
                            engine.accept_waveform(&frame);
                            if let Some(seg) = engine.take_segment() {
                                if out_tx
                                    .send(SttResult::Transcript(epoch, engine.transcribe(&seg)))
                                    .is_err()
                                {
                                    return;
                                }
                                repaint.request_repaint();
                            }
                        }
                    }
                }
            });
        match spawned {
            Ok(_) => {
                self.stt_tx = Some(cmd_tx);
                self.stt_rx = Some(out_rx);
            }
            Err(_) => record(Level::Error, "stt", "语音转写启动失败"),
        }
    }

    /// 装配系统托盘：图标 + 「显示主界面 / 退出」菜单。
    ///
    /// **只应在真实客户端里调用**（`main`）。托盘创建失败不挡启动，
    /// 只是关窗时退化为直接退出。事件没有回调——在 [`NeoApp::poll_tray`]
    /// 里逐帧轮询。
    pub fn start_tray(&mut self) {
        use tray_icon::menu::{Menu, MenuItem};
        use tray_icon::TrayIconBuilder;

        if self.tray.is_some() {
            return;
        }
        let menu = Menu::new();
        let show = MenuItem::with_id(self.tray_show_id.clone(), "显示主界面", true, None);
        let quit = MenuItem::with_id(self.tray_quit_id.clone(), "退出 Neo", true, None);
        if menu.append(&show).is_err() || menu.append(&quit).is_err() {
            record(Level::Error, "tray", "托盘菜单创建失败");
            return;
        }
        let (rgba, w, h) = crate::brand::whale_rgba(64);
        let icon = match tray_icon::Icon::from_rgba(rgba, w, h) {
            Ok(icon) => icon,
            Err(_) => {
                record(Level::Error, "tray", "托盘图标创建失败");
                return;
            }
        };
        match TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("Neo — 教室大屏 AI 助手")
            .with_icon(icon)
            .build()
        {
            Ok(tray) => self.tray = Some(tray),
            Err(_) => record(Level::Error, "tray", "系统托盘创建失败"),
        }
    }

    /// 轮询托盘事件：图标点击 / 菜单点击。
    ///
    /// 托盘没有 winit 唤醒源，隐藏期间靠 `request_repaint_after` 保持低频轮询。
    fn poll_tray(&mut self, ctx: &egui::Context) {
        if self.tray.is_none() {
            return;
        }
        while let Ok(event) = tray_icon::TrayIconEvent::receiver().try_recv() {
            // 左键按下即唤回（比双击省一步，教室里少一次操作）。
            if let tray_icon::TrayIconEvent::Click {
                button: tray_icon::MouseButton::Left,
                button_state: tray_icon::MouseButtonState::Down,
                ..
            } = event
            {
                self.show_window(ctx);
            }
        }
        while let Ok(event) = tray_icon::menu::MenuEvent::receiver().try_recv() {
            self.handle_tray_menu(ctx, &event.id);
        }
    }

    /// 所有应用内退出都经过同一屏障；系统强杀、断电不受此屏障保证。
    fn request_exit(&mut self, ctx: &egui::Context) {
        self.update_checker.cancel();
        self.update_schedule.auto_inflight = false;
        self.update_schedule.auto_done = true;
        self.update_schedule.manual_pending = false;
        self.state.update_check_requested = false;
        if matches!(self.state.update_status, crate::updates::Status::Checking) {
            self.state.update_status = crate::updates::Status::Idle;
        }
        self.stop_wake_test();
        self.cancel_dictation();
        self.wake_rx = None;
        if let Some(engine) = self.wake.take() {
            let _ = std::thread::Builder::new()
                .name("neo-wake-drop".into())
                .spawn(move || drop(engine));
        }
        self.class.tick(ctx, &self.state, false);
        let classes_saved = self.class.retry_all_saves();
        self.state.cancel_attachment_import();
        self.state.model_fetch = None;
        let messages_saved = Self::prepare_session_change(&mut self.state, self.store.as_ref());
        let preferences_saved = self.persist_preferences();
        let selection_saved = self.store.as_ref().is_some_and(|store| {
            let active = self.state.active_session.map(|id| id.to_string()).unwrap_or_default();
            Self::save_setting(Some(store), "active_session", &active)
                && Self::save_setting(Some(store), "models", &self.state.models.iter()
                    .map(|model| model.id.as_str()).collect::<Vec<_>>().join("\n"))
                && Self::save_setting(Some(store), "model", self.state.model_id())
        });
        // 纯内存模式的游标只代表已处理，绝不是落盘成功。
        if self.store.is_some() && messages_saved && preferences_saved && selection_saved && classes_saved {
            self.finish_exit(ctx);
        } else {
            self.exit_blocked = true;
            self.confirm_discard = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.show_window(ctx);
            record(Level::Error, "storage", "退出保存失败，已保留应用及内存数据");
        }
    }

    fn finish_exit(&mut self, ctx: &egui::Context) {
        self.quitting = true;
        self.exit_blocked = false;
        if self.state.desktop_execution.as_ref().is_some_and(|gate| gate.active.load(std::sync::atomic::Ordering::Acquire) != 0) {
            self.state.cancel();
            for cancel in &self.desktop_cancels { cancel.store(true, std::sync::atomic::Ordering::Release); }
            self.desktop_exit_pending = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.request_repaint_after(std::time::Duration::from_millis(10));
            return;
        }
        self.desktop_exit_pending = false;
        self.desktop_windows.restore();
        drop(self.tray.take());
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        ctx.request_repaint();
    }

    fn handle_close(&mut self, ctx: &egui::Context) {
        self.update_schedule.manual_pending = false;
        self.state.update_check_requested = false;
        if self.quitting {
            return;
        }
        if self.state.minimize_to_tray && self.tray.is_some() && !self.exit_blocked {
            self.hide_to_tray(ctx);
        } else {
            self.request_exit(ctx);
        }
    }

    fn handle_tray_menu(&mut self, ctx: &egui::Context, id: &tray_icon::menu::MenuId) {
        if id == &self.tray_show_id {
            self.show_window(ctx);
        } else if id == &self.tray_quit_id {
            self.request_exit(ctx);
        }
    }

    fn exit_dialog(&mut self, ctx: &egui::Context) {
        egui::Window::new("退出前保存失败")
            .collapsible(false).resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label("消息、设置或课堂资料尚未保存，应用已暂停新任务。请修复存储权限或磁盘空间后重试。");
                if self.class.pending_saves() > 0 {
                    ui.label(format!("仍有 {} 节课堂素材或记忆索引未保存，已保留全部卡片。", self.class.pending_saves()));
                }
                if self.state.preferences_unsaved {
                    ui.label("安全偏好尚未保存：本次运行的限制不保证重启后仍然生效。");
                }
                if self.store.is_none() {
                    ui.label("当前为纯内存模式；退出会丢失本次会话和设置变更。");
                }
                if self.confirm_discard {
                    ui.colored_label(egui::Color32::RED, "确认丢弃未保存的消息、设置、课堂素材和记忆索引？重启可能恢复旧的安全偏好。");
                    if ui.button("确认丢弃并退出").clicked() {
                        self.finish_exit(ctx);
                    }
                    if ui.button("返回保存选项").clicked() {
                        self.confirm_discard = false;
                    }
                } else {
                    if ui.button("重试保存并退出").clicked() {
                        self.request_exit(ctx);
                    }
                    if ui.button("保持打开").clicked() {
                        self.exit_blocked = false;
                    }
                    if ui.button("退出不保存…").clicked() {
                        self.confirm_discard = true;
                    }
                }
            });
    }

    /// 消费显式打开请求；同一信号不能在轮询或 multipass 中重复打断/聚焦。
    fn handle_show_signal(&mut self, ctx: &egui::Context, show_at: u64) {
        if show_at == 0 || show_at == self.show_window_seen { return; }
        self.show_window_seen = show_at;
        self.show_window(ctx);
    }

    /// 显式唤回主窗口，包括普通最小化和被其他应用遮挡的窗口。
    fn show_window(&mut self, ctx: &egui::Context) {
        let active = self.state.desktop_execution.as_ref().is_some_and(|gate| gate.active.load(std::sync::atomic::Ordering::Acquire) != 0);
        if (self.desktop_windows.main_avoided || active) && !self.desktop_show_pending {
            self.state.cancel();
        }
        if active {
            for cancel in &self.desktop_cancels {
                cancel.store(true, std::sync::atomic::Ordering::Release);
            }
            self.desktop_show_pending = true;
            return;
        }
        self.desktop_show_pending = false;
        self.desktop_windows.restore();
        self.hidden_to_tray = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    /// 关窗转后台：取消关闭，隐藏窗口（同时最小化，wgpu 对隐藏窗仍会
    /// 尝试取交换链，最小化让渲染循环走「尺寸为零跳过」的既有路径）。
    fn hide_to_tray(&mut self, ctx: &egui::Context) {
        // 用户主动转托盘覆盖临时暂避前的可见状态，也撤销尚未完成的唤回。
        self.desktop_show_pending = false;
        self.desktop_windows.main_avoided = false;
        #[cfg(all(windows, not(test)))]
        { self.desktop_windows.placement = None; }
        self.hidden_to_tray = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
    }

    fn overlay_alive(&self) -> bool {
        self.overlay.as_ref().is_some_and(|overlay| overlay.is_alive())
    }

    fn check_overlay_health(&mut self, ctx: &egui::Context) {
        if self.overlay_alive() {
            return;
        }
        if self.dictating {
            self.cancel_dictation();
            self.show_window(ctx);
            self.pending_toasts.push((
                neo_ui::ToastKind::Warning,
                "听写显示层不可用，已停止听写；请直接输入".into(),
                std::time::Instant::now() + std::time::Duration::from_secs(4),
            ));
        }
        if self.overlay.take().is_some() {
            record(Level::Warn, "overlay", "覆盖层已退出，切换到独立窗口");
        }
    }

    /// 「Hi, Neo」命中：全屏跑马灯亮起，直接进入听写，**主界面不露面**。
    /// 跑马灯或 STT 不可用（离屏测试 / 模型缺失）时退回老行为：
    /// 唤回主界面并聚焦输入框。
    fn on_wake_detected(&mut self, ctx: &egui::Context, score: f32) {
        self.handle_wake_detected(ctx, score, self.overlay_alive());
    }

    /// 显示层可用性由入口采样，测试可直接验证听写分支而不启动原生窗口。
    fn handle_wake_detected(&mut self, ctx: &egui::Context, score: f32, overlay_alive: bool) {
        eprintln!("[neo] 唤醒命中（置信度 {score:.2}），进入听写");
        if overlay_alive && self.stt_tx.is_some() {
            self.dictation_epoch = self.dictation_epoch.wrapping_add(1);
            if let Some(overlay) = &self.overlay {
                overlay.show();
            }
            if let Some(tx) = &self.stt_tx {
                let _ = tx.send(SttCmd::Reset(self.dictation_epoch));
            }
            if let Some(wake) = &self.wake {
                wake.set_dictation(true);
            }
            self.dictating = true;
            self.dictation_since = Some(std::time::Instant::now());
            // 打断沿的基线：进入瞬间正按着不算（那是触发前的点击尾巴）。
            self.dictation_lmb_down = lmb_down();
        } else {
            // 本次唤醒统一负责展示降级输入；先结束旧听写，避免健康检查重复唤窗。
            self.cancel_dictation();
            self.check_overlay_health(ctx);
            self.show_window(ctx);
            // "Hi, Neo"：焦点交给输入框，老师接着输入即可。
            ctx.memory_mut(|m| m.request_focus(Id::new(ui::COMPOSER_ID)));
            self.pending_toasts.push((
                neo_ui::ToastKind::Success,
                format!("已唤醒（置信度 {score:.2}），请直接输入"),
                std::time::Instant::now() + std::time::Duration::from_secs(3),
            ));
        }
    }

    /// 听写模式的音频帧：喂给 STT 线程，同时把电平推给跑马灯驱动光带起伏。
    fn on_dictation_audio(&mut self, ctx: &egui::Context, frame: Vec<f32>) {
        self.check_overlay_health(ctx);
        if !self.dictating {
            return;
        }
        let rms = (frame.iter().map(|x| x * x).sum::<f32>() / frame.len().max(1) as f32).sqrt();
        if let Some(overlay) = &self.overlay {
            overlay.set_level((rms * 3.0).clamp(0.0, 1.0));
        }
        if let Some(tx) = &self.stt_tx {
            if tx.send(SttCmd::Audio(self.dictation_epoch, frame)).is_err() {
                self.stt_tx = None;
                self.end_dictation();
            }
        }
    }

    /// 结束听写：跑马灯淡出，唤醒引擎回到检测模式。
    fn end_dictation(&mut self) {
        self.dictation_epoch = self.dictation_epoch.wrapping_add(1);
        self.dictating = false;
        self.dictation_since = None;
        if let Some(overlay) = &self.overlay {
            overlay.set_level(0.0);
            overlay.hide();
        }
        if let Some(wake) = &self.wake {
            wake.set_dictation(false);
        }
    }

    /// 打断听写（点击屏幕）：丢弃本次识别 —— 结束 + 重置 STT 的 VAD 缓冲。
    /// 在途文本由收取处的 `dictating` 守卫天然丢弃（弹出但不发送）。
    fn cancel_dictation(&mut self) {
        if !self.dictating {
            return;
        }
        self.end_dictation();
        if let Some(tx) = &self.stt_tx {
            let _ = tx.send(SttCmd::Reset(self.dictation_epoch));
        }
    }

    /// 从数据库读设置。缺省值与 `AppState::default` 一致。
    fn load_settings(&mut self) {
        let Some(store) = &self.store else { return };
        let get = |key: &str| -> Option<String> {
            match store.setting(key) {
                Ok(value) => value,
                Err(_) => {
                    record(Level::Warn, "settings", "配置读取失败，使用默认值");
                    None
                }
            }
        };
        self.state.context_tokens = neo_llm::restored_context_tokens(get("context_tokens").as_deref());
        self.saved_context_tokens = self.state.context_tokens;
        self.state.classroom_safe = get("classroom_safe").as_deref() != Some("0");
        self.state.silent_startup_errors = get("silent_startup_errors").as_deref() == Some("1");
        self.state.auto_check_updates = get("auto_check_updates").as_deref() != Some("0");
        self.state.sync_startup_policy(crate::startup::set_silent_policy);
        if let Some(v) = get("theme") {
            if v == "light" {
                self.state.theme_mode = ThemeMode::Light;
            }
        }
        if let Some(v) = get("distance") {
            self.state.distance = match v.as_str() {
                "classroom" => Distance::Classroom,
                "auditorium" => Distance::Auditorium,
                _ => Distance::Standard,
            };
        }
        if let Some(v) = get("show_reasoning") {
            self.state.show_reasoning = v == "1";
        }
        if let Some(v) = get("minimize_to_tray") {
            self.state.minimize_to_tray = v == "1";
        }
        if let Some(v) = get("wake_enabled") {
            self.state.wake_enabled = v == "1";
        }
        if let Some(v) = get("start_in_tray") {
            self.state.start_in_tray = v == "1";
        }
        if let Some(v) = get("class_enabled") {
            self.state.class_enabled = v == "1";
        }
        if let Some(v) = get("thinking") {
            self.state.thinking = neo_llm::Thinking::from_key(&v);
        }
        if let Some(v) = get("api_base") {
            self.state.api_base = v;
        }
        if let Some(v) = get("api_key") {
            self.state.api_key = v;
        }
        // 上次拉到的模型列表先读回来，界面立刻有东西可用（不用等网络）。
        let saved_list = get("models").unwrap_or_default();
        let saved_model = get("model");
        self.state
            .restore_models(&saved_list, saved_model.as_deref());
    }

    /// 刷新侧栏的会话列表。
    ///
    /// 窄参数而非 `&mut self`：一帧的绘制阶段会解构借用 `state`，
    /// 此时任何 `&mut self` 方法都会撞借用检查（字段级借用是分开的，
    /// 方法借的是整个 `self`）。
    fn refresh_sessions(state: &mut AppState, store: &Store) {
        state.sessions = store.sessions().unwrap_or_default();
    }

    /// 打开一个会话：从库里读消息。读取失败返回 false（调用方给提示）。
    fn open_session(state: &mut AppState, store: &Store, id: i64) -> bool {
        if !Self::prepare_session_change(state, Some(store)) {
            return false;
        }
        let Ok(rows) = store.messages(id) else {
            return false;
        };
        state.new_session();
        state.messages = rows
            .into_iter()
            .map(|r| {
                let role = Role::from_str_lossy(&r.role);
                let mut msg = crate::state::ChatMessage::new(role, r.content);
                msg.reasoning = r.reasoning;
                msg.meta = r.meta;
                match serde_json::from_str(&r.attachments) {
                    Ok(attachments) => msg.attachments = attachments,
                    Err(_) => {
                        state.attachment_error =
                            Some("此会话有附件记录损坏，已保留可读消息，请重新添加对应文件".into())
                    }
                }
                match serde_json::from_str(&r.tool_calls) {
                    Ok(calls) => msg.tool_calls = calls,
                    Err(_) => state.attachment_error = Some("历史工具协议损坏，结果按低信任参考资料恢复".into()),
                }
                if role == Role::Tool {
                    let mut tool = crate::state::ToolMeta::restored(&msg.meta);
                    tool.call_id = r.tool_call_id;
                    msg.tool = Some(tool);
                }
                msg
            })
            .collect();
        if let Ok(Some((covered, json))) = store.checkpoint(id) {
            match serde_json::from_str::<crate::state::ContextCheckpoint>(&json) {
                Ok(c) if c.covered == covered && c.valid(&state.messages) => {
                    state.checkpoint = Some(c);
                    state.compaction_status = Some("已恢复历史摘要，原聊天记录保留".into());
                }
                _ => state.compaction_status = Some("历史摘要校验失败，已恢复完整原文".into()),
            }
        }
        state.active_session = Some(id);
        state.stage = Stage::Conversation;
        // 生成/流状态在 new_session() 里已复位（含流线程取消标志）。
        state.pending_persist = state.messages.len();
        state.auto_approve_tools = false;
        true
    }

    /// 恢复上次使用的会话：库里存的 id 必须仍然存在才生效。
    fn restore_last_session(state: &mut AppState, store: &Store) {
        if let Ok(Some(v)) = store.setting("active_session") {
            if let Ok(id) = v.parse::<i64>() {
                if state.sessions.iter().any(|s| s.id == id) {
                    Self::open_session(state, store, id);
                }
            }
        }
    }

    /// 确保当前对话有落库的会话。返回会话 id。
    fn ensure_session(state: &mut AppState, store: &Store) -> Option<i64> {
        if let Some(id) = state.active_session {
            return Some(id);
        }
        // 标题取首条用户消息的前 18 个字符。
        let title: String = state
            .messages
            .iter()
            .find(|m| m.role == Role::User)
            .map(|m| m.title().chars().take(18).collect())
            .unwrap_or_else(|| "新对话".to_owned());
        let id = store.create_session(&title).ok()?;
        state.active_session = Some(id);
        Some(id)
    }

    /// 编辑器有未提交输入时只追加带来源标记的语音，不能自动发送或切会话。
    /// 返回 true 才允许调用发送入口；失败和等待确认都把文本留在草稿中。
    fn prepare_dictation_input(
        state: &mut AppState,
        store: Option<&Store>,
        text: &str,
        last_round_done: Option<std::time::Instant>,
    ) -> bool {
        let text = text.trim();
        if text.is_empty() {
            return false;
        }
        // 空白也属于用户输入；导入接收端必须保留，不能被 new_session/submit 取消。
        if !state.draft.is_empty()
            || !state.draft_attachments.is_empty()
            || state.attachment_busy()
            || state.attachment_picker_open
        {
            if !state.draft.is_empty() {
                state.draft.push_str("\n\n");
            }
            state.draft.push_str("【语音转写 · 待确认】\n");
            state.draft.push_str(text);
            return false;
        }
        if !Self::prepare_session_change(state, store) {
            state.draft = text.to_owned();
            return false;
        }
        // 只有空编辑器才能沿用五分钟续聊 / 超时新会话规则。
        let resume = last_round_done
            .is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(300))
            && !state.messages.is_empty();
        if !resume {
            state.new_session();
        }
        state.draft = text.to_owned();
        true
    }

    fn send_input(state: &mut AppState, store: Option<&Store>) {
        if !state.can_submit() {
            return;
        }
        if !state.draft_attachments.is_empty() && !state.can_call_real() {
            state.attachment_error = Some("附件已准备好；请先配置 API 密钥并选择可用模型。图片需要视觉模型，当前未发送任何附件。".into());
            return;
        }
        if state.needs_model_list() {
            state.attachment_error = Some("请在设置中核对接口地址和密钥，再点击「从模型商刷新」".into());
            state.show_settings = true;
            state.settings_tab = crate::state::SettingsTab::Model;
            return;
        }
        if !Self::persist_ready(state, store) {
            return;
        }
        let draft = state.draft.clone();
        let stage = state.stage;
        if state.submit() {
            let mut compaction_plan = None;
            let prepared_messages = if state.can_call_real() {
                let mut messages = state.api_messages(usize::MAX);
                let checked = (|| {
                    // 最新输入单独不能容纳时，摘要也无济于事，恢复草稿而不是联网。
                    let rejected_history = messages.iter().any(neo_llm::Msg::is_rejected);
                    let mut latest = if rejected_history {
                        state.latest_input_messages()
                    } else {
                        vec![neo_llm::Msg::new(neo_llm::Role::System, state.system_prompt()), messages.pop().expect("刚提交的用户消息")]
                    };
                    let latest_check = neo_llm::budget_messages(&state.llm_config(), &mut latest, &neo_tools::tool_declarations());
                    if !rejected_history { messages.push(latest.pop().unwrap()); }
                    latest_check?;
                    compaction_plan = state.compaction_plan(&messages)?;
                    if compaction_plan.is_none() {
                        neo_llm::budget_messages(&state.llm_config(), &mut messages, &neo_tools::tool_declarations())?;
                    }
                    Ok::<(), String>(())
                })();
                if let Err(error) = checked {
                    let message = state.messages.pop().expect("刚提交的用户消息");
                    state.draft = draft;
                    state.draft_attachments = message.attachments;
                    state.stage = stage;
                    state.attachment_error = Some(error);
                    state.finish_task_diagnostic(Phase::Rejected, Some(Failure::Budget));
                    return;
                }
                Some(messages)
            } else {
                None
            };
            if !Self::persist_ready(state, store) {
                state.finish_task_diagnostic(Phase::Failed, Some(Failure::Storage));
                if let Some(message) = state.messages.pop() {
                    state.draft = message.content;
                    state.draft_attachments = message.attachments;
                }
                // 回滚后消息空了就把舞台也退回空态 —— 否则界面停在
                // 「对话态 + 零消息 + 草稿已回输入框」的错觉里。
                if state.messages.is_empty() {
                    state.stage = crate::state::Stage::Hero;
                    // ensure_session 刚建行、append 就失败：空会话行一起收掉，
                    // 免得重启后侧栏冒出一个 0 消息的「新对话」。
                    if let (Some(id), Some(store)) = (state.active_session.take(), store) {
                        let _ = store.delete_session(id);
                    }
                }
                return;
            }
            if let Some(plan) = compaction_plan {
                Self::launch_compaction(state, plan);
            } else if let Some(messages) = prepared_messages {
                Self::start_real_stream_with_messages(state, messages);
            } else {
                state.wants_demo_reply = true;
            }
        }
    }

    /// 发起一轮真实请求。
    ///
    /// **每一轮都带工具声明** —— 模型因此可以在任何一次回答中途决定读文件、
    /// 改文件或跑命令；工具结果回灌后仍是同一个入口，没有第二条代码路径。
    fn start_real_stream(state: &mut AppState) {
        if state.task_limit_reached || state.task_tool_calls >= crate::state::TASK_TOOL_LIMIT {
            state.finish_task_diagnostic(Phase::Rejected, Some(Failure::ToolLimit));
            state.attachment_error = Some("本任务已达到500次工具调用上限，已停止自动续轮".into());
            return;
        }
        let mut messages = state.api_messages(usize::MAX);
        match state.compaction_plan(&messages) {
            Ok(Some(plan)) => Self::launch_compaction(state, plan),
            Ok(None) => match neo_llm::budget_messages(&state.llm_config(), &mut messages, &neo_tools::tool_declarations()) {
                Ok(()) => Self::start_real_stream_with_messages(state, messages),
                Err(e) => {
                    state.finish_task_diagnostic(Phase::Rejected, Some(Failure::Budget));
                    state.attachment_error = Some(e);
                }
            },
            Err(e) => {
                state.finish_task_diagnostic(Phase::Rejected, Some(Failure::Budget));
                state.compaction_status = Some(format!("压缩失败：{e}")); state.attachment_error = Some(e);
            }
        }
    }

    fn launch_compaction(state: &mut AppState, mut plan: crate::state::CompactionPlan) {
        let messages = std::mem::take(&mut plan.messages);
        #[cfg(not(test))]
        let stream = neo_llm::start(state.llm_config(), messages);
        #[cfg(test)]
        let stream = STREAM_START.with(|start| start.get()(state.llm_config(), messages, Vec::new()));
        state.start_compaction(plan, stream);
    }

    /// 首次发送移交预检后的消息；工具续轮重新构造，两者仍经过 LLM 通用预算。
    fn start_real_stream_with_messages(state: &mut AppState, msgs: Vec<neo_llm::Msg>) {
        let cfg = state.llm_config();
        // 注意：`tool_declarations()` 已经是"每元素一条工具"的扁平列表，
        // 不要再套一层 Vec —— `tools: [[…]]` 会被服务端 422 掉。
        //
        // **思考模式也带工具**：早先这里按"是不是推理模型"把 `tools` 摘掉，
        // 依据是 R1 时代"推理模型不支持 Function Calling"的说法；官方 Thinking
        // Mode 文档已经明确"thinking mode supports tool calls"，所以不再摘。
        // 代价是必须把历史 `reasoning_content` 完整回传（见 `state::api_messages`），
        // 那条由 `neo_llm::build_wire` 按档位把关。
        let tools = neo_tools::tool_declarations();
        // 通用入口在联网前执行预算及无分配的线协议计数；错误通过 Failed 回到消息。
        #[cfg(not(test))]
        let stream = neo_llm::start_with_tools(cfg, msgs, tools);
        #[cfg(test)]
        let stream = STREAM_START.with(|start| start.get()(cfg, msgs, tools));
        state.start_generation(StreamSource::Real(Box::new(stream)));
    }

    /// 与通知分开：无托盘也记录终态；压缩续轮/离线回复排队期间不能提前结案。
    fn finish_idle_task_diagnostic(state: &mut AppState) {
        if state.generating || state.tool_open || state.tool_round || state.tools_running()
            || state.compaction.is_some() || state.compaction_resume || state.wants_demo_reply
        { return; }
        let failed = state.messages.iter().rev().find(|msg| msg.role == Role::Assistant)
            .is_some_and(|msg| msg.error.is_some()) || state.attachment_error.is_some();
        state.finish_task_diagnostic(if state.round_cancelled { Phase::CancelRequested }
            else if failed { Phase::Failed } else { Phase::Completed }, None);
    }

    fn resume_compaction(state: &mut AppState, persistence_ok: bool) {
        // 保存失败仍可重试；保留续轮身份，不能把等待保存误记为任务结束。
        if !state.compaction_resume || !persistence_ok { return; }
        state.compaction_resume = false;
        let config_matches = state.compaction_resume_config.take().as_ref() == Some(&state.llm_config());
        if state.round_cancelled {
            state.finish_task_diagnostic(Phase::CancelRequested, None);
            return;
        }
        if !config_matches || !state.can_call_real() {
            state.compaction_status = Some("配置已改变或模型不可用，摘要后的自动续轮已取消".into());
            state.finish_task_diagnostic(Phase::Failed, Some(Failure::Configuration));
            return;
        }
        if state.task_limit_reached || state.task_tool_calls >= crate::state::TASK_TOOL_LIMIT {
            state.attachment_error = Some("本任务已达到500次工具调用上限，已停止自动续轮".into());
            state.finish_task_diagnostic(Phase::Rejected, Some(Failure::ToolLimit));
            return;
        }
        let mut messages = state.api_messages(usize::MAX);
        match neo_llm::budget_messages(&state.llm_config(), &mut messages, &neo_tools::tool_declarations()) {
            Ok(()) => Self::start_real_stream_with_messages(state, messages),
            Err(error) => {
                state.attachment_error = Some(error);
                state.finish_task_diagnostic(Phase::Rejected, Some(Failure::Budget));
            }
        }
    }

    fn advance_tools(state: &mut AppState, store: Option<&Store>) -> bool {
        // 已启动任务即使数据库失败也必须收取；执行与回灌各自经过保存屏障。
        state.poll_tool_jobs();
        if !Self::persist_ready(state, store) {
            return false;
        }
        if state.tool_open
            && state.awaiting_tool().is_none()
            && !state.tools_running()
            && !state.tools_settled()
        {
            let scope = neo_tools::Scope::new(state.workspace_root());
            state.spawn_ready_tools(&scope);
        }
        Self::persist_ready(state, store)
    }

    fn delete_session(state: &mut AppState, store: &Store, id: i64) -> bool {
        if state.active_session == Some(id) && !Self::prepare_session_change(state, Some(store)) {
            return false;
        }
        match store.delete_session(id) {
            Ok(_) => {
                Self::refresh_sessions(state, store);
                if state.active_session == Some(id) {
                    state.new_session();
                }
                true
            }
            Err(error) => {
                state.attachment_error = Some(format!("删除会话失败：{error}"));
                false
            }
        }
    }

    fn prepare_session_change(state: &mut AppState, store: Option<&Store>) -> bool {
        state.poll_tool_jobs();
        state.cancel();
        Self::persist_ready(state, store) && state.pending_persist == state.messages.len()
    }

    /// 只提交已经稳定的消息；生成中的占位与未完成工具不能提前保存。
    fn persist_ready(state: &mut AppState, store: Option<&Store>) -> bool {
        if state.pending_persist >= state.messages.len() && !state.checkpoint_dirty {
            return true;
        }
        let Some(store) = store else {
            state.checkpoint_dirty = false;
            while let Some(msg) = state.messages.get(state.pending_persist) {
                if msg.streaming || msg.tool.as_ref().is_some_and(|t| !t.state.is_settled()) {
                    break;
                }
                state.pending_persist += 1;
            }
            return true;
        };
        let had_session = state.active_session.is_some();
        let Some(session) = Self::ensure_session(state, store) else {
            state.attachment_error = Some("无法保存会话，请检查数据库位置及磁盘空间".into());
            return false;
        };
        let mut wrote = false;
        while let Some(msg) = state.messages.get(state.pending_persist) {
            if msg.streaming || msg.tool.as_ref().is_some_and(|t| !t.state.is_settled()) {
                break;
            }
            let result = serde_json::to_string(&msg.attachments)
                .map_err(|e| e.to_string())
                .and_then(|json| {
                    let calls = serde_json::to_string(&msg.tool_calls).map_err(|e| e.to_string())?;
                    store
                        .append_message_with_protocol(
                            session,
                            msg.role.as_str(),
                            &msg.content,
                            &msg.reasoning,
                            &msg.meta,
                            &json,
                            &calls,
                            msg.tool.as_ref().map_or("", |t| t.call_id.as_str()),
                        )
                        .map_err(|e| e.to_string())
                });
            if result.is_err() {
                record(Level::Error, "storage", "消息写入失败，已保留未保存数据");
                state.attachment_error = Some("消息尚未保存，请检查数据库权限及磁盘空间；请勿关闭或切换会话".into());
                return false;
            }
            state.pending_persist += 1;
            wrote = true;
        }
        if state.checkpoint_dirty {
            let saved = state.checkpoint.as_ref().filter(|c| c.valid(&state.messages))
                .and_then(|c| serde_json::to_string(c).ok().map(|json| (c.covered, json)))
                .is_some_and(|(covered, json)| store.save_checkpoint(session, covered, &json).is_ok());
            if !saved {
                state.attachment_error = Some("历史摘要尚未保存，暂停续轮；请检查数据库后重试".into());
                return false;
            }
            state.checkpoint_dirty = false;
        }
        // 只有真写过（或刚建了新会话）才刷新侧栏 —— 生成中每 33ms 都经过
        // 这里，空转时的全表 SELECT 是纯浪费。
        if wrote || !had_session {
            Self::refresh_sessions(state, store);
        }
        true
    }

    fn rename_session(state: &mut AppState, store: Option<&Store>, id: i64, title: &str) {
        if let Some(store) = store {
            if store.rename_session(id, title).is_ok() {
                Self::refresh_sessions(state, store);
                return;
            }
        }
        record(Level::Error, "storage", "会话重命名保存失败");
        state.attachment_error = Some("会话重命名未保存，请检查数据库权限及磁盘空间后重试".into());
        state.renaming = Some(id);
        state.rename_draft = title.to_owned();
        state.rename_request_focus = true;
    }

    /// 设置变更后写库。
    fn save_setting(store: Option<&Store>, key: &str, value: &str) -> bool {
        let saved = store.is_some_and(|store| store.set_setting(key, value).is_ok());
        if !saved {
            record(Level::Error, "settings", "配置保存失败，尚未持久化");
        }
        saved
    }

    fn persist_preferences(&mut self) -> bool {
        let state = &self.state;
        let mut saved = true;
        // 只在真实写入成功后推进基准，失败保留差异供下一次逻辑帧重试。
        macro_rules! save_pref {
            ($value:expr, $saved:expr, $key:literal, $encoded:expr) => {
                if $value != $saved {
                    if Self::save_setting(self.store.as_ref(), $key, $encoded) {
                        $saved = $value;
                    } else {
                        saved = false;
                    }
                }
            };
        }
        save_pref!(state.context_tokens, self.saved_context_tokens, "context_tokens", &state.context_tokens.to_string());
        save_pref!(state.show_reasoning, self.saved_prefs.0, "show_reasoning", if state.show_reasoning { "1" } else { "0" });
        save_pref!(state.thinking, self.saved_prefs.1, "thinking", state.thinking.key());
        save_pref!(state.theme_mode, self.saved_prefs.2, "theme", theme_mode_key(state.theme_mode));
        save_pref!(state.distance, self.saved_prefs.3, "distance", distance_key(state.distance));
        save_pref!(state.minimize_to_tray, self.saved_prefs.4, "minimize_to_tray", if state.minimize_to_tray { "1" } else { "0" });
        save_pref!(state.wake_enabled, self.saved_prefs.5, "wake_enabled", if state.wake_enabled { "1" } else { "0" });
        save_pref!(state.start_in_tray, self.saved_prefs.6, "start_in_tray", if state.start_in_tray { "1" } else { "0" });
        save_pref!(state.class_enabled, self.saved_prefs.7, "class_enabled", if state.class_enabled { "1" } else { "0" });
        save_pref!(state.classroom_safe, self.saved_classroom_safe, "classroom_safe", if state.classroom_safe { "1" } else { "0" });
        save_pref!(state.silent_startup_errors, self.saved_silent_startup_errors, "silent_startup_errors", if state.silent_startup_errors { "1" } else { "0" });
        save_pref!(state.auto_check_updates, self.saved_auto_check_updates, "auto_check_updates", if state.auto_check_updates { "1" } else { "0" });
        if (state.api_base.as_str(), state.api_key.as_str())
            != (self.saved_api.0.as_str(), self.saved_api.1.as_str())
        {
            let base_saved = Self::save_setting(self.store.as_ref(), "api_base", &state.api_base);
            let key_saved = Self::save_setting(self.store.as_ref(), "api_key", &state.api_key);
            if base_saved && key_saved {
                self.saved_api = (state.api_base.clone(), state.api_key.clone());
            } else {
                saved = false;
            }
        }
        self.state.preferences_unsaved = !saved;
        saved
    }

    fn sync_safety(&mut self, ctx: &egui::Context) {
        self.state.sync_startup_policy(crate::startup::set_silent_policy);
        if self.applied_classroom_safe == self.state.classroom_safe {
            return;
        }
        self.applied_classroom_safe = self.state.classroom_safe;
        self.stop_wake_test();
        if self.state.classroom_safe {
            self.state.cancel();
            self.state.auto_approve_tools = false;
            self.end_dictation();
            if let Some(tx) = &self.stt_tx {
                let _ = tx.send(SttCmd::Reset(self.dictation_epoch));
            }
            self.wake_rx = None;
            if self.hidden_to_tray {
                self.show_window(ctx);
            }
        }
    }

    fn poll_wake_events(&mut self, ctx: &egui::Context, suppress: bool) {
        loop {
            let event = self.wake_rx.as_ref().map(|rx| rx.try_recv());
            match event {
                Some(Ok(event)) if !event.is_current(self.wake_epoch) => continue,
                Some(Ok(neo_wake::WakeEvent::Detected { score, .. })) => {
                    if !suppress && self.state.effective_wake_enabled() && !self.wake_broken {
                        self.on_wake_detected(ctx, score);
                    }
                }
                Some(Ok(neo_wake::WakeEvent::Audio { frame, .. })) => {
                    if !suppress && self.state.effective_wake_enabled() && !self.wake_broken {
                        self.on_dictation_audio(ctx, frame);
                    }
                }
                Some(Ok(neo_wake::WakeEvent::Error(msg))) => {
                    self.fail_wake(msg);
                    break;
                }
                Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => {
                    self.fail_wake("唤醒线程已断开，请检查设备或学校部署的模型资源".into());
                    break;
                }
                Some(Err(std::sync::mpsc::TryRecvError::Empty)) | None => break,
            }
        }
    }

    fn fail_wake(&mut self, msg: String) {
        self.capture_wake_diagnostics();
        self.stop_wake_test();
        self.cancel_dictation();
        self.state.wake_test.error = Some(msg.chars().take(512).collect());
        self.state.wake_test.broken = true;
        self.wake_broken = true;
        record(Level::Warn, "wake", "语音唤醒发生错误，已停用");
        self.pending_toasts.push((
            neo_ui::ToastKind::Warning,
            "语音唤醒不可用：请在设置 → 麦克风测试查看错误，关闭再打开语音唤醒可重试".into(),
            std::time::Instant::now() + std::time::Duration::from_secs(4),
        ));
        self.wake_rx = None;
        self.wake = None;
    }

    fn stop_wake_test(&mut self) {
        if self.state.wake_test.running {
            self.capture_wake_diagnostics();
            self.wake_epoch = self.wake_epoch.wrapping_add(1);
            if let Some(wake) = &self.wake {
                wake.set_test_mode(false);
                self.wake_epoch = wake.event_epoch();
                wake.set_diagnostics(false);
            }
        }
        self.state.wake_test.running = false;
        self.state.wake_test.requested = false;
    }

    fn capture_wake_diagnostics(&mut self) {
        if let Some(wake) = &self.wake {
            let snapshot = wake.diagnostics();
            if let Some(error) = &snapshot.error {
                self.state.wake_test.error = Some(error.clone());
            }
            self.state.wake_test.snapshot = Some(snapshot);
        }
    }

    // 返回值覆盖进出测试的整批队列；错误事件始终保留。
    fn sync_wake_test(&mut self, engine_available: bool) -> bool {
        let was_running = self.state.wake_test.running;
        self.state.wake_test.busy = self.dictating || self.state.generating
            || self.state.tool_open || self.state.tool_round || self.state.compaction.is_some()
            || self.state.compaction_resume || self.state.wants_demo_reply;
        self.state.wake_test.broken = self.wake_broken;
        let allowed = self.state.show_settings
            && self.state.settings_tab == crate::state::SettingsTab::WakeTest
            && self.state.effective_wake_enabled()
            && !self.state.wake_test.busy && !self.wake_broken
            && !self.hidden_to_tray && !self.quitting && !self.exit_blocked;
        if !allowed || !self.state.wake_test.requested {
            self.stop_wake_test();
        } else if !was_running && engine_available {
            self.wake_epoch = self.wake_epoch.wrapping_add(1);
            if let Some(wake) = &self.wake {
                wake.set_test_mode(true);
                self.wake_epoch = wake.event_epoch();
                wake.set_diagnostics(true);
            }
            self.state.wake_test.running = true;
        }
        if self.state.wake_test.running {
            self.capture_wake_diagnostics();
        }
        was_running || self.state.wake_test.running || self.state.wake_test.requested
    }

    /// 视口尺寸 / 主题 / 距离任一变化时重建主题。
    fn sync_theme(&mut self, ctx: &egui::Context, viewport: Vec2) {
        let key: Fingerprint = (
            self.state.theme_mode,
            self.state.distance,
            viewport.y.round() as i32,
        );
        if self.fingerprint == Some(key) {
            return;
        }
        self.theme = Theme::new(self.state.theme_mode, viewport.y, self.state.distance);
        self.theme.apply(ctx);
        self.fingerprint = Some(key);
    }

    fn queue_duplicate(&mut self, duplicate: bool) {
        if duplicate {
            self.pending_toasts.push((
                neo_ui::ToastKind::Info,
                "Neo 已经在运行".into(),
                std::time::Instant::now() + std::time::Duration::from_secs(5),
            ));
        }
    }

    fn poll_updates(&mut self, ctx: &egui::Context) {
        self.poll_updates_at(ctx, std::time::Instant::now());
    }

    fn poll_updates_at(&mut self, ctx: &egui::Context, now: std::time::Instant) {
        use crate::updates::Status;
        if self.quitting || self.exit_blocked {
            self.state.update_check_requested = false;
            self.update_schedule.manual_pending = false;
            return;
        }
        if !self.state.auto_check_updates && self.update_schedule.auto_inflight {
            self.update_checker.cancel();
            self.update_schedule.auto_inflight = false;
            self.state.update_status = Status::Idle;
        }
        if let Some(status) = self.update_checker.poll() {
            self.accept_update_status(status);
        }
        let manual = std::mem::take(&mut self.state.update_check_requested);
        let checking = self.update_checker.is_running()
            || matches!(self.state.update_status, Status::Checking);
        if self.update_schedule.request_due(now, self.state.auto_check_updates, manual, checking).is_some() {
            #[cfg(not(test))]
            self.update_checker.request(ctx);
            // 测试走同一 single-flight/结果通道，只替换 worker，绝不接触网络。
            #[cfg(test)]
            self.update_checker.request_controlled(ctx, Status::UpToDate)();
            self.state.update_status = self.update_checker.status().clone();
        }
        let checking = self.update_checker.is_running()
            || matches!(self.state.update_status, Status::Checking);
        if let Some(deadline) = self.update_schedule.next_deadline(now, checking) {
            ctx.request_repaint_after(deadline.saturating_duration_since(now));
        }
    }

    fn accept_update_status(&mut self, status: crate::updates::Status) {
        use crate::updates::Status;
        if matches!(status, Status::Failed(_)) {
            record(Level::Warn, "updates", "更新检查失败，可稍后手动重试");
            crate::startup::record_error("updates", "更新检查失败，可稍后手动重试");
        }
        if self.update_schedule.auto_inflight && self.state.auto_check_updates
            && !self.update_schedule.notified && matches!(status, Status::Available { .. })
        {
            self.update_schedule.notified = true;
            self.pending_toasts.push((neo_ui::ToastKind::Info,
                "Neo 有新版本，可在设置 → 关于查看；不会自动下载".into(),
                std::time::Instant::now() + std::time::Duration::from_secs(8)));
        }
        self.update_schedule.auto_inflight = false;
        self.state.update_status = status;
    }

    /// 每次 logic pass 推进一次屏障；hide 与 verify 之间留出原生事件处理机会。
    fn tick_desktop_barrier(&mut self, ctx: &egui::Context) {
        // request_discard 会在返回原生事件循环前重跑 logic/ui，不能在该 pass 放行。
        if ctx.current_pass_index() != 0 { return; }
        use std::sync::atomic::Ordering;
        let active = self.state.desktop_execution.as_ref().is_some_and(|gate| gate.active.load(Ordering::Acquire) != 0);
        if self.desktop_windows.manually_restored() {
            self.show_window(ctx);
            if active {
                // 任务栏恢复也算用户打断；旧 UIA 退出前仍保持暂避。
                let _ = self.desktop_windows.hide(true);
            }
        }
        let blocked = self.desktop_show_pending || self.quitting || self.exit_blocked
            || self.state.awaiting_tool().is_some();
        // hide 与 verify 分属两帧；verify 不再补 hide，避免把失败当作成功。
        for request in std::mem::take(&mut self.desktop_pending) {
            let result = if !request.live() || blocked {
                Err("桌面请求已取消、超时或仍待审批，未派发".into())
            } else {
                self.desktop_windows.verify()
            };
            if result.is_err() { self.desktop_restore_pending = true; }
            if request.ack.send(result).is_err() { self.desktop_restore_pending = true; }
        }
        for request in self.desktop_rx.try_iter() {
            if !request.live() || blocked {
                let _ = request.ack.send(Err("旧桌面请求已失效或仍待审批，未暂避".into()));
                continue;
            }
            self.desktop_cancels.push(request.cancel.clone());
            match self.desktop_windows.hide(!self.hidden_to_tray) {
                Ok(()) => self.desktop_pending.push(request),
                Err(error) => {
                    self.desktop_restore_pending = true;
                    let _ = request.ack.send(Err(error));
                }
            }
        }
        // 同一轮观察→操作之间不恢复主窗，不让 snapshot 的真实前台被 Neo 改变。
        // 新审批则在所有旧 UIA 租约结束后恢复，以保持审批界面可见。
        let round_busy = self.state.generating || self.state.tool_open || self.state.tool_round;
        let hold_main = self.desktop_windows.main_avoided && round_busy && !blocked
            && !self.desktop_restore_pending;
        // 主窗可整轮暂避，但确认卡只在实际桌面租约/暂避验证期间挂起。
        let suspended = active && !self.desktop_cancels.is_empty()
            || !self.desktop_pending.is_empty();
        ctx.data_mut(|data| data.insert_temp(egui::Id::new("neo-desktop-suspended"), suspended));

        if !active && self.desktop_pending.is_empty() && !hold_main {
            self.desktop_windows.restore();
            self.desktop_cancels.clear();
            self.desktop_restore_pending = false;
            if self.desktop_exit_pending { self.finish_exit(ctx); }
            else if self.desktop_show_pending { self.show_window(ctx); }
        }
        if suspended || active {
            ctx.request_repaint_after(std::time::Duration::from_millis(10));
        }
        if let Some(gate) = &mut self.state.desktop_execution {
            gate.overlay = self.overlay.as_ref().map(|overlay| overlay.desktop_gate());
        }
    }

    fn tick(&mut self, ctx: egui::Context) {
        self.tick_desktop_barrier(&ctx);
        // 主界面已完成装配；所有提前返回路径之前都结束启动覆盖窗。
        crate::startup::finish_splash();
        if self.quitting {
            return;
        }
        self.queue_duplicate(crate::startup::poll_duplicate());
        // 重复启动事件没有界面输入，主窗静止或隐藏时仍须响应。
        ctx.request_repaint_after(std::time::Duration::from_millis(500));
        self.sync_safety(&ctx);
        if !self.persist_preferences() {
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
        }
        if ctx.input(|i| i.viewport().close_requested()) {
            self.handle_close(&ctx);
        }
        self.poll_tray(&ctx);
        // 保存失败等待用户选择时，不得再启动生成、工具或音频采集。
        if self.quitting || self.exit_blocked {
            self.stop_wake_test();
            if !self.quitting {
                self.toastwin.tick(&ctx, &mut self.pending_toasts, self.theme, self.overlay.as_ref());
            }
            return;
        }
        self.poll_updates(&ctx);
        // 启动即后台：首帧直接进托盘等「Hi, Neo」，主界面不露面。
        // 走首帧而不是 `ViewportBuilder::with_visible(false)`：hide_to_tray 的
        // 注释提到 wgpu 对隐藏窗仍会取交换链，最小化走「尺寸为零跳过」的既有路径。
        if !self.start_hidden_done {
            self.start_hidden_done = true;
            if self.state.effective_start_in_tray() && self.tray.is_some() {
                self.hidden_to_tray = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            }
        }
        // 唤醒开关热切换（设置页改完下一帧生效）：关掉立即停引擎——
        // Drop 会置停止位并 join 线程；打开则起引擎（start_wake 幂等）。
        // 放在解构借用 state 之前，否则 &mut self 方法会撞借用检查。
        if self.state.effective_wake_enabled() && !self.wake_broken {
            #[cfg(not(test))]
            self.start_wake(&ctx);
        } else if !self.state.effective_wake_enabled() {
            self.stop_wake_test();
            self.cancel_dictation();
            // Drop 会 join 引擎线程，而引擎加载模型/开麦克风流不可中断
            //（首次 0.5~3s）——在 UI 线程 join 就是冻结界面。转交后台线程等。
            if let Some(engine) = self.wake.take() {
                let _ = std::thread::Builder::new()
                    .name("neo-wake-drop".into())
                    .spawn(move || drop(engine));
                // spawn 失败（极端）时 engine 随闭包回收原地 Drop ——
                // 顶多卡这一次，比没有降级强。
            }
            self.wake_rx = None;
            // 开关拨回「关」即解锁故障锁存：重新打开视为一次手动重试。
            // 风暴防护仍在——引擎再次报错会重新置位 wake_broken。
            self.wake_broken = false;
        }
        self.state.poll_attachments();
        if self.state.attachment_busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        if let Some(rx) = &self.workspace_picker {
            match rx.try_recv() {
                Ok(path) => {
                    if let Some(path) = path {
                        self.state.workspace = Some(
                            path.file_name()
                                .map(|name| name.to_string_lossy().into_owned())
                                .unwrap_or_else(|| path.display().to_string()),
                        );
                        self.state.workspace_dir = Some(path);
                    }
                    self.workspace_picker = None;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.workspace_picker = None,
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(100));
                }
            }
        }
        let suppress_wake = self.sync_wake_test(self.wake.is_some());
        self.check_overlay_health(&ctx);
        self.poll_wake_events(&ctx, suppress_wake);
        // STT 转写结果：只有空编辑器才直接发送；有未提交输入则唤回主窗确认。
        let stt_out = self.stt_rx.as_ref().map(|rx| rx.try_recv());
        match stt_out {
            Some(Ok(SttResult::Transcript(epoch, _)))
                if !current_dictation(self.dictating, self.dictation_epoch, epoch) =>
            {
                ctx.request_repaint();
            }
            Some(Ok(SttResult::Transcript(_, Ok(text)))) => {

                let text = text.trim().to_owned();
                if self.dictating && !text.is_empty() {
                    self.end_dictation();
                    if Self::prepare_dictation_input(
                        &mut self.state,
                        self.store.as_ref(),
                        &text,
                        self.last_round_done,
                    ) {
                        Self::send_input(&mut self.state, self.store.as_ref());
                    }
                    if !self.state.draft.is_empty() {
                        // 等待确认或发送失败：不重试自动发送，主窗展示保留的两种输入。
                        self.show_window(&ctx);
                        self.pending_toasts.push((
                            neo_ui::ToastKind::Warning,
                            "语音已保留在草稿，尚未发送；请核对文字和附件后手动发送".into(),
                            std::time::Instant::now() + std::time::Duration::from_secs(6),
                        ));
                    }
                }
            }
            Some(Ok(SttResult::StartupError(msg)))
            | Some(Ok(SttResult::Transcript(_, Err(msg)))) => {
                // 模型缺失 / 转写失败：降级为无听写，提示一次。
                record(Level::Warn, "stt", "语音转写不可用");
                if self.dictating {
                    self.end_dictation();
                }
                self.stt_tx = None;
                self.pending_toasts.push((
                    neo_ui::ToastKind::Warning,
                    format!("语音转写未启用：{msg}"),
                    std::time::Instant::now() + std::time::Duration::from_secs(4),
                ));
            }
            Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => {
                self.cancel_dictation();
                self.stt_rx = None;
                self.stt_tx = None;
            }
            Some(Err(std::sync::mpsc::TryRecvError::Empty)) | None => {}
        }
        // 沉默超时：唤醒后一直没说出完整句子，自动收场回唤醒检测。
        if self.dictating {
            // 点击 = 打断：跑马灯是鼠标穿透窗（WS_EX_TRANSPARENT），收不到点击，
            // 改为轮询全局左键的按下沿；丢弃本次识别，回到唤醒检测。
            let down = lmb_down();
            if down && !self.dictation_lmb_down {
                self.cancel_dictation();
                self.pending_toasts.push((
                    neo_ui::ToastKind::Info,
                    "已取消本次听写".to_owned(),
                    std::time::Instant::now() + std::time::Duration::from_secs(2),
                ));
            }
            self.dictation_lmb_down = down;
            if self
                .dictation_since
                .is_some_and(|t| t.elapsed() > DICTATION_TIMEOUT)
            {
                self.end_dictation();
            }
            // 听写期间保持帧循环：沉默超时、点击打断与电平回落都靠它。
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        // 迷你窗：主窗藏起 + AI 在忙时贴屏幕角落（对话速览 / 截屏回避 / 打断确认）。
        // 打断判定要排除课堂总结弹窗的矩形：点它的「关闭」不是「打断 AI」。
        let classwin_rect = self.classwin.visible_rect_physical(&ctx);
        // 只有纯视觉内容进入 overlay；确认与课堂弹窗始终使用独立交互视口。
        let overlay = self.overlay.as_ref();
        self.miniwin.tick(
            &ctx,
            &mut self.state,
            self.theme,
            self.hidden_to_tray || self.desktop_windows.main_avoided,
            classwin_rect,
            overlay,
        );
        // 截屏闪光：抓帧完成后在被抓区域边缘闪一道白框。
        self.shotflash.tick(&ctx, overlay);
        // 工具确认/提问使用不透明客户区，不把留白处点击漏给桌面。
        self.confirmwin
            .tick(&ctx, &mut self.state, self.theme, overlay);
        // 课堂总结：最大化监听 / 转写 / 打磨状态机（默认关，设置里开）。
        self.class.tick(&ctx, &self.state, self.state.effective_class_enabled());
        // 课堂总结弹窗：打磨就绪后从屏幕上方滑入。
        self.classwin
            .tick(&ctx, &mut self.class, self.theme, overlay);
        // 总结起止红点：开始/结束打磨时左上角亮 5s（计数差值驱动）。
        let pings = self.class.summary_pings;
        if pings != self.class_pings_seen {
            self.class_pings_seen = pings;
            self.class_dot.ping();
        }
        self.class_dot.tick(overlay, self.theme);
        // 状态行同步给设置页（开关下方的「记录中…」提示）。
        self.state.class_status = self.class.status().map(|s| s.to_owned());
        // 忙→闲沿：任务收尾，弹一条 Windows 原生通知。被打断
        // （round_cancelled）或出错的轮次不报「完成」——那不是完成。
        let busy = self.state.generating || self.state.tool_open || self.state.tool_round;
        if self.was_busy && !busy {
            // 记「上次对话完成时刻」：5 分钟内再次语音唤醒会续上这段对话
            // （见 STT 转写收取处），而不是开一段全新对话。
            self.last_round_done = Some(std::time::Instant::now());
            if self.state.round_cancelled {
                self.state.round_cancelled = false;
            } else if self.tray.is_some() {
                // tray 存在 = 真实客户端（离屏测试不装托盘），借它当门槛。
                let last = self
                    .state
                    .messages
                    .iter()
                    .rev()
                    .find(|m| m.role == Role::Assistant);
                let failed = last.is_some_and(|m| m.error.is_some());
                if !failed {
                    let line: String = last
                        .and_then(|m| m.content.lines().find(|l| !l.trim().is_empty()))
                        .map(|l| l.trim().chars().take(80).collect())
                        .unwrap_or_default();
                    crate::notify::task_done(&line);
                }
            }
        }
        self.was_busy = busy;
        // 记忆热重载：remember/forget 工具在工具线程写盘，这里 2s 一拍沿检。
        if self.memory_poll.elapsed() > std::time::Duration::from_secs(2) {
            self.memory_poll = std::time::Instant::now();
            self.state.maybe_reload_memories();
        }
        // 记忆导入导出对话框的回执落地（toast + 立即重读）。
        let io_recv = self.state.memory_io_rx.as_ref().map(|rx| rx.try_recv());
        match io_recv {
            Some(Err(std::sync::mpsc::TryRecvError::Empty)) | None => {}
            Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => {
                // 用户在文件对话框按了「取消」：工作线程直接 return、tx 随 Drop
                // 断开。不在这里回收的话 memory_io_rx 恒为 Some，「导入/导出」
                // 按钮被 is_some() 门吞掉，对话框再也弹不出来（直到重启）。
                self.state.memory_io_rx = None;
            }
            Some(Ok(msg)) => {
                self.state.memory_io_rx = None;
                // 导入刚写完盘：清 mtime 缓存强制重读（粒度可能骗过沿检）。
                self.state.memories_file_ms = None;
                self.state.maybe_reload_memories();
                let (kind, text) = match msg {
                    crate::state::MemoryIoMsg::Imported(Ok((added, skipped))) => (
                        neo_ui::ToastKind::Success,
                        format!("已导入 {added} 条记忆（跳过 {skipped} 条重复）"),
                    ),
                    crate::state::MemoryIoMsg::Imported(Err(e)) => {
                        (neo_ui::ToastKind::Error, format!("导入失败：{e}"))
                    }
                    crate::state::MemoryIoMsg::Exported(Ok(n)) => {
                        (neo_ui::ToastKind::Success, format!("已导出 {n} 条记忆"))
                    }
                    crate::state::MemoryIoMsg::Exported(Err(e)) => {
                        (neo_ui::ToastKind::Error, format!("导出失败：{e}"))
                    }
                };
                self.pending_toasts.push((
                    kind,
                    text,
                    std::time::Instant::now() + std::time::Duration::from_secs(3),
                ));
            }
        }
        // 「打开主界面」工具：执行层只置信号位，这里真正唤窗。
        let show_at =
            neo_tools::tools::open_app::SHOW_WINDOW_AT.load(std::sync::atomic::Ordering::Relaxed);
        self.handle_show_signal(&ctx, show_at);
        // ---- 状态机推进（与绘制无关，托盘隐藏时也照跑）----
        // 这段曾经住在 render() 里：主窗一藏进托盘，生成泵 / 工具轮 / 回灌
        // 全部停摆 —— 语音唤醒的后台链路（生成中弹确认窗、批了继续跑）整个冻住。
        // 它们只依赖 state 与 ctx，本就该走 logic-only 路径。
        //
        // 每帧从流式来源泵增量；真实 / 演示两种来源在 UI 侧等价。
        if self.state.poll_compaction() {
            ctx.request_repaint();
        }
        if self.state.generating && self.state.pump() {
            ctx.request_repaint_after(std::time::Duration::from_millis(33));
        }
        // 模型列表拉取：后台线程在跑，这里只收结果；收到就刷新可选模型。
        if self.state.poll_model_fetch() {
            // 拉到的列表落库：下次启动先读回来，没网也有模型可选。
            // 列表为空时**不写** —— 别拿空值覆盖上一次拉到的好数据。
            if self.state.has_models() {
                Self::save_setting(
                    self.store.as_ref(),
                    "models",
                    &self.state.model_ids_joined(),
                );
                Self::save_setting(self.store.as_ref(), "model", self.state.model_id());
            }
            ctx.request_repaint();
        }
        // 工具轮登记：流结束在 `finish_reason = tool_calls` 时把分片聚合成消息块。
        if self.state.tool_round {
            self.state.begin_tool_round();
        }
        // 用户消息在生成开始前立即保存，流式占位只在稳定后提交。
        // 结果给渲染段做「切会话」门控（落库失败不切，免得丢消息）。
        self.persistence_ok = Self::advance_tools(&mut self.state, self.store.as_ref());
        Self::resume_compaction(&mut self.state, self.persistence_ok);
        // 演示流：未配置密钥时，submit 之后自动接一条离线演示回复。
        if self.state.wants_demo_reply && self.persistence_ok {
            self.state.wants_demo_reply = false;
            let prompt = self
                .state
                .messages
                .iter()
                .rev()
                .find(|m| m.role == Role::User)
                .map(|m| m.content.clone())
                .unwrap_or_default();
            let demo = crate::state::demo_reply(&prompt, self.state.model_display());
            self.state.start_generation(StreamSource::Demo {
                text: demo,
                cursor: 0,
            });
        }
        // 工具执行与回灌：工具在独立线程里跑（可能是几分钟的编译），
        // 这里只逐帧收结果，绝不阻塞。
        if self.state.tools_running() {
            ctx.request_repaint_after(std::time::Duration::from_millis(60));
        }
        if !self.persistence_ok {
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
        }
        if self.state.tool_open && self.persistence_ok {
            if self.state.awaiting_tool().is_none() {
                if self.state.tools_running() {
                    // 后台在跑：定期回来收结果（也顺带刷新"执行中…"的动画）。
                    ctx.request_repaint_after(std::time::Duration::from_millis(60));
                } else if self.state.tools_settled() {
                    self.state.tool_open = false;
                    if self.state.can_call_real() {
                        Self::start_real_stream(&mut self.state);
                    } else {
                        // 工具跑完了但接口配置已被清空：不说一声的话
                        // 这轮「无声结束」，用户会以为卡死。
                        self.state.finish_task_diagnostic(Phase::Failed, Some(Failure::Configuration));
                        self.state.attachment_error = Some(
                            "工具已执行完，但模型接口未配置，结果未回灌；配置好后发条消息即可继续。"
                                .into(),
                        );
                    }
                }
            }
        }
        // 本帧生成/工具/压缩续轮全部推进后结案；render 可紧接着提交或切会话，
        // 不能拖到下一帧再让 begin_task/cancel 将已完成任务误报为放弃/取消。
        Self::finish_idle_task_diagnostic(&mut self.state);
        // 活跃会话变化时落库：下次启动从这里恢复。空串表示空态。
        if self.state.active_session != self.saved_active {
            let v = self
                .state
                .active_session
                .map(|id| id.to_string())
                .unwrap_or_default();
            if Self::save_setting(self.store.as_ref(), "active_session", &v) {
                self.saved_active = self.state.active_session;
            }
        }
        self.toastwin.tick(&ctx, &mut self.pending_toasts, self.theme, self.overlay.as_ref());
        if self.state.wake_test.running {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
        // 托盘没有 winit 唤醒源：隐藏期间保持低频轮询，托盘菜单点击才有响应。
        if self.hidden_to_tray {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
    }

    /// 一帧的编排入口。
    ///
    /// 公开而非私有：离屏渲染测试直接驱动它。
    pub fn render(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        // 状态机推进（生成泵 / 工具轮 / 回灌）在 tick() 里已完成 —— 托盘隐藏时
        // 照常走。这里剩下的只有绘制：主窗不可见时若照常构建主窗 UI
        // （对话列表 + markdown + 代码高亮），每 16ms 一次全量重建会把小窗动画
        // 拖垮。反正主窗不可见，这一帧空跑 —— 恢复可见时下一帧自动全量重画
        // （动效沿检重新播种，无害）。
        if self.hidden_to_tray || self.quitting {
            return;
        }
        if self.exit_blocked {
            self.exit_dialog(&ctx);
            return;
        }
        if let Some(status) = self.state.compaction_status.clone() {
            egui::Window::new("上下文状态").collapsible(false).resizable(false)
                .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 8.0))
                .show(&ctx, |ui| {
                    ui.label(status);
                    if self.state.compaction.is_some() {
                        if ui.button("取消压缩").clicked() { self.state.cancel(); }
                    } else if ui.button("关闭提示").clicked() {
                        self.state.compaction_status = None;
                    }
                });
        }
        if self.state.preferences_unsaved {
            egui::Window::new("设置未保存")
                .collapsible(false).resizable(false)
                .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 8.0))
                .show(&ctx, |ui| {
                ui.colored_label(egui::Color32::RED,
                    "设置尚未保存；安全限制仅对本次运行生效，重启可能恢复旧值。");
                if ui.button("重试保存设置").clicked() {
                    self.persist_preferences();
                }
            });
        }
        let modal_open = self.state.show_settings
            || self.workspace_picker.is_some()
            || self.state.attachment_picker_open;
        // eframe 交给 `App::ui` 的根 `Ui` 比屏幕内缩一圈（默认 8pt）。
        // 背景与遮罩必须铺满 `screen`，否则四周会留出一条未绘制的黑边；
        // 布局仍然走 `content`。
        let screen = ctx.content_rect();
        let content = ui.max_rect();

        // ---- 0. 动效沿检 ----
        // 舞台切换 / 设置打开各播一次 0.2s 入场（spec 标准档）。egui 动画首调
        // 直接返回目标值，切沿这一帧要先播种 0 再启动到 1，之后每帧续播 1，
        // 收敛后是纯查表。prev_* 记的是「上一帧画出去的」值：状态变更都发生
        // 在绘制之后，帧首读到的 state 即本帧将要画的，检出沿后立刻记下。
        let stage_changed = self.prev_stage != self.state.stage;
        let settings_just_opened = self.state.show_settings && !self.prev_show_settings;
        self.prev_stage = self.state.stage;
        self.prev_show_settings = self.state.show_settings;
        let stage_k = if stage_changed {
            ctx.animate_value_with_time(Id::new("neo-stage-enter"), 0.0, MODAL_FADE);
            ctx.animate_value_with_time(Id::new("neo-stage-enter"), 1.0, MODAL_FADE)
        } else {
            ctx.animate_value_with_time(Id::new("neo-stage-enter"), 1.0, MODAL_FADE)
        };
        let settings_k = if settings_just_opened {
            ctx.animate_value_with_time(Id::new("neo-settings-enter"), 0.0, MODAL_FADE);
            ctx.animate_value_with_time(Id::new("neo-settings-enter"), 1.0, MODAL_FADE)
        } else {
            ctx.animate_value_with_time(Id::new("neo-settings-enter"), 1.0, MODAL_FADE)
        };

        // ---- 1. 输入裁定 ----
        // 焦点信息来自上一帧的结尾，因此可以在这里安全地判断。
        let composing = ctx.memory(|m| m.focused()) == Some(Id::new(ui::COMPOSER_ID));
        // Enter 无条件从输入流里摘掉（否则 `TextEdit` 会插换行），
        // 但**只有不在输入法组合/选词那一帧里才把它当成「发送」**：
        // 中文用户按 Enter 是选候选词，不是发消息。
        let enter_pressed =
            composing && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
        let enter_sends = enter_pressed && !ui::ime_active(&ctx) && !modal_open;
        let escape_pressed =
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));

        // 缩放按「屏幕」而非「内容区」推导：观看距离对应的是整块屏。
        self.sync_theme(&ctx, screen.size());

        // ---- 2. 绘制 ----
        let NeoApp {
            state,
            whale,
            fonts,
            theme,
            ..
        } = self;
        let skin = Skin::new(*theme, whale);
        let m = skin.m();
        let p = skin.p();

        ui.painter().rect_filled(screen, 0.0, p.bg_base);

        // 侧栏左缘对齐屏幕，不用 `content` —— 否则屏幕上会留一条背景色竖条。
        // 宽度上限 34%：窄屏下保证主区仍有可读列宽。
        // （提出到作用域外：舞台切换的入场遮罩也要盖住这块 `main`。）
        let sidebar_w = m.sidebar_w().min(screen.width() * 0.34);
        let sidebar_rect = Rect::from_min_size(screen.min, Vec2::new(sidebar_w, screen.height()));
        let main = Rect::from_min_max(egui::pos2(sidebar_rect.right(), content.top()), content.max);

        let background = ui
            .scope_builder(egui::UiBuilder::new(), |ui| {
                if modal_open {
                    ui.disable();
                }
                let sb = ui::sidebar::draw(ui, &skin, sidebar_rect, state, escape_pressed);
                let mut new_session = false;
                let mut workspace_clicked = false;

                let (cluster, composer_out) = match state.stage {
                    Stage::Hero => {
                        // 空态没有顶栏，档位控件浮在右上角。
                        let cluster_rect = Rect::from_min_size(
                            egui::pos2(
                                main.right() - m.s(22.0) - m.s(220.0),
                                main.top() + m.s(20.0),
                            ),
                            Vec2::new(m.s(220.0), m.s(32.0)),
                        );
                        let cluster = ui::settings::profile_cluster(ui, &skin, cluster_rect, state);

                        let area = Rect::from_min_max(
                            egui::pos2(main.left(), main.top() + m.s(64.0)),
                            egui::pos2(main.right(), main.bottom() - m.s(20.0)),
                        );
                        let out = ui::hero::draw(ui, &skin, area, state);
                        workspace_clicked = out.workspace_clicked;
                        (cluster, out.composer)
                    }
                    Stage::Conversation => {
                        let header_h = ui::conversation::header_height(&skin);
                        let topbar =
                            Rect::from_min_size(main.min, Vec2::new(main.width(), header_h));
                        let body =
                            Rect::from_min_max(egui::pos2(main.left(), topbar.bottom()), main.max);
                        let out = ui::conversation::draw(ui, &skin, topbar, body, state);
                        new_session = out.new_session;
                        (Default::default(), out.composer)
                    }
                };

                (sb, cluster, composer_out, new_session, workspace_clicked)
            })
            .inner;
        let (sb, cluster, composer_out, new_session, workspace_clicked) = background;

        // 舞台切换入场：新舞台已画好，用背景色「盖一层再掀开」，等价于整区
        // 淡入，不必逐形状穿透各子 Ui；收敛后 k=1 不画。侧栏不参与 ——
        // 它在两个舞台之间保持不变。
        if stage_k < 1.0 {
            ui.painter()
                .rect_filled(main, 0.0, ui::translucent(p.bg_base, 1.0 - stage_k));
        }

        if state.show_settings {
            ui.interact(
                screen,
                Id::new("neo-settings-blocker"),
                egui::Sense::click(),
            );
            // 遮罩随 settings_k 淡入；面板同时从 10pt 下方浮起 —— 平移不改
            // 尺寸，内容无需重排，也不会触发面板的高度自校验。
            let mask_a = match skin.design.theme.mode {
                ThemeMode::Dark => 140.0,
                ThemeMode::Light => 64.0,
            };
            ui.painter().rect_filled(
                screen,
                0.0,
                neo_theme::palette::black_a((mask_a * settings_k) as u8),
            );
            // 面板固定尺寸（内容区可滚动），小窗里按视口收窄。
            let size = ui::settings::panel_size(&skin)
                .min(screen.size() - egui::vec2(m.s(48.0), m.s(48.0)));
            let rise = (1.0 - settings_k) * m.s(10.0);
            let panel_rect = Rect::from_center_size(screen.center() + egui::vec2(0.0, rise), size);
            if ui::settings::panel(ui, &skin, panel_rect, state, screen.max.y, fonts) {
                state.show_settings = false;
            }
            if escape_pressed && !sb.escape_consumed {
                // 侧栏已用这记 Esc 取消重命名/删除确认的话，不再顺手关设置 ——
                // 与下面「Esc 停止生成」的门同源：一键不一二鸟。
                state.show_settings = false;
            }
            if !state.show_settings {
                // 面板关掉时记忆编辑态一并收摊：重开不带上次的陈旧草稿。
                state.memory_editing = None;
            }
        }

        // Esc = 停止当前这一轮（生成或工具执行中）。待确认的工具会随
        // cancel 置为「已取消」，独立确认窗随之关闭，不需要单独出口。
        // 侧栏先消费过的 Esc（取消重命名/删除确认）不再落到这里 —— 一键不一二鸟。
        if escape_pressed
            && !modal_open
            && !sb.escape_consumed
            && (state.generating || state.tool_open || state.tool_round)
        {
            state.cancel();
        }

        // ---- 3. 落状态 ----
        // 生成泵 / 工具轮 / 落库已在 tick() 里推进（托盘隐藏时也照跑）；
        // 这里只处理绘制产生的 UI 结果。
        if workspace_clicked && self.workspace_picker.is_none() {
            let (tx, rx) = std::sync::mpsc::channel();
            let repaint = ctx.clone();
            let initial = state.workspace_dir.clone();
            match std::thread::Builder::new()
                .name("neo-workspace-picker".into())
                .spawn(move || {
                    let mut dialog = rfd::FileDialog::new().set_title("选择工作区");
                    if let Some(path) = initial {
                        dialog = dialog.set_directory(path);
                    }
                    let _ = tx.send(dialog.pick_folder());
                    repaint.request_repaint();
                }) {
                Ok(_) => {
                    self.workspace_picker = Some(rx);
                    ctx.request_repaint();
                }
                Err(_) => record(Level::Warn, "workspace", "工作区选择器启动失败"),
            }
        }
        if sb.new_session || new_session {
            self.persistence_ok = Self::prepare_session_change(state, self.store.as_ref());
            if self.persistence_ok {
                state.new_session();
            }
        }
        if let Some(id) = sb.open_session {
            if let Some(store) = self.store.as_ref() {
                if !Self::open_session(state, store, id) {
                    // 库读失败不能无声：用户看到的是「点了没反应」。
                    self.pending_toasts.push((
                        neo_ui::ToastKind::Error,
                        "无法读取该会话，数据库文件可能已损坏".into(),
                        std::time::Instant::now() + std::time::Duration::from_secs(4),
                    ));
                }
            }
        }
        if let Some((id, title)) = sb.renamed {
            Self::rename_session(state, self.store.as_ref(), id, &title);
        }
        if let Some(id) = sb.delete_confirmed {
            if let Some(store) = self.store.as_ref() {
                Self::delete_session(state, store, id);
            }
        }
        if sb.open_settings {
            state.show_settings = !state.show_settings;
        }
        if cluster.theme {
            state.theme_mode = state.theme_mode.toggled();
        }
        if cluster.distance {
            state.distance = state.distance.next();
        }

        if composer_out.cancel_import {
            state.cancel_attachment_import();
        }
        if composer_out.clear_attachment_error {
            state.attachment_error = None;
        }
        if let Some(index) = composer_out.remove_attachment {
            if index < state.draft_attachments.len() {
                state.draft_attachments.remove(index);
            }
        }
        if composer_out.attach {
            state.pick_attachments(&ctx);
        }

        if composer_out.toggle_plan {
            state.plan_mode = !state.plan_mode;
        }
        if composer_out.toggle_read_only {
            state.read_only = !state.read_only;
        }
        if composer_out.next_model {
            if state.has_models() {
                // 可选模型来自模型商（运行时列表），不再是编译期常量。
                state.model = (state.model + 1) % state.models.len();
                // 存 id 而不是索引：列表下次刷新后索引会错位。
                Self::save_setting(self.store.as_ref(), "model", state.model_id());
            } else {
                // 没有缓存时引导明确刷新，切换模型本身不授权联网。
                state.show_settings = true;
                state.settings_tab = crate::state::SettingsTab::Model;
                ctx.request_repaint();
            }
        }
        if composer_out.stop {
            state.cancel();
        // 工具轮没走完时不接受新输入：这一轮还没结束。
        } else if (composer_out.send || enter_sends) && state.can_submit() {
            Self::send_input(state, self.store.as_ref());
            ctx.request_repaint();
        }

        let suppress_wake = self.sync_wake_test(self.wake.is_some());
        if suppress_wake {
            self.poll_wake_events(&ctx, true);
        }
        self.sync_safety(&ctx);
        // 绘制产生的提示交给下一次 tick，即使随后隐藏主窗也照常投递。
        if !self.pending_toasts.is_empty() {
            ctx.request_repaint();
        }
        // 当前帧改动立即尝试保存；首次失败立即展示，后续低频重试。
        let was_unsaved = self.state.preferences_unsaved;
        if !self.persist_preferences() && !was_unsaved {
            ctx.request_repaint();
        }

        // 有生成中的动画（光标闪烁、悬停过渡）时保持重绘节奏。
        if composing {
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }
    }
}

#[cfg(test)]
#[path = "app_restore_tests.rs"]
mod restore_tests;

#[cfg(test)]
#[path = "app_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "app_diagnostics_tests.rs"]
mod diagnostics_tests;

fn theme_mode_key(m: ThemeMode) -> &'static str {
    match m {
        ThemeMode::Dark => "dark",
        ThemeMode::Light => "light",
    }
}

fn distance_key(d: Distance) -> &'static str {
    match d {
        Distance::Standard => "standard",
        Distance::Classroom => "classroom",
        Distance::Auditorium => "auditorium",
    }
}

impl App for NeoApp {
    /// 清屏透明。主窗在 `render` 首行自己铺满主题底色，不依赖清屏色；
    /// 透明化是为了小窗 / 截屏闪光 / 确认窗这些**透明子视口** —— 它们共用
    /// 这个清屏色，不透明底色会让透明视口每帧先刷一层主题色，
    /// 闪光就成了「屏幕先变黑再闪走」。
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        #[cfg(all(windows, not(test)))]
        self.desktop_windows.bind(_frame);
        self.render(ui);
    }

    /// eframe 可见时依次调用 logic、ui，隐藏时只调用 logic。
    /// 所有业务轮询只在这里执行，不能在 render 中再次推进桌面屏障。
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        #[cfg(all(windows, not(test)))]
        self.desktop_windows.bind(_frame);
        self.tick(ctx.clone());
    }
}

#[cfg(test)]
#[path = "app_snapshot.rs"]
mod snapshot;
