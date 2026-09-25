//! 课堂总结：后台检测到应用最大化/全屏（老师开课件）→ 截屏视觉分析 +
//! 连续录音转写 → 退出最大化且两分钟无操作 → LLM 打磨成总结 → 顶部弹窗。
//!
//! # 状态机
//!
//! ```text
//! Idle ──前台应用最大化──▶ Active ──退出最大化──▶ Settling ──重新最大化──▶ Active
//!                            │                        │
//!                     截屏(冷却10s)+连续转写      全局键鼠静默 ≥2min（或挂起 30min 兜底）
//!                            │                        │
//!                            │                        ▼
//!                            │                       Polishing（LLM 打磨 ≤1500 字）
//!                            │                        │
//!                            │                        ▼
//!                            └──────── 取消 ──── Presenting（顶部弹窗，关掉回 Idle）
//! ```
//!
//! # 线程布局
//!
//! - **watch 线程**（功能开着就跑）：500ms 轮询前台窗口的最大化沿；
//!   低级键盘钩子只记时间戳（**不读键值**，隐私）；
//! - **STT 线程**（Active 期间跑）：`neo_wake::AudioTap` 独立采集 +
//!   `neo_stt::SttEngine` 连续转写，逐句回传；
//! - **分析 worker**（按需 spawn，一次一个在飞）：截图 → data URL →
//!   `neo_llm::start` 静默视觉分析；打磨同理（纯文本）。
//!
//! 三个线程都通过同一条 mpsc 把事件送回 UI 线程的 [`ClassMonitor::tick`]，
//! 状态转移只发生在 tick 里 —— 单点决策，没有跨线程共享可变状态。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use neo_llm::{Msg, Role};

use crate::state::AppState;

/// 截屏冷却：键盘活动触发的再次截屏至少间隔这么久。
const SHOT_COOLDOWN: Duration = Duration::from_secs(10);
/// 退出最大化后，全局键鼠静默这么久就开始打磨收尾。
const SETTLE_IDLE: Duration = Duration::from_secs(120);
/// Settling 兜底：一直有人在动电脑也不能无限等，挂起这么久强制收尾。
const SETTLE_MAX: Duration = Duration::from_secs(30 * 60);
/// 进入最大化后稍等再截首屏（PPT 放映动画还在播，截早了是上一页）。
const FIRST_SHOT_DELAY: Duration = Duration::from_millis(1200);
/// 喂给打磨的转写文本上限（字符）：一节 40 分钟的课转写约一两万字，够了。
const POLISH_TRANSCRIPT_CHARS: usize = 24_000;
/// 视觉分析的总超时（connect_timeout 15s 之外的总闸）。
const VISION_TIMEOUT: Duration = Duration::from_secs(90);
/// 打磨的总超时（素材长，生成慢）。
const POLISH_TIMEOUT: Duration = Duration::from_secs(240);

// ---------------------------------------------------------------------------
// 事件与结构
// ---------------------------------------------------------------------------

/// 后台线程送回 UI 线程的事件。
enum ClassEvent {
    /// 前台应用进入最大化/全屏（沿）。
    Maximized,
    /// 退出最大化（沿）。
    Unmaximized,
    /// STT 转出一句完整的话。
    Line(String),
    /// STT / 音频采集线程死亡（转写中断，其余照常）。
    SttDied(String),
    /// 一次视觉分析完成（subject 只有首屏才有）。
    Vision { subject: Option<String>, note: String },
    /// 视觉分析失败（静默忽略，下一次触发再来）。
    VisionFailed(String),
    /// 打磨完成（session 随结果送回 —— 打磨飞行期间可能又开了新课）。
    Polished { summary: String, session: Box<Session> },
    /// 打磨失败：兜底总结在 UI 线程拼（素材不能丢）。
    PolishFailed { error: String, session: Box<Session> },
}

/// 一节课的进行中数据（Active / Settling 共享；打磨时随请求走、随结果回）。
struct Session {
    started_ms: i64,
    subject: Option<String>,
    screen_notes: Vec<String>,
    transcript: Vec<String>,
    /// 上次截屏时刻（冷却用）。
    last_shot: Instant,
}

impl Session {
    fn new() -> Self {
        Self {
            started_ms: now_ms(),
            subject: None,
            screen_notes: Vec::new(),
            transcript: Vec::new(),
            last_shot: Instant::now(),
        }
    }
}

/// 打磨完成、等待弹窗展示的一节课。
pub struct ReadySummary {
    pub subject: String,
    pub summary: String,
    pub over_limit: bool,
    pub date: String,
}

/// 状态机。
enum ClassPhase {
    Idle,
    /// 最大化中：采集 + 分析。`Option<Instant>` = 首屏截屏的预定时刻（None = 已截）。
    Active(Session, Option<Instant>),
    /// 退出最大化：等静默。时间是退出时刻。
    Settling(Session, Instant),
    /// 打磨在飞（session 在 worker 手里，随结果事件送回）。
    Polishing,
    /// 总结就绪：弹窗展示中。
    Presenting,
}

/// 课堂总结控制器。app 持有一个实例，每帧 `tick`。
pub struct ClassMonitor {
    phase: ClassPhase,
    tx: mpsc::Sender<ClassEvent>,
    rx: mpsc::Receiver<ClassEvent>,
    /// watch 线程停止信号（功能开着 = Some）。
    watch_stop: Option<Arc<AtomicBool>>,
    /// STT 线程停止信号（Active 期间 = Some）。
    stt_stop: Option<Arc<AtomicBool>>,
    /// 分析 worker 在飞标志（视觉与打磨共用，一次一个）。
    analysis_in_flight: bool,
    /// 已见到的键盘活动时间戳（沿检测）。
    seen_key_ms: u64,
    /// 打磨结果，等弹窗取走。
    ready: Option<ReadySummary>,
    /// 状态行（设置页显示「记录中…」之类）。
    status: String,
}

impl Default for ClassMonitor {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            phase: ClassPhase::Idle,
            tx,
            rx,
            watch_stop: None,
            stt_stop: None,
            analysis_in_flight: false,
            seen_key_ms: 0,
            ready: None,
            status: String::new(),
        }
    }
}

impl ClassMonitor {
    /// 弹窗要展示的内容（有 = 正在展示）。
    pub fn presenting(&self) -> Option<&ReadySummary> {
        self.ready.as_ref()
    }

    /// 用户关掉弹窗：回 Idle，下一节课从头来。
    pub fn dismiss(&mut self) {
        if matches!(self.phase, ClassPhase::Presenting) {
            self.phase = ClassPhase::Idle;
            self.ready = None;
            self.status.clear();
        }
    }

    /// 一句话状态（设置页用；空闲返回 None）。
    pub fn status(&self) -> Option<&str> {
        if self.status.is_empty() {
            None
        } else {
            Some(&self.status)
        }
    }

    /// 每帧驱动：泵事件 + 推进状态机。`enabled` 来自设置开关。
    pub fn tick(&mut self, ctx: &egui::Context, state: &AppState, enabled: bool) {
        if !enabled {
            self.shutdown();
            return;
        }
        if self.watch_stop.is_none() {
            self.watch_stop = Some(watch::start(self.tx.clone()));
        }
        // 慢轮询节奏：首屏延迟 / 冷却 / 静默计时都靠帧循环推进。
        ctx.request_repaint_after(Duration::from_millis(250));

        // 1) 收事件（drain，就地消化；phase 转移全在这里）。
        while let Ok(ev) = self.rx.try_recv() {
            self.on_event(ev);
        }

        // 2) 周期检查：先记账再落地 —— match 持有 phase 借用时调不了
        //    &mut self 方法，动作统一收进 pending、循环外执行。
        enum Pending {
            Shot(bool),
            Polish,
        }
        let mut pending: Option<Pending> = None;
        match &mut self.phase {
            ClassPhase::Active(_, Some(first_at)) => {
                if Instant::now() >= *first_at {
                    // 首屏到点：标记已截 + 记账。
                    self.seen_key_ms = watch::last_key_ms();
                    pending = Some(Pending::Shot(true));
                    // 消掉预定时刻：借 phase 写不了（pending 落地时 phase 可能
                    // 变了），换成在落地处统一处理 —— 这里先把标记清掉。
                    if let ClassPhase::Active(_, slot) = &mut self.phase {
                        *slot = None;
                    }
                } else {
                    // 还没到首屏时刻，键盘沿不查（首屏还没拍，拍了也没用）。
                }
            }
            ClassPhase::Active(session, None) => {
                let key_ms = watch::last_key_ms();
                if key_ms != self.seen_key_ms {
                    self.seen_key_ms = key_ms;
                    if session.last_shot.elapsed() >= SHOT_COOLDOWN {
                        session.last_shot = Instant::now();
                        pending = Some(Pending::Shot(false));
                    }
                }
            }
            ClassPhase::Settling(_, exited_at) => {
                let due = watch::idle_duration() >= SETTLE_IDLE
                    || exited_at.elapsed() >= SETTLE_MAX;
                if due && !self.analysis_in_flight {
                    pending = Some(Pending::Polish);
                }
            }
            _ => {}
        }
        match pending {
            Some(Pending::Shot(first)) => self.spawn_vision(state, first),
            Some(Pending::Polish) => {
                let old = std::mem::replace(&mut self.phase, ClassPhase::Polishing);
                if let ClassPhase::Settling(session, _) = old {
                    self.spawn_polish(state, session);
                }
            }
            None => {}
        }
    }

    /// 关掉所有后台活动（功能被关 / app 退出）。
    fn shutdown(&mut self) {
        if let Some(stop) = self.watch_stop.take() {
            stop.store(true, Ordering::Relaxed);
        }
        self.stop_stt();
        self.phase = ClassPhase::Idle;
        self.ready = None;
        self.status.clear();
    }

    fn stop_stt(&mut self) {
        if let Some(stop) = self.stt_stop.take() {
            stop.store(true, Ordering::Relaxed);
            // 线程内 flush 尾音转写完自行退出，不 join（UI 线程不阻塞）。
        }
    }

    // ------------------------------------------------------------------
    // 事件处理
    // ------------------------------------------------------------------

    fn on_event(&mut self, ev: ClassEvent) {
        match ev {
            ClassEvent::Maximized => match &mut self.phase {
                ClassPhase::Idle => {
                    self.start_stt();
                    self.phase = ClassPhase::Active(
                        Session::new(),
                        Some(Instant::now() + FIRST_SHOT_DELAY),
                    );
                    // 键盘沿的基线：上课前的打字不该算「课堂中的键盘活动」。
                    self.seen_key_ms = watch::last_key_ms();
                    self.status = "课堂记录中…".into();
                }
                ClassPhase::Settling(..) => {
                    // 课间退出又回来：接着记，重启转写。
                    let old = std::mem::replace(&mut self.phase, ClassPhase::Idle);
                    if let ClassPhase::Settling(session, _) = old {
                        self.start_stt();
                        self.phase = ClassPhase::Active(
                            session,
                            Some(Instant::now() + FIRST_SHOT_DELAY),
                        );
                        self.seen_key_ms = watch::last_key_ms();
                        self.status = "课堂记录中…".into();
                    }
                }
                // Polishing 期间最大化 = 新的一课：旧课在 worker 手里随结果送回，
                // 这里照开新 session（罕见交叉，结果回来时 finish 优先展示）。
                ClassPhase::Polishing => {
                    self.start_stt();
                    self.phase = ClassPhase::Active(
                        Session::new(),
                        Some(Instant::now() + FIRST_SHOT_DELAY),
                    );
                    self.seen_key_ms = watch::last_key_ms();
                    self.status = "课堂记录中…".into();
                }
                _ => {}
            },
            ClassEvent::Unmaximized => {
                if matches!(self.phase, ClassPhase::Active(..)) {
                    let old = std::mem::replace(&mut self.phase, ClassPhase::Idle);
                    if let ClassPhase::Active(session, _) = old {
                        self.stop_stt();
                        self.phase = ClassPhase::Settling(session, Instant::now());
                        self.status = "等待收尾（两分钟无操作后生成总结）…".into();
                    }
                }
            }
            // Settling 也收：STT 线程 flush 出来的最后几句在停后才到。
            ClassEvent::Line(text) => match &mut self.phase {
                ClassPhase::Active(session, _) | ClassPhase::Settling(session, _) => {
                    if !text.trim().is_empty() {
                        session.transcript.push(text);
                    }
                }
                _ => {}
            },
            ClassEvent::SttDied(e) => {
                eprintln!("[neo-class] 转写线程退出：{e}");
            }
            ClassEvent::Vision { subject, note } => {
                self.analysis_in_flight = false;
                match &mut self.phase {
                    ClassPhase::Active(session, _) | ClassPhase::Settling(session, _) => {
                        if let Some(s) = subject {
                            if !s.is_empty() && s != "未知" && session.subject.is_none() {
                                session.subject = Some(s);
                            }
                        }
                        if !note.trim().is_empty() {
                            session.screen_notes.push(note);
                        }
                    }
                    _ => {}
                }
            }
            ClassEvent::VisionFailed(e) => {
                self.analysis_in_flight = false;
                eprintln!("[neo-class] 视觉分析失败（跳过这次截图）：{e}");
            }
            ClassEvent::Polished { summary, session } => {
                self.analysis_in_flight = false;
                self.finish(*session, summary);
            }
            ClassEvent::PolishFailed { error, session } => {
                self.analysis_in_flight = false;
                let fallback = fallback_summary(&session, &error);
                self.finish(*session, fallback);
            }
        }
    }

    /// 打磨完成（或兜底）：落分记忆 + 总记忆索引 + 弹窗。
    fn finish(&mut self, session: Session, summary: String) {
        // 罕见交叉：打磨飞行期间又开了新课 —— 收尾优先，新课的转写停掉。
        self.stop_stt();
        let subject = session.subject.clone().unwrap_or_else(|| "未知".into());
        let date = neo_tools::classlog::today_key();
        let note = neo_tools::classlog::NewClassNote {
            started_ms: session.started_ms,
            subject: subject.clone(),
            summary: summary.clone(),
            screen_notes: session.screen_notes.clone(),
            transcript: session.transcript.clone(),
        };
        let over_limit = match neo_tools::classlog::append_class(note) {
            Ok(saved) => {
                // 总记忆只留一句索引（分记忆不回读总记忆，单向引用）。
                let first_line = summary
                    .lines()
                    .find(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
                    .unwrap_or("")
                    .trim();
                let first_line: String = first_line.chars().take(50).collect();
                let _ = neo_tools::tools::memory::add_memory(&format!(
                    "{date} {subject}课：{first_line}（完整总结见分记忆 class/{date}.json）"
                ));
                saved.over_limit
            }
            Err(e) => {
                // 落盘失败也要让人看到总结（内存里至少有）。
                eprintln!("[neo-class] 分记忆落盘失败：{}", e.message);
                summary.chars().count() > neo_tools::classlog::SUMMARY_LIMIT
            }
        };
        self.ready = Some(ReadySummary {
            subject,
            summary,
            over_limit,
            date,
        });
        self.phase = ClassPhase::Presenting;
        self.status.clear();
    }

    // ------------------------------------------------------------------
    // 线程启动
    // ------------------------------------------------------------------

    /// 启动连续转写线程（Active 期间）。
    fn start_stt(&mut self) {
        self.stop_stt();
        let stop = Arc::new(AtomicBool::new(false));
        let tx = self.tx.clone();
        let stop2 = stop.clone();
        let spawned = std::thread::Builder::new()
            .name("neo-class-stt".into())
            .spawn(move || stt_main(tx, stop2));
        match spawned {
            Ok(_) => self.stt_stop = Some(stop),
            Err(e) => {
                let _ = self.tx.send(ClassEvent::SttDied(format!("spawn: {e}")));
            }
        }
    }

    /// 截屏 + 视觉分析（一次一个；飞行中来了直接丢，冷却外还会再触发）。
    fn spawn_vision(&mut self, state: &AppState, first: bool) {
        if self.analysis_in_flight {
            return;
        }
        let Some(cfg) = llm_config(state) else {
            return; // 没配模型：视觉分析整体跳过，转写不受影响
        };
        self.analysis_in_flight = true;
        let tx = self.tx.clone();
        let _ = std::thread::Builder::new()
            .name("neo-class-vision".into())
            .spawn(move || {
                let _ = tx.send(run_vision(&cfg, first));
            });
    }

    /// 打磨收尾（调用方保证 !analysis_in_flight，且已把 phase 转到 Polishing）。
    fn spawn_polish(&mut self, state: &AppState, session: Session) {
        self.analysis_in_flight = true;
        self.status = "正在整理课堂总结…".into();
        let tx = self.tx.clone();
        match llm_config(state) {
            Some(cfg) => {
                let _ = std::thread::Builder::new()
                    .name("neo-class-polish".into())
                    .spawn(move || {
                        let _ = tx.send(run_polish(&cfg, session));
                    });
            }
            None => {
                let _ = tx.send(ClassEvent::PolishFailed {
                    error: "未配置模型接口".into(),
                    session: Box::new(session),
                });
            }
        }
    }
}

impl Drop for ClassMonitor {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// 取 LLM 配置；未配置返回 None（静默任务不打扰用户）。
fn llm_config(state: &AppState) -> Option<neo_llm::Config> {
    if state.can_call_real() {
        Some(state.llm_config())
    } else {
        None
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 打磨失败的兜底总结：屏幕笔记全量 + 转写节选，素材不丢。
fn fallback_summary(session: &Session, err: &str) -> String {
    let mut s = format!("（自动总结失败：{err}。以下为原始记录，未整理）\n");
    if let Some(sub) = &session.subject {
        s.push_str(&format!("\n## 科目\n{sub}\n"));
    }
    if !session.screen_notes.is_empty() {
        s.push_str("\n## 屏幕笔记\n");
        for n in &session.screen_notes {
            s.push_str(&format!("- {n}\n"));
        }
    }
    if !session.transcript.is_empty() {
        s.push_str("\n## 讲课转写（节选）\n");
        let joined = session.transcript.join(" ");
        let tail: String = joined.chars().take(6000).collect();
        s.push_str(&tail);
    }
    s
}

// ---------------------------------------------------------------------------
// STT 线程：AudioTap 采集 + SttEngine 连续转写
// ---------------------------------------------------------------------------

fn stt_main(tx: mpsc::Sender<ClassEvent>, stop: Arc<AtomicBool>) {
    let engine = match neo_stt::SttEngine::create(&neo_stt::SttConfig::default()) {
        Ok(e) => e,
        Err(e) => {
            let _ = tx.send(ClassEvent::SttDied(format!("加载 STT 模型失败：{e}")));
            return;
        }
    };
    let tap = match neo_wake::AudioTap::start() {
        Ok(t) => t,
        Err(e) => {
            let _ = tx.send(ClassEvent::SttDied(format!("打开麦克风失败：{e}")));
            return;
        }
    };
    eprintln!("[neo-class] 课堂转写开始（{}）", tap.device());

    // 收帧 → VAD 断句 → 成句即转写回传；stop 后 flush 尾音再退。
    while !stop.load(Ordering::Relaxed) {
        let Some(frame) = tap.next_frame(Duration::from_millis(50)) else {
            continue; // 超时空转；流死了 tap 线程退出，这里持续 None 无害
        };
        engine.accept_waveform(&frame);
        while let Some(seg) = engine.take_segment() {
            if let Ok(text) = engine.transcribe(&seg) {
                if !text.trim().is_empty() && tx.send(ClassEvent::Line(text)).is_err() {
                    return;
                }
            }
        }
    }
    // 收尾：把没说完的尾音冲出来转掉（Settling 相位仍会收这些 Line）。
    engine.flush();
    while let Some(seg) = engine.take_segment() {
        if let Ok(text) = engine.transcribe(&seg) {
            let _ = tx.send(ClassEvent::Line(text));
        }
    }
    engine.reset();
    eprintln!("[neo-class] 课堂转写结束");
}

// ---------------------------------------------------------------------------
// 分析 worker：视觉分析 + 打磨（共用 neo_llm::start 静默流）
// ---------------------------------------------------------------------------

/// 消费一条静默流到结束，返回全文。Disconnected = 发送端收尾退出，
/// 已累积的文本照用（Done 事件在极端时序下可能丢，文本不丢）。
fn drain_stream(stream: &neo_llm::Stream, timeout: Duration) -> Result<String, String> {
    let mut out = String::new();
    let started = Instant::now();
    loop {
        match stream.rx.recv_timeout(Duration::from_millis(200)) {
            Ok(neo_llm::Event::Delta { content, .. }) => out.push_str(&content),
            Ok(neo_llm::Event::Done { .. }) => return Ok(out),
            Ok(neo_llm::Event::Failed(e)) => return Err(e),
            Ok(neo_llm::Event::ToolCall(_)) => {} // 静默轮不带工具，不会来
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if started.elapsed() > timeout {
                    stream.cancel.store(true, Ordering::Relaxed);
                    return Err("模型响应超时".into());
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(out),
        }
    }
}

/// 截一次全屏，送给视觉模型认科目 + 记笔记。
fn run_vision(cfg: &neo_llm::Config, first: bool) -> ClassEvent {
    use base64::Engine as _;
    // 静默截图：不广播截屏信号（不触发闪光动画与迷你窗回避）——
    // 后台监听不该惊扰正在上课的屏幕。
    let shot =
        match neo_tools::tools::screen::capture_silent(neo_tools::tools::screen::virtual_screen())
        {
        Ok(s) => s,
        Err(e) => return ClassEvent::VisionFailed(format!("截屏失败：{}", e.message)),
    };
    let png = match shot.to_png() {
        Ok(p) => p,
        Err(e) => return ClassEvent::VisionFailed(format!("编码截图失败：{}", e.message)),
    };
    let url = format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(png)
    );
    let prompt = if first {
        "这是一张课堂场景的屏幕截图（老师正在上课，屏幕上是课件或板书）。\n\
         请用中文回答：\n\
         1. 第一行只写「科目：X」（数学/语文/英语/物理/化学/生物/历史/地理/政治/信息/其他；认不出写「未知」）\n\
         2. 之后每行一条屏幕上的教学内容要点（板书标题、例题、概念），至多 5 条，每条一句话。\n\
         不要寒暄。"
    } else {
        "这是课堂进行中的屏幕截图。请用中文列出当前屏幕上的教学内容要点\
         （每行一条，至多 5 条，每条一句话）。不要寒暄，不要写科目行。"
    };
    let msgs = vec![Msg::new(Role::User, prompt).with_image(url)];
    let stream = neo_llm::start(cfg.clone(), msgs);
    match drain_stream(&stream, VISION_TIMEOUT) {
        Err(e) => ClassEvent::VisionFailed(e),
        Ok(text) => {
            if first {
                let mut lines = text.lines();
                let subject = lines.next().and_then(|l| {
                    let l = l.trim();
                    l.strip_prefix("科目：")
                        .or_else(|| l.strip_prefix("科目:"))
                        .map(|s| s.trim().to_owned())
                });
                let note = lines.collect::<Vec<_>>().join("\n").trim().to_owned();
                ClassEvent::Vision { subject, note }
            } else {
                ClassEvent::Vision {
                    subject: None,
                    note: text.trim().to_owned(),
                }
            }
        }
    }
}

/// 把一节课的素材打磨成 ≤1500 字的课堂总结。
fn run_polish(cfg: &neo_llm::Config, session: Session) -> ClassEvent {
    let system = "你是课堂记录整理器。把一节中学课的「屏幕笔记」和「老师讲课的语音转写」\
        整理成一份课堂总结。\n要求：\n\
        - 中文，Markdown，面向学生课后复习\n\
        - 结构：## 课堂要点（3~7 条）/ ## 内容展开（按主题）/ ## 作业与遗留（素材提到才写）\n\
        - 全文 1500 字以内\n\
        - 转写来自语音识别，可能有同音错字，按学科常识纠正\n\
        - 不要编造素材里没有的内容；素材少就少写，诚实优先";
    let mut user = format!(
        "科目：{}\n\n【屏幕笔记】\n",
        session.subject.as_deref().unwrap_or("未知")
    );
    for n in &session.screen_notes {
        user.push_str(&format!("- {n}\n"));
    }
    user.push_str("\n【讲课转写】\n");
    let joined = session.transcript.join(" ");
    let cut: String = joined.chars().take(POLISH_TRANSCRIPT_CHARS).collect();
    user.push_str(&cut);

    let msgs = vec![Msg::new(Role::System, system), Msg::new(Role::User, user)];
    let stream = neo_llm::start(cfg.clone(), msgs);
    match drain_stream(&stream, POLISH_TIMEOUT) {
        Ok(text) if !text.trim().is_empty() => ClassEvent::Polished {
            summary: text,
            session: Box::new(session),
        },
        Ok(_) => ClassEvent::PolishFailed {
            error: "模型返回了空总结".into(),
            session: Box::new(session),
        },
        Err(e) => ClassEvent::PolishFailed {
            error: e,
            session: Box::new(session),
        },
    }
}

// ---------------------------------------------------------------------------
// watch 线程：最大化沿检测 + 键盘活动时间戳 + 输入静默查询（Windows）
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod watch {
    use super::{ClassEvent, Duration};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{mpsc, Arc};
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows_sys::Win32::System::SystemInformation::GetTickCount64;
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, DispatchMessageW, GetClassNameW, GetForegroundWindow, GetSystemMetrics,
        GetWindowRect, GetWindowThreadProcessId, IsZoomed, PeekMessageW, SetWindowsHookExW,
        TranslateMessage, UnhookWindowsHookEx, MSG, PM_REMOVE, SM_CXSCREEN,
        SM_CYSCREEN, WH_KEYBOARD_LL,
    };

    /// 最近一次键盘活动的 tick（GetTickCount64 毫秒）。
    /// 钩子回调里**只写这个时间戳，不读键值** —— 感知「在打字」，
    /// 不感知「打了什么」，这是课堂场景能接受的最低侵入。
    static LAST_KEY_MS: AtomicU64 = AtomicU64::new(0);

    pub fn last_key_ms() -> u64 {
        LAST_KEY_MS.load(Ordering::Relaxed)
    }

    /// 距上次全局键鼠输入的时长（GetLastInputInfo，键鼠都算）。
    pub fn idle_duration() -> Duration {
        let mut lii = LASTINPUTINFO {
            cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
            dwTime: 0,
        };
        // SAFETY: lii 是本栈帧内的有效缓冲区，cbSize 已按契约填好。
        let ok = unsafe { GetLastInputInfo(&mut lii) };
        if ok == 0 {
            return Duration::ZERO;
        }
        let now = unsafe { GetTickCount64() };
        Duration::from_millis(now.saturating_sub(lii.dwTime as u64))
    }

    /// 启动监听线程；返回停止信号（置位即停）。
    pub fn start(tx: mpsc::Sender<ClassEvent>) -> Arc<AtomicBool> {
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let _ = std::thread::Builder::new()
            .name("neo-class-watch".into())
            .spawn(move || watch_main(tx, stop2));
        stop
    }

    fn watch_main(tx: mpsc::Sender<ClassEvent>, stop: Arc<AtomicBool>) {
        // 低级键盘钩子必须挂在有消息循环的线程上 —— 本线程边轮询边泵消息。
        // SAFETY: 回调是 'static 函数；线程退出前 Unhook。
        let hook = unsafe {
            SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), std::ptr::null_mut(), 0)
        };

        let mut maximized = false;
        while !stop.load(Ordering::Relaxed) {
            // 泵掉消息：低级钩子的回调靠本线程的消息循环派发。
            let mut msg: MSG = unsafe { std::mem::zeroed() };
            while unsafe { PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) } != 0 {
                unsafe {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }

            let now = foreground_is_teaching_window();
            if now != maximized {
                maximized = now;
                let ev = if now {
                    ClassEvent::Maximized
                } else {
                    ClassEvent::Unmaximized
                };
                if tx.send(ev).is_err() {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        if !hook.is_null() {
            unsafe { UnhookWindowsHookEx(hook) };
        }
    }

    /// 前台窗口是不是「在上课」：最大化，或无边框覆盖整个主屏（PPT 放映）。
    /// 排除 Neo 自己的窗口与桌面外壳。
    fn foreground_is_teaching_window() -> bool {
        let hwnd: HWND = unsafe { GetForegroundWindow() };
        if hwnd.is_null() {
            return false;
        }
        // 排除自己（Neo 主窗最大化 / 弹窗不算上课）。
        let mut pid: u32 = 0;
        unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        if pid == unsafe { GetCurrentProcessId() } {
            return false;
        }
        // 排除桌面与任务栏。
        let mut class = [0u16; 32];
        let n = unsafe { GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32) };
        if n > 0 {
            let name = String::from_utf16_lossy(&class[..n as usize]);
            if matches!(name.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd") {
                return false;
            }
        }
        if unsafe { IsZoomed(hwnd) } != 0 {
            return true;
        }
        // 全屏判定：窗口矩形覆盖主屏（放映类无边框窗口）。
        // 已知局限：副屏放映（矩形在扩展屏上）认不出来 —— 教室一体机
        // 多为单屏或复制模式，够用。
        let mut rect: RECT = unsafe { std::mem::zeroed() };
        if unsafe { GetWindowRect(hwnd, &mut rect) } == 0 {
            return false;
        }
        let sw = unsafe { GetSystemMetrics(SM_CXSCREEN) };
        let sh = unsafe { GetSystemMetrics(SM_CYSCREEN) };
        rect.left <= 0 && rect.top <= 0 && rect.right >= sw && rect.bottom >= sh
    }

    /// 键盘钩子回调：只记时间戳（毫秒），立即放行。
    unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        // HC_ACTION = 0：负数代码按契约必须直通，不能碰数据。
        if code >= 0 {
            LAST_KEY_MS.store(unsafe { GetTickCount64() }, Ordering::Relaxed);
        }
        // SAFETY: 链式调用是钩子契约。
        unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
    }
}

/// 非 Windows 只是编译保活：课堂监听为空壳（永不触发）。
#[cfg(not(windows))]
mod watch {
    use super::{ClassEvent, Duration};
    use std::sync::atomic::AtomicBool;
    use std::sync::{mpsc, Arc};

    pub fn last_key_ms() -> u64 {
        0
    }
    pub fn idle_duration() -> Duration {
        Duration::ZERO
    }
    pub fn start(_tx: mpsc::Sender<ClassEvent>) -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
    }
}
