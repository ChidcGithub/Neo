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
// 文本均按 UTF-8 字节计预算（比估算中文 token 更保守），截断只落在字符边界。
const SEGMENT_BYTES: usize = 3_072;
const NOTES_BYTES: usize = 24_576;
const TRANSCRIPT_BYTES: usize = 73_728;
const SUBJECT_BYTES: usize = 96;
// 两类素材合计最多 96 KiB；按至少 32K 上下文保守分配：请求 12 KiB、
// 返回 8 KiB，其余留给消息封装和单图。静默课堂关闭思考，不挤占返回预算。
const REQUEST_BYTES: usize = 12_288;
const REQUEST_NOTES_BYTES: usize = 4_096;
const REQUEST_TRANSCRIPT_BYTES: usize = 6_144;
const VISION_OUTPUT_BYTES: usize = 3_072;
const SUMMARY_BYTES: usize = 8_192;
const STREAM_BYTES: usize = 32_768;
const STREAM_EVENTS: usize = 8_192;
const IMAGE_EDGE: u32 = 1_280;
const IMAGE_BYTES: usize = 2 * 1024 * 1024;
// 展示、排队及打磨中的课共用四个槽；满时停止接新课，绝不覆盖失败结果。
const SUMMARY_SLOTS: usize = 4;
const LIMIT_NOTICE: &str = "\n（素材达到预算，已节选；优先保留近期内容）\n";
const OUTPUT_NOTICE: &str = "\n（模型输出达到安全预算，已停止生成并保留节选）\n";
const AUDIO_MISSING_NOTICE: &str = "部分课堂音频因缓冲过载或停止异常而缺失，转写可能不完整";

fn prefix(text: &str, bytes: usize) -> &str {
    let mut end = bytes.min(text.len());
    while !text.is_char_boundary(end) { end -= 1; }
    &text[..end]
}

fn suffix(text: &str, bytes: usize) -> &str {
    let mut start = text.len().saturating_sub(bytes);
    while !text.is_char_boundary(start) { start += 1; }
    &text[start..]
}

fn bounded_text(text: &str, bytes: usize) -> String {
    if text.len() <= bytes { return text.to_owned(); }
    let room = bytes.saturating_sub(LIMIT_NOTICE.len());
    // 单段保留开头的主题与较多的结尾，避免漏掉最近作业或结论。
    format!("{}{}{}", prefix(text, room / 4), LIMIT_NOTICE, suffix(text, room - room / 4))
}

fn push_material(items: &mut Vec<String>, text: &str, budget: usize) {
    if text.trim().is_empty() { return; }
    items.push(bounded_text(text, SEGMENT_BYTES));
    let mut bytes: usize = items.iter().map(|s| s.len() + 1).sum();
    let mut removed = false;
    while bytes + LIMIT_NOTICE.len() + 1 > budget && !items.is_empty() {
        bytes -= items.remove(0).len() + 1;
        removed = true;
    }
    if removed && items.first().is_none_or(|s| s != LIMIT_NOTICE) {
        items.insert(0, LIMIT_NOTICE.to_owned());
    }
}

fn class_warning(message: &'static str) {
    crate::diagnostics::record(crate::diagnostics::Level::Warn, "class", message);
}
/// 视觉分析的总超时（connect_timeout 15s 之外的总闸）。
const VISION_TIMEOUT: Duration = Duration::from_secs(90);
/// 打磨的总超时（素材长，生成慢）。
const POLISH_TIMEOUT: Duration = Duration::from_secs(240);

// ---------------------------------------------------------------------------
// 事件与结构
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ClassScope {
    generation: u64,
    session: u64,
}

struct ScopedEvent {
    scope: ClassScope,
    event: ClassEvent,
}

#[derive(Clone)]
struct ClassSender {
    tx: mpsc::Sender<ScopedEvent>,
    scope: ClassScope,
}

impl ClassSender {
    fn send(&self, event: ClassEvent) -> Result<(), mpsc::SendError<ScopedEvent>> {
        self.tx.send(ScopedEvent { scope: self.scope, event })
    }
}

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
    /// 音频过载或停流超时；只传固定提示，不传音频或设备详情。
    AudioMissing,
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
#[derive(Clone)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveState {
    Unsaved,
    IndexFailed,
    Saved,
}

/// 打磨完成不等于保存完成；失败素材随卡片保留，重试只执行尚未成功的阶段。
pub struct ReadySummary {
    pub subject: String,
    pub summary: String,
    pub over_limit: bool,
    /// 待保存结果创建时固定的目标日；落盘、索引和重试共用。
    pub date: String,
    pub save_state: SaveState,
    pub generated: bool,
    pub close_blocked: bool,
    pending_note: Option<neo_tools::classlog::NewClassNote>,
    index_text: Option<String>,
}

impl ReadySummary {
    fn new(session: Session, summary: String, generated: bool) -> Self {
        let excerpted = session.screen_notes.iter().any(|s| s.contains(LIMIT_NOTICE) || s.contains(OUTPUT_NOTICE))
            || session.transcript.iter().any(|s| s.contains(LIMIT_NOTICE))
            || session.screen_notes.iter().map(|s| s.len() + 1).sum::<usize>() > REQUEST_NOTES_BYTES
            || session.transcript.iter().map(|s| s.len() + 1).sum::<usize>() > REQUEST_TRANSCRIPT_BYTES;
        let summary = if excerpted {
            format!("{}{}", LIMIT_NOTICE, bounded_text(&summary, SUMMARY_BYTES - LIMIT_NOTICE.len()))
        } else { bounded_text(&summary, SUMMARY_BYTES) };
        let subject = bounded_text(session.subject.as_deref().unwrap_or("未知"), SUBJECT_BYTES);
        let pending_note = Some(neo_tools::classlog::NewClassNote {
            started_ms: session.started_ms,
            subject: subject.clone(),
            summary: summary.clone(),
            screen_notes: session.screen_notes,
            transcript: session.transcript,
        });
        Self {
            subject, over_limit: summary.chars().count() > neo_tools::classlog::SUMMARY_LIMIT,
            summary, date: neo_tools::classlog::today_key(), save_state: SaveState::Unsaved,
            generated, close_blocked: false, pending_note, index_text: None,
        }
    }

    fn retry(&mut self) {
        self.retry_with(neo_tools::classlog::append_class_on, |text| {
            neo_tools::tools::memory::add_memory(text).map(|_| ())
        });
    }

    fn retry_with(
        &mut self,
        mut append: impl FnMut(&str, neo_tools::classlog::NewClassNote) -> Result<neo_tools::classlog::ClassNote, neo_tools::ToolError>,
        mut index: impl FnMut(&str) -> Result<(), neo_tools::ToolError>,
    ) {
        if let Some(note) = &self.pending_note {
            let attempt = neo_tools::classlog::NewClassNote {
                started_ms: note.started_ms, subject: note.subject.clone(), summary: note.summary.clone(),
                screen_notes: note.screen_notes.clone(), transcript: note.transcript.clone(),
            };
            match append(&self.date, attempt) {
                Ok(saved) => {
                    self.over_limit = saved.over_limit;
                    let first_line: String = self.summary.lines()
                        .find(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
                        .unwrap_or("").trim().chars().take(50).collect();
                    self.index_text = Some(format!(
                        "{} {}课：{}（完整总结见分记忆 class/{}.json，第 {} 节）",
                        self.date, self.subject, first_line, self.date, saved.id
                    ));
                    self.pending_note = None;
                    self.save_state = SaveState::IndexFailed;
                }
                Err(_) => {
                    class_warning("课堂总结保存失败，结果保留待重试");
                    return;
                }
            }
        }
        if let Some(text) = &self.index_text {
            match index(text) {
                Ok(()) => {
                    self.index_text = None;
                    self.save_state = SaveState::Saved;
                    self.close_blocked = false;
                }
                Err(_) => class_warning("课堂总结已保存，记忆索引失败待重试"),
            }
        }
    }
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
    tx: ClassSender,
    rx: mpsc::Receiver<ScopedEvent>,
    enabled: bool,
    /// watch 线程停止信号（功能开着 = Some）。
    watch_stop: Option<Arc<AtomicBool>>,
    /// STT 线程停止信号（Active 期间 = Some）。
    stt_stop: Option<Arc<AtomicBool>>,
    /// 当前课的分析 worker 在飞标志（旧课打磨不阻塞新课）。
    analysis_in_flight: bool,
    /// 跨课复用的 STT 引擎；worker 持锁直到 reset/归还完成，防止并行加载。
    /// UI 只能 try_lock，不能等待重模型加载或尾音转写。
    stt_engine: Arc<std::sync::Mutex<Option<neo_stt::SttEngine>>>,
    cache_enabled: Arc<AtomicBool>,
    /// 只在功能关闭时撤销整代任务；正常下课及跨课打磨不置位。
    cancelled: Arc<AtomicBool>,
    stt_pending: Arc<std::sync::atomic::AtomicUsize>,
    /// 已见到的键盘活动时间戳（沿检测）。
    seen_key_ms: u64,
    /// 打磨结果，等弹窗取走。
    ready: Option<ReadySummary>,
    queued: std::collections::VecDeque<ReadySummary>,
    polishing: Vec<ClassScope>,
    // 有界备份让停用/安全模式切换无需等待 worker 也能保住已采集素材。
    polishing_material: Vec<(ClassScope, Session)>,
    paused_maximized: bool,
    /// 总结起止计数（开始打磨 +1、总结就绪 +1）：app 按差值点亮左上角红点。
    pub summary_pings: u64,
    /// 状态行（设置页显示「记录中…」之类）。
    status: String,
}

impl Default for ClassMonitor {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            phase: ClassPhase::Idle,
            tx: ClassSender { tx, scope: ClassScope::default() },
            rx,
            enabled: false,
            watch_stop: None,
            stt_stop: None,
            analysis_in_flight: false,
            stt_engine: Arc::new(std::sync::Mutex::new(None)),
            cache_enabled: Arc::new(AtomicBool::new(false)),
            cancelled: Arc::new(AtomicBool::new(false)),
            stt_pending: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            seen_key_ms: 0,
            ready: None,
            queued: Default::default(),
            polishing: Vec::new(),
            polishing_material: Vec::new(),
            paused_maximized: false,
            summary_pings: 0,
            status: String::new(),
        }
    }
}

impl ClassMonitor {
    /// 弹窗要展示的内容（有 = 正在展示）。
    pub fn presenting(&self) -> Option<&ReadySummary> {
        self.ready.as_ref()
    }

    /// 用户关掉弹窗：清掉待展示结果；只有状态机还在 Presenting 时才回 Idle
    ///（打磨与新课交叉时，phase 可能是进行中的 Active —— 弹窗照样要关得上）。
    pub fn dismiss(&mut self) {
        if let Some(ready) = &mut self.ready {
            if ready.save_state != SaveState::Saved {
                // 关闭失败卡片只显示保留提示，不能隐式丢弃尚未保存/索引的内容。
                ready.close_blocked = true;
                return;
            }
        }
        self.ready = self.queued.pop_front();
        if self.ready.is_none() && matches!(self.phase, ClassPhase::Presenting) {
            self.phase = ClassPhase::Idle;
            self.status.clear();
        }
    }

    pub fn retry_save(&mut self) {
        if let Some(ready) = &mut self.ready { ready.retry(); }
    }

    pub fn pending_saves(&self) -> usize {
        self.ready.iter().chain(self.queued.iter())
            .filter(|ready| ready.save_state != SaveState::Saved).count()
    }

    pub fn retry_all_saves(&mut self) -> bool {
        for ready in self.ready.iter_mut().chain(self.queued.iter_mut()) {
            ready.retry();
        }
        self.pending_saves() == 0
    }

    fn can_resume(&self) -> bool {
        self.enabled && self.paused_maximized && self.summary_slots() < SUMMARY_SLOTS
    }

    #[cfg(test)]
    pub(crate) fn seed_pending_summaries(&mut self) {
        for i in 0..SUMMARY_SLOTS {
            self.publish_summary(ReadySummary::new(Session::new(), format!("result{i}"), true), true, false);
        }
    }

    fn summary_slots(&self) -> usize {
        usize::from(self.ready.is_some()) + self.queued.len() + self.polishing.len()
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
            while let Ok(ev) = self.rx.try_recv() {
                self.on_event(ev);
            }
            return;
        }
        if !self.enabled {
            // 重新启用不能复活旧 worker，也不能等旧代尾音计数归零。
            self.cancelled = Arc::new(AtomicBool::new(false));
            self.cache_enabled = Arc::new(AtomicBool::new(true));
            self.stt_pending = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        }
        self.enabled = true;
        self.cache_enabled.store(true, Ordering::Relaxed);
        if self.watch_stop.is_none() {
            // None = 还没起，或上次 spawn 失败 —— 每帧重试，失败日志在 watch 里。
            self.watch_stop = watch::start(self.tx.clone());
        }
        // 慢轮询节奏：首屏延迟 / 冷却 / 静默计时都靠帧循环推进。
        ctx.request_repaint_after(Duration::from_millis(250));

        // 1) 收事件（drain，就地消化；phase 转移全在这里）。
        while let Ok(ev) = self.rx.try_recv() {
            self.on_event(ev);
        }

        // 满槽时保留最大化沿；释放槽位后不必让老师重新切一次课件。
        if self.can_resume() {
            self.on_event(ScopedEvent { scope: self.tx.scope, event: ClassEvent::Maximized });
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
                if due && !self.analysis_in_flight && self.stt_pending.load(Ordering::Acquire) == 0 {
                    pending = Some(Pending::Polish);
                }
            }
            _ => {}
        }
        match pending {
            Some(Pending::Shot(first)) => {
                let started = self.spawn_vision(state, first);
                if !started && first {
                    // 首屏被在飞任务/未配置挡下：留 5s 后重试 —— 它是这节课
                    // 唯一的科目来源，丢了这节课就永远「未知」。
                    if let ClassPhase::Active(_, slot) = &mut self.phase {
                        *slot = Some(Instant::now() + Duration::from_secs(5));
                    }
                }
            }
            Some(Pending::Polish) => {
                // pending == 0 的 acquire 之后再排一次：最后一条 Line 可能在
                // 本帧第一次 drain 与 worker 退出之间才入队。
                while let Ok(ev) = self.rx.try_recv() {
                    self.on_event(ev);
                }
                // drain 中可能发生「恢复 → 再退出」，重验新 worker 与静默期。
                let ClassPhase::Settling(_, exited_at) = &self.phase else {
                    return;
                };
                if self.analysis_in_flight || self.stt_pending.load(Ordering::Acquire) != 0
                    || (watch::idle_duration() < SETTLE_IDLE && exited_at.elapsed() < SETTLE_MAX)
                {
                    return;
                }
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
        self.cancelled.store(true, Ordering::Release);
        self.cache_enabled.store(false, Ordering::Relaxed);
        // 先收下已入队的素材/总结，不处理可能启动新采集的窗口沿。
        while let Ok(ev) = self.rx.try_recv() {
            if !matches!(&ev.event, ClassEvent::Maximized | ClassEvent::Unmaximized) {
                self.on_event(ev);
            }
        }
        let phase = std::mem::replace(&mut self.phase, ClassPhase::Idle);
        if let ClassPhase::Active(session, _) | ClassPhase::Settling(session, _) = phase {
            if !session.screen_notes.is_empty() || !session.transcript.is_empty() {
                let summary = fallback_summary(&session, "课堂采集已停止");
                self.publish_summary(ReadySummary::new(session, summary, false), true, true);
            }
        }
        for (_, session) in std::mem::take(&mut self.polishing_material) {
            let summary = fallback_summary(&session, "课堂整理已停止");
            self.publish_summary(ReadySummary::new(session, summary, false), true, true);
        }
        if self.enabled {
            self.enabled = false;
            self.tx.scope.generation += 1;
        }
        if let Some(stop) = self.watch_stop.take() {
            stop.store(true, Ordering::Relaxed);
        }
        self.stop_stt();
        self.phase = ClassPhase::Idle;
        // 关闭功能会撤销迟到结果，但已经展示的失败结果仍可离线重试。
        self.polishing.clear();
        self.paused_maximized = false;
        self.status.clear();
        // 新一代独立记账，已撤销代际的迟到结果不再保存或展示。
        self.analysis_in_flight = false;
        // 缓存的 STT 引擎（数百 MB 模型）随功能关闭释放。
        // 加载/转写持有锁时绝不在 UI 等待；worker 退出时负责释放。
        if let Ok(mut slot) = self.stt_engine.try_lock() {
            slot.take();
        }
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

    fn on_event(&mut self, ev: ScopedEvent) {
        let current_generation = self.enabled && ev.scope.generation == self.tx.scope.generation;
        let summary = matches!(&ev.event, ClassEvent::Polished { .. } | ClassEvent::PolishFailed { .. });
        let watch = matches!(&ev.event, ClassEvent::Maximized | ClassEvent::Unmaximized);
        if !current_generation || (!summary && !watch && ev.scope != self.tx.scope) {
            return;
        }
        if summary {
            let Some(index) = self.polishing.iter().position(|scope| *scope == ev.scope) else { return; };
            self.polishing.remove(index);
            self.polishing_material.retain(|(scope, _)| *scope != ev.scope);
        }
        if watch {
            self.paused_maximized = false;
        }
        if matches!(&ev.event, ClassEvent::Maximized)
            && matches!(self.phase, ClassPhase::Idle | ClassPhase::Polishing | ClassPhase::Presenting)
            && self.summary_slots() >= SUMMARY_SLOTS
        {
            self.paused_maximized = true;
            self.status = "课堂待处理结果已满，已暂停新课；请保存并关闭总结卡片".into();
            return;
        }
        match ev.event {
            ClassEvent::Maximized => match &mut self.phase {
                ClassPhase::Idle => {
                    self.begin_session();
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
                // Polishing / Presenting 期间最大化 = 新的一课：旧课结果随
                // worker 送回（finish 只存好结果，不掐进行中的新课）。
                // Presenting 的弹窗继续挂着，与新课的记录互不干扰 ——
                // 之前这里落入 `_ => {}`，弹窗不关则后续课程全部静默漏记。
                ClassPhase::Polishing | ClassPhase::Presenting => {
                    self.begin_session();
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
                #[allow(clippy::collapsible_match)] // match arm body keeps the phase filter separate from the content check
                ClassPhase::Active(session, _) | ClassPhase::Settling(session, _) => {
                    if !text.trim().is_empty() {
                        push_material(&mut session.transcript, &text, TRANSCRIPT_BYTES);
                    }
                }
                _ => {}
            },
            ClassEvent::AudioMissing => {
                class_warning(AUDIO_MISSING_NOTICE);
                self.status = AUDIO_MISSING_NOTICE.into();
            }
            ClassEvent::SttDied(_e) => {
                class_warning("课堂转写线程中断");
                // 必须进状态行：只写 stderr 的话，用户以为整节课在录音，
                // 实际转写早已中断 —— 课堂记录最怕「事后才发现没记上」。
                self.status = "课堂转写已中断，请检查麦克风和本地模型".into();
            }
            ClassEvent::Vision { subject, note } => {
                self.analysis_in_flight = false;
                match &mut self.phase {
                    ClassPhase::Active(session, _) | ClassPhase::Settling(session, _) => {
                        if let Some(s) = subject {
                            if !s.is_empty() && s != "未知" && session.subject.is_none() {
                                session.subject = Some(bounded_text(&s, SUBJECT_BYTES));
                            }
                        }
                        if !note.trim().is_empty() {
                            push_material(&mut session.screen_notes, &note, NOTES_BYTES);
                        }
                    }
                    _ => {}
                }
            }
            ClassEvent::VisionFailed(e) => {
                self.analysis_in_flight = false;
                class_warning("课堂视觉分析失败，本次截图已跳过");
                if e.contains("预算") {
                    if let ClassPhase::Active(session, _) | ClassPhase::Settling(session, _) = &mut self.phase {
                        push_material(&mut session.screen_notes, OUTPUT_NOTICE, NOTES_BYTES);
                    }
                }
            }
            ClassEvent::Polished { summary, session } => {
                let (publish, crossing) = self.complete_polish(ev.scope);
                self.finish(*session, summary, publish, crossing, true);
            }
            ClassEvent::PolishFailed { error, session } => {
                let (publish, crossing) = self.complete_polish(ev.scope);
                let fallback = fallback_summary(&session, &error);
                class_warning("课堂总结生成失败，已使用有界素材节选");
                self.finish(*session, fallback, publish, crossing, false);
            }
        }
    }

    fn begin_session(&mut self) {
        self.tx.scope.session += 1;
        self.analysis_in_flight = false;
    }

    fn complete_polish(&mut self, scope: ClassScope) -> (bool, bool) {
        let publish = self.enabled && scope.generation == self.tx.scope.generation;
        let current_session = publish && scope == self.tx.scope;
        if current_session {
            self.analysis_in_flight = false;
        }
        // 新课也可能已在 Polishing；不能只凭 phase 认领旧课结果。
        let crossing = !current_session || !matches!(self.phase, ClassPhase::Polishing);
        (publish, crossing)
    }

    /// 打磨完成（或兜底）：落分记忆 + 总记忆索引 + 弹窗。
    fn finish(&mut self, session: Session, summary: String, publish: bool, crossing: bool, generated: bool) {
        if !publish { return; }
        // 交叉新课只存结果与发布弹窗，不改新课的转写及 phase。
        if !crossing { self.stop_stt(); }
        let mut ready = ReadySummary::new(session, summary, generated);
        ready.retry();
        self.publish_summary(ready, publish, crossing);
    }

    fn publish_summary(&mut self, ready: ReadySummary, publish: bool, crossing: bool) {
        if !publish {
            return;
        }
        if self.ready.is_none() {
            self.ready = Some(ready);
        } else {
            // 新课开始前已经预留槽位，不允许失败卡片被后来结果覆盖。
            self.queued.push_back(ready);
        }
        self.summary_pings += 1; // 「总结完成」红点（交叉新课也算完成）
        if crossing {
            // 新课进行中：结果存进 ready 即可，弹窗会被用户看到；
            // phase 与状态行都别动新课的。
        } else {
            self.phase = ClassPhase::Presenting;
            self.status.clear();
        }
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
        let slot = self.stt_engine.clone();
        let cache_enabled = self.cache_enabled.clone();
        let cancelled = self.cancelled.clone();
        let pending = self.stt_pending.clone();
        pending.fetch_add(1, Ordering::Release);
        let done = SttDone(pending);
        let spawned = std::thread::Builder::new()
            .name("neo-class-stt".into())
            .spawn(move || {
                let _done = done;
                // 在 pending 归零前上报异常，避免静默漏记；不捕获或记录语音内容。
                let failed = tx.clone();
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    stt_main(tx, stop2, slot, cache_enabled, cancelled);
                })).is_err() {
                    let _ = failed.send(ClassEvent::SttDied("转写线程异常中断".into()));
                }
            });
        match spawned {
            Ok(_) => self.stt_stop = Some(stop),
            Err(e) => {
                let _ = self.tx.send(ClassEvent::SttDied(format!("spawn: {e}")));
            }
        }
    }

    /// 截屏 + 视觉分析（一次一个；飞行中来了直接丢，冷却外还会再触发）。
    /// 返回是否真的启动了 —— 首屏被挡下时调用方要留标记重试。
    fn spawn_vision(&mut self, state: &AppState, first: bool) -> bool {
        if self.analysis_in_flight {
            return false;
        }
        let Some(cfg) = llm_config(state) else {
            return false; // 没配模型：视觉分析整体跳过，转写不受影响
        };
        self.analysis_in_flight = true;
        let tx = self.tx.clone();
        let cancelled = self.cancelled.clone();
        let spawned = std::thread::Builder::new()
            .name("neo-class-vision".into())
            .spawn(move || {
                // worker panic（GDI 抓帧 / 编码 / 网络栈任何一处展开）时不会有
                // 任何事件回传，analysis_in_flight 永久卡死状态机。guard 兜底。
                struct VisionGuard {
                    tx: ClassSender,
                    armed: bool,
                }
                impl Drop for VisionGuard {
                    fn drop(&mut self) {
                        if self.armed {
                            let _ = self
                                .tx
                                .send(ClassEvent::VisionFailed("分析线程异常中断".into()));
                        }
                    }
                }
                let mut guard = VisionGuard {
                    tx: tx.clone(),
                    armed: true,
                };
                let ev = run_vision(&cfg, first, &cancelled);
                let _ = tx.send(ev);
                guard.armed = false;
            });
        match spawned {
            Ok(_) => true,
            Err(_) => {
                // 线程起不来：没有 worker 也就永远等不到结果事件，当场复位。
                self.analysis_in_flight = false;
                class_warning("课堂视觉分析线程启动失败");
                false
            }
        }
    }

    /// 打磨收尾（调用方保证 !analysis_in_flight，且已把 phase 转到 Polishing）。
    fn spawn_polish(&mut self, state: &AppState, session: Session) {
        self.polishing.push(self.tx.scope);
        self.polishing_material.push((self.tx.scope, session.clone()));
        self.analysis_in_flight = true;
        self.status = "正在整理课堂总结…".into();
        self.summary_pings += 1; // 「开始总结」红点
        let tx = self.tx.clone();
        let Some(cfg) = llm_config(state) else {
            let _ = tx.send(ClassEvent::PolishFailed {
                error: "未配置模型接口".into(),
                session: Box::new(session),
            });
            return;
        };
        // spawn 失败时闭包整个被丢弃、session 随之消失 —— 放进共享槽，
        // 失败路径还能取回来（这节课的素材不能丢得无声无息）。
        let slot = Arc::new(std::sync::Mutex::new(Some(session)));
        let slot2 = slot.clone();
        let cancelled = self.cancelled.clone();
        let spawned = std::thread::Builder::new()
            .name("neo-class-polish".into())
            .spawn(move || {
                let session = slot2.lock().unwrap().take().unwrap_or_else(Session::new);
                // 同 VisionGuard：panic 展开时用 Drop 补发失败事件，
                // session 还在 guard 手里，素材不丢。
                struct PolishGuard {
                    tx: ClassSender,
                    session: Option<Session>,
                }
                impl Drop for PolishGuard {
                    fn drop(&mut self) {
                        if let Some(session) = self.session.take() {
                            let _ = self.tx.send(ClassEvent::PolishFailed {
                                error: "整理线程异常中断".into(),
                                session: Box::new(session),
                            });
                        }
                    }
                }
                let mut guard = PolishGuard {
                    tx: tx.clone(),
                    session: Some(session),
                };
                let result = match guard.session.as_ref() {
                    Some(s) => run_polish(&cfg, s, &cancelled),
                    None => Err("内部错误：打磨会话丢失".into()),
                };
                // 取出即解除 guard（正常路径不重复发事件）。
                let session = guard
                    .session
                    .take()
                    .map(Box::new)
                    .unwrap_or_else(|| Box::new(Session::new()));
                let ev = match result {
                    Ok(summary) => ClassEvent::Polished { summary, session },
                    Err(error) => ClassEvent::PolishFailed { error, session },
                };
                let _ = tx.send(ev);
            });
        if let Err(e) = spawned {
            let session = slot.lock().unwrap().take().unwrap_or_else(Session::new);
            let _ = self.tx.send(ClassEvent::PolishFailed {
                error: format!("无法启动整理线程：{e}"),
                session: Box::new(session),
            });
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
        let mut cfg = state.llm_config();
        cfg.thinking = neo_llm::Thinking::Off;
        Some(cfg)
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

/// 请求及兜底共用节选，不把网络原始错误或超长素材带进卡片。
fn material_excerpt(session: &Session, notes: usize, transcript: usize) -> String {
    format!("科目：{}\n\n【屏幕笔记】\n{}\n\n【讲课转写】\n{}",
        bounded_text(session.subject.as_deref().unwrap_or("未知"), SUBJECT_BYTES),
        bounded_text(&session.screen_notes.join("\n"), notes),
        bounded_text(&session.transcript.join(" "), transcript))
}

fn fallback_summary(session: &Session, err: &str) -> String {
    let reason = if err.contains("预算") { "自动总结达到安全预算，已停止" } else { "自动总结失败" };
    bounded_text(&format!("（{reason}。以下为原始记录节选，未整理）\n{}",
        material_excerpt(session, 2_048, 4_096)), SUMMARY_BYTES)
}

// ---------------------------------------------------------------------------
// STT 线程：AudioTap 采集 + SttEngine 连续转写
// ---------------------------------------------------------------------------

struct SttDone(Arc<std::sync::atomic::AtomicUsize>);

impl Drop for SttDone {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Release);
    }
}

fn stt_main(
    tx: ClassSender,
    stop: Arc<AtomicBool>,
    engine_slot: Arc<std::sync::Mutex<Option<neo_stt::SttEngine>>>,
    cache_enabled: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
) {
    // 租约覆盖加载、采集、flush 和归还全过程。只有后台 worker 等待，
    // UI 关闭仅 try_lock；None 不再被误认成「别的 worker 没在用」。
    let mut lease = loop {
        if stop.load(Ordering::Relaxed) || cancelled.load(Ordering::Acquire) {
            return;
        }
        match engine_slot.try_lock() {
            Ok(lease) => break lease,
            Err(std::sync::TryLockError::Poisoned(p)) => break p.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    };
    if stop.load(Ordering::Relaxed) || cancelled.load(Ordering::Acquire) {
        return;
    }
    let cached = lease.take();
    let engine = match cached {
        Some(e) => e,
        None => match neo_stt::SttEngine::create(&neo_stt::SttConfig::default()) {
            Ok(e) => e,
            Err(e) => {
                let _ = tx.send(ClassEvent::SttDied(format!("加载 STT 模型失败：{e}")));
                return;
            }
        },
    };
    if stop.load(Ordering::Relaxed) || cancelled.load(Ordering::Acquire) {
        if cache_enabled.load(Ordering::Relaxed) && !cancelled.load(Ordering::Acquire) {
            *lease = Some(engine);
        }
        return;
    }
    let mut tap = match neo_wake::AudioTap::start_with_stop(stop.clone(), cancelled.clone()) {
        Ok(t) => t,
        Err(e) => {
            // 没用过的引擎放回去，下节课直接复用。
            if cache_enabled.load(Ordering::Relaxed) && !cancelled.load(Ordering::Acquire) {
                *lease = Some(engine);
            }
            if !stop.load(Ordering::Relaxed) && !cancelled.load(Ordering::Acquire) {
                let _ = tx.send(ClassEvent::SttDied(format!("打开麦克风失败：{e}")));
            }
            return;
        }
    };
    eprintln!("[neo-class] 课堂转写开始（{}）", tap.device());

    // 正常下课排空尾音；整代取消则不再启动下一次 VAD/转写。
    let mut audio_warning_sent = false;
    while !stop.load(Ordering::Relaxed) && !cancelled.load(Ordering::Acquire) {
        report_missing_audio(tap.has_missing_audio(), &mut audio_warning_sent, &tx, &cancelled);
        let Some(frame) = tap.next_frame(Duration::from_millis(50)) else {
            // 超时空转是常态；但 tap 线程死了（设备拔出/流错误）会持续
            // None —— 不上报的话课堂录音会静默录进一片空白。
            if !tap.is_alive() {
                if !stop.load(Ordering::Relaxed) && !cancelled.load(Ordering::Acquire) {
                    let _ = tx.send(ClassEvent::SttDied(
                        "麦克风连接中断（设备被拔出或被安全软件拦截），请重新开启课堂记录".into(),
                    ));
                }
                break;
            }
            continue;
        };
        if cancelled.load(Ordering::Acquire) {
            break;
        }
        engine.accept_waveform(&frame);
        transcribe_segments(&engine, &tx, &cancelled);
    }
    // 有界等待停流，再排空已入队音频；驱动卡住也不能阻塞收尾。
    if !cancelled.load(Ordering::Acquire) {
        let stopped = tap.stop_capture();
        report_missing_audio(!stopped || tap.has_missing_audio(), &mut audio_warning_sent, &tx, &cancelled);
    }
    while !cancelled.load(Ordering::Acquire) {
        let Some(frame) = tap.next_frame(Duration::ZERO) else { break };
        engine.accept_waveform(&frame);
        transcribe_segments(&engine, &tx, &cancelled);
    }
    if !cancelled.load(Ordering::Acquire) {
        engine.flush();
        transcribe_segments(&engine, &tx, &cancelled);
    }
    engine.reset();
    if cache_enabled.load(Ordering::Relaxed) && !cancelled.load(Ordering::Acquire) {
        *lease = Some(engine);
    }
    eprintln!("[neo-class] 课堂转写结束");
}

fn report_missing_audio(missing: bool, sent: &mut bool, tx: &ClassSender, cancelled: &AtomicBool) {
    if missing && !*sent && !cancelled.load(Ordering::Acquire) {
        let _ = tx.send(ClassEvent::AudioMissing);
        *sent = true;
    }
}

fn transcribe_segments(engine: &neo_stt::SttEngine, tx: &ClassSender, cancelled: &AtomicBool) {
    while !cancelled.load(Ordering::Acquire) {
        let Some(seg) = engine.take_segment() else { break };
        if cancelled.load(Ordering::Acquire) {
            break;
        }
        match engine.transcribe(&seg) {
            Ok(text) => {
                if cancelled.load(Ordering::Acquire)
                    || tx.send(ClassEvent::Line(bounded_text(&text, SEGMENT_BYTES))).is_err() {
                    break;
                }
            }
            Err(_) => class_warning("课堂语音片段转写失败，本段已跳过"),
        }
    }
}

// ---------------------------------------------------------------------------
// 分析 worker：视觉分析 + 打磨（共用 neo_llm::start 静默流）
// ---------------------------------------------------------------------------

// 截屏、素材整理与流消费共用绝对截止，不因中间阶段或队列积压重置。
fn check_job(cancelled: &AtomicBool, deadline: Instant) -> Result<(), String> {
    if cancelled.load(Ordering::Acquire) {
        Err("课堂任务已取消".into())
    } else if Instant::now() >= deadline {
        Err("模型响应超时".into())
    } else {
        Ok(())
    }
}

#[cfg(test)]
fn drain_stream(stream: &neo_llm::Stream, deadline: Instant, cancelled: &AtomicBool) -> Result<String, String> {
    drain_limited(stream, deadline, cancelled, SUMMARY_BYTES)
}

fn drain_limited(stream: &neo_llm::Stream, deadline: Instant, cancelled: &AtomicBool, output_bytes: usize) -> Result<String, String> {
    let mut out = String::new();
    let mut received = 0usize;
    let mut events = 0usize;
    loop {
        if let Err(e) = check_job(cancelled, deadline) {
            // 协作取消不回滚已上传内容，也不能立即中断阻塞中的 HTTP。
            stream.cancel.store(true, Ordering::Relaxed);
            return Err(e);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        let event = stream.rx.recv_timeout(remaining.min(Duration::from_millis(200)));
        if let Err(e) = check_job(cancelled, deadline) {
            stream.cancel.store(true, Ordering::Relaxed);
            return Err(e);
        }
        if event.is_ok() { events += 1; }
        if events >= STREAM_EVENTS {
            stream.cancel.store(true, Ordering::Relaxed);
            return Err("模型流事件达到安全预算，已停止".into());
        }
        match event {
            Ok(neo_llm::Event::Delta { content, reasoning }) => {
                received = received.saturating_add(content.len()).saturating_add(reasoning.len());
                let room = output_bytes.saturating_sub(OUTPUT_NOTICE.len()).saturating_sub(out.len());
                out.push_str(prefix(&content, room));
                if content.len() >= room || received >= STREAM_BYTES {
                    stream.cancel.store(true, Ordering::Relaxed);
                    if out.trim().is_empty() { return Err("模型流文本达到安全预算，已停止".into()); }
                    out.push_str(OUTPUT_NOTICE);
                    return Ok(out);
                }
            }
            Ok(neo_llm::Event::Done { .. }) => return Ok(out),
            Ok(neo_llm::Event::Failed(_)) => return Err("模型请求失败".into()),
            Ok(neo_llm::Event::ToolCall(_)) => {
                stream.cancel.store(true, Ordering::Relaxed);
                return Err("课堂静默请求收到意外工具调用，已停止".into());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err("模型响应中断（未收到完成标记）".into()),
        }
    }
}

/// 截一次全屏，送给视觉模型认科目 + 记笔记。
fn run_vision(cfg: &neo_llm::Config, first: bool, cancelled: &AtomicBool) -> ClassEvent {
    use base64::Engine as _;
    let deadline = Instant::now() + VISION_TIMEOUT;
    if let Err(e) = check_job(cancelled, deadline) {
        return ClassEvent::VisionFailed(e);
    }
    // 静默截图：不广播截屏信号（不触发闪光动画与迷你窗回避）——
    // 后台监听不该惊扰正在上课的屏幕。
    let shot =
        match neo_tools::tools::screen::capture_silent(neo_tools::tools::screen::virtual_screen())
        {
        Ok(s) => s,
        Err(e) => return ClassEvent::VisionFailed(format!("截屏失败：{}", e.message)),
    };
    if let Err(e) = check_job(cancelled, deadline) {
        return ClassEvent::VisionFailed(e);
    }
    let Some(image) = image::RgbaImage::from_raw(shot.width, shot.height, shot.rgba) else {
        return ClassEvent::VisionFailed("截图像素格式错误".into());
    };
    // 沿用截图公开的 RGBA 接口，课堂只上传单张缩略 JPEG，不扩展通用截图 API。
    let image = image::DynamicImage::ImageRgba8(image).thumbnail(IMAGE_EDGE, IMAGE_EDGE).to_rgb8();
    let mut jpeg = Vec::new();
    if image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 80).encode_image(&image).is_err() {
        return ClassEvent::VisionFailed("截图编码失败".into());
    }
    if jpeg.len() > IMAGE_BYTES {
        return ClassEvent::VisionFailed("截图达到上传预算，已跳过".into());
    }
    let url = format!(
        "data:image/jpeg;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(jpeg)
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
    if let Err(e) = check_job(cancelled, deadline) {
        return ClassEvent::VisionFailed(e);
    }
    let stream = neo_llm::start(cfg.clone(), msgs);
    match drain_limited(&stream, deadline, cancelled, VISION_OUTPUT_BYTES) {
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
/// 只借用 session：worker 持有所有权，panic 兜底时素材还能随失败事件送回。
fn run_polish(cfg: &neo_llm::Config, session: &Session, cancelled: &AtomicBool) -> Result<String, String> {
    let deadline = Instant::now() + POLISH_TIMEOUT;
    check_job(cancelled, deadline)?;
    let system = "你是课堂记录整理器。把一节中学课的「屏幕笔记」和「老师讲课的语音转写」\
        整理成一份课堂总结。\n要求：\n\
        - 中文，Markdown，面向学生课后复习\n\
        - 结构：## 课堂要点（3~7 条）/ ## 内容展开（按主题）/ ## 作业与遗留（素材提到才写）\n\
        - 全文 1500 字以内\n\
        - 转写来自语音识别，可能有同音错字，按学科常识纠正\n\
        - 不要编造素材里没有的内容；素材少就少写，诚实优先";
    let user = material_excerpt(session, REQUEST_NOTES_BYTES, REQUEST_TRANSCRIPT_BYTES);
    if system.len() + user.len() > REQUEST_BYTES {
        return Err("课堂请求达到安全预算，未发送".into());
    }
    let msgs = vec![Msg::new(Role::System, system), Msg::new(Role::User, user)];
    check_job(cancelled, deadline)?;
    let stream = neo_llm::start(cfg.clone(), msgs);
    match drain_limited(&stream, deadline, cancelled, SUMMARY_BYTES) {
        Ok(text) if !text.trim().is_empty() => Ok(text),
        Ok(_) => Err("模型返回了空总结".into()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
#[path = "class_regression_tests.rs"]
mod regression_tests;

// ---------------------------------------------------------------------------
// watch 线程：最大化沿检测 + 键盘活动时间戳 + 输入静默查询（Windows）
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod watch {
    use super::{ClassEvent, ClassSender, Duration};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::Arc;
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows_sys::Win32::System::SystemInformation::{GetTickCount, GetTickCount64};
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
        // dwTime 与 GetTickCount 同源、都是 32 位：开机超 49.7 天回绕时，
        // 拿 64 位值去减会恒得「约 49.7 天」—— SETTLE_IDLE 判定静默失效
        //（退出最大化立即打磨）。同位宽 wrapping_sub 天然消化回绕。
        let now = unsafe { GetTickCount() };
        Duration::from_millis(now.wrapping_sub(lii.dwTime) as u64)
    }

    /// 启动监听线程；返回停止信号（置位即停）。spawn 失败返回 None ——
    /// 调用方（tick）下一帧会重试，别让「开关开着但什么都没在跑」静默发生。
    pub fn start(tx: ClassSender) -> Option<Arc<AtomicBool>> {
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        match std::thread::Builder::new()
            .name("neo-class-watch".into())
            .spawn(move || watch_main(tx, stop2))
        {
            Ok(_) => Some(stop),
            Err(e) => {
                eprintln!("[neo-class] watch 线程启动失败（下一帧重试）：{e}");
                None
            }
        }
    }

    fn watch_main(tx: ClassSender, stop: Arc<AtomicBool>) {
        if stop.load(Ordering::Relaxed) {
            return;
        }
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
            // 泵节奏 100ms：钩子回调靠本线程消息循环派发，泵得太慢会逼近
            // LowLevelHooksTimeout 被系统静默摘钩（键盘沿截屏无声消失）；
            // 100ms 下检测沿的额外延迟无感（首屏延迟 1.2s / 冷却 10s）。
            std::thread::sleep(Duration::from_millis(100));
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
    use super::Duration;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    pub fn last_key_ms() -> u64 {
        0
    }
    pub fn idle_duration() -> Duration {
        Duration::ZERO
    }
    pub fn start(_tx: super::ClassSender) -> Option<Arc<AtomicBool>> {
        Some(Arc::new(AtomicBool::new(false)))
    }
}
