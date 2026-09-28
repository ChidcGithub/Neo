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
use crate::diagnostics::{record, Level};
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
                (None, false)
            }
        };
        let db_path = db_path.to_string_lossy().into_owned();

        let state = AppState::with_store(store_ok, Some(db_path));

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
        // 启动只读本地缓存；联网刷新必须由用户在设置页明确触发。
        app.saved_classroom_safe = app.state.classroom_safe;
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
                Err(_) => record(Level::Error, "overlay", "覆盖层初始化失败"),
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
        drop(self.tray.take());
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        ctx.request_repaint();
    }

    fn handle_close(&mut self, ctx: &egui::Context) {
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

    /// 从托盘唤回主窗口。
    fn show_window(&mut self, ctx: &egui::Context) {
        self.hidden_to_tray = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    /// 关窗转后台：取消关闭，隐藏窗口（同时最小化，wgpu 对隐藏窗仍会
    /// 尝试取交换链，最小化让渲染循环走「尺寸为零跳过」的既有路径）。
    fn hide_to_tray(&mut self, ctx: &egui::Context) {
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
        eprintln!("[neo] 唤醒命中（置信度 {score:.2}），进入听写");
        self.check_overlay_health(ctx);
        if self.overlay_alive() && self.stt_tx.is_some() {
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
            if self.hidden_to_tray {
                self.show_window(ctx);
            }
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
                    return;
                }
                Some(messages)
            } else {
                None
            };
            if !Self::persist_ready(state, store) {
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
            state.attachment_error = Some("本任务已达到500次工具调用上限，已停止自动续轮".into());
            return;
        }
        let mut messages = state.api_messages(usize::MAX);
        match state.compaction_plan(&messages) {
            Ok(Some(plan)) => Self::launch_compaction(state, plan),
            Ok(None) => match neo_llm::budget_messages(&state.llm_config(), &mut messages, &neo_tools::tool_declarations()) {
                Ok(()) => Self::start_real_stream_with_messages(state, messages),
                Err(e) => state.attachment_error = Some(e),
            },
            Err(e) => { state.compaction_status = Some(format!("压缩失败：{e}")); state.attachment_error = Some(e); }
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

    /// 一帧的纯逻辑部分：托盘事件、语音唤醒、STT 结果、超时与轮询调度。
    ///
    /// 独立出来的原因：窗口隐藏（托盘）时 eframe 不再调 `App::ui`，只调
    /// `App::logic`（默认是空实现）——不挂它，后台时托盘点击与唤醒检测全停摆。
    /// 可见时由 `render` 开头调用，隐藏时由 `App::logic` 调用，二者互斥。
    fn tick(&mut self, ctx: egui::Context) {
        if self.quitting {
            return;
        }
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
        let classwin_rect = if self.class.presenting().is_some() {
            let monitor = ctx
                .input(|i| i.viewport().monitor_size)
                .unwrap_or(egui::Vec2::new(1920.0, 1080.0));
            Some(ui::classwin::ClassWin::target_rect(self.theme, monitor))
        } else {
            None
        };
        // 迷你窗 / 确认卡 / 课堂总结窗：有渲染层就画进层（无新建窗口、
        // 无闪黑）；离屏测试没有层，各窗口内部退回独立视口路径。
        let overlay = self.overlay.as_ref();
        self.miniwin.tick(
            &ctx,
            &mut self.state,
            self.theme,
            self.hidden_to_tray,
            classwin_rect,
            overlay,
        );
        // 截屏闪光：抓帧完成后在被抓区域边缘闪一道白框。
        self.shotflash.tick(&ctx, overlay);
        // 工具确认卡：画进统一渲染层（没有新建窗口，闪黑在结构上不存在）；
        // 离屏测试没有渲染层，confirmwin 内部退回独立视口路径。
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
        if show_at != self.show_window_seen {
            self.show_window_seen = show_at;
            if show_at != 0 && self.hidden_to_tray {
                self.show_window(&ctx);
            }
        }
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
        if self.state.compaction_resume && self.persistence_ok {
            self.state.compaction_resume = false;
            let config_matches = self.state.compaction_resume_config.take().as_ref() == Some(&self.state.llm_config());
            if !config_matches {
                self.state.compaction_status = Some("配置已改变，摘要后的自动续轮已取消".into());
            }
            if config_matches && self.state.can_call_real() && !self.state.round_cancelled && !self.state.task_limit_reached {
                let mut messages = self.state.api_messages(usize::MAX);
                match neo_llm::budget_messages(&self.state.llm_config(), &mut messages, &neo_tools::tool_declarations()) {
                    Ok(()) => Self::start_real_stream_with_messages(&mut self.state, messages),
                    Err(e) => self.state.attachment_error = Some(e),
                }
            }
        }
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
                        self.state.attachment_error = Some(
                            "工具已执行完，但模型接口未配置，结果未回灌；配置好后发条消息即可继续。"
                                .into(),
                        );
                    }
                }
            }
        }
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
        self.tick(ctx.clone());
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
mod restore_tests {
    //! 启动恢复：不依赖 egui，直接驱动存储与状态。
    use super::NeoApp;
    use crate::state::{AppState, Stage};
    use neo_store::Store;

    fn temp_store(tag: &str) -> Store {
        let dir = std::env::temp_dir().join(format!("neo-restore-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Store::open(&dir.join("neo.db")).expect("打开测试库")
    }

    // SQLite URI 只读连接：使用真实数据库写错误，不用空 Store 冒充失败。
    fn readonly_store(path: &std::path::Path) -> Store {
        let path = path.to_string_lossy().replace('\\', "/")
            .replace('%', "%25").replace(' ', "%20").replace('#', "%23").replace('?', "%3F");
        Store::open(std::path::Path::new(&format!("file:{path}?mode=ro")))
            .expect("打开真实 SQLite 只读连接")
    }

    #[test]
    fn dictation_input_conflicts_preserve_text_attachments_and_session() {
        use crate::state::{ChatMessage, Role};
        use std::time::{Duration, Instant};
        for last_done in [None, Some(Instant::now()), Some(Instant::now() - Duration::from_secs(301))] {
            for draft in ["", "  \n", "  尚未提交的文字  "] {
                for attachment in [false, true] {
                    if draft.is_empty() && !attachment {
                        continue;
                    }
                    let mut state = AppState::default();
                    state.active_session = Some(42);
                    state.messages.push(ChatMessage::new(Role::User, "原会话"));
                    state.draft = draft.into();
                    if attachment {
                        state.add_attachment(sample_attachment(true)).unwrap();
                    }
                    let attachments = serde_json::to_string(&state.draft_attachments).unwrap();
                    let epoch = state.session_epoch;
                    assert!(!NeoApp::prepare_dictation_input(&mut state, None, " 新语音 ", last_done));
                    let separator = if draft.is_empty() { "" } else { "\n\n" };
                    assert_eq!(state.draft, format!("{draft}{separator}【语音转写 · 待确认】\n新语音"));
                    assert_eq!(serde_json::to_string(&state.draft_attachments).unwrap(), attachments);
                    assert_eq!(state.active_session, Some(42));
                    assert_eq!(state.session_epoch, epoch);
                    assert_eq!(state.messages.len(), 1);
                    assert_eq!(state.messages[0].content, "原会话");
                    assert_eq!(state.pending_persist, 0);
                    assert!(!state.generating && !state.wants_demo_reply && state.stream.is_none());
                }
            }
        }
    }

    #[test]
    fn dictation_input_picker_in_flight_never_switches_or_auto_sends() {
        let mut state = AppState::default();
        state.attachment_picker_open = true;
        state.attachment_status = Some("正在选择附件…".into());
        state.active_session = Some(42);
        assert!(!NeoApp::prepare_dictation_input(&mut state, None, "讲解这道题", None));
        assert!(state.attachment_picker_open);
        assert_eq!(state.attachment_status.as_deref(), Some("正在选择附件…"));
        assert_eq!(state.active_session, Some(42));
        // 模拟稍后导入完成：语音依然只是待确认草稿，不因附件到达而触发发送。
        state.attachment_picker_open = false;
        state.add_attachment(sample_attachment(false)).unwrap();
        assert_eq!(state.draft, "【语音转写 · 待确认】\n讲解这道题");
        assert_eq!(state.draft_attachments.len(), 1);
        assert!(state.messages.is_empty());
        assert!(!state.generating && !state.wants_demo_reply);
    }

    #[test]
    fn dictation_input_empty_editor_resumes_only_within_five_minutes() {
        use crate::state::{ChatMessage, Role};
        use std::time::{Duration, Instant};
        for (last_done, resume) in [
            (None, false),
            (Some(Instant::now() - Duration::from_secs(299)), true),
            (Some(Instant::now() - Duration::from_secs(300)), false),
            (Some(Instant::now() - Duration::from_secs(301)), false),
        ] {
            let mut state = AppState::default();
            state.messages.push(ChatMessage::new(Role::User, "原会话"));
            state.active_session = Some(42);
            let epoch = state.session_epoch;
            assert!(NeoApp::prepare_dictation_input(&mut state, None, "  那第二问呢  ", last_done));
            assert_eq!(state.active_session, resume.then_some(42));
            assert_eq!(state.session_epoch, epoch + u64::from(!resume));
            assert_eq!(state.messages.len(), usize::from(resume));
            assert_eq!(state.draft, "那第二问呢");
            // submit 仅落纯状态，不经过真实发送、演示生成或任何网络。
            assert!(state.submit());
            let message = state.messages.last().unwrap();
            assert_eq!(message.content, "那第二问呢");
            assert!(message.attachments.is_empty());
            assert!(!state.generating && !state.wants_demo_reply && state.stream.is_none());
        }
    }

    #[test]
    fn dictation_input_recent_completion_without_messages_starts_new_session() {
        let mut state = AppState::default();
        state.active_session = Some(42);
        let epoch = state.session_epoch;
        assert!(NeoApp::prepare_dictation_input(&mut state, None, "新问题", Some(std::time::Instant::now())));
        assert_eq!(state.active_session, None);
        assert_eq!(state.session_epoch, epoch + 1);
        assert_eq!(state.draft, "新问题");
        assert!(state.messages.is_empty());
    }

    #[test]
    fn dictation_input_readonly_failure_keeps_both_sources_and_history() {
        use crate::state::{ChatMessage, Role};
        let store = temp_store("dictation-readonly");
        let id = store.create_session("原会话").unwrap();
        let path = std::env::temp_dir().join(format!("neo-restore-dictation-readonly-{}", std::process::id())).join("neo.db");
        drop(store);
        let store = readonly_store(&path);
        for conflict in [false, true] {
            let mut state = AppState::default();
            state.active_session = Some(id);
            state.messages.push(ChatMessage::new(Role::User, "尚未保存的消息"));
            if conflict {
                state.draft = "原草稿".into();
                state.add_attachment(sample_attachment(false)).unwrap();
            }
            assert!(!NeoApp::prepare_dictation_input(&mut state, Some(&store), "识别文本", None));
            assert_eq!(state.active_session, Some(id));
            assert_eq!(state.messages[0].content, "尚未保存的消息");
            assert_eq!(state.pending_persist, 0);
            assert!(state.draft.contains("识别文本"));
            if conflict {
                assert!(state.draft.starts_with("原草稿\n\n"));
                assert_eq!(state.draft_attachments.len(), 1);
            } else {
                assert!(state.attachment_error.is_some());
            }
            assert!(store.messages(id).unwrap().is_empty());
            assert!(!state.generating && !state.wants_demo_reply);
        }
    }

    #[test]
    fn dictation_input_send_rejections_keep_transcript_without_stream() {
        // 缺模型与超预算都在真实请求之前拒绝；不启动发送线程。
        for budget_failure in [false, true] {
            let mut state = AppState::default();
            state.context_tokens = 32 * 1024;
            state.api_key = "test-key".into();
            let text = if budget_failure {
                state.restore_models("deepseek-chat", Some("deepseek-chat"));
                "教学正文".repeat(20_000)
            } else {
                "识别文本".into()
            };
            assert!(NeoApp::prepare_dictation_input(&mut state, None, &text, None));
            NeoApp::send_input(&mut state, None);
            assert_eq!(state.draft, text);
            assert!(state.attachment_error.is_some());
            assert!(state.messages.is_empty());
            assert!(!state.generating && !state.wants_demo_reply && state.stream.is_none());
        }
    }

    #[test]
    fn dictation_input_append_save_failure_restores_voice_draft() {
        let store = temp_store("dictation-append");
        let path = std::env::temp_dir().join(format!("neo-restore-dictation-append-{}", std::process::id())).join("neo.db");
        drop(store);
        let store = readonly_store(&path);
        let mut state = AppState::default();
        // 空会话准备阶段无需写库，提交后的首次保存失败必须退回语音草稿。
        assert!(NeoApp::prepare_dictation_input(&mut state, Some(&store), "识别文本", None));
        NeoApp::send_input(&mut state, Some(&store));
        assert_eq!(state.draft, "识别文本");
        assert!(state.messages.is_empty());
        assert!(state.attachment_error.is_some());
        assert!(!state.generating && !state.wants_demo_reply && state.stream.is_none());
    }

    #[test]
    fn dictation_input_fake_channel_accepts_only_current_nonempty_transcripts() {
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(super::SttResult::Transcript(6, Ok("旧结果".into()))).unwrap();
        tx.send(super::SttResult::Transcript(7, Ok(" \n ".into()))).unwrap();
        tx.send(super::SttResult::Transcript(7, Ok("第一句".into()))).unwrap();
        tx.send(super::SttResult::Transcript(8, Ok("第二句".into()))).unwrap();
        drop(tx);
        let mut state = AppState::default();
        state.draft = "原草稿".into();
        for result in rx {
            if let super::SttResult::Transcript(epoch, Ok(text)) = result {
                if super::current_dictation(true, 7, epoch) {
                    assert!(!NeoApp::prepare_dictation_input(&mut state, None, &text, None));
                }
            }
        }
        assert_eq!(state.draft, "原草稿\n\n【语音转写 · 待确认】\n第一句");
        assert!(!NeoApp::prepare_dictation_input(&mut state, None, "再次唤醒", None));
        assert_eq!(state.draft, "原草稿\n\n【语音转写 · 待确认】\n第一句\n\n【语音转写 · 待确认】\n再次唤醒");
        assert!(state.messages.is_empty());
        assert!(!state.generating && !state.wants_demo_reply);
    }

    #[test]
    fn safety_sqlite_readonly_blocks_close_and_tray_then_retry_saves() {
        use crate::state::{ChatMessage, Role, StreamSource};
        for tray in [false, true] {
            let ctx = egui::Context::default();
            let mut app = NeoApp::install(&ctx);
            app.state.new_session();
            let path = super::test_db_path();
            let store = app.store.as_ref().unwrap();
            store.set_setting("classroom_safe", "0").unwrap();
            let id = store.create_session("original").unwrap();
            app.saved_classroom_safe = false;
            app.state.active_session = Some(id);
            app.state.messages.push(ChatMessage::new(Role::User, "keep"));
            app.state.start_generation(StreamSource::Demo { text: "partial".into(), cursor: 0 });
            app.state.pump();
            drop(app.store.take());
            app.store = Some(readonly_store(&path));
            assert!(app.store.as_ref().unwrap().set_setting("probe", "fail").is_err());
            app.hidden_to_tray = tray;
            let mut input = egui::RawInput::default();
            if !tray {
                input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap()
                    .events.push(egui::ViewportEvent::Close);
            }
            let mut output = ctx.run_ui(input, |_| {
                if tray {
                    app.handle_tray_menu(&ctx, &app.tray_quit_id.clone());
                } else {
                    app.tick(ctx.clone());
                }
            });
            output.textures_delta.clear();
            let commands = &output.viewport_output[&egui::ViewportId::ROOT].commands;
            assert!(commands.iter().any(|command| matches!(command, egui::ViewportCommand::CancelClose)));
            assert!(!commands.iter().any(|command| matches!(command, egui::ViewportCommand::Close)));
            assert!(!app.quitting);
            assert!(app.exit_blocked);
            assert!(!app.hidden_to_tray);
            assert!(!app.state.generating);
            assert!(app.state.messages.iter().all(|message| !message.streaming));
            assert_eq!(app.state.pending_persist, 0);
            assert!(app.state.preferences_unsaved);
            assert!(!app.saved_classroom_safe);
            assert_eq!(app.store.as_ref().unwrap().setting("classroom_safe").unwrap().as_deref(), Some("0"));
            assert!(app.store.as_ref().unwrap().messages(id).unwrap().is_empty());
            // 等待下一帧不会偷偷放行或恢复生成。
            app.tick(ctx.clone());
            assert!(app.exit_blocked && !app.quitting && !app.state.generating);
            NeoApp::rename_session(&mut app.state, app.store.as_ref(), id, "retry title");
            assert_eq!(app.state.renaming, Some(id));
            assert_eq!(app.state.rename_draft, "retry title");
            assert!(app.state.attachment_error.as_deref().unwrap().contains("重命名未保存"));
            assert_eq!(app.store.as_ref().unwrap().sessions().unwrap().into_iter().find(|row| row.id == id).unwrap().title, "original");
            drop(app.store.take());
            app.store = Some(Store::open(&path).unwrap());
            app.request_exit(&ctx);
            assert!(app.quitting && !app.exit_blocked);
            assert!(!app.state.preferences_unsaved);
            assert!(app.saved_classroom_safe);
            assert_eq!(app.store.as_ref().unwrap().messages(id).unwrap().len(), app.state.messages.len());
            assert_eq!(app.store.as_ref().unwrap().setting("classroom_safe").unwrap().as_deref(), Some("1"));
        }
    }

    #[test]
    fn safety_class_exit_retries_all_four_cards_and_index_failures() {
        struct HomeGuard(Option<std::ffi::OsString>, std::path::PathBuf);
        impl Drop for HomeGuard {
            fn drop(&mut self) {
                match &self.0 {
                    Some(old) => std::env::set_var("NEO_HOME", old),
                    None => std::env::remove_var("NEO_HOME"),
                }
                let _ = std::fs::remove_dir_all(&self.1);
            }
        }
        let path = std::env::temp_dir().join(format!("neo-exit-class-{}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        let _home = HomeGuard(std::env::var_os("NEO_HOME"), path.clone());
        std::env::set_var("NEO_HOME", &path);
        std::fs::write(path.join("class"), b"blocked").unwrap();
        let ctx = egui::Context::default();
        let mut app = NeoApp::install(&ctx);
        app.class.seed_pending_summaries();
        app.request_exit(&ctx);
        assert!(app.exit_blocked && !app.quitting);
        assert_eq!(app.class.pending_saves(), 4);
        // 安全模式停用/重复退出也不能覆盖或丢掉队列。
        app.state.classroom_safe = true;
        app.class.tick(&ctx, &app.state, false);
        assert_eq!(app.class.pending_saves(), 4);
        std::fs::remove_file(path.join("class")).unwrap();
        std::fs::create_dir(path.join("memories.json")).unwrap();
        app.request_exit(&ctx);
        assert!(app.exit_blocked && !app.quitting);
        assert_eq!(app.class.pending_saves(), 4);
        assert_eq!(app.class.presenting().unwrap().save_state, crate::class::SaveState::IndexFailed);
        let date = app.class.presenting().unwrap().date.clone();
        assert_eq!(neo_tools::classlog::load_day(&date).len(), 4);
        app.request_exit(&ctx);
        assert_eq!(neo_tools::classlog::load_day(&date).len(), 4);
        std::fs::remove_dir(path.join("memories.json")).unwrap();
        app.request_exit(&ctx);
        assert!(app.quitting && !app.exit_blocked);
        assert_eq!(app.class.pending_saves(), 0);
        assert_eq!(neo_tools::classlog::load_day(&date).len(), 4);
        let memories: serde_json::Value = serde_json::from_slice(&std::fs::read(path.join("memories.json")).unwrap()).unwrap();
        assert_eq!(memories.as_array().unwrap().len(), 4);
    }

    #[test]
    fn safety_budget_rejection_keeps_draft_attachments_and_does_not_start_stream() {
        let mut state = AppState::default();
        state.context_tokens = 32 * 1024;
        state.api_key = "test-key".into();
        state.restore_models("deepseek-chat", Some("deepseek-chat"));
        state.draft = format!("  {}  ", "教学正文".repeat(20_000));
        state.add_attachment(sample_attachment(false)).unwrap();
        let draft = state.draft.clone();
        assert!(state.can_call_real());
        NeoApp::send_input(&mut state, None);
        assert_eq!(state.draft, draft);
        assert_eq!(state.draft_attachments.len(), 1);
        assert!(state.messages.is_empty());
        assert!(!state.generating && state.stream.is_none());
        assert!(state.attachment_error.as_deref().unwrap().contains("预算"));
    }

    #[test]
    fn safety_memory_only_exit_requires_explicit_discard() {
        let ctx = egui::Context::default();
        let mut app = NeoApp::install(&ctx);
        app.store = None;
        app.request_exit(&ctx);
        assert!(app.exit_blocked && !app.quitting && !app.confirm_discard);
        app.confirm_discard = true;
        app.tick(ctx.clone());
        assert!(!app.quitting);
        app.finish_exit(&ctx);
        assert!(app.quitting);
    }

    #[test]
    fn ui_safety_exit_discard_is_two_step_and_error_is_visible() {
        let pointer = |pos, pressed| vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton { pos, button: egui::PointerButton::Primary,
                pressed, modifiers: egui::Modifiers::NONE },
        ];
        let ctx = egui::Context::default();
        let mut app = NeoApp::install(&ctx);
        app.store = None;
        app.state.classroom_safe = !app.saved_classroom_safe;
        app.request_exit(&ctx);
        let size = egui::vec2(900.0, 600.0);
        let draw = |app: &mut NeoApp, events| {
            let mut output = ctx.run_ui(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                events, ..Default::default()
            }, |_| app.exit_dialog(&ctx));
            output.textures_delta.clear();
            output
        };
        draw(&mut app, vec![]);
        let output = draw(&mut app, vec![]);
        let text_rect = |output: &egui::FullOutput, needle: &str| {
            output.shapes.iter().find_map(|shape| {
                if let egui::Shape::Text(text) = &shape.shape {
                    if text.galley.job.text.contains(needle) {
                        return Some(egui::Rect::from_min_size(text.pos, text.galley.size()));
                    }
                }
                None
            }).unwrap_or_else(|| panic!("缺少可见提示：{needle}"))
        };
        text_rect(&output, "安全偏好尚未保存");
        text_rect(&output, "纯内存模式");
        text_rect(&output, "重试保存并退出");
        let discard = text_rect(&output, "退出不保存…");
        draw(&mut app, pointer(discard.center(), true));
        draw(&mut app, pointer(discard.center(), false));
        assert!(app.confirm_discard && !app.quitting);
        draw(&mut app, vec![]);
        let output = draw(&mut app, vec![]);
        let confirm = text_rect(&output, "确认丢弃并退出");
        draw(&mut app, pointer(confirm.center(), true));
        let output = draw(&mut app, pointer(confirm.center(), false));
        assert!(app.quitting);
        assert!(output.viewport_output[&egui::ViewportId::ROOT].commands.iter()
            .any(|command| matches!(command, egui::ViewportCommand::Close)));
    }

    fn wake_test_app(ctx: &egui::Context) -> NeoApp {
        let mut app = NeoApp::install(ctx);
        app.state.classroom_safe = false;
        app.applied_classroom_safe = false;
        app.state.show_settings = true;
        app.state.settings_tab = crate::state::SettingsTab::WakeTest;
        app.state.wake_enabled = true;
        app.state.wake_test.requested = true;
        app
    }

    #[test]
    fn wake_test_admission_blocks_safety_disabled_tasks_and_dictation() {
        for case in 0..9 {
            let ctx = egui::Context::default();
            let mut app = wake_test_app(&ctx);
            match case {
                0 => app.state.classroom_safe = true,
                1 => app.state.wake_enabled = false,
                2 => app.state.generating = true,
                3 => app.state.tool_open = true,
                4 => app.state.tool_round = true,
                5 => app.dictating = true,
                6 => app.wake_broken = true,
                7 => app.state.compaction_resume = true,
                _ => app.exit_blocked = true,
            }
            app.sync_wake_test(true); // 只注入引擎可用性，不创建硬件引擎。
            assert!(!app.state.wake_test.running && !app.state.wake_test.requested);
            assert!(app.wake.is_none());
        }
    }

    #[test]
    fn wake_test_leaving_stops_and_keeps_errors_without_resuming() {
        for case in 0..5 {
            let ctx = egui::Context::default();
            let mut app = wake_test_app(&ctx);
            assert!(app.sync_wake_test(true));
            assert!(app.state.wake_test.running);
            app.state.wake_test.error = Some("resource unavailable".into());
            match case {
                0 => app.state.settings_tab = crate::state::SettingsTab::General,
                1 => app.state.show_settings = false,
                2 => app.state.classroom_safe = true,
                3 => app.hidden_to_tray = true,
                _ => app.state.wake_test.requested = false,
            }
            assert!(app.sync_wake_test(true));
            assert!(!app.state.wake_test.running && !app.state.wake_test.requested);
            assert_eq!(app.state.wake_test.error.as_deref(), Some("resource unavailable"));
            app.state.show_settings = true;
            app.state.settings_tab = crate::state::SettingsTab::WakeTest;
            app.state.classroom_safe = false;
            app.hidden_to_tray = false;
            assert!(!app.sync_wake_test(true));
        }
    }

    #[test]
    fn wake_test_queued_events_never_start_dictation_or_tasks_and_errors_latch() {
        let ctx = egui::Context::default();
        let mut app = wake_test_app(&ctx);
        let (tx, rx) = std::sync::mpsc::channel();
        let (stt_tx, stt_rx) = std::sync::mpsc::channel();
        app.stt_tx = Some(stt_tx);
        app.wake_rx = Some(rx);
        app.state.draft = "untouched".into();
        for stopping in [false, true] {
            tx.send(neo_wake::WakeEvent::Detected { epoch: app.wake_epoch, score: 0.99 }).unwrap();
            tx.send(neo_wake::WakeEvent::Audio { epoch: app.wake_epoch, frame: vec![0.1; 16] }).unwrap();
            app.state.wake_test.requested = !stopping;
            let suppress = app.sync_wake_test(true);
            assert!(suppress);
            app.poll_wake_events(&ctx, suppress);
            assert!(!app.dictating && !app.state.generating && app.state.messages.is_empty());
            assert_eq!(app.state.draft, "untouched");
            assert!(stt_rx.try_recv().is_err());
            assert!(app.pending_toasts.is_empty());
        }
        tx.send(neo_wake::WakeEvent::Error("school resource missing".into())).unwrap();
        app.poll_wake_events(&ctx, true);
        assert!(app.wake_broken && app.state.wake_test.broken);
        assert_eq!(app.state.wake_test.error.as_deref(), Some("school resource missing"));
        app.state.wake_enabled = false;
        ctx.set_embed_viewports(false);
        ctx.begin_pass(egui::RawInput::default());
        app.tick(ctx.clone()); // 复用开关的故障解锁，测试构建不会开麦克风。
        ctx.end_pass().textures_delta.clear();
        assert!(!app.wake_broken);
        app.state.wake_enabled = true;
        ctx.begin_pass(egui::RawInput::default());
        app.tick(ctx.clone());
        ctx.end_pass().textures_delta.clear();
        assert!(app.wake.is_none() && !app.state.wake_test.requested);
        assert_eq!(app.state.wake_test.error.as_deref(), Some("school resource missing"));
    }

    #[test]
    fn wake_test_delayed_forwarder_rejects_old_detection_and_audio_after_exit() {
        use neo_wake::WakeEvent;
        use std::sync::mpsc;
        use std::time::Duration;

        let ctx = egui::Context::default();
        let mut app = wake_test_app(&ctx);
        let (engine_tx, engine_rx) = mpsc::channel();
        let (ui_tx, ui_rx) = mpsc::channel();
        let (held_tx, held_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (sent_tx, sent_rx) = mpsc::channel();
        let forward_tx = ui_tx.clone();
        let forwarder = std::thread::spawn(move || {
            for event in engine_rx {
                held_tx.send(()).unwrap();
                if release_rx.recv().is_err() { break; }
                forward_tx.send(event).unwrap();
                sent_tx.send(()).unwrap();
            }
        });
        app.wake_rx = Some(ui_rx);
        let (stt_tx, stt_rx) = mpsc::channel();
        app.stt_tx = Some(stt_tx);
        app.state.draft = "untouched".into();
        engine_tx.send(WakeEvent::Detected { epoch: app.wake_epoch, score: 0.99 }).unwrap();
        engine_tx.send(WakeEvent::Audio { epoch: app.wake_epoch, frame: vec![0.1; 16] }).unwrap();
        held_rx.recv_timeout(Duration::from_secs(2)).unwrap();

        // 转发线程已收到旧命中，但尚未写入 UI 队列；完整进出测试并排空两次。
        let suppress = app.sync_wake_test(true);
        assert!(suppress);
        app.poll_wake_events(&ctx, suppress);
        app.state.wake_test.requested = false;
        let suppress = app.sync_wake_test(true);
        assert!(suppress);
        app.poll_wake_events(&ctx, suppress);
        assert_eq!(app.wake_epoch, 2);
        assert!(!app.sync_wake_test(true));
        app.hidden_to_tray = true;
        release_tx.send(()).unwrap();
        sent_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        app.poll_wake_events(&ctx, false);
        assert!(app.hidden_to_tray);
        assert!(!app.dictating && app.pending_toasts.is_empty());

        // 独立验证 Audio 没有进入处理器：无覆盖层时，误消费会取消此哨兵听写。
        held_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        app.dictating = true;
        app.dictation_epoch = 7;
        release_tx.send(()).unwrap();
        sent_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        app.poll_wake_events(&ctx, false);
        assert!(app.dictating && app.hidden_to_tray);
        assert_eq!(app.dictation_epoch, 7);
        assert!(stt_rx.try_recv().is_err());
        assert!(!app.state.generating && app.state.messages.is_empty());
        assert_eq!(app.state.draft, "untouched");
        drop(engine_tx);
        forwarder.join().unwrap();

        // 同一接收端仍接受新代事件，不是永久封禁或额外排空队列。
        ui_tx.send(WakeEvent::Audio { epoch: app.wake_epoch, frame: vec![0.2; 16] }).unwrap();
        app.poll_wake_events(&ctx, false);
        assert!(!app.dictating);
        assert!(matches!(stt_rx.try_recv(), Ok(super::SttCmd::Reset(8))));
        app.hidden_to_tray = true;
        ui_tx.send(WakeEvent::Detected { epoch: app.wake_epoch, score: 0.8 }).unwrap();
        app.poll_wake_events(&ctx, false);
        assert!(!app.hidden_to_tray);
        assert!(!app.pending_toasts.is_empty());
        ui_tx.send(WakeEvent::Error("delayed engine failure".into())).unwrap();
        app.poll_wake_events(&ctx, false);
        assert!(app.wake_broken);
        assert_eq!(app.state.wake_test.error.as_deref(), Some("delayed engine failure"));
    }

    #[test]
    fn toast_app_hidden_and_exit_blocked_still_deliver() {
        for blocked in [false, true] {
            let ctx = egui::Context::default();
            ctx.set_embed_viewports(false);
            let mut app = NeoApp::install(&ctx);
            app.hidden_to_tray = !blocked;
            app.exit_blocked = blocked;
            app.pending_toasts.push((neo_ui::ToastKind::Warning, "notice".into(),
                std::time::Instant::now() + std::time::Duration::from_secs(4)));
            ctx.begin_pass(egui::RawInput::default());
            app.tick(ctx.clone());
            let mut output = ctx.end_pass();
            output.textures_delta.clear();
            assert!(app.pending_toasts.is_empty());
            let toast = &output.viewport_output[&egui::ViewportId::from_hash_of("neo-toastwin")];
            assert!(toast.viewport_ui_cb.is_some());
            assert!(toast.builder.inner_size.unwrap().x > 1.0);
        }
    }

    #[test]
    fn safety_send_without_models_only_opens_settings() {
        let mut state = AppState::default();
        state.api_base = "http://127.0.0.1:9".into();
        state.api_key = "test-key".into();
        state.draft = "keep draft".into();
        NeoApp::send_input(&mut state, None);
        assert!(state.model_fetch.is_none());
        assert!(state.show_settings);
        assert_eq!(state.draft, "keep draft");
        assert!(state.messages.is_empty());
    }

    #[test]
    fn safety_settings_missing_corrupt_and_failed_save() {
        let ctx = egui::Context::default();
        let mut app = NeoApp::install(&ctx);
        assert!(app.state.classroom_safe);
        for value in ["broken", "", "false", "1", "0"] {
            app.store.as_ref().unwrap().set_setting("classroom_safe", value).unwrap();
            app.load_settings();
            assert_eq!(app.state.classroom_safe, value != "0");
        }
        app.state.wake_enabled = false;
        app.state.class_enabled = false;
        let store = app.store.take();
        app.tick(ctx.clone());
        assert!(app.saved_classroom_safe);
        app.store = store;
        app.tick(ctx.clone());
        assert!(!app.saved_classroom_safe);
        assert_eq!(app.store.as_ref().unwrap().setting("classroom_safe").unwrap().as_deref(), Some("0"));
        app.state.set_classroom_safe(true);
        app.tick(ctx);
        assert!(app.saved_classroom_safe);
    }

    #[test]
    fn context_settings_persist_restore_and_invalid_fallback() {
        let ctx = egui::Context::default();
        let mut app = NeoApp::install(&ctx);
        app.store = Some(temp_store("context-config"));
        app.load_settings();
        assert_eq!(app.state.context_tokens, 1_000_000);
        app.state.context_tokens = 65_536;
        assert!(app.persist_preferences());
        app.state.context_tokens = 8192;
        app.load_settings();
        assert_eq!(app.state.context_tokens, 65_536);
        assert_eq!(app.saved_context_tokens, 65_536);
        app.store.as_ref().unwrap().set_setting("context_tokens", "corrupt").unwrap();
        app.load_settings();
        assert_eq!(app.state.context_tokens, 1_000_000);
        app.state.context_tokens = 32_768;
        app.store = None;
        assert!(!app.persist_preferences());
        assert_eq!(app.saved_context_tokens, 1_000_000);
        assert!(app.state.preferences_unsaved);
    }

    #[test]
    fn safety_hot_switch_invalidates_dictation_without_render() {
        let ctx = egui::Context::default();
        let mut app = NeoApp::install(&ctx);
        app.applied_classroom_safe = false;
        app.dictating = true;
        app.dictation_epoch = 5;
        let (tx, rx) = std::sync::mpsc::channel();
        app.stt_tx = Some(tx);
        let (wake_tx, wake_rx) = std::sync::mpsc::channel();
        app.wake_rx = Some(wake_rx);
        app.tick(ctx);
        assert!(!app.dictating);
        assert_eq!(app.dictation_epoch, 6);
        assert!(matches!(rx.try_recv(), Ok(super::SttCmd::Reset(6))));
        assert!(wake_tx.send(neo_wake::WakeEvent::Audio { epoch: 0, frame: vec![] }).is_err());
        assert!(!super::current_dictation(app.dictating, app.dictation_epoch, 5));
    }

    #[test]
    fn safety_overlay_unavailable_stops_audio_and_wake_uses_visible_input() {
        let ctx = egui::Context::default();
        let mut app = NeoApp::install(&ctx);
        let (tx, rx) = std::sync::mpsc::channel();
        app.stt_tx = Some(tx);
        app.dictating = true;
        app.dictation_epoch = 7;
        app.hidden_to_tray = true;
        app.state.draft = "keep draft".into();
        app.check_overlay_health(&ctx);
        assert!(!app.dictating && !app.hidden_to_tray);
        assert!(matches!(rx.try_recv(), Ok(super::SttCmd::Reset(8))));
        assert_eq!(app.state.draft, "keep draft");
        assert!(!super::current_dictation(app.dictating, app.dictation_epoch, 7));
        app.hidden_to_tray = true;
        app.on_wake_detected(&ctx, 0.9);
        assert!(!app.dictating && !app.hidden_to_tray);
        assert!(rx.try_recv().is_err());
        app.dictating = true;
        app.on_dictation_audio(&ctx, vec![0.5; 16]);
        assert!(!app.dictating);
        assert!(matches!(rx.try_recv(), Ok(super::SttCmd::Reset(9))));
        assert!(rx.try_recv().is_err());
        app.overlay_attempted = true;
        for _ in 0..10 {
            app.start_overlay();
        }
        assert!(app.overlay.is_none());
    }

    #[test]
    fn safety_dictation_rejects_previous_generation() {
        assert!(super::current_dictation(true, 3, 3));
        assert!(!super::current_dictation(true, 3, 1));
        assert!(!super::current_dictation(false, 3, 3));
    }

    #[test]
    fn safety_cancel_dictation_invalidates_queued_results_and_resets_once() {
        let ctx = egui::Context::default();
        let mut app = NeoApp::install(&ctx);
        let (tx, rx) = std::sync::mpsc::channel();
        app.stt_tx = Some(tx);
        app.dictating = true;
        app.dictation_epoch = 7;
        app.dictation_since = Some(std::time::Instant::now());
        app.cancel_dictation();
        assert!(!app.dictating);
        assert!(app.dictation_since.is_none());
        assert_eq!(app.dictation_epoch, 8);
        assert!(matches!(rx.try_recv(), Ok(super::SttCmd::Reset(8))));
        assert!(!super::current_dictation(
            app.dictating,
            app.dictation_epoch,
            7
        ));
        app.cancel_dictation();
        assert_eq!(app.dictation_epoch, 8);
        assert!(matches!(
            rx.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
        app.dictating = true;
        app.dictation_epoch += 1;
        assert!(!super::current_dictation(
            app.dictating,
            app.dictation_epoch,
            7
        ));
        assert!(app.wake.is_none());
    }

    #[test]
    fn safety_session_switch_stabilizes_and_preserves_unsaved_messages() {
        use crate::state::{ChatMessage, Role, StreamSource};
        let store = temp_store("switch-barrier");
        let target = store.create_session("target").unwrap();
        let mut state = AppState::default();
        state.messages.push(ChatMessage::new(Role::User, "keep"));
        state.start_generation(StreamSource::Demo {
            text: "partial".into(),
            cursor: 0,
        });
        state.pump();
        assert!(NeoApp::prepare_session_change(&mut state, Some(&store)));
        assert!(!state.generating);
        let old = state.active_session.unwrap();
        assert_eq!(store.messages(old).unwrap().len(), state.messages.len());
        state.messages.push(ChatMessage::new(Role::User, "unsaved"));
        store.delete_session(old).unwrap();
        assert!(!NeoApp::open_session(&mut state, &store, target));
        assert_eq!(state.active_session, Some(old));
        assert_eq!(state.messages.last().unwrap().content, "unsaved");
        assert!(!state.generating);
    }

    #[test]
    fn safety_save_failure_blocks_spawn_and_recovers_without_livelock() {
        use crate::state::{ChatMessage, Role, ToolMeta, ToolState};
        let store = temp_store("save-gate");
        let mut state = AppState::default();
        let missing = store.create_session("removed").unwrap();
        store.delete_session(missing).unwrap();
        state.active_session = Some(missing);
        state
            .messages
            .push(ChatMessage::new(Role::Assistant, "request"));
        let mut meta = ToolMeta::restored("write_file");
        meta.state = ToolState::Running;
        state
            .messages
            .push(ChatMessage::tool_result(meta, String::new()));
        state.tool_open = true;
        assert!(!NeoApp::advance_tools(&mut state, Some(&store)));
        assert!(!state.tools_running());
        assert_eq!(
            state.messages[1].tool.as_ref().unwrap().state,
            ToolState::Running
        );
        state.active_session = None;
        state.plan_mode = true;
        assert!(NeoApp::advance_tools(&mut state, Some(&store)));
        assert!(state.tools_settled());
        assert_eq!(state.pending_persist, state.messages.len());
        let rows = store.messages(state.active_session.unwrap()).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(!state.messages[1].content.is_empty());
    }

    #[test]
    fn safety_pending_tool_is_not_a_save_failure_but_settled_result_is() {
        use crate::state::{ChatMessage, Role, ToolMeta, ToolState};
        let store = temp_store("pending-versus-failed");
        let mut state = AppState::default();
        state
            .messages
            .push(ChatMessage::new(Role::Assistant, "request"));
        let mut meta = ToolMeta::restored("ask_user");
        meta.state = ToolState::AwaitingConfirm;
        state
            .messages
            .push(ChatMessage::tool_result(meta, String::new()));
        state.tool_open = true;
        assert!(NeoApp::advance_tools(&mut state, Some(&store)));
        assert_eq!(state.pending_persist, 1);
        assert!(!state.tools_settled());
        let id = state.active_session.unwrap();
        store.delete_session(id).unwrap();
        state.answer_question(1, Some("answer".into()));
        for _ in 0..2 {
            assert!(!NeoApp::advance_tools(&mut state, Some(&store)));
            assert!(state.tool_open);
            assert!(state.tools_settled());
            assert_eq!(state.pending_persist, 1);
        }
        assert!(!NeoApp::prepare_session_change(&mut state, Some(&store)));
        assert_eq!(state.messages.len(), 2);
        assert!(state.messages[1].content.contains("answer"));
    }

    #[test]
    fn safety_approved_batch_rechecks_plan_before_spawning() {
        use crate::state::{ChatMessage, Role, ToolState};
        let store = temp_store("approved-plan");
        let mut state = AppState::default();
        state.classroom_safe = false;
        state
            .messages
            .push(ChatMessage::new(Role::Assistant, "request"));
        for (index, (name, args)) in [
            (
                "write_file",
                serde_json::json!({"path":"must-not-write", "content":"x"}),
            ),
            (
                "powershell",
                serde_json::json!({"command":"echo must-not-run"}),
            ),
            (
                "web_search",
                serde_json::json!({"query":"test", "open_browser":true}),
            ),
        ]
        .into_iter()
        .enumerate()
        {
            state.tool_frags.push(neo_llm::ToolCallFrag {
                index,
                id: Some(format!("call-{index}")),
                name: Some(name.into()),
                args: args.to_string(),
            });
        }
        state.begin_tool_round();
        state.approve_all_awaiting();
        for message in &state.messages[1..] {
            assert_eq!(message.tool.as_ref().unwrap().state, ToolState::Running);
        }
        state.plan_mode = true;
        assert!(NeoApp::advance_tools(&mut state, Some(&store)));
        assert!(!state.tools_running());
        assert!(state.tools_settled());
        assert_eq!(state.pending_persist, state.messages.len());
        for message in &state.messages[1..] {
            assert_eq!(message.tool.as_ref().unwrap().state, ToolState::Denied);
            assert!(!message.content.is_empty());
        }
        assert_eq!(
            store.messages(state.active_session.unwrap()).unwrap().len(),
            4
        );
    }

    #[test]
    fn safety_deleting_other_session_keeps_current_round_running() {
        use crate::state::{ChatMessage, Role, StreamSource};
        let store = temp_store("delete-other");
        let other = store.create_session("other").unwrap();
        let mut state = AppState::default();
        state.messages.push(ChatMessage::new(Role::User, "keep"));
        assert!(NeoApp::persist_ready(&mut state, Some(&store)));
        let current = state.active_session;
        state.start_generation(StreamSource::Demo {
            text: "partial".into(),
            cursor: 0,
        });
        state.pump();
        let epoch = state.session_epoch;
        assert!(NeoApp::delete_session(&mut state, &store, other));
        assert!(state.generating);
        assert!(!state.round_cancelled);
        assert_eq!(state.active_session, current);
        assert_eq!(state.session_epoch, epoch);
        assert_eq!(state.messages[1].content, "pa");
        assert!(NeoApp::delete_session(&mut state, &store, current.unwrap()));
        assert!(state.messages.is_empty());
        assert!(!state.generating);
        assert_ne!(state.session_epoch, epoch);
    }

    #[test]
    fn safety_delete_current_does_not_discard_failed_save() {
        use crate::state::{ChatMessage, Role};
        let store = temp_store("delete-failed");
        let id = store.create_session("gone").unwrap();
        store.delete_session(id).unwrap();
        let mut state = AppState::default();
        state.active_session = Some(id);
        state.messages.push(ChatMessage::new(Role::User, "unsaved"));
        assert!(!NeoApp::delete_session(&mut state, &store, id));
        assert_eq!(state.active_session, Some(id));
        assert_eq!(state.messages[0].content, "unsaved");
        assert_eq!(state.pending_persist, 0);
    }

    #[test]
    fn safety_test_databases_are_thread_local() {
        let own = super::test_db_path();
        let other = std::thread::spawn(super::test_db_path).join().unwrap();
        assert_ne!(own, other);
        assert_eq!(own, super::test_db_path());
    }

    #[test]
    fn restores_last_active_session() {
        let store = temp_store("ok");
        let keep = store.create_session("留着的").unwrap();
        store.append_message(keep, "user", "你好", "", "").unwrap();
        store.create_session("别的").unwrap();
        store
            .set_setting("active_session", &keep.to_string())
            .unwrap();

        let mut state = AppState::default();
        NeoApp::refresh_sessions(&mut state, &store);
        NeoApp::restore_last_session(&mut state, &store);

        assert_eq!(state.active_session, Some(keep));
        assert_eq!(state.stage, Stage::Conversation);
        assert_eq!(state.messages.len(), 1);
        assert_eq!(state.messages[0].content, "你好");
        assert_eq!(
            state.pending_persist,
            state.messages.len(),
            "历史消息不应重复落库"
        );
        state.auto_approve_tools = true;
        NeoApp::open_session(&mut state, &store, keep);
        assert!(!state.auto_approve_tools, "切换会话必须清除临时授权");
        state.auto_approve_tools = true;
        let title = state.current_title();
        state.new_session();
        assert!(!state.auto_approve_tools, "新会话必须重新询问授权");
        assert_eq!(title, "留着的");
        NeoApp::open_session(&mut state, &store, keep);
        // 顶栏标题读会话行。
        assert_eq!(state.current_title(), "留着的");
    }

    fn sample_attachment(image: bool) -> crate::attachments::Attachment {
        crate::attachments::Attachment {
            name: if image {
                "题图.png"
            } else {
                "课堂资料.docx"
            }
            .into(),
            kind: if image { "image" } else { "document" }.into(),
            bytes: 32,
            text: "课堂资料正文".into(),
            image_url: image.then(|| "data:image/png;base64,SAMPLE".into()),
            warning: None,
        }
    }

    thread_local! {
        static CAPTURED_REQUESTS: std::cell::RefCell<Vec<Vec<neo_llm::Msg>>> = const { std::cell::RefCell::new(Vec::new()) };
        static INCOMING_MESSAGES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    struct MockStream;

    impl MockStream {
        fn install() -> Self {
            crate::state::API_MESSAGE_BUILDS.with(|count| count.set(0));
            CAPTURED_REQUESTS.with(|requests| requests.borrow_mut().clear());
            super::STREAM_START.with(|start| start.set(|cfg, mut messages, tools| {
                // 与通用入口一样，续轮也必须预算；只发送本地 channel 事件。
                INCOMING_MESSAGES.with(|count| count.set(messages.len()));
                let result = neo_llm::budget_messages(&cfg, &mut messages, &tools);
                CAPTURED_REQUESTS.with(|requests| requests.borrow_mut().push(messages));
                let (tx, rx) = std::sync::mpsc::channel();
                tx.send(match result {
                    Ok(()) => neo_llm::Event::Done { tool_calls: false },
                    Err(error) => neo_llm::Event::Failed(error),
                }).unwrap();
                neo_llm::Stream::new_for_test(rx)
            }));
            Self
        }
    }

    impl Drop for MockStream {
        fn drop(&mut self) {
            super::STREAM_START.with(|start| start.set(neo_llm::start_with_tools));
            CAPTURED_REQUESTS.with(|requests| requests.borrow_mut().clear());
        }
    }

    fn synthetic_send_state(image: bool) -> AppState {
        use crate::state::{ChatMessage, Role};
        let mut state = AppState::default();
        state.context_tokens = 32 * 1024;
        state.api_key = "synthetic-test".into();
        state.restore_models("deepseek-chat", Some("deepseek-chat"));
        state.messages.push(ChatMessage::new(Role::User, "历史问题"));
        state.messages.push(ChatMessage::new(Role::Assistant, "历史回答"));
        state.draft = "  分析课堂资料  ".into();
        let mut attachment = sample_attachment(image);
        if image {
            use base64::Engine;
            // 纯合成 PNG 头及载荷，只供预算验证，不调用解码器或模型。
            let mut bytes = vec![0; 768 * 1024];
            bytes[..24].copy_from_slice(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01");
            attachment.image_url = Some(format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes)));
        }
        state.add_attachment(attachment).unwrap();
        state
    }

    #[test]
    fn send_reuse_synthetic_build_count() {
        for image in [false, true] {
            let _mock = MockStream::install();
            let mut state = synthetic_send_state(image);
            // 含图片也低于80%压缩阈值；本测试只验证正常发送的构建复用。
            state.context_tokens = 64 * 1024;
            NeoApp::send_input(&mut state, None);
            assert!(state.generating);
            let builds = crate::state::API_MESSAGE_BUILDS.with(|count| count.get());
            assert_eq!(builds, 1);
            CAPTURED_REQUESTS.with(|requests| {
                let requests = requests.borrow();
                assert_eq!(requests.len(), 1);
                let messages = &requests[0];
                assert_eq!(messages.len(), 4, "image={image}; first={}", messages[0].content);
                assert!(messages[0].content.contains("课堂安全模式限制"));
                assert!(messages[3].content.contains("课堂资料正文"));
                let image_bytes: usize = messages.iter().flat_map(|m| &m.images).map(String::len).sum();
                assert_eq!(image_bytes, if image { 1024 * 1024 + 22 } else { 0 });
                println!("synthetic image={image}: api_messages={builds}, image_bytes_per_build={image_bytes}");
            });
            state.pump();
            assert!(!state.generating);
            assert!(state.messages.last().unwrap().error.is_none());
        }
    }

    #[test]
    fn send_reuse_compacts_instead_of_silently_trimming() {
        let _mock = MockStream::install();
        super::STREAM_START.with(|start| start.set(|cfg, mut messages, tools| {
            assert!(tools.is_empty(), "摘要必须是独立无工具请求");
            neo_llm::budget_messages(&cfg, &mut messages, &tools).unwrap();
            assert!(messages[1].content.contains(&"a".repeat(20_000)));
            CAPTURED_REQUESTS.with(|requests| requests.borrow_mut().push(messages));
            let (tx, rx) = std::sync::mpsc::channel();
            tx.send(neo_llm::Event::Delta { content: "旧目标与已完成回答".into(), reasoning: String::new() }).unwrap();
            tx.send(neo_llm::Event::Done { tool_calls: false }).unwrap();
            neo_llm::Stream::new_for_test(rx)
        }));
        let store = temp_store("compaction-roundtrip");
        let mut state = synthetic_send_state(false);
        state.messages[0].content = "a".repeat(20_000);
        NeoApp::send_input(&mut state, Some(&store));
        assert!(state.compaction.is_some() && state.generating);
        assert!(state.stream.is_none());
        assert!(state.poll_compaction());
        assert!(state.compaction_resume && state.checkpoint_dirty);
        assert!(NeoApp::persist_ready(&mut state, Some(&store)));
        let id = state.active_session.unwrap();
        assert!(store.checkpoint(id).unwrap().is_some());
        let expected = state.api_messages(24);
        assert_eq!(expected.len(), 3);
        assert_eq!(expected[1].role, neo_llm::Role::User);
        assert!(expected[1].content.contains("旧目标与已完成回答"));
        assert!(expected[2].content.contains("分析课堂资料"));
        assert_eq!(state.messages[0].content.len(), 20_000, "摘要不改本地历史");
        let mut restored = AppState::default();
        assert!(NeoApp::open_session(&mut restored, &store, id));
        assert!(restored.checkpoint.as_ref().unwrap().valid(&restored.messages));
        assert_eq!(restored.messages.len(), 3);
        assert_eq!(restored.api_messages(24)[1].content, expected[1].content);
    }

    #[test]
    fn send_reuse_compaction_large_history_is_not_mistaken_for_latest_input() {
        let _mock = MockStream::install();
        let mut state = synthetic_send_state(false);
        state.messages[0].content = "x".repeat(neo_llm::MAX_REQUEST_BYTES + 1);
        let draft = state.draft.clone();
        NeoApp::send_input(&mut state, None);
        assert_eq!(state.draft, draft);
        assert_eq!(state.messages.len(), 2);
        assert_eq!(state.messages[0].content.len(), neo_llm::MAX_REQUEST_BYTES + 1);
        assert!(state.attachment_error.as_ref().unwrap().contains("旧历史无法"));
        assert!(!state.generating);
        CAPTURED_REQUESTS.with(|requests| assert!(requests.borrow().is_empty()));
    }

    #[test]
    fn compaction_tool_protocol_survives_sqlite_reopen_with_uncovered_results() {
        use crate::state::{ChatMessage, Role};
        let store = temp_store("compaction-tool-protocol");
        let path = std::env::temp_dir().join(format!("neo-restore-compaction-tool-protocol-{}", std::process::id())).join("neo.db");
        let mut state = synthetic_send_state(false);
        state.messages[0].content = "a".repeat(20_000);
        assert!(state.submit());
        let mut checkpoint = state.compaction_plan(&state.api_messages(24)).unwrap().unwrap().checkpoint;
        checkpoint.summary = "旧历史摘要".into();
        state.checkpoint = Some(checkpoint);
        state.checkpoint_dirty = true;
        state.messages.push(ChatMessage::new(Role::Assistant, "需要确认"));
        let arguments = r#"{ "question": "保留原始参数?", "options": ["是", "否"] }"#;
        state.tool_frags = vec![neo_llm::ToolCallFrag {
            index: 0, id: Some("persist-call".into()), name: Some("ask_user".into()), args: arguments.into(),
        }];
        state.begin_tool_round();
        state.answer_question(4, Some("是".into()));
        assert!(NeoApp::persist_ready(&mut state, Some(&store)));
        let id = state.active_session.unwrap();
        let expected = state.messages[4].content.clone();
        drop(store);
        let store = Store::open(&path).unwrap();
        let mut restored = AppState::default();
        assert!(NeoApp::open_session(&mut restored, &store, id));
        assert!(restored.checkpoint.is_some());
        assert_eq!(restored.messages[3].tool_calls[0].arguments, arguments);
        let mut messages = restored.api_messages(24);
        neo_llm::budget_messages(&restored.llm_config(), &mut messages, &neo_tools::tool_declarations()).unwrap();
        assert_eq!(messages.last().unwrap().tool_call_id.as_deref(), Some("persist-call"));
        assert_eq!(messages.last().unwrap().content, expected);
        drop(store);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn context_legacy_tool_results_restore_as_low_trust_without_fabricated_calls() {
        let store = temp_store("context-legacy-tool");
        let id = store.create_session("旧工具记录").unwrap();
        store.append_message(id, "assistant", "读取结果", "", "").unwrap();
        store.append_message(id, "tool", "历史结果不能消失", "", "read_file · 读取完成").unwrap();
        let mut state = AppState::default();
        assert!(NeoApp::open_session(&mut state, &store, id));
        let mut messages = state.api_messages(24);
        assert!(messages.iter().all(|m| m.tool_calls.is_empty()));
        let result = messages.last().unwrap();
        assert_eq!(result.role, neo_llm::Role::User);
        assert!(result.content.contains("低信任历史工具结果"));
        assert!(result.content.contains("历史结果不能消失"));
        neo_llm::budget_messages(&state.llm_config(), &mut messages, &neo_tools::tool_declarations()).unwrap();
    }

    #[test]
    fn task_tool_limit_500th_completes_and_persists_but_never_continues() {
        use crate::state::{ChatMessage, Role, ToolState};
        for batch in [1, 3] {
            let _mock = MockStream::install();
            let store = temp_store(&format!("task-tool-limit-{batch}"));
            let mut state = synthetic_send_state(false);
            assert!(state.submit());
            state.task_tool_calls = 499;
            state.messages.push(ChatMessage::new(Role::Assistant, "确认"));
            state.tool_frags = (0..batch).map(|index| neo_llm::ToolCallFrag {
                index, id: Some(format!("limit-{index}")), name: Some("ask_user".into()), args: r#"{"question":"继续?"}"#.into(),
            }).collect();
            assert_eq!(state.begin_tool_round(), batch);
            assert!(state.task_limit_reached);
            assert_eq!(state.messages[4].tool.as_ref().unwrap().state, ToolState::AwaitingConfirm);
            state.answer_question(4, Some("第500次完成".into()));
            assert!(state.messages[4].tool.as_ref().unwrap().outcome.as_ref().unwrap().is_ok());
            for msg in &state.messages[5..] {
                assert_eq!(msg.tool.as_ref().unwrap().state, ToolState::Denied);
            }
            assert!(NeoApp::advance_tools(&mut state, Some(&store)));
            let rows = store.messages(state.active_session.unwrap()).unwrap();
            assert_eq!(rows.len(), state.messages.len());
            assert!(rows[4].content.contains("第500次完成"));
            assert_eq!(rows[4].tool_call_id, "limit-0");
            let mut messages = state.api_messages(24);
            neo_llm::budget_messages(&state.llm_config(), &mut messages, &neo_tools::tool_declarations()).unwrap();
            NeoApp::start_real_stream(&mut state);
            assert!(!state.generating);
            CAPTURED_REQUESTS.with(|requests| assert!(requests.borrow().is_empty()));
        }
    }

    #[test]
    fn task_tool_limit_500th_read_dispatches_after_save_and_persists() {
        use crate::state::{ChatMessage, Role, ToolState};
        let _mock = MockStream::install();
        let store = temp_store("task-tool-limit-read");
        let dir = std::env::temp_dir().join(format!("neo-restore-task-tool-limit-read-{}", std::process::id()));
        std::fs::write(dir.join("input.txt"), "第500次读取成功").unwrap();
        let mut state = synthetic_send_state(false);
        assert!(state.submit());
        state.task_tool_calls = 499;
        state.messages.push(ChatMessage::new(Role::Assistant, "读取"));
        state.tool_frags = vec![neo_llm::ToolCallFrag {
            index: 0, id: Some("last-read".into()), name: Some("read_file".into()), args: r#"{"path":"input.txt"}"#.into(),
        }];
        state.begin_tool_round();
        assert!(state.task_limit_reached);
        assert_eq!(state.messages[4].tool.as_ref().unwrap().state, ToolState::Running);
        assert!(NeoApp::persist_ready(&mut state, Some(&store)));
        assert_eq!(state.pending_persist, 4);
        assert_eq!(state.spawn_ready_tools(&neo_tools::Scope::new(&dir)), 1);
        assert!(state.wait_tool_jobs(std::time::Duration::from_secs(5)));
        assert!(state.messages[4].tool.as_ref().unwrap().outcome.as_ref().unwrap().is_ok());
        assert!(NeoApp::persist_ready(&mut state, Some(&store)));
        assert!(store.messages(state.active_session.unwrap()).unwrap()[4].content.contains("第500次读取成功"));
        NeoApp::start_real_stream(&mut state);
        CAPTURED_REQUESTS.with(|requests| assert!(requests.borrow().is_empty()));
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn compaction_restore_rejects_corrupt_checkpoint_without_losing_history() {
        let store = temp_store("compaction-corrupt");
        let mut state = synthetic_send_state(false);
        state.messages[0].content = "a".repeat(20_000);
        assert!(state.submit());
        assert!(NeoApp::persist_ready(&mut state, Some(&store)));
        let plan = state.compaction_plan(&state.api_messages(24)).unwrap().unwrap();
        let mut checkpoint = plan.checkpoint;
        checkpoint.summary = "历史摘要".into();
        checkpoint.fingerprint ^= 1;
        let id = state.active_session.unwrap();
        store.save_checkpoint(id, checkpoint.covered, &serde_json::to_string(&checkpoint).unwrap()).unwrap();
        let mut restored = AppState::default();
        assert!(NeoApp::open_session(&mut restored, &store, id));
        assert!(restored.checkpoint.is_none());
        assert!(restored.compaction_status.as_ref().unwrap().contains("校验失败"));
        assert_eq!(restored.messages.len(), 3);
        assert_eq!(restored.api_messages(24)[1].content.len(), 20_000);
    }

    #[test]
    fn compaction_checkpoint_save_failure_holds_resume_and_cancel_stops_it() {
        let store = temp_store("compaction-readonly");
        let mut state = synthetic_send_state(false);
        state.messages[0].content = "a".repeat(20_000);
        assert!(state.submit());
        assert!(NeoApp::persist_ready(&mut state, Some(&store)));
        let plan = state.compaction_plan(&state.api_messages(24)).unwrap().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        state.start_compaction(plan, neo_llm::Stream::new_for_test(rx));
        tx.send(neo_llm::Event::Delta { content: "完整摘要".into(), reasoning: String::new() }).unwrap();
        tx.send(neo_llm::Event::Done { tool_calls: false }).unwrap();
        state.poll_compaction();
        drop(store);
        let path = std::env::temp_dir().join(format!("neo-restore-compaction-readonly-{}", std::process::id())).join("neo.db");
        let store = readonly_store(&path);
        assert!(!NeoApp::persist_ready(&mut state, Some(&store)));
        assert!(state.compaction_resume && state.checkpoint_dirty);
        state.draft = "不能越过保存屏障".into();
        assert!(!state.can_submit());
        state.cancel();
        assert!(!state.compaction_resume);
        assert_eq!(state.messages.len(), 3);
        assert!(store.checkpoint(state.active_session.unwrap()).unwrap().is_none());
    }

    #[test]
    fn compaction_tick_resumes_once_after_checkpoint_save_and_limit_stops_continuation() {
        let _mock = MockStream::install();
        super::STREAM_START.with(|start| start.set(|_, messages, tools| {
            let summary = tools.is_empty();
            CAPTURED_REQUESTS.with(|requests| requests.borrow_mut().push(messages));
            let (tx, rx) = std::sync::mpsc::channel();
            if summary { tx.send(neo_llm::Event::Delta { content: "历史整理完成".into(), reasoning: String::new() }).unwrap(); }
            tx.send(neo_llm::Event::Done { tool_calls: false }).unwrap();
            neo_llm::Stream::new_for_test(rx)
        }));
        let ctx = egui::Context::default();
        let mut app = NeoApp::install(&ctx);
        app.store = Some(temp_store("compaction-tick"));
        app.state = synthetic_send_state(false);
        app.state.messages[0].content = "a".repeat(20_000);
        NeoApp::send_input(&mut app.state, app.store.as_ref());
        assert!(app.state.compaction.is_some());
        app.tick(ctx.clone());
        assert!(!app.state.checkpoint_dirty);
        assert!(app.store.as_ref().unwrap().checkpoint(app.state.active_session.unwrap()).unwrap().is_some());
        app.tick(ctx.clone());
        app.tick(ctx);
        CAPTURED_REQUESTS.with(|requests| assert_eq!(requests.borrow().len(), 2));
        assert!(!app.state.generating && !app.state.compaction_resume);
        app.state.task_tool_calls = 500;
        app.state.task_limit_reached = true;
        NeoApp::start_real_stream(&mut app.state);
        CAPTURED_REQUESTS.with(|requests| assert_eq!(requests.borrow().len(), 2));
        assert!(app.state.attachment_error.as_ref().unwrap().contains("500"));
    }

    #[test]
    fn compaction_config_change_after_completion_cancels_automatic_resume() {
        let _mock = MockStream::install();
        let ctx = egui::Context::default();
        let mut app = NeoApp::install(&ctx);
        app.store = Some(temp_store("compaction-config-change"));
        app.state = synthetic_send_state(false);
        app.state.messages[0].content = "a".repeat(20_000);
        assert!(app.state.submit());
        assert!(NeoApp::persist_ready(&mut app.state, app.store.as_ref()));
        let plan = app.state.compaction_plan(&app.state.api_messages(24)).unwrap().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        app.state.start_compaction(plan, neo_llm::Stream::new_for_test(rx));
        tx.send(neo_llm::Event::Delta { content: "历史摘要".into(), reasoning: String::new() }).unwrap();
        tx.send(neo_llm::Event::Done { tool_calls: false }).unwrap();
        assert!(app.state.poll_compaction());
        assert!(app.state.compaction_resume);
        app.state.context_tokens += 1024;
        app.tick(ctx);
        assert!(!app.state.compaction_resume && !app.state.generating);
        assert!(app.state.compaction_status.as_ref().unwrap().contains("自动续轮已取消"));
        CAPTURED_REQUESTS.with(|requests| assert!(requests.borrow().is_empty()));
        assert_eq!(app.state.messages.len(), 3);
    }

    #[test]
    fn send_reuse_save_barriers_preserve_draft_and_attachments() {
        use crate::state::{ChatMessage, Role};
        let store = temp_store("send-reuse-readonly");
        let id = store.create_session("原会话").unwrap();
        let path = std::env::temp_dir().join(format!("neo-restore-send-reuse-readonly-{}", std::process::id())).join("neo.db");
        drop(store);
        let store = readonly_store(&path);
        for before_submit in [true, false] {
            let _mock = MockStream::install();
            let mut state = synthetic_send_state(true);
            state.messages.clear();
            state.active_session = Some(id);
            if before_submit {
                state.messages.push(ChatMessage::new(Role::User, "未保存历史"));
                state.stage = Stage::Conversation;
            }
            let draft = state.draft.clone();
            let attachments = serde_json::to_string(&state.draft_attachments).unwrap();
            NeoApp::send_input(&mut state, Some(&store));
            // 原语义：首屏障失败不触碰草稿，提交后的保存失败恢复已 trim 的正文。
            assert_eq!(state.draft, if before_submit { draft.as_str() } else { draft.trim() });
            assert_eq!(serde_json::to_string(&state.draft_attachments).unwrap(), attachments);
            assert_eq!(state.messages.len(), usize::from(before_submit));
            assert_eq!(state.pending_persist, 0);
            assert_eq!(state.stage, if before_submit { Stage::Conversation } else { Stage::Hero });
            assert_eq!(state.active_session, before_submit.then_some(id));
            assert!(state.attachment_error.is_some());
            assert!(!state.generating && !state.wants_demo_reply && state.stream.is_none());
            assert!(store.messages(id).unwrap().is_empty());
            CAPTURED_REQUESTS.with(|requests| assert!(requests.borrow().is_empty()));
            crate::state::API_MESSAGE_BUILDS.with(|count| assert_eq!(count.get(), usize::from(!before_submit)));
        }
    }

    #[test]
    fn send_reuse_budget_failure_restores_original_draft_and_image() {
        let _mock = MockStream::install();
        let mut state = synthetic_send_state(true);
        state.draft = format!("  {}  ", "课堂正文".repeat(20_000));
        state.stage = Stage::Conversation;
        let draft = state.draft.clone();
        let attachments = serde_json::to_string(&state.draft_attachments).unwrap();
        NeoApp::send_input(&mut state, None);
        assert_eq!(state.draft, draft);
        assert_eq!(serde_json::to_string(&state.draft_attachments).unwrap(), attachments);
        assert_eq!(state.messages.len(), 2);
        assert_eq!(state.pending_persist, 2);
        assert_eq!(state.stage, Stage::Conversation);
        assert!(state.attachment_error.is_some());
        assert!(!state.generating && !state.wants_demo_reply && state.stream.is_none());
        CAPTURED_REQUESTS.with(|requests| assert!(requests.borrow().is_empty()));
        crate::state::API_MESSAGE_BUILDS.with(|count| assert_eq!(count.get(), 1));
    }

    #[test]
    fn send_reuse_tool_round_still_checks_budget_and_pairing() {
        use crate::state::{ChatMessage, Role, ToolMeta};
        for over_budget in [false, true] {
            let _mock = MockStream::install();
            let mut state = synthetic_send_state(false);
            assert!(state.submit());
            let mut assistant = ChatMessage::new(Role::Assistant, "");
            assistant.reasoning = if over_budget { "推理".repeat(20_000) } else { "先读取资料".into() };
            assistant.tool_calls.push(neo_llm::ToolCall {
                id: "synthetic-call".into(), name: "read_file".into(), arguments: "{}".into(),
            });
            state.messages.push(assistant);
            let mut meta = ToolMeta::restored("read_file");
            meta.call_id = "synthetic-call".into();
            state.messages.push(ChatMessage::tool_result(meta, "工具结果".into()));
            NeoApp::start_real_stream(&mut state);
            crate::state::API_MESSAGE_BUILDS.with(|count| assert_eq!(count.get(), 1));
            CAPTURED_REQUESTS.with(|requests| {
                let requests = requests.borrow();
                assert_eq!(requests.len(), 1);
                if !over_budget {
                    let messages = &requests[0];
                    assert_eq!(messages[4].tool_calls[0].id, "synthetic-call");
                    assert_eq!(messages[5].tool_call_id.as_deref(), Some("synthetic-call"));
                    assert_eq!(messages[5].content, "工具结果");
                }
            });
            state.poll_compaction();
            state.pump();
            assert!(!state.generating);
            assert_eq!(state.attachment_error.is_some(), over_budget);
            assert!(state.messages.iter().any(|m| m.content == "工具结果"));
        }
    }

    #[test]
    fn attachment_persistence_restores_payload_and_tolerates_bad_json() {
        let store = temp_store("attachments-restore");
        let mut state = AppState::default();
        state.add_attachment(sample_attachment(true)).unwrap();
        assert!(state.submit());
        state.start_generation(crate::state::StreamSource::Demo {
            text: "稍后回复".into(),
            cursor: 0,
        });
        assert!(NeoApp::persist_ready(&mut state, Some(&store)));
        assert_eq!(
            state.pending_persist, 1,
            "生成过程中必须已保存用户附件，不能保存流式占位"
        );
        let id = state.active_session.unwrap();
        let path = std::env::temp_dir()
            .join(format!(
                "neo-restore-attachments-restore-{}",
                std::process::id()
            ))
            .join("neo.db");
        drop(store);
        let store = neo_store::Store::open(&path).unwrap();
        store
            .append_message_with_attachments(id, "user", "仍可读", "", "", "invalid-json")
            .unwrap();
        let mut restored = AppState::default();
        restored.draft = "清除旧草稿".into();
        restored.add_attachment(sample_attachment(false)).unwrap();
        NeoApp::open_session(&mut restored, &store, id);
        assert!(restored.draft.is_empty() && restored.draft_attachments.is_empty());
        assert_eq!(restored.messages.len(), 2);
        assert_eq!(restored.messages[0].attachments.len(), 1);
        assert!(restored.attachment_error.as_ref().unwrap().contains("损坏"));
        let body =
            neo_llm::request_body(&restored.llm_config(), &restored.api_messages(24), vec![]);
        assert_eq!(
            body["messages"][1]["content"][1]["image_url"]["url"],
            "data:image/png;base64,SAMPLE"
        );
    }

    #[test]
    fn attachment_send_without_model_preserves_draft() {
        for configured in [false, true] {
            let mut state = AppState::default();
            if configured {
                state.api_key = "test-only".into();
                state.api_base = String::new();
            }
            state.draft = "分析这份资料".into();
            state.add_attachment(sample_attachment(false)).unwrap();
            NeoApp::send_input(&mut state, None);
            assert_eq!(state.draft_attachments.len(), 1);
            assert_eq!(state.draft, "分析这份资料");
            assert!(state.messages.is_empty() && !state.wants_demo_reply);
            assert!(state.attachment_error.as_ref().unwrap().contains("未发送"));
        }
    }

    #[test]
    fn attachment_save_failure_does_not_advance_cursor() {
        let store = temp_store("attachment-save-failure");
        let mut state = AppState::default();
        state.active_session = Some(999_999);
        state.add_attachment(sample_attachment(false)).unwrap();
        assert!(state.submit());
        assert!(!NeoApp::persist_ready(&mut state, Some(&store)));
        assert_eq!(state.pending_persist, 0);
        assert!(state.attachment_error.is_some());
    }

    #[test]
    fn attachment_and_text_sends_reach_local_mock_model() {
        use std::io::{Read, Write};
        for mode in 0..3 {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let address = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
                let mut stream = loop {
                    if let Ok((stream, _)) = listener.accept() {
                        break stream;
                    }
                    assert!(std::time::Instant::now() < deadline, "模型没有收到请求");
                    std::thread::sleep(std::time::Duration::from_millis(10));
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut chunk = [0u8; 4096];
                let (offset, length) = loop {
                    let count = stream.read(&mut chunk).unwrap();
                    assert!(count > 0);
                    bytes.extend_from_slice(&chunk[..count]);
                    if let Some(pos) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let header = String::from_utf8_lossy(&bytes[..pos]);
                        let length = header
                            .lines()
                            .find_map(|line| {
                                let (key, value) = line.split_once(':')?;
                                key.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().unwrap())
                            })
                            .unwrap();
                        break (pos + 4, length);
                    }
                };
                while bytes.len() < offset + length {
                    let count = stream.read(&mut chunk).unwrap();
                    assert!(count > 0);
                    bytes.extend_from_slice(&chunk[..count]);
                }
                let body: serde_json::Value =
                    serde_json::from_slice(&bytes[offset..offset + length]).unwrap();
                let response = "data: {\"choices\":[{\"delta\":{\"content\":\"已收到\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n";
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", response.len(), response).unwrap();
                body
            });
            let mut state = AppState::default();
            state.api_base = format!("http://{address}");
            state.api_key = "local-test".into();
            state.set_models_from_provider(vec!["test-vision".into()]);
            if mode == 0 {
                state.draft = "纯文字".into();
            } else {
                state.add_attachment(sample_attachment(mode == 2)).unwrap();
            }
            NeoApp::send_input(&mut state, None);
            assert!(state.generating);
            let body = server.join().unwrap();
            let user = &body["messages"][1]["content"];
            if mode == 0 {
                assert_eq!(user, "纯文字");
            } else if mode == 1 {
                assert!(user.as_str().unwrap().contains("课堂资料正文"));
            } else {
                assert_eq!(user[1]["type"], "image_url");
            }
            state.cancel();
        }
    }

    #[test]
    fn ignores_stale_session_id() {
        let store = temp_store("stale");
        store.create_session("现存会话").unwrap();
        store.set_setting("active_session", "99999").unwrap();

        let mut state = AppState::default();
        NeoApp::refresh_sessions(&mut state, &store);
        NeoApp::restore_last_session(&mut state, &store);

        assert_eq!(state.active_session, None);
        assert_eq!(state.stage, Stage::Hero);
    }
}

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
        self.render(ui);
    }

    /// 窗口隐藏（托盘）时 eframe 只调这个——纯逻辑照跑，后台才能响应
    /// 托盘点击与「Hi, Neo」唤醒。与 `ui` 互斥，不会同帧双跑。
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.tick(ctx.clone());
    }
}

#[cfg(test)]
mod snapshot {
    //! 离屏渲染：把主界面渲成 PNG，落到 `docs/screens/`。
    //!
    //! 这些不是"必须通过"的断言型测试 —— 它们的价值是**让人能看见界面**。
    //! 跑 `cargo test -p neo-app -- --nocapture` 之后直接看 `docs/screens/*.png`。
    //!
    //! 之所以能这么做，是因为 [`NeoApp::install`] 只依赖 `egui::Context`：
    //! 测试与真机走完全相同的字体装配与主题构建路径。
    //!
    //! # 测试与数据库
    //!
    //! install 在测试构建中使用线程独立的显式临时数据库路径，不修改进程环境。

    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::rc::Rc;

    use egui::Vec2;

    use super::NeoApp;
    use crate::state::{AppState, ChatMessage, Role, Stage, StreamSource, ToolMeta, ToolState};
    use neo_theme::{Distance, ThemeMode};

    fn isolate_db() {
        let _ = super::test_db_path();
    }

    /// 只清理当前测试线程的库，不会删除并行测试的数据库。
    fn fresh_db() {
        let _ = std::fs::remove_file(super::test_db_path());
    }

    /// 一次性生效的初始化回调（只允许取用一次）。
    type Setup = Box<dyn FnOnce(&mut NeoApp)>;

    /// 输出目录：`<workspace>/docs/screens`。
    fn out_dir() -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/screens");
        std::fs::create_dir_all(&dir).expect("无法创建 docs/screens");
        dir.canonicalize().unwrap_or(dir)
    }

    /// 渲染一帧并落盘。
    fn shoot(name: &str, size: Vec2, setup: impl FnOnce(&mut NeoApp) + 'static) -> PathBuf {
        shoot_steps(name, size, 2, setup)
    }

    /// 指定帧数的版本。持续重绘的界面（脉动点、鲸鱼游动）会让
    /// [`Harness::run`] 撞上 max_steps 上限 —— 它假设界面最终会静止。
    fn shoot_steps(
        name: &str,
        size: Vec2,
        steps: usize,
        setup: impl FnOnce(&mut NeoApp) + 'static,
    ) -> PathBuf {
        isolate_db();
        let app: RefCell<Option<NeoApp>> = RefCell::new(None);
        let setup: RefCell<Option<Setup>> = RefCell::new(Some(Box::new(setup)));

        let mut harness = egui_kittest::Harness::builder()
            .with_size(size)
            .wgpu()
            .build_ui(|ui| {
                let mut slot = app.borrow_mut();
                let Some(app) = slot.as_mut() else {
                    // 首帧：只装配（字体 / 主题 / 品牌纹理），不绘制。
                    fresh_db();
                    let mut fresh = NeoApp::install(ui.ctx());
                    // ⚠️ 快照必须从**干净空态**开始：`install` 会从共享的测试库里
                    // 恢复「上次会话」并把 stage 切成对话态 —— 于是标称 hero 的用例
                    // 实际画的是对话视图（顶右出现「新对话」的加号而不是主题钮）。
                    // 这里把会话状态清空；需要对话的用例自己在 setup 里 seed。
                    fresh.state.messages.clear();
                    fresh.state.active_session = None;
                    fresh.state.stage = Stage::Hero;
                    fresh.state.pending_persist = 0;
                    if let Some(f) = setup.borrow_mut().take() {
                        f(&mut fresh);
                    }
                    *slot = Some(fresh);
                    ui.ctx().request_repaint();
                    return;
                };
                app.render(ui);
            });

        harness.run_steps(steps);

        if name == "25-commonmark-conversation" {
            let (viewport, content, offset) = harness.ctx.data(|data| {
                data.get_temp::<(egui::Rect, Vec2, Vec2)>(egui::Id::new("neo-test-thread-geometry"))
                    .unwrap()
            });
            assert!(
                content.y < viewport.height(),
                "short conversation must fit: {content:?} in {viewport:?}"
            );
            assert!(offset.y.abs() < 1.0, "no phantom bottom scroll: {offset:?}");
            let visible = harness.output().shapes.iter().any(|shape| {
                if let egui::Shape::Text(t) = &shape.shape {
                    t.galley.text() == "目录与文件清单" && shape.clip_rect.contains(t.pos)
                } else {
                    false
                }
            });
            assert!(
                visible,
                "table heading must actually be visible, not only stored in message state"
            );
        }

        let image = harness
            .render()
            .unwrap_or_else(|e| panic!("离屏渲染 {name} 失败: {e}"));
        let path = out_dir().join(format!("{name}.png"));
        image.save(&path).expect("PNG 落盘失败");
        println!("[snapshot] {} × {} → {}", size.x, size.y, path.display());
        path
    }

    /// 填一段演示对话（含流式结束后的助手回复）。
    fn seed_dialogue(app: &mut NeoApp) {
        app.state.draft = "帮我用板书的结构讲解「楞次定律」".to_owned();
        app.state.submit();
        let demo = crate::state::demo_reply("楞次定律", app.state.model_display());
        app.state.start_generation(StreamSource::Demo {
            text: demo,
            cursor: 0,
        });
        // 一次泵完，不依赖时间推进。
        while app.state.pump() {}
        app.state.messages[1].meta = "DeepSeek-V3.2 · 1.4s".to_owned();
    }

    /// 搭一个「对话态、输入卡已聚焦」的 Harness。
    ///
    /// 返回 `(harness, app, ctx)`：`app` 通过 `Rc` 共享，测试体在跑完帧后读它断言；
    /// `ctx` 已把焦点请求到输入卡（下一帧 `m.focused()` 生效）。
    fn input_harness(
        draft: &'static str,
    ) -> (
        egui_kittest::Harness<'static>,
        Rc<RefCell<Option<NeoApp>>>,
        egui::Context,
    ) {
        let app: Rc<RefCell<Option<NeoApp>>> = Rc::new(RefCell::new(None));
        let ctx_slot: Rc<RefCell<Option<egui::Context>>> = Rc::new(RefCell::new(None));
        let (app2, ctx2) = (app.clone(), ctx_slot.clone());
        let mut harness = egui_kittest::Harness::builder()
            .with_size(Vec2::new(1600.0, 1000.0))
            .wgpu()
            .build_ui(move |ui| {
                *ctx2.borrow_mut() = Some(ui.ctx().clone());
                let mut slot = app2.borrow_mut();
                if slot.is_none() {
                    // 首帧只装配，不绘制（字体 `set_fonts` 下一帧才生效）。
                    fresh_db();
                    let mut fresh = NeoApp::install(ui.ctx());
                    // 断言型测试要与既有持久化隔离：丢弃 store 走纯内存，
                    // 并清掉 install 恢复出来的上次会话，从空白对话开始。
                    fresh.store = None;
                    fresh.state.messages.clear();
                    fresh.state.active_session = None;
                    fresh.state.pending_persist = 0;
                    fresh.state.generating = false;
                    fresh.state.wants_demo_reply = false;
                    fresh.state.stage = Stage::Conversation;
                    fresh.state.draft = draft.to_owned();
                    *slot = Some(fresh);
                    ui.ctx().request_repaint();
                    return;
                }
                slot.as_mut().unwrap().render(ui);
            });
        harness.run_steps(2);
        let ctx = ctx_slot.borrow().clone().unwrap();
        ctx.memory_mut(|m| m.request_focus(egui::Id::new(crate::ui::COMPOSER_ID)));
        (harness, app, ctx)
    }

    fn visual_attachment(name: &str, image: bool) -> crate::attachments::Attachment {
        use base64::Engine;
        let image_url = image.then(|| {
            let mut bytes = std::io::Cursor::new(Vec::new());
            let image = image::RgbImage::from_fn(240, 150, |x, y| {
                if (x / 30 + y / 30) % 2 == 0 {
                    image::Rgb([76, 126, 208])
                } else {
                    image::Rgb([196, 220, 252])
                }
            });
            image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
            format!(
                "data:image/png;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(bytes.get_ref())
            )
        });
        crate::attachments::Attachment {
            name: name.into(),
            kind: if image { "image" } else { "document" }.into(),
            bytes: 254_810,
            text: "课堂学习资料".into(),
            image_url,
            warning: (!image).then(|| "仅提取文字；不含嵌入图片和复杂排版".into()),
        }
    }

    #[test]
    fn attachment_draft_and_sent_snapshots() {
        for (name, mode, size, sent) in [
            (
                "19-attachments-dark-1080p",
                ThemeMode::Dark,
                Vec2::new(1920.0, 1080.0),
                false,
            ),
            (
                "20-attachments-light-narrow",
                ThemeMode::Light,
                Vec2::new(1024.0, 768.0),
                false,
            ),
            (
                "21-attachments-conversation",
                ThemeMode::Dark,
                Vec2::new(1920.0, 1080.0),
                true,
            ),
        ] {
            shoot(name, size, move |app| {
                app.store = None;
                app.state.theme_mode = mode;
                app.state.distance = Distance::Standard;
                for (name, image) in [
                    ("磁通量变化题图.png", true),
                    ("高中物理必修第三册课堂讲解与练习资料.docx", false),
                    ("课堂实验演示课件.pptx", false),
                ] {
                    app.state
                        .add_attachment(visual_attachment(name, image))
                        .unwrap();
                }
                app.state.draft = "请对照题图和课件，整理这节课的重点".into();
                if sent {
                    app.state.submit();
                    app.state.messages.push(ChatMessage::new(
                        Role::Assistant,
                        "已收到附件。图片将通过视觉通道发送，文档文字则随这条消息提供给模型。",
                    ));
                } else if mode == ThemeMode::Light {
                    app.state.attachment_error =
                        Some("损坏的旧课件.ppt：无法读取文件记录，请另存为 PPTX 后重试".into());
                }
            });
        }
    }

    #[test]
    fn attachment_remove_button_really_removes_last_item() {
        isolate_db();
        let (mut harness, app, ctx) = input_harness("");
        app.borrow_mut()
            .as_mut()
            .unwrap()
            .state
            .add_attachment(visual_attachment("题图.png", true))
            .unwrap();
        harness.run_steps(2);
        let rect = app
            .borrow()
            .as_ref()
            .map(|a| {
                ctx.data(|data| {
                    data.get_temp::<egui::Rect>(egui::Id::new("neo-test-first-attachment-remove"))
                })
                .unwrap_or_else(|| {
                    panic!("附件移除按钮没有绘制：{}", a.state.draft_attachments.len())
                })
            })
            .unwrap();
        let pos = rect.center();
        harness
            .input_mut()
            .events
            .push(egui::Event::PointerMoved(pos));
        for pressed in [true, false] {
            harness.input_mut().events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
            harness.run_steps(1);
        }
        let borrowed = app.borrow();
        let state = &borrowed.as_ref().unwrap().state;
        assert!(state.draft_attachments.is_empty());
        assert!(!state.can_submit());
    }

    /// 中文输入法选词那一帧里按 Enter，不应把消息发出去。
    #[test]
    fn enter_during_ime_commit_does_not_send() {
        isolate_db();
        let (mut harness, app, _ctx) = input_harness("");
        // 同帧塞入：输入法提交「你好」+ 一个 Enter（模拟 winit 未过滤掉的那一下）。
        harness
            .input_mut()
            .events
            .push(egui::Event::Ime(egui::ImeEvent::Commit("你好".to_owned())));
        harness.input_mut().events.push(egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        harness.run_steps(1);

        let app = app.borrow();
        let app = app.as_ref().unwrap();
        assert!(
            app.state.messages.is_empty(),
            "IME 选词的 Enter 不该发送，实际发了 {:?}",
            app.state
                .messages
                .iter()
                .map(|m| &m.content)
                .collect::<Vec<_>>()
        );
        assert!(
            app.state.draft.contains("你好"),
            "候选字应落入草稿，实测 draft={:?}",
            app.state.draft
        );
    }

    /// 英文直接输入（无输入法事件）时，Enter 照常发送。
    #[test]
    fn enter_without_ime_sends() {
        isolate_db();
        let (mut harness, app, _ctx) = input_harness("hello");
        harness.input_mut().events.push(egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        harness.run_steps(1);

        let app = app.borrow();
        let app = app.as_ref().unwrap();
        assert_eq!(
            app.state.messages.len(),
            1,
            "无输入法事件时 Enter 应正常发送"
        );
        assert_eq!(app.state.messages[0].content, "hello");
        assert!(app.state.draft.is_empty());
    }

    #[test]
    fn modal_blocks_enter_and_background_clicks() {
        isolate_db();
        let (mut harness, app, _) = input_harness("不要发送这个草稿");
        app.borrow_mut().as_mut().unwrap().state.show_settings = true;
        harness.run_steps(2);
        harness.input_mut().events.push(egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        // 左侧「新对话」：即使点击真实命中区，也不能操作模态框后的界面。
        let pos = egui::pos2(100.0, 90.0);
        harness
            .input_mut()
            .events
            .push(egui::Event::PointerMoved(pos));
        for pressed in [true, false] {
            harness.input_mut().events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
            harness.run_steps(1);
        }
        let borrowed = app.borrow();
        let state = &borrowed.as_ref().unwrap().state;
        assert!(state.messages.is_empty());
        assert_eq!(state.draft, "不要发送这个草稿");
        assert_eq!(state.stage, Stage::Conversation);
        assert!(state.show_settings);
    }

    #[test]
    fn tool_confirm_long_parameters_really_scroll() {
        isolate_db();
        let (mut harness, app, ctx) = input_harness("");
        let mut message = ChatMessage::new(Role::Assistant, String::new());
        message.tool = Some(ToolMeta {
            call_id: "scroll-test".into(),
            name: "write_file".into(),
            title: "写入文件",
            risk: "write",
            preview: "写入长文本".into(),
            args: serde_json::json!({"content": "完整参数不能丢失。".repeat(1000)}),
            state: ToolState::AwaitingConfirm,
            outcome: None,
        });
        app.borrow_mut()
            .as_mut()
            .unwrap()
            .state
            .messages
            .push(message);
        harness.run_steps(4);
        let probe = egui::Id::new("neo-confirm-scroll-probe");
        let (rect, before, full_h) = ctx
            .data(|d| d.get_temp::<(egui::Rect, f32, f32)>(probe))
            .unwrap();
        assert!(full_h > rect.height());
        harness
            .input_mut()
            .events
            .push(egui::Event::PointerMoved(rect.center()));
        harness.run_steps(1);
        harness.input_mut().events.push(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: Vec2::new(0.0, -500.0),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
        });
        harness.run_steps(5);
        let (_, after, _) = ctx
            .data(|d| d.get_temp::<(egui::Rect, f32, f32)>(probe))
            .unwrap();
        assert!(after > before, "参数区必须能实际滚动");
        assert!(app
            .borrow()
            .as_ref()
            .unwrap()
            .state
            .awaiting_tool()
            .is_some());
    }

    #[test]
    fn disabled_components_never_register_click_sense() {
        let mut installed = false;
        let mut harness = egui_kittest::Harness::builder().build_ui(|ui| {
            if !installed {
                neo_theme::fonts::install(ui.ctx());
                installed = true;
                ui.ctx().request_repaint();
                return;
            }
            let d = neo_ui::Design::new(neo_theme::Theme::new(
                ThemeMode::Dark,
                1080.0,
                Distance::Standard,
            ));
            let button = neo_ui::Button::new("禁用").enabled(false).show(ui, &d);
            let icon = neo_ui::IconButton::new(neo_ui::Icon::Plus)
                .enabled(false)
                .show_at(ui, &d, egui::pos2(180.0, 80.0));
            let switch = neo_ui::Switch::new(true).enabled(false).show(ui, &d);
            for response in [button, icon, switch] {
                assert!(!response.sense.senses_click());
                assert!(!response.clicked());
            }
        });
        harness.run_steps(3);
    }

    #[test]
    fn appearance_changes_persist_and_reload() {
        isolate_db();
        let (mut harness, app, _) = input_harness("");
        {
            let mut slot = app.borrow_mut();
            let app = slot.as_mut().unwrap();
            app.store = Some(neo_store::Store::open(&super::test_db_path()).unwrap());
            app.state.theme_mode = ThemeMode::Light;
            app.state.distance = Distance::Auditorium;
            app.state.show_reasoning = true;
        }
        harness.run_steps(2);
        {
            let mut slot = app.borrow_mut();
            let app = slot.as_mut().unwrap();
            let store = app.store.as_ref().unwrap();
            assert_eq!(store.setting("theme").unwrap().as_deref(), Some("light"));
            assert_eq!(
                store.setting("distance").unwrap().as_deref(),
                Some("auditorium")
            );
            app.state.theme_mode = ThemeMode::Dark;
            app.state.distance = Distance::Standard;
            app.state.show_reasoning = false;
            app.load_settings();
            assert_eq!(app.state.theme_mode, ThemeMode::Light);
            assert_eq!(app.state.distance, Distance::Auditorium);
            assert!(app.state.show_reasoning);
        }
    }

    #[test]
    fn workspace_picker_result_and_cancel_are_nonblocking() {
        isolate_db();
        let (mut harness, app, _) = input_harness("");
        let (tx, rx) = std::sync::mpsc::channel();
        app.borrow_mut().as_mut().unwrap().workspace_picker = Some(rx);
        harness.run_steps(2); // 未返回结果时仍能绘制。
        let path = std::env::temp_dir().join("neo-workspace-result");
        tx.send(Some(path.clone())).unwrap();
        harness.run_steps(2);
        assert_eq!(
            app.borrow().as_ref().unwrap().state.workspace_dir,
            Some(path.clone())
        );
        let (tx, rx) = std::sync::mpsc::channel();
        app.borrow_mut().as_mut().unwrap().workspace_picker = Some(rx);
        tx.send(None).unwrap();
        harness.run_steps(2);
        let slot = app.borrow();
        let app = slot.as_ref().unwrap();
        assert_eq!(app.state.workspace_dir, Some(path));
        assert!(app.workspace_picker.is_none());
    }

    /// 造一条工具调用分片。
    fn frag(id: &str, name: &str, args: &str) -> neo_llm::ToolCallFrag {
        neo_llm::ToolCallFrag {
            index: 0,
            id: Some(id.to_owned()),
            name: Some(name.to_owned()),
            args: args.to_owned(),
        }
    }

    /// 造一条已出结果的工具消息（用于快照）。
    fn tool_msg(name: &str, args: serde_json::Value, outcome: neo_tools::Outcome) -> ChatMessage {
        let def = neo_tools::find(name);
        let title = def.map(|t| t.title).unwrap_or("工具");
        let risk = def.map(|t| t.risk.as_str()).unwrap_or("?");
        let preview = def
            .map(|t| (t.preview)(&neo_tools::Args::new(t, &args)))
            .unwrap_or_default();
        let content = neo_tools::to_model_message(&outcome);
        ChatMessage::tool_result(
            ToolMeta {
                call_id: format!("call_{name}"),
                name: name.to_owned(),
                title,
                risk,
                preview,
                args,
                state: if outcome.is_ok()
                    || outcome.error.as_ref().map(|e| e.kind)
                        == Some(neo_tools::ErrorKind::NotAllowed)
                {
                    if outcome.is_ok() {
                        ToolState::Done
                    } else {
                        ToolState::Denied
                    }
                } else {
                    ToolState::Done
                },
                outcome: Some(outcome),
            },
            content,
        )
    }

    /// 只读模式下写类工具被策略直接拒绝（不打扰用户），结果照实回灌给模型。
    #[test]
    fn read_only_tool_round_is_denied_without_asking() {
        let mut st = AppState::default();
        st.read_only = true;
        st.messages
            .push(ChatMessage::new(Role::Assistant, "我来改一下这个文件"));
        st.tool_frags = vec![frag(
            "call_1",
            "write_file",
            r#"{"path":"a.txt","content":"hi"}"#,
        )];
        st.tool_round = true;

        assert_eq!(st.begin_tool_round(), 1, "应登记一条工具消息");
        assert!(st.awaiting_tool().is_none(), "只读拒绝不该弹确认框");
        assert!(st.tools_settled(), "策略拒绝也要立刻出结果");

        let msg = st.messages.last().unwrap();
        let tool = msg.tool.as_ref().unwrap();
        assert_eq!(tool.state, ToolState::Denied);
        assert_eq!(
            tool.outcome.as_ref().unwrap().error.as_ref().unwrap().kind,
            neo_tools::ErrorKind::NotAllowed
        );
        // 回灌内容必须是可解析的协议 JSON，且带上"为什么没成"
        let v: serde_json::Value = serde_json::from_str(&msg.content).unwrap();
        assert_eq!(v["ok"], false);
        assert_eq!(v["error"]["kind"], "not_allowed");
        // 助手消息要挂上 tool_calls，否则 role=tool 没有可配对的调用
        assert_eq!(st.messages[0].tool_calls.len(), 1);
    }

    /// 写类工具默认要用户点头；批准后才落盘。
    #[test]
    fn write_tool_asks_then_runs_after_approval() {
        let dir = std::env::temp_dir().join(format!("neo-tool-app-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let scope = neo_tools::Scope::new(&dir);

        let mut st = AppState::default();
        st.classroom_safe = false;
        st.messages
            .push(ChatMessage::new(Role::Assistant, String::new()));
        st.tool_frags = vec![frag(
            "call_9",
            "write_file",
            r#"{"path":"n.txt","content":"你好"}"#,
        )];
        st.tool_round = true;
        st.begin_tool_round();

        assert_eq!(st.awaiting_tool(), Some(1), "写文件必须先问");
        assert!(!st.tools_settled());
        assert!(!dir.join("n.txt").exists(), "没批准之前不该落盘");

        st.approve_tool(1);
        // 执行在后台线程：等它收工再断言。
        assert_eq!(st.spawn_ready_tools(&scope), 1);
        assert!(st.wait_tool_jobs(std::time::Duration::from_secs(10)));

        assert_eq!(std::fs::read_to_string(dir.join("n.txt")).unwrap(), "你好");
        assert!(st.tools_settled());
        let v: serde_json::Value = serde_json::from_str(&st.messages[1].content).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["tool"], "write_file");
    }

    /// 工具执行必须在**另一个线程**：`spawn_ready_tools` 要立刻返回，
    /// 界面才不会因为一个可能要跑几分钟的命令冻住。
    #[test]
    fn tool_execution_does_not_block_the_caller() {
        let dir = std::env::temp_dir().join(format!("neo-tool-async-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let scope = neo_tools::Scope::new(&dir);

        let mut st = AppState::default();
        st.classroom_safe = false;
        st.messages
            .push(ChatMessage::new(Role::Assistant, String::new()));
        st.tool_frags = vec![frag(
            "call_sleep",
            "powershell",
            r#"{"command":"Start-Sleep -Seconds 2"}"#,
        )];
        st.tool_round = true;
        st.begin_tool_round();
        st.approve_tool(1);

        // 命令自己要睡 2 秒：启动必须"立刻"回来，否则就是同步执行。
        let t0 = std::time::Instant::now();
        assert_eq!(st.spawn_ready_tools(&scope), 1);
        let cost = t0.elapsed();
        assert!(
            cost < std::time::Duration::from_millis(400),
            "spawn 花了 {cost:?}，说明它在等命令返回"
        );
        assert!(st.tools_running(), "应当仍在后台跑");
        assert!(!st.tools_settled(), "还没出结果就不该算完成");
        assert!(
            st.messages[1]
                .tool
                .as_ref()
                .unwrap()
                .line()
                .contains("执行中"),
            "卡片要说明还在跑，而不是让用户以为命令没反应"
        );

        // 收工后结果落进消息，并回灌成协议 JSON
        assert!(st.wait_tool_jobs(std::time::Duration::from_secs(20)));
        assert!(st.tools_settled());
        let v: serde_json::Value = serde_json::from_str(&st.messages[1].content).unwrap();
        assert_eq!(v["data"]["exit_code"], 0);
    }

    /// `background: true`：立即返回、带 PID、不管进程死活。
    #[test]
    fn background_command_returns_immediately_with_pid() {
        let dir = std::env::temp_dir().join(format!("neo-tool-bg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let scope = neo_tools::Scope::new(&dir);

        let t0 = std::time::Instant::now();
        let out = neo_tools::dispatch(
            &scope,
            "powershell",
            &serde_json::json!({ "command": "Start-Sleep -Seconds 3", "background": true }),
        );
        assert!(out.is_ok(), "{:?}", out.error.map(|e| e.message));
        assert!(
            t0.elapsed() < std::time::Duration::from_millis(800),
            "后台模式不该等命令结束"
        );
        assert_eq!(out.data["background"], true);
        assert!(out.data["pid"].as_u64().unwrap() > 0, "要给出 PID 才能管它");
        assert_eq!(out.data["timeout_applies"], false, "后台模式超时不适用");
        assert!(out.summary.contains("后台"));
    }

    /// 拒绝也要回灌：模型据此换个做法，而不是干等。
    #[test]
    fn denied_tool_reports_back_to_model() {
        let mut st = AppState::default();
        st.messages
            .push(ChatMessage::new(Role::Assistant, String::new()));
        st.tool_frags = vec![frag(
            "call_2",
            "powershell",
            r#"{"command":"Remove-Item -Recurse -Force build"}"#,
        )];
        st.tool_round = true;
        st.begin_tool_round();
        assert_eq!(st.awaiting_tool(), Some(1));

        st.deny_tool(1);
        assert!(st.awaiting_tool().is_none());
        assert!(st.tools_settled());
        let v: serde_json::Value = serde_json::from_str(&st.messages[1].content).unwrap();
        assert_eq!(v["ok"], false);
        assert_eq!(v["error"]["kind"], "not_allowed");
        assert!(v["error"]["hint"].as_str().unwrap().contains("不要重复"));
    }

    /// 窗口截断不能把工具块劈开（否则服务端会因为缺配对直接 400）。
    #[test]
    fn history_window_keeps_tool_block_intact() {
        let mut st = AppState::default();
        for i in 0..20 {
            st.messages
                .push(ChatMessage::new(Role::User, format!("问 {i}")));
            st.messages
                .push(ChatMessage::new(Role::Assistant, format!("答 {i}")));
        }
        let mut asker = ChatMessage::new(Role::Assistant, "我查一下");
        asker.tool_calls = vec![neo_llm::ToolCall {
            id: "call_read_file".into(),
            name: "read_file".into(),
            arguments: r#"{"path":"a.txt"}"#.into(),
        }];
        st.messages.push(asker);
        st.messages.push(tool_msg(
            "read_file",
            serde_json::json!({ "path": "a.txt" }),
            neo_tools::Outcome::ok(
                "read_file",
                "读取 a.txt（1 行）",
                serde_json::json!({ "content": "x" }),
            ),
        ));

        let msgs = st.api_messages(2);
        let roles: Vec<&str> = msgs.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(roles[0], "system");
        // 末尾的 assistant(tool_calls) + tool 必须成对出现
        assert_eq!(roles[roles.len() - 2], "assistant");
        assert_eq!(roles[roles.len() - 1], "tool");
        assert_eq!(
            msgs[msgs.len() - 1].tool_call_id.as_deref(),
            Some("call_read_file")
        );
    }

    /// 工具卡片（成功 / 失败 / 待确认三态）与权限确认弹窗的快照。
    #[test]
    fn tool_cards_1080p() {
        let p = shoot("14-tool-cards-1080p", Vec2::new(1920.0, 1080.0), |app| {
            app.state.theme_mode = ThemeMode::Dark;
            app.state.distance = Distance::Classroom;
            app.state.workspace = Some(".".to_owned());
            // shoot 现在从空态开始（不恢复上次会话），所以进入对话态要显式写。
            app.state.stage = Stage::Conversation;
            app.state.messages.push(ChatMessage::new(
                Role::User,
                "看一下 workspace 里的说明文件，顺便编译一下",
            ));
            app.state
                .messages
                .push(ChatMessage::new(Role::Assistant, "好，我先读文件。"));
            app.state.messages.push(tool_msg(
                "read_file",
                serde_json::json!({ "path": "README.md" }),
                neo_tools::Outcome::ok(
                    "read_file",
                    "读取 README.md（12 行）",
                    serde_json::json!({ "lines_total": 12, "content": "…" }),
                ),
            ));
            app.state.messages.push(tool_msg(
                "edit_file",
                serde_json::json!({ "path": "src/lib.rs", "old_string": "let x = 1;" }),
                neo_tools::Outcome::fail(
                    "edit_file",
                    neo_tools::ToolError::new(
                        neo_tools::ErrorKind::NotUnique,
                        "`old_string` 在 src/lib.rs 里命中 3 处（第 [12, 40, 88] 行），无法确定改哪一处",
                    )
                    .with_hint("多带几行上下文让旧文本唯一；确定要全部替换时把 `replace_all` 设为 true"),
                ),
            ));
            app.state.messages.push(tool_msg(
                "powershell",
                serde_json::json!({ "command": "cargo test" }),
                neo_tools::Outcome::fail(
                    "powershell",
                    neo_tools::ToolError::not_allowed(
                        "当前处于「只读」模式：写文件与执行命令已被禁用",
                    ),
                ),
            ));
            // 非零退出码：上游那一枚红色胶囊，也是教室里最常见的失败。
            app.state.messages.push(tool_msg(
                "powershell",
                serde_json::json!({ "command": "cargo test" }),
                neo_tools::Outcome::ok(
                    "powershell",
                    "`cargo test` 退出码 1（2310 ms）",
                    serde_json::json!({ "exit_code": 1, "duration_ms": 2310, "stderr": "1 failed" }),
                ),
            ));
            // 屏幕交互自成一类（`Variant::Screen` → 标题「屏幕」）：
            // 它既不是读文件也不是跑命令，学生要能一眼区分。
            app.state.messages.push(tool_msg(
                "screenshot",
                serde_json::json!({ "x": 0, "y": 0, "width": 1280, "height": 720 }),
                neo_tools::Outcome::ok(
                    "screenshot",
                    "截屏 1280×720 已保存到 screenshots/shot-1.png（1.2 MB），已把图交给模型",
                    serde_json::json!({
                        "path": "screenshots/shot-1.png",
                        "region": { "x": 0, "y": 0, "width": 1280, "height": 720 },
                        "image_attached": true,
                    }),
                ),
            ));
            // 类 Unix 环境的那一份：同样归到 `Bash` 变体，标题一致、摘要不同 ——
            // 学生扫一眼就能看出"这次是在用 Unix 工具集"。
            app.state.messages.push(tool_msg(
                "bash",
                serde_json::json!({ "command": "grep -rn 'TODO' crates/ | wc -l" }),
                neo_tools::Outcome::ok(
                    "bash",
                    "`grep -rn 'TODO' crates/ | wc -l` 执行成功（86 ms）",
                    serde_json::json!({
                        "exit_code": 0, "duration_ms": 86, "stdout": "3\n", "host": "gitbash-bundled"
                    }),
                ),
            ));
        });
        assert!(p.is_file());
    }

    /// 权限确认弹窗的快照 —— 用户点头前看到的那一屏。
    #[test]
    fn tool_confirm_1080p() {
        let p = shoot("15-tool-confirm-1080p", Vec2::new(1920.0, 1080.0), |app| {
            app.state.theme_mode = ThemeMode::Dark;
            app.state.distance = Distance::Classroom;
            app.state.stage = Stage::Conversation;
            app.state
                .messages
                .push(ChatMessage::new(Role::User, "把构建产物清掉再重新编译"));
            let mut asker = ChatMessage::new(Role::Assistant, String::new());
            asker.tool_calls = vec![neo_llm::ToolCall {
                id: "call_ps".into(),
                name: "powershell".into(),
                arguments: r#"{"command":"Remove-Item -Recurse -Force build; cargo build --release","cwd":"."}"#.into(),
            }];
            app.state.messages.push(asker);
            app.state.tool_frags = vec![frag(
                "call_ps",
                "powershell",
                r#"{"command":"Remove-Item -Recurse -Force build; cargo build --release","cwd":"."}"#,
            )];
            app.state.classroom_safe = false;
            app.state.tool_round = true;
            app.state.begin_tool_round();
        });
        assert!(p.is_file());
    }

    /// 小窗完成态：只留工具流水 + 正文（markdown/LaTeX），高度自适应最后一段。
    #[test]
    fn miniwin_done_1080p() {
        let p = shoot_steps(
            "30-miniwin-done-1080p",
            Vec2::new(1920.0, 1080.0),
            8,
            |app| {
                app.hidden_to_tray = true;
                app.state.theme_mode = ThemeMode::Dark;
                app.state.distance = Distance::Classroom;
                app.state
                    .messages
                    .push(ChatMessage::new(Role::User, "总结一下楞次定律"));
                let mut reply = ChatMessage::new(
                Role::Assistant,
                "先回顾磁通量的定义与变化方式。\n\n**结论**：感应电流的效果总是阻碍磁通量的变化，\
                 即 $E = -\\frac{d\\Phi}{dt}$；判断方向用右手定则。"
                    .to_owned(),
            );
                reply.streaming = false;
                app.state.messages.push(reply);
                // 一条已完成的工具记录：覆盖工具流水行。
                let meta = crate::state::ToolMeta {
                    call_id: "call_rf".into(),
                    name: "read_file".into(),
                    title: "查看文件",
                    risk: "read",
                    preview: "查看 板书设计.md".into(),
                    args: serde_json::json!({"path": "板书设计.md"}),
                    state: crate::state::ToolState::Done,
                    outcome: Some(neo_tools::Outcome::ok(
                        "read_file",
                        "已读取 板书设计.md",
                        serde_json::json!({"bytes": 512}),
                    )),
                };
                app.state
                    .messages
                    .push(crate::state::ChatMessage::tool_result(meta, String::new()));
                // 首帧忙（驱动驻留排程），tick 内泵完即闲 → 进入完成驻留态。
                app.state.generating = true;
            },
        );
        assert!(p.is_file());
    }

    /// 长参数与多行预览不应把操作按钮挤出卡片。
    #[test]
    fn tool_confirm_long_args_1080p() {
        let p = shoot(
            "16-tool-confirm-long-1080p",
            Vec2::new(1920.0, 1080.0),
            |app| {
                app.state.theme_mode = ThemeMode::Dark;
                app.state.distance = Distance::Classroom;
                app.state.stage = Stage::Conversation;
                let mut message = ChatMessage::new(Role::Assistant, String::new());
                message.tool = Some(ToolMeta {
                call_id: "long-confirm".into(),
                name: "powershell".into(),
                title: "PowerShell 命令",
                risk: "exec",
                preview: "将执行一条较长的 PowerShell 命令；请仔细核对参数与工作目录，然后再决定是否允许。这个描述会自动换行。".repeat(3),
                args: serde_json::json!({"command": "Get-ChildItem -Recurse -File | Where-Object { $_.Length -gt 1000000 } | Select-Object FullName, Length".repeat(8), "cwd": "D:/projects/test", "timeout": 500}),
                state: ToolState::AwaitingConfirm,
                outcome: None,
            });
                app.state.messages.push(message);
            },
        );
        assert!(p.is_file());
    }

    #[test]
    fn tool_confirm_scroll_light_narrow() {
        let p = shoot(
            "18-tool-confirm-scroll-light",
            Vec2::new(960.0, 720.0),
            |app| {
                app.state.theme_mode = ThemeMode::Light;
                app.state.distance = Distance::Classroom;
                app.state.stage = Stage::Conversation;
                let mut message = ChatMessage::new(Role::Assistant, String::new());
                message.tool = Some(ToolMeta {
                    call_id: "scroll-confirm".into(),
                    name: "write_file".into(),
                    title: "写入文件",
                    risk: "write",
                    preview: "写入教学示例文件；请滚动核对完整参数。".into(),
                    args: serde_json::json!({"path": "lesson.txt", "content": "这是一段需要完整核对而不是直接截掉的长文本。".repeat(300)}),
                    state: ToolState::AwaitingConfirm,
                    outcome: None,
                });
                app.state.messages.push(message);
            },
        );
        assert!(p.is_file());
    }

    /// 窄屏两列场景卡必须各占完整行高。
    #[test]
    fn hero_two_columns_portrait() {
        let p = shoot(
            "17-hero-two-columns-portrait",
            Vec2::new(960.0, 1080.0),
            |app| {
                app.state.theme_mode = ThemeMode::Dark;
                app.state.distance = Distance::Standard;
            },
        );
        assert!(p.is_file());
    }

    /// 设置面板（侧边导航正式版）逐页渲染冒烟：五个页签各铺三帧，
    /// 任何一页把面板画炸（布局溢出、组件 id 冲突、断言触发）都会在这里炸出来。
    ///
    /// 前身是「分段控件 Id 唯一性」回归 —— 那条线上 bug 的两个控件
    /// （页签分段与「显示思考过程」分段）已随正式版改版分别被导航项与开关取代，
    /// 同页撞 Id 的场景不存在了，测试职责随之改为逐页冒烟。
    #[test]
    fn settings_panel_renders_every_tab() {
        for &(tab, name) in crate::state::SettingsTab::ALL {
            let app = Rc::new(RefCell::new(None::<NeoApp>));
            let app2 = Rc::clone(&app);
            let mut harness = egui_kittest::Harness::builder()
                .with_size(Vec2::new(1920.0, 1080.0))
                .wgpu()
                .build_ui(move |ui| {
                    let mut slot = app2.borrow_mut();
                    let Some(a) = slot.as_mut() else {
                        fresh_db();
                        let mut fresh = NeoApp::install(ui.ctx());
                        fresh.state.show_settings = true;
                        fresh.state.settings_tab = tab;
                        *slot = Some(fresh);
                        ui.ctx().request_repaint();
                        return;
                    };
                    a.render(ui);
                });
            harness.run_steps(3);
            // 显式收尾：释放 app（含数据库连接），下一个页签重新铺。
            drop(harness);
            let _ = name;
        }
    }

    #[test]
    fn hero_dark_1080p() {
        let p = shoot("01-hero-dark-1080p", Vec2::new(1920.0, 1080.0), |app| {
            app.state.theme_mode = ThemeMode::Dark;
            app.state.distance = Distance::Classroom;
        });
        assert!(p.is_file());
    }

    #[test]
    fn hero_light_1080p() {
        let p = shoot("02-hero-light-1080p", Vec2::new(1920.0, 1080.0), |app| {
            app.state.theme_mode = ThemeMode::Light;
            app.state.distance = Distance::Classroom;
        });
        assert!(p.is_file());
    }

    #[test]
    fn hero_dark_4k_far() {
        let p = shoot("03-hero-dark-4k-far", Vec2::new(3840.0, 2160.0), |app| {
            app.state.theme_mode = ThemeMode::Dark;
            app.state.distance = Distance::Auditorium;
        });
        assert!(p.is_file());
    }

    #[test]
    fn conversation_dark_1080p() {
        let p = shoot(
            "04-conversation-dark-1080p",
            Vec2::new(1920.0, 1080.0),
            |app| {
                app.state.theme_mode = ThemeMode::Dark;
                app.state.distance = Distance::Classroom;
                seed_dialogue(app);
            },
        );
        assert!(p.is_file());
    }

    #[test]
    fn conversation_light_1080p() {
        let p = shoot(
            "05-conversation-light-1080p",
            Vec2::new(1920.0, 1080.0),
            |app| {
                app.state.theme_mode = ThemeMode::Light;
                app.state.distance = Distance::Classroom;
                seed_dialogue(app);
            },
        );
        assert!(p.is_file());
    }

    /// 生成态：脉动点 + 发送位变成停止按钮。
    #[test]
    fn generating_1080p() {
        let p = shoot_steps("08-generating-1080p", Vec2::new(1920.0, 1080.0), 5, |app| {
            app.state.theme_mode = ThemeMode::Dark;
            app.state.distance = Distance::Classroom;
            app.state.draft = "帮我出一道例题".to_owned();
            app.state.submit();
            let demo = crate::state::demo_reply("例题", app.state.model_display());
            app.state.start_generation(StreamSource::Demo {
                text: demo,
                cursor: 0,
            });
        });
        assert!(p.is_file());
    }

    /// 思考过程（`show_reasoning`）的换行与缩进检查。
    ///
    /// 这块一直没有快照 —— 于是"换行位置不对"这种问题只能等用户肉眼发现。
    #[test]
    fn reasoning_wrap_1080p() {
        let p = shoot("17-reasoning-1080p", Vec2::new(1920.0, 1080.0), |app| {
            app.state.theme_mode = ThemeMode::Dark;
            app.state.distance = Distance::Classroom;
            app.state.show_reasoning = true;
            app.state.draft = "讲讲楞次定律".to_owned();
            app.state.submit();
            let text = "先看题目的已知条件。导体棒在磁场中运动，回路面积变化，\
                磁通量随之变化。\n\
                根据楞次定律，感应电流的效果总要阻碍引起它的原因，\
                所以要先判断磁通量是增加还是减少，再用右手定则确定方向。";
            app.state.start_generation(StreamSource::Demo {
                text: text.to_owned(),
                cursor: 0,
            });
            while app.state.pump() {}
            app.state.messages[1].reasoning = "第一步：判断磁场方向与回路面积的变化趋势，\
                注意导体棒的有效长度是它在垂直磁场方向上的投影。第二步：用楞次定律定方向，\
                再用安培定则定电流方向。两者不要混用。"
                .to_owned();
            app.state.messages[1].content = "感应电流的方向总是**阻碍**磁通量的变化。".to_owned();
        });
        assert!(p.is_file());
    }

    /// 设置 → 模型页：接口地址 / 密钥 / 当前模型 / 可选模型（含刷新键）。
    #[test]
    fn settings_model_1080p() {
        let p = shoot(
            "18-settings-model-1080p",
            Vec2::new(1920.0, 1080.0),
            |app| {
                app.state.theme_mode = ThemeMode::Dark;
                app.state.distance = Distance::Classroom;
                app.state.show_settings = true;
                app.state.settings_tab = crate::state::SettingsTab::Model;
                // 模拟"已从模型商拉到列表"的样子（真拉取要网络，快照里不连）。
                app.state.set_models_from_provider(vec![
                    "deepseek-chat".to_owned(),
                    "deepseek-reasoner".to_owned(),
                    "deepseek-coder".to_owned(),
                ]);
            },
        );
        assert!(p.is_file());
    }

    /// 公式渲染检查：行内 `$…$`、行间 `$$…$$`、上下标、分数、根号、大运算符。
    #[test]
    fn math_render_1080p() {
        let p = shoot("16-math-1080p", Vec2::new(1920.0, 1080.0), |app| {
            app.state.theme_mode = ThemeMode::Dark;
            app.state.distance = Distance::Classroom;
            app.state.draft = "推导一下".to_owned();
            app.state.submit();
            // 公式里全是反斜杠，用原始字符串装 LaTeX，换行仍用普通字符串，
            // 免得陷入"到底要几个反斜杠"的泥潭。
            let rich = concat!(
                "## 匀变速直线运动\n\n",
                "位移与时间的关系是 $x = v_0 t + ",
                r"\frac",
                "{1}{2} a t^2$，两边对 $t$ 求导就得到 $v = v_0 + a t$。\n\n",
                "$$",
                r"\bar{v} = \frac{v_0 + v}{2} = \frac{x}{t}",
                "$$\n\n",
                "当加速度恒定时，$",
                r"\Delta x = aT^2",
                "$（逐差法）。\n\n",
                "由牛顿第二定律 $F = ma$ 可知，质量越大惯性越强：$m = ",
                r"\frac{F}{a}",
                "$。",
            );
            app.state.start_generation(StreamSource::Demo {
                text: rich.to_owned(),
                cursor: 0,
            });
            while app.state.pump() {}
            app.state.messages[1].meta = "DeepSeek-V3.2 · 0.8s".to_owned();
        });
        assert!(p.is_file());
    }

    /// 有序列表的**序号**渲染：递增、非 1 起点、两位数对齐、嵌套。
    ///
    /// 曾经每一项都渲染成「1.」—— 这张快照就是给这类回归看的。
    #[test]
    fn ordered_list_numbering_1080p() {
        let p = shoot(
            "19-list-numbering-1080p",
            Vec2::new(1920.0, 1080.0),
            |app| {
                app.state.theme_mode = ThemeMode::Dark;
                app.state.distance = Distance::Classroom;
                app.state.draft = "解题步骤".to_owned();
                app.state.submit();
                let rich = "### 解题步骤\n\n\
                1. 明确研究对象\n\
                2. 受力分析\n\
                3. 列出牛顿第二定律\n\
                4. 解出加速度\n\
                5. 判断方向\n\
                6. 检查单位\n\
                7. 代入数据\n\
                8. 求位移\n\
                9. 核对量级\n\
                10. 写出答案\n\
                11. 标注条件\n\n\
                从第 3 步开始数：\n\n\
                3. 甲\n\
                4. 乙\n\
                5. 丙\n\n\
                嵌套：\n\n\
                1. 甲\n   1. 甲一\n   1. 甲二\n2. 乙";
                app.state.start_generation(StreamSource::Demo {
                    text: rich.to_owned(),
                    cursor: 0,
                });
                while app.state.pump() {}
                app.state.messages[1].meta = "DeepSeek-V3.2 · 0.8s".to_owned();
            },
        );
        assert!(p.is_file());
    }

    /// Markdown 渲染检查：标题 / 列表 / 代码块 / 引用 / 行内样式。
    #[test]
    fn markdown_richness_1080p() {
        let p = shoot("10-markdown-1080p", Vec2::new(1920.0, 1080.0), |app| {
            app.state.theme_mode = ThemeMode::Dark;
            app.state.distance = Distance::Classroom;
            app.state.draft = "讲讲楞次定律".to_owned();
            app.state.submit();
            let rich = "## 楞次定律\n\n\
                感应电流的效果，总是**阻碍**引起它的磁通量变化。\n\n\
                - `增反减同`：磁通量增加时反向\n\
                - 来拒去留：相对运动时阻碍\n\n\
                1. 判断原磁场方向\n\
                2. 判断磁通量增减\n\
                3. 用右手定则定感应磁场\n\n\
                > 1885 年由楞次提出。\n\n\
                ```rust\n\
                fn main() {\n    println!(\"hello\");\n}\n\
                ```\n\n\
                更多内容请看 `docs/design-spec.md`。";
            app.state.start_generation(StreamSource::Demo {
                text: rich.to_owned(),
                cursor: 0,
            });
            while app.state.pump() {}
            app.state.messages[1].meta = "DeepSeek-V3.2 · 0.8s".to_owned();
        });
        assert!(p.is_file());
    }

    #[test]
    fn commonmark_conversation_regression_snapshot() {
        let path = shoot_steps(
            "25-commonmark-conversation",
            Vec2::new(1920.0, 1080.0),
            5,
            |app| {
                app.store = None;
                app.state.theme_mode = ThemeMode::Dark;
                app.state.distance = Distance::Classroom;
                app.state.draft = "整理这些文件的内容，保留表格和公式。".to_owned();
                app.state.submit();
                let text = "## 目录与文件清单\n\n| 文件 | 类型 | 大小 | 说明 |\n| :--- | :---: | ---: | :--- |\n| `LCC_cleaned/` | 文件夹 | 空 | 目录中暂无文件 |\n| 屏幕截图 132019.png | 图片 | 1.1 MB | 目标检测结果 |\n| 演示脚本.docx | Word | 18 KB | 项目演示脚本 |\n| 课程讲义.pptx | PPT | 2.4 MB | 按幻灯片顺序读取 |\n\n## 内容概览\n\n1. **检测结果**：两瓶饮料，保留类别与置信度。\n2. **课件与脚本**：直接调用文档读取工具。\n   - Word 正文和表格\n   - PPT 按页提取文字\n3. **公式示例**：$E=mc^2$，与正文正常排版。\n\n> 读取结果带分页信息；长文档按 `next_offset` 继续。";
                app.state
                    .messages
                    .push(crate::state::ChatMessage::new(Role::Assistant, text));
                app.state.messages.last_mut().unwrap().meta = "Markdown 与文档工具回归".to_owned();
            },
        );
        assert!(path.is_file());
    }

    /// 设置面板：外观页签。
    #[test]
    fn settings_appearance_1080p() {
        let p = shoot(
            "11-settings-appearance-1080p",
            Vec2::new(1920.0, 1080.0),
            |app| {
                app.state.theme_mode = ThemeMode::Dark;
                app.state.distance = Distance::Classroom;
                seed_dialogue(app);
                app.state.show_settings = true;
                // 这个用例叫 appearance，就该开外观页 —— 早先这里开的是模型页，
                // 于是「外观」快照里全是模型设置（同 hero 快照那次的毛病）。
                app.state.settings_tab = crate::state::SettingsTab::Appearance;
            },
        );
        assert!(p.is_file());
    }

    /// 组件库陈列室：把 neo-ui 的全部控件渲到一张图上，
    /// 作为设计接口的"一页速览"与回归基准（docs/design-kit.md 的配图）。
    #[test]
    fn design_kit_gallery_1080p() {
        isolate_db();
        let app: RefCell<Option<NeoApp>> = RefCell::new(None);

        let mut harness = egui_kittest::Harness::builder()
            .with_size(Vec2::new(1920.0, 1200.0))
            .wgpu()
            .build_ui(|ui| {
                let mut slot = app.borrow_mut();
                if slot.is_none() {
                    // 首帧：装配字体（画廊只依赖全局字体与主题，不需要 app 状态）。
                    fresh_db();
                    let fresh = NeoApp::install(ui.ctx());
                    *slot = Some(fresh);
                    ui.ctx().request_repaint();
                    return;
                }
                design_kit_gallery(ui);
            });

        harness.run_steps(3);
        let image = harness
            .render()
            .unwrap_or_else(|e| panic!("离屏渲染 design-kit 失败: {e}"));
        let path = out_dir().join("13-design-kit-1080p.png");
        image.save(&path).expect("PNG 落盘失败");
        println!("[snapshot] → {}", path.display());
        assert!(path.is_file());
    }

    /// 会话管理：常规行的恒显动作按钮 + 一行删除确认态。
    #[test]
    fn session_actions_1080p() {
        let p = shoot(
            "12-session-actions-1080p",
            Vec2::new(1920.0, 1080.0),
            |app| {
                app.state.theme_mode = ThemeMode::Dark;
                app.state.distance = Distance::Classroom;
                let seed = |app: &mut NeoApp, title: &str| -> Option<i64> {
                    let store = app.store.as_ref()?;
                    let id = store.create_session(title).ok()?;
                    let _ = store.append_message(id, "user", "第一条消息", "", "");
                    NeoApp::refresh_sessions(&mut app.state, store);
                    Some(id)
                };
                let first = seed(app, "楞次定律讲解");
                let second = seed(app, "随堂测验出题");
                // 第二个是当前会话；对第一个发起删除确认。
                app.state.active_session = second;
                app.state.confirming_delete = first;
            },
        );
        assert!(p.is_file());
    }

    #[test]
    fn display_panel_1080p() {
        let p = shoot("06-display-panel-1080p", Vec2::new(1920.0, 1080.0), |app| {
            app.state.theme_mode = ThemeMode::Dark;
            app.state.distance = Distance::Classroom;
            app.state.show_settings = true;
            app.state.settings_tab = crate::state::SettingsTab::Display;
        });
        assert!(p.is_file());
    }

    #[test]
    fn hero_dark_1080p_near() {
        // 近距档：验证 1x 基准值下命中区扩边仍在生效（视觉尺寸保持上游比例）。
        let p = shoot(
            "07-hero-dark-1080p-near",
            Vec2::new(1920.0, 1080.0),
            |app| {
                app.state.theme_mode = ThemeMode::Dark;
                app.state.distance = Distance::Standard;
            },
        );
        assert!(p.is_file());
    }

    #[test]
    fn readonly_notice_1080p() {
        let p = shoot(
            "09-readonly-notice-1080p",
            Vec2::new(1920.0, 1080.0),
            |app| {
                app.state.theme_mode = ThemeMode::Dark;
                app.state.distance = Distance::Classroom;
                app.state.read_only = true;
            },
        );
        assert!(p.is_file());
    }

    #[test]
    fn hero_is_the_default_stage() {
        // 模型列表默认为空：只由模型商提供，见 `model_list_tests`
        assert!(AppState::default().models.is_empty());
        let app_state = AppState::default();
        assert_eq!(app_state.stage, Stage::Hero);
    }

    // ------------------------------------------------------------------
    // 设计接口陈列室（docs/design-kit.md 的配图）
    // ------------------------------------------------------------------

    /// 把 neo-ui 的控件渲满一屏：按钮族 / 图标按钮 / Chip / 分段 / 表单 /
    /// 徽标 / 反馈 / 卡片 / 列表行 / 迷你对话框。
    ///
    /// 左列 = 动作与输入，右列 = 容器与反馈；所有色值与度量都来自
    /// [`neo_theme`]，本函数不出现一个裸颜色 —— 它本身就是"只用设计接口
    /// 能拼出什么"的证明。
    fn design_kit_gallery(ui: &mut egui::Ui) {
        use neo_ui as nui;

        let theme = neo_theme::Theme::new(ThemeMode::Dark, 1080.0, neo_theme::Distance::Standard);
        let d = nui::Design::new(theme);
        let painter = ui.painter().clone();
        let screen = ui.max_rect();
        painter.rect_filled(screen, 0.0, d.p().bg_base);

        // 标题。
        painter.text(
            egui::pos2(screen.left() + 32.0, 44.0),
            egui::Align2::LEFT_CENTER,
            "neo-ui · 设计接口陈列室",
            d.font_bold(d.t().headline),
            d.p().label_primary,
        );
        painter.text(
            egui::pos2(screen.right() - 32.0, 44.0),
            egui::Align2::RIGHT_CENTER,
            "暗色 · 教室档 · 1080p",
            d.font_mono(d.t().caption),
            d.p().label_caption,
        );

        let lx = 32.0; // 左列起点
        let rx = 976.0; // 右列起点
        let w = 912.0; // 列宽
        let mut ly = 104.0;
        let mut ry = 104.0;

        // ---- 左列 1：按钮族 ----
        ly = section(&painter, &d, lx, ly, w, "按钮 · Button");
        let slot = w / 5.0;
        type BtnFx = fn(nui::Button<'static>) -> nui::Button<'static>;
        let variants: [(&str, BtnFx); 5] = [
            ("主操作", |b| b.primary()),
            ("次级", |b| b.elevated()),
            ("幽灵", |b| b.ghost()),
            ("危险", |b| b.danger()),
            ("反色", |b| b.contrast()),
        ];
        for (i, (label, mk)) in variants.into_iter().enumerate() {
            let r = egui::Rect::from_min_size(
                egui::pos2(lx + slot * i as f32, ly),
                Vec2::new(slot, 44.0),
            );
            nui::at(ui, r, |ui| {
                mk(nui::Button::new(label)).show(ui, &d);
            });
        }
        ly += 52.0;
        // 尺寸 / 禁用 / 加载。
        let states: [(&str, BtnFx); 5] = [
            ("小号", |b| b.primary().small()),
            ("中号", |b| b.primary()),
            ("大号", |b| b.primary().large()),
            ("禁用", |b| b.primary().enabled(false)),
            ("加载中", |b| b.primary().loading(true)),
        ];
        for (i, (label, mk)) in states.into_iter().enumerate() {
            let r = egui::Rect::from_min_size(
                egui::pos2(lx + slot * i as f32, ly),
                Vec2::new(slot, 48.0),
            );
            nui::at(ui, r, |ui| {
                mk(nui::Button::new(label))
                    .id_salt(("gk-btn2", i))
                    .show(ui, &d);
            });
        }
        ly += 64.0;

        // ---- 左列 2：图标按钮 ----
        ly = section(&painter, &d, lx, ly, w, "图标按钮 · IconButton");
        type IBtnFx = fn(nui::IconButton) -> nui::IconButton;
        let ibtns: [(nui::Icon, IBtnFx); 8] = [
            (nui::Icon::Plus, |b| b.ghost()),
            (nui::Icon::Cog, |b| b.elevated()),
            (nui::Icon::Folder, |b| b.floating()),
            (nui::Icon::Trash, |b| b.danger()),
            (nui::Icon::ArrowUp, |b| b.accent()),
            (nui::Icon::Checklist, |b| b.subtle()),
            // 主题图标成对出现：新月是双圆相减（尖角对齐交点），太阳是圆环 + 8 道光。
            (nui::Icon::Moon, |b| b.elevated()),
            (nui::Icon::Sun, |b| b.elevated()),
        ];
        for (i, (icon, mk)) in ibtns.into_iter().enumerate() {
            mk(nui::IconButton::new(icon))
                .id_salt(("gk-ibtn", i))
                .show_at(ui, &d, egui::pos2(lx + 26.0 + 52.0 * i as f32, ly + 18.0));
        }
        ly += 56.0;

        // ---- 左列 3：Chip 与分段 ----
        ly = section(&painter, &d, lx, ly, w, "胶囊与分段 · Chip / Segmented");
        let mut cx = lx;
        for (label, chevron, active) in [
            ("Plan", false, true),
            ("只读", false, false),
            ("DeepSeek-R1", true, false),
        ] {
            let cw = nui::Chip::width(&painter, &d, label, chevron);
            let r = egui::Rect::from_min_size(egui::pos2(cx, ly), Vec2::new(cw, d.m().chip_h()));
            nui::Chip::new(label)
                .chevron(chevron)
                .active(active)
                .id_salt(("gk-chip", label))
                .show_at(ui, &d, r);
            cx += cw + 12.0;
        }
        ly += d.m().chip_h() + 10.0;
        let seg = egui::Rect::from_min_size(egui::pos2(lx, ly), Vec2::new(360.0, 40.0));
        nui::at(ui, seg, |ui| {
            nui::Segmented::new(&["近距", "教室", "远距"], 1).show(ui, &d, 360.0);
        });
        ly += 56.0;

        // ---- 左列 4：表单 ----
        ly = section(
            &painter,
            &d,
            lx,
            ly,
            w,
            "表单 · TextField / Switch / FieldRow",
        );
        let mut draft = String::new();
        let field = egui::Rect::from_min_size(egui::pos2(lx, ly), Vec2::new(420.0, 40.0));
        nui::at(ui, field, |ui| {
            nui::TextField::new(&mut draft)
                .hint("输入课堂问题…")
                .id_salt("gk-field")
                .show(ui, &d, 420.0);
        });
        let mut secret = "sk-...".to_owned();
        let field2 = egui::Rect::from_min_size(egui::pos2(lx + 440.0, ly), Vec2::new(300.0, 40.0));
        nui::at(ui, field2, |ui| {
            nui::TextField::new(&mut secret)
                .secret(true)
                .id_salt("gk-secret")
                .show(ui, &d, 300.0);
        });
        ly += 52.0;
        // 开关。
        let sw = egui::Rect::from_min_size(egui::pos2(lx, ly), nui::Switch::size(&d));
        nui::Switch::new(true).id_salt("gk-sw1").show_at(ui, &d, sw);
        let sw2 = egui::Rect::from_min_size(egui::pos2(lx + 64.0, ly), nui::Switch::size(&d));
        nui::Switch::new(false)
            .id_salt("gk-sw2")
            .show_at(ui, &d, sw2);
        ly += 40.0;
        // 键值行。
        let row1 = egui::Rect::from_min_size(egui::pos2(lx, ly), Vec2::new(w, 24.0));
        nui::at(ui, row1, |ui| {
            nui::FieldRow::new("最终倍率", "1.28 ×").show(ui, &d, w);
        });
        ly += 26.0;
        let row2 = egui::Rect::from_min_size(egui::pos2(lx, ly), Vec2::new(w, 24.0));
        nui::at(ui, row2, |ui| {
            nui::FieldRow::new("数据库", r"%APPDATA%\Neo\neo.db").show(ui, &d, w);
        });
        ly += 46.0;

        // ---- 左列 5：反馈 ----
        ly = section(&painter, &d, lx, ly, w, "反馈 · Badge / Spinner");
        let mut bx = lx;
        for (text, tone) in [
            ("v0.2.0", nui::BadgeTone::Neutral),
            ("R1", nui::BadgeTone::Accent),
            ("已保存", nui::BadgeTone::Success),
            ("低电量", nui::BadgeTone::Warn),
            ("连接失败", nui::BadgeTone::Danger),
        ] {
            let b = nui::Badge::new(text).tone(tone);
            let bw = b.width(ui, &d);
            b.show_at(
                ui,
                &d,
                egui::Rect::from_min_size(egui::pos2(bx, ly + 2.0), Vec2::new(bw, 20.0)),
            );
            bx += bw + 10.0;
        }
        nui::Spinner::new().show_painter(
            &painter,
            &d,
            egui::Rect::from_min_size(egui::pos2(bx + 12.0, ly - 4.0), Vec2::splat(28.0)),
            d.p().label_tertiary,
        );
        ly += 44.0;

        // ---- 右列 2：列表行 ----
        ry = section(&painter, &d, rx, ry, w, "列表 · NavItem / ListRow / 确认条");
        let lw = 420.0;
        let nav =
            egui::Rect::from_min_size(egui::pos2(rx, ry), Vec2::new(lw, nui::NavItem::height(&d)));
        nui::at(ui, nav, |ui| {
            nui::NavItem::new("新对话", nui::Icon::Plus)
                .id_salt("gk-nav")
                .show(ui, &d, lw);
        });
        ry += nui::NavItem::height(&d) + 8.0;
        let row_h = nui::ListRow::height(&d);
        let r1 = egui::Rect::from_min_size(egui::pos2(rx, ry), Vec2::new(lw, row_h));
        nui::at(ui, r1, |ui| {
            nui::ListRow::new(1, "楞次定律讲解", "14:32")
                .active(true)
                .show_normal(ui, &d, lw);
        });
        ry += row_h + 8.0;
        let r2 = egui::Rect::from_min_size(egui::pos2(rx, ry), Vec2::new(lw, row_h));
        nui::at(ui, r2, |ui| {
            nui::ListRow::new(2, "随堂测验出题", "13:58").show_normal(ui, &d, lw);
        });
        ry += row_h + 8.0;
        let r3 = egui::Rect::from_min_size(egui::pos2(rx, ry), Vec2::new(lw, row_h));
        nui::list::confirm_row(ui, &d, r3, 3, &nui::list::ConfirmBar::new("删除这条会话？"));
        ry += row_h + 24.0;

        // ---- 右列 3：迷你对话框（Panel + 按钮的组合，即 Modal 的构成）----
        ry = section(
            &painter,
            &d,
            rx,
            ry,
            w,
            "对话框 · Modal 的构成（Panel + Button）",
        );
        let dlg_w = 420.0;
        let dlg_h = 148.0;
        let dlg = egui::Rect::from_min_size(egui::pos2(rx, ry), Vec2::new(dlg_w, dlg_h));
        let body = nui::Panel::new().paint(ui, &d, dlg, 20.0);
        painter.text(
            egui::pos2(body.left(), body.top() + 10.0),
            egui::Align2::LEFT_CENTER,
            "删除会话",
            d.font_bold(d.t().label + 2.0),
            d.p().label_primary,
        );
        painter.text(
            egui::pos2(body.left(), body.top() + 38.0),
            egui::Align2::LEFT_CENTER,
            "「楞次定律讲解」及其消息将被移除。",
            d.font(d.t().body),
            d.p().label_secondary,
        );
        let btn_y = dlg.bottom() - 56.0;
        nui::at(
            ui,
            egui::Rect::from_min_size(
                egui::pos2(dlg.right() - 96.0 - 12.0 - 88.0, btn_y),
                Vec2::new(88.0, 36.0),
            ),
            |ui| {
                nui::Button::new("取消").ghost().full_width().show(ui, &d);
            },
        );
        nui::at(
            ui,
            egui::Rect::from_min_size(egui::pos2(dlg.right() - 96.0, btn_y), Vec2::new(96.0, 36.0)),
            |ui| {
                nui::Button::new("删除").danger().full_width().show(ui, &d);
            },
        );
        ry += dlg_h + 28.0;

        // ---- 右列 4：Toast ----
        ry = section(&painter, &d, rx, ry, w, "浮动提示 · Toast");
        nui::toast_at(
            ui,
            &d,
            nui::ToastKind::Success,
            "已保存到本机数据库",
            egui::pos2(rx + 200.0, ry + 24.0),
        );
        nui::toast_at(
            ui,
            &d,
            nui::ToastKind::Error,
            "连接已中断",
            egui::pos2(rx + 200.0, ry + 76.0),
        );

        let _ = ly;
        let _ = ry;

        /// 画一个小节标题并推进游标。
        fn section(
            painter: &egui::Painter,
            d: &nui::Design,
            x: f32,
            y: f32,
            w: f32,
            title: &str,
        ) -> f32 {
            nui::section_label(
                painter,
                d,
                egui::Rect::from_min_size(egui::pos2(x, y), egui::Vec2::new(w, 18.0)),
                title,
            );
            y + 30.0
        }
    }

    // ------------------------------------------------------------------
    // 模型列表：默认为空之后的各种现场
    // ------------------------------------------------------------------

    /// 启动恢复缓存但不隐式联网，即使用户已关闭安全模式。
    #[test]
    fn safety_startup_loads_saved_models_without_refresh() {
        isolate_db();
        fresh_db();

        {
            let store = neo_store::Store::open(&super::test_db_path()).expect("测试库");
            store.set_setting("api_base", "http://127.0.0.1:9").unwrap();
            store.set_setting("api_key", "sk-test").unwrap();
            store.set_setting("classroom_safe", "0").unwrap();
            store
                .set_setting("models", "deepseek-chat\ndeepseek-coder")
                .unwrap();
            store.set_setting("model", "deepseek-coder").unwrap();
        }

        let ctx = egui::Context::default();
        let app = NeoApp::install(&ctx);

        assert_eq!(app.state.models.len(), 2, "上次拉到的列表要读回来");
        assert_eq!(app.state.model_id(), "deepseek-coder", "选中项按 id 恢复");
        assert!(
            app.state.model_fetch.is_none(),
            "启动只恢复缓存，刷新需要用户明确授权"
        );
    }

    /// 配好了密钥却还没有模型列表：输入卡上方那条提示。
    ///
    /// 这是**空列表最常见的现场**（首次启动，或上次拉取失败）——
    /// 界面必须能画，而且要说清"怎么才有模型"。
    #[test]
    fn no_model_notice_1080p() {
        let p = shoot(
            "20-no-model-notice-1080p",
            Vec2::new(1920.0, 1080.0),
            |app| {
                app.state.theme_mode = ThemeMode::Dark;
                app.state.distance = Distance::Classroom;
                // 有密钥、没有模型 —— 正是"该去拉一次"的状态
                app.state.api_key = "sk-demo".to_owned();
            },
        );
        assert!(p.is_file());
    }

    /// 模型页的"还没拉到"现场：显示什么、怎么引导。
    #[test]
    fn settings_model_empty_1080p() {
        let p = shoot(
            "21-settings-model-empty-1080p",
            Vec2::new(1920.0, 1080.0),
            |app| {
                app.state.theme_mode = ThemeMode::Dark;
                app.state.distance = Distance::Classroom;
                app.state.show_settings = true;
                app.state.settings_tab = crate::state::SettingsTab::Model;
                // 密钥留空：hint 会引导"先填密钥再刷新"
                app.state.api_base = "https://api.deepseek.com".to_owned();
            },
        );
        assert!(p.is_file());
    }
}
