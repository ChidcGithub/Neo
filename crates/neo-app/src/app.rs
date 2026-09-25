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
use crate::state::{AppState, Role, Stage, StreamSource, SCENES};
use crate::ui::{self, Skin};

/// 主题重建的输入指纹。
type Fingerprint = (ThemeMode, Distance, i32);

/// 舞台切换 / 模态弹出的入场时长：spec 标准档（0.2s）。
const MODAL_FADE: f32 = 0.2;

/// 唤醒听写的沉默超时：这么久没识别出完整句子就自动收场，回到唤醒检测。
const DICTATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// 发给 STT 线程的命令。音频帧直接投喂；`Reset` 在每次唤醒听写前清掉
/// VAD 缓冲，避免把上一轮的尾巴算进这一轮。
enum SttCmd {
    Audio(Vec<f32>),
    Reset,
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
    /// 上次落库的活跃会话 id，作用同上。
    saved_active: Option<i64>,
    /// 上次落库的面板内设置。直接修改 state 的控件统一在帧末检测变化。
    /// 元素顺序：思考过程 / 思考挡位 / 主题 / 距离 / 最小化到托盘 / 语音唤醒 / 启动即后台。
    saved_prefs: (bool, neo_llm::Thinking, ThemeMode, Distance, bool, bool, bool),
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
    /// 托盘菜单点了「退出」：下一次 close 请求直接放行，不再拦截。
    quitting: bool,
    /// 一闪而过的轻提示：类别 + 内容 + 截止时间。
    toast: Option<(neo_ui::ToastKind, String, std::time::Instant)>,
    /// 全屏跑马灯覆盖层（唤醒聆听时亮起）。离屏测试没有 GPU 窗口线程，为 `None`。
    overlay: Option<neo_overlay::OverlayHandle>,
    /// STT 线程的投喂端（音频帧 / 复位命令）。
    stt_tx: Option<std::sync::mpsc::Sender<SttCmd>>,
    /// STT 转写结果的回收端。
    stt_rx: Option<std::sync::mpsc::Receiver<Result<String, String>>>,
    /// 唤醒后的听写进行中：音频帧正经唤醒引擎流向 STT。
    dictating: bool,
    /// 听写起点，用于沉默超时。
    dictation_since: Option<std::time::Instant>,
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
        let (store, store_ok) = match Store::open_default() {
            Ok(s) => (Some(s), true),
            Err(e) => {
                eprintln!("[neo] 数据库打开失败，将以纯内存模式运行：{e}");
                (None, false)
            }
        };
        let db_path = neo_store::default_db_path().to_string_lossy().into_owned();

        let state = AppState::with_store(store_ok, Some(db_path));

        let mut app = Self {
            state,
            whale,
            fonts,
            theme: Theme::new(ThemeMode::Dark, 1080.0, Distance::default()),
            fingerprint: None,
            store,
            saved_api: (String::new(), String::new()),
            saved_active: None,
            workspace_picker: None,
            prev_stage: Stage::Hero,
            prev_show_settings: false,
            wake: None,
            wake_rx: None,
            wake_broken: false,
            tray: None,
            tray_show_id: tray_icon::menu::MenuId::new("neo-tray-show"),
            tray_quit_id: tray_icon::menu::MenuId::new("neo-tray-quit"),
            hidden_to_tray: false,
            quitting: false,
            toast: None,
            overlay: None,
            stt_tx: None,
            stt_rx: None,
            dictating: false,
            dictation_since: None,
            start_hidden_done: false,
            saved_prefs: (
                false,
                neo_llm::Thinking::default(),
                ThemeMode::Dark,
                Distance::default(),
                true,
                true,
                true,
            ),
        };

        app.load_settings();
        app.saved_api = (app.state.api_base.clone(), app.state.api_key.clone());
        // **顺序要紧**：`start_model_fetch` 会看密钥决定拉不拉，所以必须先装载。
        // 早先这里写在 `load_settings` 之前，于是每次启动都因为"密钥还是空的"
        // 而直接返回 —— 自动刷新从来没真的跑起来过。
        //
        // 每次启动都刷一次：服务商那边增减了模型，不用手动点。
        app.state.start_model_fetch();
        // 装载之后同步一次基准值，否则第一帧会把"从库里读到的"当成"用户刚改的"。
        app.saved_prefs = (
            app.state.show_reasoning,
            app.state.thinking,
            app.state.theme_mode,
            app.state.distance,
            app.state.minimize_to_tray,
            app.state.wake_enabled,
            app.state.start_in_tray,
        );
        if let Some(store) = app.store.as_ref() {
            Self::refresh_sessions(&mut app.state, store);
            // 恢复上次使用的会话。
            Self::restore_last_session(&mut app.state, store);
            app.saved_active = app.state.active_session;
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
        if !self.state.wake_enabled || self.wake.is_some() {
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
                self.wake = Some(engine);
                self.wake_rx = Some(ui_rx);
            }
            Err(error) => eprintln!("[neo] 无法启动语音唤醒：{error}"),
        }
    }

    /// 启动全屏跑马灯覆盖层（独立窗口线程，初始隐藏，等唤醒时 `show`）。
    ///
    /// **只应在真实客户端里调用**（`main`），离屏测试没有 GPU 窗口线程。
    pub fn start_overlay(&mut self) {
        if self.overlay.is_none() {
            // 失败（无 DX12 / 建窗失败）降级为无跑马灯，不拖垮主程序。
            match neo_overlay::start() {
                Ok(h) => self.overlay = Some(h),
                Err(e) => eprintln!("[neo-overlay] 初始化失败，跑马灯不可用: {e}"),
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
        let (out_tx, out_rx) = std::sync::mpsc::channel::<Result<String, String>>();
        let repaint = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("neo-stt".into())
            .spawn(move || {
                let engine = match neo_stt::SttEngine::create(&neo_stt::SttConfig::default()) {
                    Ok(engine) => engine,
                    Err(msg) => {
                        let _ = out_tx.send(Err(msg));
                        repaint.request_repaint();
                        return;
                    }
                };
                // 引擎约定在同一线程内驱动：收帧 → VAD 断句 → 成句即转写。
                while let Ok(cmd) = cmd_rx.recv() {
                    match cmd {
                        SttCmd::Reset => engine.reset(),
                        SttCmd::Audio(frame) => {
                            engine.accept_waveform(&frame);
                            if let Some(seg) = engine.take_segment() {
                                if out_tx.send(engine.transcribe(&seg)).is_err() {
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
            Err(error) => eprintln!("[neo] 无法启动语音转写：{error}"),
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
            eprintln!("[neo] 托盘菜单创建失败");
            return;
        }
        let (rgba, w, h) = crate::brand::whale_rgba(64);
        let icon = match tray_icon::Icon::from_rgba(rgba, w, h) {
            Ok(icon) => icon,
            Err(error) => {
                eprintln!("[neo] 托盘图标创建失败：{error}");
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
            Err(error) => eprintln!("[neo] 系统托盘创建失败：{error}"),
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
            if event.id == self.tray_show_id {
                self.show_window(ctx);
            } else if event.id == self.tray_quit_id {
                self.quitting = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
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

    /// 「Hi, Neo」命中：全屏跑马灯亮起，直接进入听写，**主界面不露面**。
    /// 跑马灯或 STT 不可用（离屏测试 / 模型缺失）时退回老行为：
    /// 唤回主界面并聚焦输入框。
    fn on_wake_detected(&mut self, ctx: &egui::Context, score: f32) {
        eprintln!("[neo] 唤醒命中（置信度 {score:.2}），进入听写");
        if self.overlay.is_some() && self.stt_tx.is_some() {
            if let Some(overlay) = &self.overlay {
                overlay.show();
            }
            if let Some(tx) = &self.stt_tx {
                let _ = tx.send(SttCmd::Reset);
            }
            if let Some(wake) = &self.wake {
                wake.set_dictation(true);
            }
            self.dictating = true;
            self.dictation_since = Some(std::time::Instant::now());
        } else {
            if self.hidden_to_tray {
                self.show_window(ctx);
            }
            // "Hi, Neo"：焦点交给输入框，老师接着输入即可。
            ctx.memory_mut(|m| m.request_focus(Id::new(ui::COMPOSER_ID)));
            self.toast = Some((
                neo_ui::ToastKind::Success,
                format!("已唤醒（置信度 {score:.2}），请直接输入"),
                std::time::Instant::now() + std::time::Duration::from_secs(3),
            ));
        }
    }

    /// 听写模式的音频帧：喂给 STT 线程，同时把电平推给跑马灯驱动光带起伏。
    fn on_dictation_audio(&mut self, frame: Vec<f32>) {
        if !self.dictating {
            return;
        }
        let rms = (frame.iter().map(|x| x * x).sum::<f32>() / frame.len().max(1) as f32).sqrt();
        if let Some(overlay) = &self.overlay {
            overlay.set_level((rms * 3.0).clamp(0.0, 1.0));
        }
        if let Some(tx) = &self.stt_tx {
            if tx.send(SttCmd::Audio(frame)).is_err() {
                self.stt_tx = None;
                self.end_dictation();
            }
        }
    }

    /// 结束听写：跑马灯淡出，唤醒引擎回到检测模式。
    fn end_dictation(&mut self) {
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

    /// 从数据库读设置。缺省值与 `AppState::default` 一致。
    fn load_settings(&mut self) {
        let Some(store) = &self.store else { return };
        let get = |key: &str| -> Option<String> { store.setting(key).ok().flatten() };
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

    /// 打开一个会话：从库里读消息。
    fn open_session(state: &mut AppState, store: &Store, id: i64) {
        let Ok(rows) = store.messages(id) else { return };
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
                // 工具消息在库里只留了 name + 摘要（见 `ChatMessage::tool_result`）；
                // 参数与结果不落库，重启后卡片降级为"只读摘要"。
                if role == Role::Tool {
                    msg.tool = Some(crate::state::ToolMeta::restored(&msg.meta));
                }
                msg
            })
            .collect();
        state.active_session = Some(id);
        state.stage = Stage::Conversation;
        // 整批恢复的历史消息不播入场动画（`new_session` 已把代次 +1）。
        state.entered_count = state.messages.len();
        state.generating = false;
        state.stream = None;
        state.pending_persist = state.messages.len();
        state.auto_approve_tools = false;
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

    fn send_input(state: &mut AppState, store: Option<&Store>) {
        if !state.can_submit() {
            return;
        }
        if !state.draft_attachments.is_empty() && !state.can_call_real() {
            state.attachment_error = Some("附件已准备好；请先配置 API 密钥并选择可用模型。图片需要视觉模型，当前未发送任何附件。".into());
            if state.needs_model_list() {
                state.start_model_fetch();
            }
            return;
        }
        if state.needs_model_list() {
            state.start_model_fetch();
            return;
        }
        if !Self::persist_ready(state, store) {
            return;
        }
        if state.submit() {
            if !Self::persist_ready(state, store) {
                if let Some(message) = state.messages.pop() {
                    state.draft = message.content;
                    state.draft_attachments = message.attachments;
                }
                return;
            }
            if state.can_call_real() {
                Self::start_real_stream(state);
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
        let cfg = state.llm_config();
        let msgs = state.api_messages(24);
        // 注意：`tool_declarations()` 已经是"每元素一条工具"的扁平列表，
        // 不要再套一层 Vec —— `tools: [[…]]` 会被服务端 422 掉。
        //
        // **思考模式也带工具**：早先这里按"是不是推理模型"把 `tools` 摘掉，
        // 依据是 R1 时代"推理模型不支持 Function Calling"的说法；官方 Thinking
        // Mode 文档已经明确"thinking mode supports tool calls"，所以不再摘。
        // 代价是必须把历史 `reasoning_content` 完整回传（见 `state::api_messages`），
        // 那条由 `neo_llm::build_wire` 按档位把关。
        let tools = neo_tools::tool_declarations();
        if msgs
            .iter()
            .any(|m| m.content.contains("本轮因内容预算省略"))
        {
            state.attachment_error =
                Some("本轮只保留预算内的最近附件；部分历史附件已省略，需要时请重新添加。".into());
        }
        // 按真实线协议计算整包（含 JSON 转义、图片和工具定义），不猜 base64 大小。
        let size = serde_json::to_vec(&neo_llm::request_body(&cfg, &msgs, tools.clone()))
            .map_or(usize::MAX, |body| body.len());
        if size > 32 * 1024 * 1024 {
            let mut message = crate::state::ChatMessage::new(Role::Assistant, "");
            message.error =
                Some("本轮请求超过 32 MiB，尚未发给模型。请新建对话并减少附件或文字。".into());
            state.messages.push(message);
            return;
        }
        let stream = neo_llm::start_with_tools(cfg, msgs, tools);
        state.start_generation(StreamSource::Real(Box::new(stream)));
    }

    /// 只提交已经稳定的消息；生成中的占位与未完成工具不能提前保存。
    fn persist_ready(state: &mut AppState, store: Option<&Store>) -> bool {
        if state.pending_persist >= state.messages.len() {
            return true;
        }
        let Some(store) = store else {
            state.pending_persist = state.messages.len();
            return true;
        };
        let Some(session) = Self::ensure_session(state, store) else {
            state.attachment_error = Some("无法保存会话，请检查数据库位置及磁盘空间".into());
            return false;
        };
        while let Some(msg) = state.messages.get(state.pending_persist) {
            if msg.streaming || msg.tool.as_ref().is_some_and(|t| !t.state.is_settled()) {
                break;
            }
            let result = serde_json::to_string(&msg.attachments)
                .map_err(|e| e.to_string())
                .and_then(|json| {
                    store
                        .append_message_with_attachments(
                            session,
                            msg.role.as_str(),
                            &msg.content,
                            &msg.reasoning,
                            &msg.meta,
                            &json,
                        )
                        .map_err(|e| e.to_string())
                });
            if let Err(e) = result {
                state.attachment_error = Some(format!("消息尚未保存，请勿关闭或切换会话：{e}"));
                return false;
            }
            state.pending_persist += 1;
        }
        Self::refresh_sessions(state, store);
        true
    }

    /// 设置变更后写库。
    fn save_setting(store: Option<&Store>, key: &str, value: &str) -> bool {
        // 纯内存模式没有持久化目标；有数据库时，只有实际写入成功才更新基准值。
        store.is_none_or(|store| store.set_setting(key, value).is_ok())
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
        // 关窗拦截：默认转系统托盘后台运行；托盘菜单点了「退出」（quitting）
        // 或用户在设置里关掉该行为时才走默认退出。
        if ctx.input(|i| i.viewport().close_requested())
            && !self.quitting
            && self.state.minimize_to_tray
            && self.tray.is_some()
        {
            self.hide_to_tray(&ctx);
        }
        self.poll_tray(&ctx);
        // 启动即后台：首帧直接进托盘等「Hi, Neo」，主界面不露面。
        // 走首帧而不是 `ViewportBuilder::with_visible(false)`：hide_to_tray 的
        // 注释提到 wgpu 对隐藏窗仍会取交换链，最小化走「尺寸为零跳过」的既有路径。
        if !self.start_hidden_done {
            self.start_hidden_done = true;
            if self.state.start_in_tray && self.tray.is_some() {
                self.hidden_to_tray = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            }
        }
        // 唤醒开关热切换（设置页改完下一帧生效）：关掉立即停引擎——
        // Drop 会置停止位并 join 线程；打开则起引擎（start_wake 幂等）。
        // 放在解构借用 state 之前，否则 &mut self 方法会撞借用检查。
        if self.state.wake_enabled && !self.wake_broken {
            self.start_wake(&ctx);
        } else if !self.state.wake_enabled {
            self.wake = None;
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
        // 语音唤醒：事件由转发线程唤起本帧，这里只摘结果。
        // 先收成局部值再处理 —— 处理时要改 `self.toast` / 清通道，
        // 不能一直借着自己的 `wake_rx`。听写期间音频帧 80ms 一批，
        // 必须一次排空，否则队列越积越深。
        let mut wake_events: Vec<neo_wake::WakeEvent> = Vec::new();
        let mut wake_disconnected = false;
        if let Some(rx) = &self.wake_rx {
            loop {
                match rx.try_recv() {
                    Ok(event) => wake_events.push(event),
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        wake_disconnected = true;
                        break;
                    }
                }
            }
        }
        for event in wake_events {
            match event {
                neo_wake::WakeEvent::Detected { score } => self.on_wake_detected(&ctx, score),
                neo_wake::WakeEvent::Audio(frame) => self.on_dictation_audio(frame),
                neo_wake::WakeEvent::Error(msg) => {
                    // 引擎线程已退出：本次运行不再尝试，提示一次就好。
                    // wake_broken 挡住帧首的热切换逻辑，避免每帧重启风暴。
                    eprintln!("[neo] 语音唤醒停用：{msg}");
                    if self.dictating {
                        self.end_dictation();
                    }
                    self.wake_broken = true;
                    self.toast = Some((
                        neo_ui::ToastKind::Warn,
                        format!("语音唤醒未启用：{msg}（重新打开开关可重试）"),
                        std::time::Instant::now() + std::time::Duration::from_secs(4),
                    ));
                    self.wake_rx = None;
                    self.wake = None;
                }
            }
        }
        if wake_disconnected {
            if self.dictating {
                self.end_dictation();
            }
            self.wake_rx = None;
            self.wake = None;
        }
        // STT 转写结果：第一句完整的话到了就作为输入直接发送，
        // 主界面在这时才唤回，展示这轮对话与回复。
        let stt_out = self.stt_rx.as_ref().map(|rx| rx.try_recv());
        match stt_out {
            Some(Ok(Ok(text))) => {
                let text = text.trim().to_owned();
                if self.dictating && !text.is_empty() {
                    self.end_dictation();
                    self.state.draft = text;
                    if self.hidden_to_tray {
                        self.show_window(&ctx);
                    }
                    Self::send_input(&mut self.state, self.store.as_ref());
                }
            }
            Some(Ok(Err(msg))) => {
                // 模型缺失 / 转写失败：降级为无听写，提示一次。
                eprintln!("[neo] 语音转写不可用：{msg}");
                if self.dictating {
                    self.end_dictation();
                }
                self.stt_tx = None;
                self.toast = Some((
                    neo_ui::ToastKind::Warn,
                    format!("语音转写未启用：{msg}"),
                    std::time::Instant::now() + std::time::Duration::from_secs(4),
                ));
            }
            Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => {
                self.stt_rx = None;
                self.stt_tx = None;
            }
            Some(Err(std::sync::mpsc::TryRecvError::Empty)) | None => {}
        }
        // 沉默超时：唤醒后一直没说出完整句子，自动收场回唤醒检测。
        if self.dictating {
            if self.dictation_since.is_some_and(|t| t.elapsed() > DICTATION_TIMEOUT) {
                self.end_dictation();
            }
            // 听写期间保持帧循环：沉默超时与电平回落都靠它。
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
        let tool_modal_open = self.state.awaiting_tool().is_some();
        let modal_open = self.state.show_settings
            || tool_modal_open
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
        let main = Rect::from_min_max(
            egui::pos2(sidebar_rect.right(), content.top()),
            content.max,
        );

        let background = ui
            .scope_builder(egui::UiBuilder::new(), |ui| {
                if modal_open {
                    ui.disable();
                }
                let sb = ui::sidebar::draw(ui, &skin, sidebar_rect, state, escape_pressed);
                let mut scene_clicked = None;
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
                        scene_clicked = out.scene_clicked;
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

                (
                    sb,
                    cluster,
                    composer_out,
                    scene_clicked,
                    new_session,
                    workspace_clicked,
                )
            })
            .inner;
        let (sb, cluster, composer_out, scene_clicked, new_session, workspace_clicked) = background;

        // 舞台切换入场：新舞台已画好，用背景色「盖一层再掀开」，等价于整区
        // 淡入，不必逐形状穿透各子 Ui；收敛后 k=1 不画。侧栏不参与 ——
        // 它在两个舞台之间保持不变。
        if stage_k < 1.0 {
            ui.painter()
                .rect_filled(main, 0.0, ui::translucent(p.bg_base, 1.0 - stage_k));
        }

        // 工具确认位于最上层时，设置面板保持状态但不参与交互。
        if state.show_settings && !tool_modal_open {
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
            let panel_rect =
                Rect::from_center_size(screen.center() + egui::vec2(0.0, rise), size);
            if ui::settings::panel(ui, &skin, panel_rect, state, screen.max.y, fonts) {
                state.show_settings = false;
            }
            if escape_pressed {
                state.show_settings = false;
            }
        }

        // ---- 3.4 工具确认弹窗（覆盖一切，与设置面板同层）----
        if let Some(index) = state.awaiting_tool() {
            let remaining = state.awaiting_tool_count();
            let meta = state.messages[index].tool.clone();
            if let Some(meta) = meta {
                match ui::tools::confirm(ui, &skin, &meta, remaining) {
                    Some(ui::tools::Answer::Once) => state.approve_tool(index),
                    Some(ui::tools::Answer::Always) => {
                        // "都允许"只在本次会话内有效，重启即失效。
                        state.auto_approve_tools = true;
                        state.approve_tool(index);
                    }
                    Some(ui::tools::Answer::Deny) => state.deny_tool(index),
                    None => {}
                }
                // Esc 与"拒绝"同义：弹窗不能没有出口。
                if escape_pressed {
                    state.deny_tool(index);
                }
            }
        }

        // 没有任何弹窗时，Esc = 停止当前这一轮（生成或工具执行中）。
        // 弹窗开着时 Esc 已各有含义（关设置 / 拒工具），不抢。
        if escape_pressed
            && !modal_open
            && (state.generating || state.tool_open || state.tool_round)
        {
            state.cancel();
        }

        // ---- 3. 生成态推进 ----
        // 每帧从流式来源泵增量；真实 / 演示两种来源在 UI 侧等价。
        if state.generating && state.pump() {
            ctx.request_repaint_after(std::time::Duration::from_millis(33));
        }

        // ---- 3.4 模型列表拉取 ----
        // 后台线程在跑，这里只收结果；收到就刷新可选模型（当前选中按 id 保留）。
        if state.poll_model_fetch() {
            // 拉到的列表落库：下次启动先读回来，没网也有模型可选。
            // 列表为空时**不写** —— 别拿空值覆盖上一次拉到的好数据。
            if state.has_models() {
                Self::save_setting(self.store.as_ref(), "models", &state.model_ids_joined());
                Self::save_setting(self.store.as_ref(), "model", state.model_id());
            }
            ctx.request_repaint();
        }

        // ---- 3.5 工具轮 ----
        // 流结束在 `finish_reason = tool_calls` 时，把分片聚合成消息块。
        // 这一步只登记；执行放到确认弹窗之后（见本函数末尾），
        // 否则用户这一帧既看不到卡片也看不到弹窗。
        if state.tool_round {
            state.begin_tool_round();
        }

        // 用户消息在生成开始前立即保存，流式占位只在稳定后提交。
        let persistence_ok = Self::persist_ready(state, self.store.as_ref());

        // 演示流：未配置密钥时，submit 之后自动接一条离线演示回复。
        if state.wants_demo_reply {
            state.wants_demo_reply = false;
            let prompt = state
                .messages
                .iter()
                .rev()
                .find(|m| m.role == Role::User)
                .map(|m| m.content.clone())
                .unwrap_or_default();
            let demo = crate::state::demo_reply(&prompt, state.model_display());
            state.start_generation(StreamSource::Demo {
                text: demo,
                cursor: 0,
            });
        }

        // ---- 4. 落状态 ----
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
                Err(error) => eprintln!("[neo] 无法打开工作区选择器：{error}"),
            }
        }
        if (sb.new_session || new_session) && persistence_ok {
            state.new_session();
        }
        if let Some(id) = sb.open_session.filter(|_| persistence_ok) {
            if let Some(store) = self.store.as_ref() {
                Self::open_session(state, store, id);
            }
        }
        if let Some((id, title)) = sb.renamed {
            if let Some(store) = self.store.as_ref() {
                let _ = store.rename_session(id, &title);
                Self::refresh_sessions(state, store);
            }
        }
        if let Some(id) = sb.delete_confirmed {
            if let Some(store) = self.store.as_ref() {
                let _ = store.delete_session(id);
                Self::refresh_sessions(state, store);
                // 删的是当前会话：回到空态。
                if state.active_session == Some(id) {
                    state.new_session();
                }
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
        if let Some(i) = scene_clicked {
            if let Some(scene) = SCENES.get(i) {
                state.active_scene = Some(i);
                state.draft = scene.prompt.to_owned();
            }
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
                // 还没有列表可切：这一下点击用来"去拉一次" ——
                // 比"点了什么都不发生"要好。（没配密钥时 `start_model_fetch`
                // 自己会判断并直接返回。）
                state.start_model_fetch();
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

        // 设置面板里**直接改 state** 的项（没有单独的按钮可挂）：与上次落库的值
        // 比对，变了才写。顺带补上了「显示思考过程」此前从不落库的问题 ——
        // 它在面板里能读能写，重启却会丢。
        if state.show_reasoning != self.saved_prefs.0
            && Self::save_setting(
                self.store.as_ref(),
                "show_reasoning",
                if state.show_reasoning { "1" } else { "0" },
            )
        {
            self.saved_prefs.0 = state.show_reasoning;
        }
        if state.thinking != self.saved_prefs.1
            && Self::save_setting(self.store.as_ref(), "thinking", state.thinking.key())
        {
            self.saved_prefs.1 = state.thinking;
        }
        if state.theme_mode != self.saved_prefs.2
            && Self::save_setting(
                self.store.as_ref(),
                "theme",
                theme_mode_key(state.theme_mode),
            )
        {
            self.saved_prefs.2 = state.theme_mode;
        }
        if state.distance != self.saved_prefs.3
            && Self::save_setting(
                self.store.as_ref(),
                "distance",
                distance_key(state.distance),
            )
        {
            self.saved_prefs.3 = state.distance;
        }
        if state.minimize_to_tray != self.saved_prefs.4
            && Self::save_setting(
                self.store.as_ref(),
                "minimize_to_tray",
                if state.minimize_to_tray { "1" } else { "0" },
            )
        {
            self.saved_prefs.4 = state.minimize_to_tray;
        }
        if state.wake_enabled != self.saved_prefs.5
            && Self::save_setting(
                self.store.as_ref(),
                "wake_enabled",
                if state.wake_enabled { "1" } else { "0" },
            )
        {
            self.saved_prefs.5 = state.wake_enabled;
        }
        if state.start_in_tray != self.saved_prefs.6
            && Self::save_setting(
                self.store.as_ref(),
                "start_in_tray",
                if state.start_in_tray { "1" } else { "0" },
            )
        {
            self.saved_prefs.6 = state.start_in_tray;
        }
        // 接口配置变化时落库（输入框每帧都在读，不能每帧写）。
        if (state.api_base.as_str(), state.api_key.as_str())
            != (self.saved_api.0.as_str(), self.saved_api.1.as_str())
        {
            let base_saved = Self::save_setting(self.store.as_ref(), "api_base", &state.api_base);
            let key_saved = Self::save_setting(self.store.as_ref(), "api_key", &state.api_key);
            if base_saved && key_saved {
                self.saved_api = (state.api_base.clone(), state.api_key.clone());
            }
        }

        // ---- 4. 工具执行与回灌 ----
        // 工具在**独立线程**里跑（可能是几分钟的编译），这里只逐帧收结果，
        // 绝不阻塞渲染。放在最后：确认弹窗（上一步）可能刚把状态推到 Running。
        if state.tool_open {
            state.poll_tool_jobs();
            if state.awaiting_tool().is_none() {
                if !state.tools_running() && !state.tools_settled() {
                    let scope = neo_tools::Scope::new(state.workspace_root());
                    state.spawn_ready_tools(&scope);
                }
                if state.tools_running() {
                    // 后台在跑：定期回来收结果（也顺带刷新"执行中…"的动画）。
                    ctx.request_repaint_after(std::time::Duration::from_millis(60));
                } else if state.tools_settled() {
                    state.tool_open = false;
                    if state.can_call_real() {
                        Self::start_real_stream(state);
                    }
                }
            }
        }

        // 活跃会话变化时落库：下次启动从这里恢复。空串表示空态。
        if state.active_session != self.saved_active {
            let v = state
                .active_session
                .map(|id| id.to_string())
                .unwrap_or_default();
            if Self::save_setting(self.store.as_ref(), "active_session", &v) {
                self.saved_active = state.active_session;
            }
        }

        // ---- 5. 轻提示（最上层，一闪而过）----
        // 唤醒成功 / 唤醒不可用都走它。过期即弃。
        let expired = self
            .toast
            .as_ref()
            .is_some_and(|(.., until)| std::time::Instant::now() >= *until);
        if expired {
            self.toast = None;
        }
        if let Some((kind, text, _)) = &self.toast {
            neo_ui::Toast::new(*kind, text).show_at(
                ui,
                &skin.design,
                egui::pos2(screen.center().x, screen.max.y - m.s(150.0)),
            );
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
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
    /// 窗口清屏色与主题一致，避免缩放或首帧出现白闪。
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        self.theme.palette.bg_base.to_normalized_gamma_f32()
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
    //! 测试进程会继承开发机的 `APPDATA`，`install` 会真的打开
    //! `%APPDATA%\Neo\neo.db`。为避免污染真实数据库，测试统一把 `NEO_HOME`
    //! 指到临时目录 —— `default_db_path` 优先读它。
    //! 注意：`std::env::set_var` 在多线程下是 UB，测试必须串行
    //! （`--test-threads=1`），这与截图测试的既有约定一致。

    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::rc::Rc;
    use std::sync::Once;

    use egui::Vec2;

    use super::NeoApp;
    use crate::state::{
        AppState, ChatMessage, Role, Stage, StreamSource, ToolMeta, ToolState, SCENES,
    };
    use neo_theme::{Distance, ThemeMode};

    static ENV: Once = Once::new();

    /// 把 `NEO_HOME` 指到一次性目录（进程级，一次即可）。
    fn isolate_db() {
        ENV.call_once(|| {
            let dir = std::env::temp_dir().join(format!("neo-snapshot-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            // 测试进程是单线程驱动的（--test-threads=1），这里 set_var 是安全的。
            std::env::set_var("NEO_HOME", &dir);
        });
    }

    /// 把测试库清干净，让紧随其后的 `install` 等价于「全新安装」。
    ///
    /// 测试之间**共享同一个临时库**（`isolate_db` 只把 `NEO_HOME` 指过去一次），
    /// 而 `install` 会从库里装载设置 —— 于是"谁先跑、往库里写过什么"会顺着
    /// `install` 影响后面每一张快照。这条已经咬过两次：
    ///
    /// - 某张快照设了 `api_key`，被落库后**后续每张快照启动时都会真的联网**
    ///   拉模型列表（快照里出现"正在拉取…"，还依赖外面有没有网）；
    /// - 更早的一次是"上次会话"被恢复，标称 hero 的用例画成了对话态。
    ///
    /// 所以每次装配前删掉库文件。需要预置数据的用例，在 `setup` 里自己造
    /// （共享库不再承担"记住上次"的职责）。
    fn fresh_db() {
        let _ = std::fs::remove_file(neo_store::default_db_path());
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
            app.store = Some(neo_store::Store::open_default().unwrap());
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
            app.state.tool_round = true;
            app.state.begin_tool_round();
        });
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
    fn scene_cards_visible_in_hero() {
        assert_eq!(SCENES.len(), 4);
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
        ly = section(&painter, &d, lx, ly, w, "反馈 · Badge / Spinner / 空态");
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
        // 空态卡。
        let empty_card = egui::Rect::from_min_size(egui::pos2(lx, ly), Vec2::new(400.0, 150.0));
        nui::Card::new(nui::CardSurface::Tip).paint(ui, &d, empty_card);
        nui::at(ui, nui::inset_all(empty_card, 12.0), |ui| {
            nui::EmptyState::new(nui::Icon::Mic, "还没有语音记录")
                .hint("点击麦克风开始")
                .show(ui, &d, nui::inset_all(empty_card, 12.0));
        });

        // ---- 右列 1：卡片 ----
        ry = section(&painter, &d, rx, ry, w, "卡片 · Card");
        let cw3 = (w - 24.0) / 3.0;
        for (i, (label, card)) in [
            ("输入卡", nui::Card::input()),
            ("气泡", nui::Card::bubble()),
            ("选中态", nui::Card::raised().selected(true)),
        ]
        .into_iter()
        .enumerate()
        {
            let r = egui::Rect::from_min_size(
                egui::pos2(rx + (cw3 + 12.0) * i as f32, ry),
                Vec2::new(cw3, 120.0),
            );
            card.paint(ui, &d, r);
            painter.text(
                egui::pos2(r.left() + 14.0, r.bottom() - 12.0),
                egui::Align2::LEFT_CENTER,
                label,
                d.font(d.t().caption),
                d.p().label_caption,
            );
        }
        ry += 136.0;

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

        // ---- 右列 4：Toast 与 Tooltip ----
        ry = section(&painter, &d, rx, ry, w, "浮动提示 · Toast / Tooltip");
        nui::Toast::new(nui::ToastKind::Success, "已保存到本机数据库").show_at(
            ui,
            &d,
            egui::pos2(rx + 200.0, ry + 24.0),
        );
        nui::Toast::new(nui::ToastKind::Error, "连接已中断").show_at(
            ui,
            &d,
            egui::pos2(rx + 200.0, ry + 76.0),
        );
        nui::Tooltip::new("Enter 发送").show_at(ui, &d, egui::pos2(rx + 440.0, ry + 16.0));

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

    /// 启动链路：**先装载、再拉取**（这条守着一个真出过的错）。
    ///
    /// `install` 里拉取一度写在装载**之前**，于是每次启动都因为"密钥还没读进来"
    /// 而直接返回 —— 自动刷新的开关形同虚设。所以这里同时断言两件互相依赖的事：
    /// ① 上次存下的列表读回来了；② 紧接着**发起了**刷新（密钥也是从库里读的，
    /// 顺序反了这里就会是 `None`）。
    ///
    /// 拉取目标指向本机一个必定拒绝连接的端口：不碰真实网络、也不依赖外网。
    /// （依赖测试串行 —— `set_var` 在多线程下不安全，与既有的 `--test-threads=1` 约定一致。）
    #[test]
    fn startup_loads_saved_models_then_refreshes() {
        isolate_db();
        let dir = std::env::temp_dir().join(format!("neo-startup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let old_home = std::env::var("NEO_HOME").ok();
        std::env::set_var("NEO_HOME", &dir);

        {
            let store = neo_store::Store::open_default().expect("测试库");
            store.set_setting("api_base", "http://127.0.0.1:9").unwrap();
            store.set_setting("api_key", "sk-test").unwrap();
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
            app.state.model_fetch.is_some(),
            "启动就该发起一次刷新 —— 这里为 None 通常意味着装载与拉取的顺序又反了"
        );

        match old_home {
            Some(v) => std::env::set_var("NEO_HOME", v),
            None => std::env::remove_var("NEO_HOME"),
        }
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
