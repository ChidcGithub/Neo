//! 语音唤醒引擎：复刻 livekit-wakeword 的推理链路。
//!
//! 链路（与 Python 版逐一对应）：
//!   麦克风 16kHz i16 → 1280 样本（80ms）一帧 → 25 帧（2s）滑窗
//!   → melspectrogram.onnx（输出 x/10+2）→ 76/8 滑窗 → embedding_model.onnx
//!   → 最后 16 个 96 维 embedding → hi_neo.onnx → score
//!   → 连续 CONFIRM_FRAMES 帧 score >= threshold 且过 debounce → WakeEvent::Detected

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ort::session::{builder::GraphOptimizationLevel, Session};
use ort::value::Tensor;

// ---- 与 livekit-wakeword defaults 完全一致的常量 ----
pub const SAMPLE_RATE: u32 = 16_000;
const FRAME_SAMPLES: usize = 1280; // 80ms / 帧
const CHUNK_FRAMES: usize = 25; // 2s 滑窗
const EMB_WINDOW: usize = 76; // mel 帧 / embedding
const EMB_STRIDE: usize = 8;
const MIN_EMBEDDINGS: usize = 16;
const MEL_BINS: usize = 32;
const EMB_DIM: usize = 96;

/// 连续确认帧数：滑窗每 80ms 预测一次，要求**连续 2 帧**过线才触发。
/// 「随便说几个字」的误触发几乎都是单帧尖峰；「嗨，Neo」持续约 0.6s，
/// 覆盖 5~7 个连续窗口，稳稳两连。阈值卡在真唤醒得分下缘（≈0.2）时，
/// 这个比硬提阈值更能保住唤醒率。
const CONFIRM_FRAMES: usize = 2;

/// 采集回调 → 引擎线程的缓冲块数。回调线程绝不能阻塞（实时音频线程），
/// 满了就丢**新**块：积压说明推理跟不上实时，丢新块让引擎追当前
/// （唤醒只看最近 2s 滑窗，丢一块等于一个几十毫秒的洞，比越落越远强）。
const SAMPLE_CHUNKS: usize = 8;

/// 麦克风静默看门狗：流活着时数据回调按设备周期持续到达（静音也是零
/// 数据帧，不是没回调）。超过它一个样本都没到 = 流实质已死（拔出且无
/// 后继设备等），必须上报而不是永远空转装没事。
const MIC_SILENCE_TIMEOUT: Duration = Duration::from_secs(5);

/// Unix 毫秒（0 表示「还没有」）。
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Debug, Clone)]
pub struct WakeConfig {
    /// 含 melspectrogram.onnx / embedding_model.onnx / hi_neo.onnx / onnxruntime.dll 的目录
    pub model_dir: PathBuf,
    pub threshold: f32,
    pub debounce: Duration,
}

impl Default for WakeConfig {
    fn default() -> Self {
        Self {
            // 模型目录查找顺序：NEO_WAKE_MODEL_DIR 环境变量
            // > exe 同级 assets/（release 打包布局）
            // > 源码内 assets/（cargo run 开发布局）
            model_dir: default_model_dir(),
            // 默认 0.25：混入机主真人录音重训后，实测真唤醒 0.34~0.83，
            // 日常说话/环境噪声峰值 ≤0.01 —— 两侧都是几十倍距离；
            // 再叠加连续 2 帧确认（CONFIRM_FRAMES），误触发基本没有空间。
            // 调试期可用 NEO_WAKE_THRESHOLD=0.3 之类临时压阈值试灵敏度，
            // 不必改代码重新构建。
            threshold: std::env::var("NEO_WAKE_THRESHOLD")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.25),
            debounce: Duration::from_secs(2),
        }
    }
}

/// 模型目录回退链：环境变量 > exe 同级 assets/ > 源码 assets/
fn default_model_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("NEO_WAKE_MODEL_DIR") {
        let dir = PathBuf::from(dir);
        if dir.is_dir() {
            return dir;
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let dir = dir.join("assets");
            if dir.is_dir() {
                return dir;
            }
        }
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("assets")
}

#[derive(Debug, Clone)]
pub enum WakeEvent {
    /// epoch 为生成时的测试模式代次，转发时不得重新标记。
    Detected { epoch: u64, score: f32 },
    /// 听写模式下转发的 16kHz 单声道 f32 音频块（每块 80ms / 1280 样本）
    Audio { epoch: u64, frame: Vec<f32> },
    /// 引擎线程遇到无法恢复的错误后退出（模型缺失 / 无麦克风 / 推理失败）
    Error(String),
}

impl WakeEvent {
    /// 在最终消费端校验：进出测试前已发送、但仍滞留于转发线程的事件也会失效。
    /// 错误不受代次或测试开关影响；普通听写模式切换不改变此代次。
    pub fn is_current(&self, current_epoch: u64) -> bool {
        match self {
            Self::Detected { epoch, .. } | Self::Audio { epoch, .. } => {
                current_epoch & 1 == 0 && *epoch == current_epoch
            }
            Self::Error(_) => true,
        }
    }
}

// 引擎工作模式（AtomicU8 编码）
const MODE_DETECT: u8 = 0; // 常规：只跑唤醒词检测
const MODE_DICTATE: u8 = 1; // 唤醒后听写：转发音频帧，暂停唤醒检测（省 CPU、防复读）

/// 生命周期状态；Ready 表示流已启动，是否填满推理窗口看 warmup_frames。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakePhase {
    Loading,
    Ready,
    Error,
    Stopped,
}

/// 单份内存快照，不包含音频或识别文本。所有 *_ms 都是 Unix 毫秒，0 表示尚无数据。
#[derive(Debug, Clone)]
pub struct WakeDiagnostics {
    pub enabled: bool,
    pub test_mode: bool,
    pub phase: WakePhase,
    /// 启动时选中的默认输入设备；设备自动切换后名称可能已过时。
    pub device_name: Option<String>,
    pub sample_rate: Option<u32>,
    /// 归一化单声道输入电平；None 表示未启用或尚无样本，静音为 -120 dBFS。
    pub rms: Option<f32>,
    pub dbfs: Option<f32>,
    pub peak: Option<f32>,
    pub score: Option<f32>,
    pub score_peak_2s: Option<f32>,
    pub threshold: f32,
    pub warmup_frames: usize,
    pub warmup_total: usize,
    pub dictating: bool,
    /// 整个引擎实例的确认命中数（包括测试模式），不是单帧过阈值次数。
    pub hit_count: u64,
    pub last_hit_ms: u64,
    pub updated_at_ms: u64,
    pub last_audio_ms: u64,
    /// 最后一次错误保持到实例销毁，不写日志；最多 512 个字符。
    pub error: Option<String>,
}

struct WakeMonitor {
    enabled: AtomicBool,
    // 最低位为测试开关，高位为版本；快速 on/off 也必须被工作线程观察到。
    test_epoch: AtomicU64,
    snapshot: Mutex<WakeDiagnostics>,
}

impl WakeMonitor {
    fn new(threshold: f32) -> Self {
        Self {
            enabled: AtomicBool::new(false),
            test_epoch: AtomicU64::new(0),
            snapshot: Mutex::new(WakeDiagnostics {
                enabled: false, test_mode: false, phase: WakePhase::Loading,
                device_name: None, sample_rate: None, rms: None, dbfs: None, peak: None,
                score: None, score_peak_2s: None, threshold, warmup_frames: 0,
                warmup_total: CHUNK_FRAMES, dictating: false, hit_count: 0,
                last_hit_ms: 0, updated_at_ms: now_ms(), last_audio_ms: 0, error: None,
            }),
        }
    }

    fn update(&self, update: impl FnOnce(&mut WakeDiagnostics)) {
        let mut state = self.snapshot.lock().unwrap_or_else(|p| p.into_inner());
        update(&mut state);
        state.updated_at_ms = now_ms();
    }

    fn fail(&self, error: &str) {
        self.update(|s| {
            s.phase = WakePhase::Error;
            s.error = Some(error.chars().take(512).collect());
        });
    }

    fn set_test_mode(&self, on: bool) {
        self.update(|s| {
            if s.test_mode != on {
                s.test_mode = on;
                self.test_epoch.fetch_add(1, Ordering::Release);
                s.warmup_frames = 0;
                s.score = None;
                s.score_peak_2s = None;
            }
        });
    }

    // 与 set_test_mode 共用锁，返回后工作线程不会再发送测试开始前的命中。
    fn detected(&self, epoch: u64, score: f32, tx: &mpsc::Sender<WakeEvent>) -> bool {
        let mut s = self.snapshot.lock().unwrap_or_else(|p| p.into_inner());
        if self.test_epoch.load(Ordering::Acquire) != epoch {
            return true;
        }
        s.hit_count = s.hit_count.saturating_add(1);
        s.last_hit_ms = now_ms();
        s.updated_at_ms = s.last_hit_ms;
        s.test_mode || tx.send(WakeEvent::Detected { epoch, score }).is_ok()
    }
}

const DIAGNOSTIC_INTERVAL: Duration = Duration::from_millis(200);
const SCORE_HISTORY: usize = 32;

struct DiagnosticMeter {
    enabled: bool,
    published: Instant,
    power: f64,
    samples: usize,
    peak: f32,
    score: Option<f32>,
    scores: VecDeque<(Instant, f32)>,
    last_audio_ms: u64,
}

impl DiagnosticMeter {
    fn new(now: Instant) -> Self {
        Self { enabled: false, published: now, power: 0.0, samples: 0,
            peak: 0.0, score: None, scores: VecDeque::with_capacity(SCORE_HISTORY), last_audio_ms: 0 }
    }

    fn enable(&mut self, enabled: bool, now: Instant) {
        if self.enabled != enabled {
            *self = Self::new(now);
            self.enabled = enabled;
        }
    }

    fn audio(&mut self, raw: &[i16]) {
        if !self.enabled || raw.is_empty() { return; }
        for &sample in raw {
            let sample = sample as f32 / 32768.0;
            self.power += (sample as f64).powi(2);
            self.peak = self.peak.max(sample.abs());
        }
        self.samples += raw.len();
        self.last_audio_ms = now_ms();
    }

    fn score(&mut self, now: Instant, score: f32) {
        if !self.enabled { return; }
        self.score = Some(score);
        if self.scores.len() == SCORE_HISTORY { self.scores.pop_front(); }
        self.scores.push_back((now, score));
    }

    fn publish(&mut self, monitor: &WakeMonitor, now: Instant, warmup: usize, dictating: bool) {
        if !self.enabled || now.duration_since(self.published) < DIAGNOSTIC_INTERVAL { return; }
        while self.scores.front().is_some_and(|(t, _)| now.duration_since(*t) >= Duration::from_secs(2)) {
            self.scores.pop_front();
        }
        monitor.update(|s| {
            if !s.enabled { return; }
            s.rms = (self.samples > 0).then(|| (self.power / self.samples as f64).sqrt() as f32);
            s.dbfs = s.rms.map(|rms| 20.0 * rms.max(1e-6).log10());
            s.peak = (self.samples > 0).then_some(self.peak);
            s.score = self.score;
            s.score_peak_2s = self.scores.iter().map(|(_, s)| *s).reduce(f32::max);
            s.warmup_frames = warmup;
            s.dictating = dictating;
            s.last_audio_ms = self.last_audio_ms;
        });
        self.published = now;
        self.power = 0.0;
        self.samples = 0;
        self.peak = 0.0;
    }
}

pub struct WakeEngine {
    stop: Arc<AtomicBool>,
    mode: Arc<AtomicU8>,
    monitor: Arc<WakeMonitor>,
    thread: Option<JoinHandle<()>>,
}

impl WakeEngine {
    /// 启动后台采集 + 推理线程，返回事件接收端。
    /// hi_neo.onnx 尚未训练出来时也会正常返回，错误通过 WakeEvent::Error 上报。
    pub fn start(config: WakeConfig) -> (Self, mpsc::Receiver<WakeEvent>) {
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let mode = Arc::new(AtomicU8::new(MODE_DETECT));
        let mode2 = mode.clone();
        let monitor = Arc::new(WakeMonitor::new(config.threshold));
        let monitor2 = monitor.clone();
        let failed = tx.clone();
        let thread = std::thread::Builder::new()
            .name("neo-wake".into())
            .spawn(move || {
                let failed = tx.clone();
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    engine_main(config, tx, stop2, mode2, monitor2.clone());
                })).is_err() {
                    monitor2.fail("唤醒线程异常中断");
                    let _ = failed.send(WakeEvent::Error("唤醒线程异常中断".into()));
                }
                monitor2.update(|s| {
                    if s.phase != WakePhase::Error { s.phase = WakePhase::Stopped; }
                });
            });
        let thread = match thread {
            Ok(thread) => Some(thread),
            Err(e) => {
                let error = format!("无法启动唤醒线程：{e}");
                monitor.fail(&error);
                let _ = failed.send(WakeEvent::Error(error));
                None
            }
        };
        (
            Self {
                stop,
                mode,
                monitor,
                thread,
            },
            rx,
        )
    }

    /// 仅开关额外电平/得分遥测；不启停麦克风、不重载模型、不改变工作模式。
    pub fn set_diagnostics(&self, on: bool) {
        self.monitor.update(|s| {
            self.monitor.enabled.store(on, Ordering::Release);
            s.enabled = on;
            if !on {
                s.rms = None;
                s.dbfs = None;
                s.peak = None;
                s.score = None;
                s.score_peak_2s = None;
            }
        });
    }

    /// 拉取最新快照（电平最多 5Hz；生命周期/命中即时更新）。错误跨诊断开关保留。
    pub fn diagnostics(&self) -> WakeDiagnostics {
        self.monitor.snapshot.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// 测试时只计数、不发送 Detected 或听写 Audio。进出均重新预热且保留原阈值。
    /// 调用方须在最终消费时以 [`Self::event_epoch`] 校验事件代次，不能仅排空队列。
    pub fn set_test_mode(&self, on: bool) {
        self.monitor.set_test_mode(on);
    }

    /// 当前测试模式代次；普通听写切换不改变它。
    pub fn event_epoch(&self) -> u64 {
        self.monitor.test_epoch.load(Ordering::Acquire)
    }

    /// 切换听写模式。
    /// - true：停止唤醒词检测，改为把 16kHz 音频帧经 WakeEvent::Audio 转发出来
    /// - false：清空缓冲并回到唤醒词检测（带完整防抖间隔，防止残留音频立刻再触发）
    pub fn set_dictation(&self, on: bool) {
        self.mode.store(
            if on { MODE_DICTATE } else { MODE_DETECT },
            Ordering::Relaxed,
        );
    }
}

impl Drop for WakeEngine {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // 关闭开关发生在 UI；模型加载、推理或驱动析构不能阻塞这一帧。
        drop(self.thread.take());
    }
}

// ---------------- 引擎线程 ----------------

// 分离的旧线程退出前，新线程不能并行加载模型或打开另一条唤醒采集流。
static WAKE_LEASE: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct Models {
    mel: Session,
    embed: Session,
    classifier: Session,
}

fn engine_main(
    config: WakeConfig,
    tx: mpsc::Sender<WakeEvent>,
    stop: Arc<AtomicBool>,
    mode: Arc<AtomicU8>,
    monitor: Arc<WakeMonitor>,
) {
    let _lease = loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        match WAKE_LEASE.try_lock() {
            Ok(lease) => break lease,
            Err(std::sync::TryLockError::Poisoned(p)) => break p.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    };
    if stop.load(Ordering::Relaxed) {
        return;
    }
    let mut models = match load_models(&config.model_dir) {
        Ok(m) => m,
        Err(e) => {
            monitor.fail(&e);
            let _ = tx.send(WakeEvent::Error(e));
            return;
        }
    };

    if stop.load(Ordering::Relaxed) {
        return;
    }
    let (sample_tx, sample_rx) = mpsc::sync_channel::<Vec<i16>>(SAMPLE_CHUNKS);
    let err_tx = tx.clone();
    let err_monitor = monitor.clone();
    let stream = match build_stream(sample_tx, move |msg| {
        err_monitor.fail(&msg);
        let _ = err_tx.send(WakeEvent::Error(msg));
    }) {
        Ok(s) => s,
        Err(e) => {
            monitor.fail(&e);
            let _ = tx.send(WakeEvent::Error(e));
            return;
        }
    };
    if stop.load(Ordering::Relaxed) {
        return;
    }
    monitor.update(|s| {
        s.device_name = Some(stream.device_name.chars().take(256).collect());
        s.sample_rate = Some(stream.sample_rate);
    });
    if let Err(e) = stream.stream.play() {
        let error = format!("start mic stream: {e}");
        monitor.fail(&error);
        let _ = tx.send(WakeEvent::Error(error));
        return;
    }
    monitor.update(|s| {
        if s.phase != WakePhase::Error { s.phase = WakePhase::Ready; }
    });

    let mut resampler = Resampler::new(stream.sample_rate, SAMPLE_RATE);
    let mut pending: Vec<i16> = Vec::with_capacity(FRAME_SAMPLES * 2);
    let mut frames: VecDeque<Vec<f32>> = VecDeque::with_capacity(CHUNK_FRAMES);
    let mut last_fire = Instant::now() - config.debounce * 2;
    let mut chunk = vec![0f32; FRAME_SAMPLES * CHUNK_FRAMES];

    let mut meter = DiagnosticMeter::new(Instant::now());
    let mut test_seen = 0;
    let mut mode_seen = MODE_DETECT;
    // 连续过线帧数（见 CONFIRM_FRAMES）：单帧尖峰不算数。
    let mut streak = 0usize;

    while !stop.load(Ordering::Relaxed) {
        // 模式切换：清缓冲 + 重置防抖。
        // 回到检测模式时，滑窗需要约 2s 重新填满，期间不会误触发；
        // 听写残留的唤醒词尾音也不会在切回后立刻再烧一次。
        let test = monitor.test_epoch.load(Ordering::Acquire);
        let m = if test & 1 != 0 { MODE_DETECT } else { mode.load(Ordering::Relaxed) };
        if m != mode_seen || test != test_seen {
            mode_seen = m;
            frames.clear();
            pending.clear();
            streak = 0;
            last_fire = Instant::now();
            if test != test_seen {
                reset_test_audio(&sample_rx, &mut resampler, stream.sample_rate, &mut chunk);
                test_seen = test;
            }
            meter = DiagnosticMeter::new(Instant::now());
            monitor.update(|s| {
                s.warmup_frames = 0;
                s.dictating = m == MODE_DICTATE;
                s.score = None;
                s.score_peak_2s = None;
            });
        }
        let now = Instant::now();
        meter.enable(monitor.enabled.load(Ordering::Acquire), now);
        meter.publish(&monitor, now, frames.len(), mode_seen == MODE_DICTATE);
        let raw = match sample_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(v) => v,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // 流活着就有持续的采样回调（静音也是零帧，不是没帧）。
                // 一个样本都没有超过看门狗时间 = 流实质已死（设备拔出且
                // 无后继等）—— DeviceChanged 错误回调分不出真假死，
                // 只能靠数据兜底。上报后退出，别永远空转装没事。
                let silent = now_ms().saturating_sub(stream.last_sample.load(Ordering::Relaxed));
                if silent > MIC_SILENCE_TIMEOUT.as_millis() as u64 {
                    let error = "麦克风已断开（5 秒没有采到任何声音）".to_owned();
                    monitor.fail(&error);
                    let _ = tx.send(WakeEvent::Error(error));
                    return;
                }
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        meter.enable(monitor.enabled.load(Ordering::Acquire), Instant::now());
        meter.audio(&raw);
        resampler.process(&raw, &mut pending);

        while pending.len() >= FRAME_SAMPLES {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            if monitor.test_epoch.load(Ordering::Acquire) != test_seen {
                break; // 外层统一重置，不能用测试退出前的帧继续推理。
            }
            let frame: Vec<f32> = pending
                .drain(..FRAME_SAMPLES)
                .map(|s| s as f32 / 32768.0)
                .collect();

            // 听写模式：帧直接转发给上层（VAD/STT），不跑唤醒推理
            if mode_seen == MODE_DICTATE {
                let state = monitor.snapshot.lock().unwrap_or_else(|p| p.into_inner());
                if monitor.test_epoch.load(Ordering::Acquire) != test_seen { break; }
                if !state.test_mode && tx.send(WakeEvent::Audio { epoch: test_seen, frame }).is_err() {
                    return;
                }
                continue;
            }

            if frames.len() == CHUNK_FRAMES {
                frames.pop_front();
            }
            frames.push_back(frame);

            if frames.len() < CHUNK_FRAMES {
                continue;
            }
            // 每 80ms 对最近 2s 音频做一次完整预测（与 Python listener 一致）
            for (dst, src) in chunk.chunks_exact_mut(FRAME_SAMPLES).zip(frames.iter()) {
                dst.copy_from_slice(src);
            }
            match predict(&mut models, &chunk) {
                Ok(score) => {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    if monitor.test_epoch.load(Ordering::Acquire) != test_seen { break; }
                    meter.enable(monitor.enabled.load(Ordering::Acquire), Instant::now());
                    meter.score(Instant::now(), score);
                    if score >= config.threshold {
                        streak += 1;
                    } else {
                        streak = 0;
                    }
                    if streak >= CONFIRM_FRAMES && last_fire.elapsed() >= config.debounce {
                        streak = 0;
                        last_fire = Instant::now();
                        // 检测后清空缓冲，与 Python listener 的 pause 行为一致
                        frames.clear();
                        pending.clear();
                        if !monitor.detected(test_seen, score, &tx) {
                            return;
                        }
                    }
                }
                Err(e) => {
                    let error = format!("inference: {e}");
                    monitor.fail(&error);
                    let _ = tx.send(WakeEvent::Error(error));
                    return;
                }
            }
        }
    }
    // stream 随作用域结束而 drop，采集回调停止
}

fn reset_test_audio(
    samples: &mpsc::Receiver<Vec<i16>>,
    resampler: &mut Resampler,
    sample_rate: u32,
    chunk: &mut [f32],
) {
    // 有界排空此前采集的块，且清掉重采样滤波器的尾音。
    for _ in 0..SAMPLE_CHUNKS {
        if samples.try_recv().is_err() { break; }
    }
    *resampler = Resampler::new(sample_rate, SAMPLE_RATE);
    chunk.fill(0.0);
}

// ---------------- 模型加载与推理 ----------------

static ORT_INIT: OnceLock<()> = OnceLock::new();

fn init_ort(dir: &Path) -> Result<(), String> {
    // 只缓存成功：失败（比如当时 dll 缺失）不该被记到进程寿命 ——
    // 用户补好文件、经设置开关重启引擎后必须能重试。
    if ORT_INIT.get().is_some() {
        return Ok(());
    }
    let dll = dir.join("onnxruntime.dll");
    // commit() 返回 bool：false 表示 environment 已配置过（幂等，可忽略）
    ort::init_from(&dll)
        .map_err(|e| format!("load {}: {e}", dll.display()))?
        .with_name("neo-wake")
        .commit();
    let _ = ORT_INIT.set(()); // 并发首装下另一个线程可能抢先，幂等
    Ok(())
}

fn load_models(dir: &Path) -> Result<Models, String> {
    for name in ["melspectrogram.onnx", "embedding_model.onnx"] {
        if !dir.join(name).is_file() {
            return Err(format!("missing {}", dir.join(name).display()));
        }
    }
    let classifier_path = dir.join("hi_neo.onnx");
    if !classifier_path.is_file() {
        return Err("hi_neo.onnx 尚未训练，请先跑 wake-training 管线".into());
    }
    init_ort(dir)?;
    let build = |path: &Path| -> Result<Session, String> {
        Session::builder()
            .map_err(|e| e.to_string())?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| e.to_string())?
            .with_intra_threads(2)
            .map_err(|e| e.to_string())?
            .commit_from_file(path)
            .map_err(|e| format!("load {}: {e}", path.display()))
    };
    Ok(Models {
        mel: build(&dir.join("melspectrogram.onnx"))?,
        embed: build(&dir.join("embedding_model.onnx"))?,
        classifier: build(&classifier_path)?,
    })
}

fn predict(models: &mut Models, audio: &[f32]) -> Result<f32, String> {
    // 1) mel：(1, samples) -> (1, 1, T, 32)，后处理 x/10+2
    let mel_in = Tensor::from_array((vec![1i64, audio.len() as i64], audio.to_vec()))
        .map_err(|e| e.to_string())?;
    let mel_out = models.mel.run(ort::inputs![mel_in]).map_err(|e| e.to_string())?;
    let (mel_shape, mel_data) = mel_out[0].try_extract_tensor::<f32>().map_err(|e| e.to_string())?;
    // 形状校验：模型版本不符时宁可报错也别静默错切（把 32 当成 T 之类的
    // 事故没有任何报错、唤醒纯静默失效）或索引越界 panic。
    if mel_shape.len() != 4 || mel_shape[3] as usize != MEL_BINS {
        return Err(format!(
            "mel 输出形状异常 {mel_shape:?}（应为 (1,1,T,{MEL_BINS})），模型版本不符？"
        ));
    }
    let t = mel_shape[2] as usize;
    if mel_data.len() != t * MEL_BINS {
        return Err(format!(
            "mel 数据长度 {} 与形状 {mel_shape:?} 不符",
            mel_data.len()
        ));
    }
    let mut feats = vec![0f32; t * MEL_BINS];
    for (dst, &src) in feats.iter_mut().zip(mel_data.iter()) {
        *dst = src / 10.0 + 2.0;
    }

    // 2) embedding：76/8 滑窗，batch (n, 76, 32, 1) -> (n, 1, 1, 96)
    let n_emb = if t >= EMB_WINDOW {
        (t - EMB_WINDOW) / EMB_STRIDE + 1
    } else {
        0
    };
    if n_emb < MIN_EMBEDDINGS {
        return Ok(0.0);
    }
    let mut batch = Vec::with_capacity(n_emb * EMB_WINDOW * MEL_BINS);
    for w in 0..n_emb {
        let start = w * EMB_STRIDE * MEL_BINS;
        batch.extend_from_slice(&feats[start..start + EMB_WINDOW * MEL_BINS]);
    }
    let emb_in = Tensor::from_array((
        vec![n_emb as i64, EMB_WINDOW as i64, MEL_BINS as i64, 1],
        batch,
    ))
    .map_err(|e| e.to_string())?;
    let emb_out = models.embed.run(ort::inputs![emb_in]).map_err(|e| e.to_string())?;
    let (_, emb_data) = emb_out[0].try_extract_tensor::<f32>().map_err(|e| e.to_string())?;
    if emb_data.len() != n_emb * EMB_DIM {
        return Err(format!(
            "embedding 数据长度 {} 与期望 {} 不符，模型版本不符？",
            emb_data.len(),
            n_emb * EMB_DIM
        ));
    }

    // 3) 分类器：取最后 16 个 embedding，(1, 16, 96) -> (1, 1)
    let tail = &emb_data[(n_emb - MIN_EMBEDDINGS) * EMB_DIM..];
    let cls_in = Tensor::from_array((
        vec![1i64, MIN_EMBEDDINGS as i64, EMB_DIM as i64],
        tail.to_vec(),
    ))
    .map_err(|e| e.to_string())?;
    let cls_out = models
        .classifier
        .run(ort::inputs![cls_in])
        .map_err(|e| e.to_string())?;
    let (_, score) = cls_out[0].try_extract_tensor::<f32>().map_err(|e| e.to_string())?;
    score
        .first()
        .copied()
        .ok_or_else(|| "分类器输出为空，模型版本不符？".to_owned())
}

// ---------------- 音频采集 ----------------

struct AudioStream {
    stream: cpal::Stream,
    /// 实际采集采样率（可能不是 16000，需要重采样）
    sample_rate: u32,
    /// 设备名（启动遥测用）
    device_name: String,
    /// 最近一次数据回调的 Unix 毫秒（看门狗用；回调每到一个块就刷新）
    last_sample: Arc<AtomicU64>,
}

/// 有界队列 + 满了丢新块（理由见 [`SAMPLE_CHUNKS`]），顺手刷新看门狗。
fn push_samples(tx: &mpsc::SyncSender<Vec<i16>>, last: &AtomicU64, buf: Vec<i16>) {
    last.store(now_ms(), Ordering::Relaxed);
    let _ = tx.try_send(buf); // Full / Disconnected 都丢：回调线程不能阻塞
}

/// `on_err` 只在致命错误时调用；Xrun / 设备切换这类瞬态事件内部消化。
fn build_stream(
    tx: mpsc::SyncSender<Vec<i16>>,
    on_err: impl Fn(String) + Send + Clone + 'static,
) -> Result<AudioStream, String> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| "no input device".to_string())?;

    let device_name = device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| "?".into());

    // 看门狗起始值给「现在」：流刚建好、首个回调还没到之前不算死亡。
    let last_sample = Arc::new(AtomicU64::new(now_ms()));

    let err_cb = move |e: cpal::Error| {
        match e.kind() {
            // Xrun（缓冲区欠载/过载）与 DeviceChanged（默认设备切换、流自动跟随）
            // 都是瞬态事件，流仍在继续采集——启动期 CPU 尖峰很容易触发一次 Xrun，
            // 误当致命错误会让唤醒整局失效，这里只记录不上报。
            // （设备拔出且无后继的「假 DeviceChanged、真死流」由数据看门狗兜住。）
            cpal::ErrorKind::Xrun | cpal::ErrorKind::DeviceChanged => {
                eprintln!("[neo-wake] mic stream 瞬态事件（流仍存活，忽略）");
            }
            _ => on_err(format!("mic stream: {e}")),
        }
    };

    // 优先直接请求 16kHz mono i16（WASAPI 共享模式下部分设备支持）
    let want_16k = cpal::StreamConfig {
        channels: 1,
        sample_rate: SAMPLE_RATE,
        buffer_size: cpal::BufferSize::Default,
    };
    let supports_16k = device
        .supported_input_configs()
        .map_err(|e| e.to_string())?
        .any(|r| {
            r.channels() == 1
                && r.min_sample_rate() <= SAMPLE_RATE
                && r.max_sample_rate() >= SAMPLE_RATE
                && r.sample_format() == cpal::SampleFormat::I16
        });
    if supports_16k {
        let tx16 = tx.clone();
        let last = last_sample.clone();
        if let Ok(stream) = device.build_input_stream(
            want_16k,
            move |d: &[i16], _| {
                push_samples(&tx16, &last, d.to_vec());
            },
            err_cb.clone(),
            None,
        ) {
            return Ok(AudioStream {
                stream,
                sample_rate: SAMPLE_RATE,
                device_name,
                last_sample,
            });
        }
    }

    // 退回设备默认格式，混单声道（各声道平均 —— 只取左声道会对左右
    // 不对称的 USB 阵列采到近静音）+ 转 i16，重采样交给引擎线程
    let def = device.default_input_config().map_err(|e| e.to_string())?;
    let ch = def.channels() as usize;
    let sample_rate = def.sample_rate();
    let config: cpal::StreamConfig = def.clone().into();

    let stream = match def.sample_format() {
        cpal::SampleFormat::I16 => {
            let last = last_sample.clone();
            device
                .build_input_stream(
                    config.clone(),
                    move |d: &[i16], _| {
                        let mono = if ch == 1 {
                            d.to_vec()
                        } else {
                            d.chunks_exact(ch)
                                .map(|f| {
                                    (f.iter().map(|&s| s as i32).sum::<i32>() / ch as i32) as i16
                                })
                                .collect()
                        };
                        push_samples(&tx, &last, mono);
                    },
                    err_cb.clone(),
                    None,
                )
                .map_err(|e| e.to_string())?
        }
        cpal::SampleFormat::F32 => {
            let last = last_sample.clone();
            device
                .build_input_stream(
                    config.clone(),
                    move |d: &[f32], _| {
                        let mono = d
                            .chunks_exact(ch)
                            .map(|f| {
                                let avg = f.iter().sum::<f32>() / ch as f32;
                                (avg.clamp(-1.0, 1.0) * 32767.0) as i16
                            })
                            .collect();
                        push_samples(&tx, &last, mono);
                    },
                    err_cb.clone(),
                    None,
                )
                .map_err(|e| e.to_string())?
        }
        cpal::SampleFormat::U16 => {
            let last = last_sample.clone();
            device
                .build_input_stream(
                    config,
                    move |d: &[u16], _| {
                        let mono: Vec<i16> = d
                            .chunks_exact(ch)
                            .map(|f| {
                                (f.iter().map(|&s| s as i32 - 32768).sum::<i32>() / ch as i32)
                                    as i16
                            })
                            .collect();
                        push_samples(&tx, &last, mono);
                    },
                    err_cb.clone(),
                    None,
                )
                .map_err(|e| e.to_string())?
        }
        f => return Err(format!("unsupported sample format: {f:?}")),
    };
    Ok(AudioStream {
        stream,
        sample_rate,
        device_name,
        last_sample,
    })
}

// ---------------- 重采样（低通 + 线性插值） ----------------

/// RBJ biquad 低通，抽取前抗混叠
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl Biquad {
    fn lowpass(fs: f32, fc: f32) -> Self {
        let q = std::f32::consts::FRAC_1_SQRT_2;
        let w0 = 2.0 * std::f32::consts::PI * fc / fs;
        let alpha = w0.sin() / (2.0 * q);
        let cosw = w0.cos();
        let a0 = 1.0 + alpha;
        Self {
            b0: (1.0 - cosw) / 2.0 / a0,
            b1: (1.0 - cosw) / a0,
            b2: (1.0 - cosw) / 2.0 / a0,
            a1: -2.0 * cosw / a0,
            a2: (1.0 - alpha) / a0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }

    fn tick(&mut self, x: f32) -> f32 {
        let y =
            self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2 - self.a1 * self.y1 - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

struct Resampler {
    step: f64, // fs_in / fs_out
    frac: f64, // 下一个输出点在 [prev, cur] 间的位置
    prev: f32,
    started: bool,
    lp: Option<Biquad>,
}

impl Resampler {
    fn new(fs_in: u32, fs_out: u32) -> Self {
        // 驱动上报 0Hz 的病态情形：step=0 会让内层 while 死循环且输出无限
        // 增长（挂死 + OOM）。当成不重采样（step=1）处理，至少链路还活着。
        let fs_in = if fs_in == 0 { fs_out } else { fs_in };
        let lp = if fs_in > fs_out {
            Some(Biquad::lowpass(fs_in as f32, fs_out as f32 / 2.0 * 0.9))
        } else {
            None
        };
        Self {
            step: fs_in as f64 / fs_out as f64,
            frac: 0.0,
            prev: 0.0,
            started: false,
            lp,
        }
    }

    fn process(&mut self, input: &[i16], out: &mut Vec<i16>) {
        for &s in input {
            let x = match &mut self.lp {
                Some(lp) => lp.tick(s as f32),
                None => s as f32,
            };
            if !self.started {
                self.started = true;
                self.prev = x;
                continue;
            }
            while self.frac < 1.0 {
                let y = self.prev + (x - self.prev) * self.frac as f32;
                out.push(y.round().clamp(-32768.0, 32767.0) as i16);
                self.frac += self.step;
            }
            self.frac -= 1.0;
            self.prev = x;
        }
    }
}

// ---------------- 独立音频采集（AudioTap） ----------------

// 最多缓冲 10 秒（625 KiB 样本）；STT 落后时丢新帧，生产端绝不等待消费者。
const TAP_FRAMES: usize = 125;
const TAP_STOP_TIMEOUT: Duration = Duration::from_millis(500);

fn push_tap_frame(tx: &mpsc::SyncSender<Vec<f32>>, missing: &AtomicBool, frame: Vec<f32>) -> bool {
    match tx.try_send(frame) {
        Ok(()) => true,
        Err(mpsc::TrySendError::Full(_)) => {
            if !missing.swap(true, Ordering::Relaxed) {
                eprintln!("[neo-audio-tap] 音频缓冲已满，部分音频已跳过");
            }
            true
        }
        Err(mpsc::TrySendError::Disconnected(_)) => false,
    }
}

/// 一路独立的 16kHz 单声道音频采集，按 80ms（1280 样本）切成 f32 帧。
///
/// 唤醒引擎内部用的就是同一条采集链路；课堂记录等需要原始音频的功能
/// 自己 `start` 一路 —— 与唤醒流并存是安全的（WASAPI 共享模式允许多个
/// 客户端同开一支麦克风）。
pub struct AudioTap {
    rx: mpsc::Receiver<Vec<f32>>,
    stop: Arc<AtomicBool>,
    missing: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    device: String,
}

impl AudioTap {
    /// 开流并启动采集线程。拿不到麦克风 / 启动失败时立即报错（同步握手）。
    pub fn start() -> Result<Self, String> {
        Self::start_cancellable(Arc::new(AtomicBool::new(false)))
    }

    /// 外部取消覆盖建流握手与采集循环；正常 stop_capture 仍保留尾帧。
    pub fn start_cancellable(cancelled: Arc<AtomicBool>) -> Result<Self, String> {
        Self::start_with_stop(Arc::new(AtomicBool::new(false)), cancelled)
    }

    /// 正常停止同时覆盖建流握手；与整代取消不同，停流后仍排出已采集尾音。
    pub fn start_with_stop(session_stop: Arc<AtomicBool>, cancelled: Arc<AtomicBool>) -> Result<Self, String> {
        if session_stop.load(Ordering::Relaxed) || cancelled.load(Ordering::Acquire) {
            return Err("音频采集已取消".into());
        }
        let (frame_tx, frame_rx) = mpsc::sync_channel::<Vec<f32>>(TAP_FRAMES);
        let missing = Arc::new(AtomicBool::new(false));
        let missing2 = missing.clone();
        let (hello_tx, hello_rx) = mpsc::channel::<Result<String, String>>();
        // 握手失败只停止本次采集，不能伪装成调用方主动下课。
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let cancelled2 = cancelled.clone();
        let session_stop2 = session_stop.clone();
        let thread = std::thread::Builder::new()
            .name("neo-audio-tap".into())
            .spawn(move || tap_main(frame_tx, hello_tx, stop2, session_stop2, cancelled2, missing2))
            .map_err(|e| format!("spawn audio tap: {e}"))?;
        // 等采集线程握手：设备名或启动错误。流活着时线程不会主动退出。
        // 必须有超时：坏驱动/杀软钩住音频栈时建流可能永久卡住 —— 无超时
        // 会把调用方线程一起 wedge 掉。超时后不能 join（同样会卡死），
        // 置 stop 并分离线程：它若将来醒来会自己看到 stop 退出。
        let device = match wait_tap_hello(&hello_rx, &session_stop, &cancelled, Instant::now() + Duration::from_secs(10)) {
            Ok(name) => name,
            Err(e) => {
                stop.store(true, Ordering::Relaxed);
                // 错误事件可能先于驱动析构返回；错误路径也不能无限 join。
                drop(thread);
                return Err(e);
            }
        };
        Ok(Self {
            rx: frame_rx,
            stop,
            missing,
            thread: Some(thread),
            device,
        })
    }

    /// 采集设备名（遥测用）。
    pub fn device(&self) -> &str {
        &self.device
    }

    /// 取下一帧（80ms / 1280 样本 f32）。超时返回 `None`；
    /// 流死亡（线程退出）后持续返回 `None` —— 用 [`AudioTap::is_alive`]
    /// 区分「安静的教室」与「死流」。
    pub fn next_frame(&self, timeout: Duration) -> Option<Vec<f32>> {
        self.rx.recv_timeout(timeout).ok()
    }

    /// 停止采集，最多等 500ms；驱动卡住时返回 false，Drop 会分离线程。
    /// 已入队帧仍可排空；过载或超时可能缺失尾音。仅在后台消费线程调用。
    pub fn stop_capture(&mut self) -> bool {
        self.stop.store(true, Ordering::Relaxed);
        let deadline = Instant::now() + TAP_STOP_TIMEOUT;
        while self.is_alive() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                self.missing.store(true, Ordering::Relaxed);
                eprintln!("[neo-audio-tap] 停止采集超时，部分尾音可能缺失");
                return false;
            }
            std::thread::sleep(remaining.min(Duration::from_millis(5)));
        }
        if let Some(t) = self.thread.take() {
            if t.join().is_err() {
                self.missing.store(true, Ordering::Relaxed);
                return false;
            }
        }
        true
    }

    /// 本次采集是否因缓冲过载或停止异常而可能缺音；标志保持到实例销毁。
    pub fn has_missing_audio(&self) -> bool {
        self.missing.load(Ordering::Relaxed)
    }

    /// 采集线程是否还活着。`next_frame` 持续返回 `None` 时用它区分
    /// 「没声音」与「流死了」。
    pub fn is_alive(&self) -> bool {
        self.thread.as_ref().is_some_and(|t| !t.is_finished())
    }
}

impl Drop for AudioTap {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Drop 可能位于 UI 或异常展开路径；只有显式 stop_capture 才等待尾音。
        drop(self.thread.take());
    }
}

fn wait_tap_hello(
    hello: &mpsc::Receiver<Result<String, String>>,
    stop: &AtomicBool,
    cancelled: &AtomicBool,
    deadline: Instant,
) -> Result<String, String> {
    loop {
        if stop.load(Ordering::Relaxed) || cancelled.load(Ordering::Acquire) {
            return Err("音频采集已取消".into());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("打开麦克风超时（10s）——设备繁忙或被安全软件拦截".into());
        }
        match hello.recv_timeout(remaining.min(Duration::from_millis(50))) {
            Ok(result) if !stop.load(Ordering::Relaxed) && !cancelled.load(Ordering::Acquire) && Instant::now() < deadline => return result,
            Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err("audio tap 线程启动即退出".into()),
        }
    }
}

fn tap_main(
    tx: mpsc::SyncSender<Vec<f32>>,
    hello: mpsc::Sender<Result<String, String>>,
    stop: Arc<AtomicBool>,
    session_stop: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
    missing: Arc<AtomicBool>,
) {
    if stop.load(Ordering::Relaxed) || session_stop.load(Ordering::Relaxed) || cancelled.load(Ordering::Acquire) {
        return;
    }
    let (sample_tx, sample_rx) = mpsc::sync_channel::<Vec<i16>>(SAMPLE_CHUNKS);
    // 致命流错误：置死标志，主循环见到即退出 —— 线程退出后帧通道断开，
    // 对端经 is_alive() 得知流死（只 eprintln 的话对端永远分不清
    // 「安静的教室」与「死流」）。
    let dead = Arc::new(AtomicBool::new(false));
    let dead2 = dead.clone();
    let stream = match build_stream(sample_tx, move |msg| {
        eprintln!("[neo-audio-tap] {msg}");
        dead2.store(true, Ordering::Relaxed);
    }) {
        Ok(s) => s,
        Err(e) => {
            let _ = hello.send(Err(e));
            return;
        }
    };
    if stop.load(Ordering::Relaxed) || session_stop.load(Ordering::Relaxed) || cancelled.load(Ordering::Acquire) {
        return;
    }
    if let Err(e) = stream.stream.play() {
        let _ = hello.send(Err(format!("start mic stream: {e}")));
        return;
    }
    if hello.send(Ok(stream.device_name.clone())).is_err() {
        return; // 启动方已放弃
    }

    // 收样本 → 重采样 → 组帧 → 转发，直到 stop、流报错或看门狗判定死流。
    let mut resampler = Resampler::new(stream.sample_rate, SAMPLE_RATE);
    let mut pending: Vec<i16> = Vec::with_capacity(FRAME_SAMPLES * 2);
    while !stop.load(Ordering::Relaxed) && !session_stop.load(Ordering::Relaxed) && !cancelled.load(Ordering::Acquire) {
        let raw = match sample_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(v) => v,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if dead.load(Ordering::Relaxed) {
                    break; // 停流后统一排空已有样本
                }
                // 同唤醒引擎的数据看门狗：持续无样本 = 流实质已死。
                let silent = now_ms().saturating_sub(stream.last_sample.load(Ordering::Relaxed));
                if silent > MIC_SILENCE_TIMEOUT.as_millis() as u64 {
                    eprintln!("[neo-audio-tap] 麦克风 {MIC_SILENCE_TIMEOUT:?} 无数据，判定死流退出");
                    break;
                }
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if cancelled.load(Ordering::Acquire) {
            return;
        }
        resampler.process(&raw, &mut pending);
        while pending.len() >= FRAME_SAMPLES {
            if cancelled.load(Ordering::Acquire) {
                return;
            }
            let frame: Vec<f32> = pending
                .drain(..FRAME_SAMPLES)
                .map(|s| s as f32 / 32768.0)
                .collect();
            if !push_tap_frame(&tx, &missing, frame) {
                return; // 消费端走了
            }
        }
    }
    // drop 流使回调停止后，原始队列才是封闭的。不能先等通道断开，
    // 因为发送端由仍活着的 stream 持有。
    drop(stream);
    if cancelled.load(Ordering::Acquire) {
        return;
    }
    for raw in sample_rx.try_iter() {
        resampler.process(&raw, &mut pending);
    }
    for chunk in pending.chunks(FRAME_SAMPLES) {
        if cancelled.load(Ordering::Acquire)
            || !push_tap_frame(&tx, &missing, chunk.iter().map(|&s| s as f32 / 32768.0).collect()) {
            break;
        }
    }
}

#[cfg(test)]
mod diagnostic_tests {
    use super::*;

    fn fake_engine() -> WakeEngine {
        WakeEngine {
            stop: Arc::new(AtomicBool::new(false)),
            mode: Arc::new(AtomicU8::new(MODE_DETECT)),
            monitor: Arc::new(WakeMonitor::new(0.25)),
            thread: None,
        }
    }

    #[test]
    fn diagnostics_are_opt_in_and_rate_limited_without_events() {
        let engine = fake_engine();
        let start = Instant::now();
        let mut meter = DiagnosticMeter::new(start);
        meter.audio(&[i16::MIN, i16::MAX]);
        meter.score(start, 0.9);
        assert_eq!(meter.samples, 0);
        assert_eq!(meter.power, 0.0);
        assert!(meter.scores.is_empty());
        engine.set_diagnostics(true);
        meter.enable(true, start);
        meter.audio(&[16384, -16384]);
        meter.score(start, 0.7);
        meter.publish(&engine.monitor, start + Duration::from_millis(199), 7, false);
        assert!(engine.diagnostics().rms.is_none());
        meter.publish(&engine.monitor, start + DIAGNOSTIC_INTERVAL, 7, false);
        let s = engine.diagnostics();
        assert_eq!(s.rms, Some(0.5));
        assert!((s.dbfs.unwrap() + 6.0206).abs() < 0.001);
        assert_eq!(s.peak, Some(0.5));
        assert_eq!(s.score, Some(0.7));
        assert_eq!(s.score_peak_2s, Some(0.7));
        assert_eq!(s.warmup_frames, 7);
        assert_eq!(s.warmup_total, 25);
        assert!(s.last_audio_ms > 0);
        engine.set_diagnostics(false);
        meter.enable(false, start + DIAGNOSTIC_INTERVAL);
        meter.audio(&[i16::MIN; 100]);
        assert_eq!(meter.samples, 0);
        assert!(engine.diagnostics().rms.is_none());
        assert!(engine.diagnostics().score.is_none());
        assert_eq!(engine.mode.load(Ordering::Relaxed), MODE_DETECT);
        assert!(!engine.stop.load(Ordering::Relaxed));
    }

    #[test]
    fn silence_and_full_scale_are_finite_and_old_scores_expire() {
        let engine = fake_engine();
        engine.set_diagnostics(true);
        let start = Instant::now();
        let mut meter = DiagnosticMeter::new(start);
        meter.enable(true, start);
        meter.audio(&[0; 10]);
        meter.score(start, 0.9);
        meter.publish(&engine.monitor, start + DIAGNOSTIC_INTERVAL, 25, false);
        assert_eq!(engine.diagnostics().dbfs, Some(-120.0));
        meter.audio(&[i16::MIN; 10]);
        meter.score(start + Duration::from_secs(1), 0.2);
        meter.publish(&engine.monitor, start + Duration::from_secs(2), 25, false);
        let s = engine.diagnostics();
        assert_eq!(s.dbfs, Some(0.0));
        assert_eq!(s.peak, Some(1.0));
        assert_eq!(s.score_peak_2s, Some(0.2));
        meter.publish(&engine.monitor, start + Duration::from_secs(3), 25, false);
        assert!(engine.diagnostics().score_peak_2s.is_none());
        assert!(engine.diagnostics().rms.is_none());
        for _ in 0..1000 { meter.score(start + Duration::from_secs(3), 0.4); }
        assert_eq!(meter.scores.len(), SCORE_HISTORY);
    }

    #[test]
    fn lifecycle_error_is_bounded_and_survives_diagnostic_toggles() {
        let engine = fake_engine();
        assert_eq!(engine.diagnostics().phase, WakePhase::Loading);
        engine.monitor.update(|s| {
            s.phase = WakePhase::Ready;
            s.device_name = Some("fake microphone".into());
            s.sample_rate = Some(48_000);
        });
        engine.monitor.fail(&"错".repeat(1000));
        engine.set_diagnostics(true);
        engine.set_diagnostics(false);
        let s = engine.diagnostics();
        assert_eq!(s.phase, WakePhase::Error);
        assert_eq!(s.error.unwrap().chars().count(), 512);
        assert_eq!(s.device_name.as_deref(), Some("fake microphone"));
        assert_eq!(s.sample_rate, Some(48_000));
        assert_eq!(s.threshold, 0.25);
    }

    #[test]
    fn test_mode_counts_without_dispatch_and_rejects_in_flight_results() {
        let engine = fake_engine();
        let (tx, rx) = mpsc::channel();
        assert!(engine.monitor.detected(0, 0.6, &tx));
        assert!(matches!(rx.try_recv(), Ok(WakeEvent::Detected { epoch: 0, score }) if score == 0.6));
        engine.set_test_mode(true);
        engine.set_test_mode(true); // 幂等，不重复重置正在测试的窗口。
        assert_eq!(engine.monitor.test_epoch.load(Ordering::Acquire), 1);
        assert!(engine.monitor.detected(0, 0.8, &tx));
        assert!(engine.monitor.detected(1, 0.8, &tx));
        assert!(rx.try_recv().is_err());
        let s = engine.diagnostics();
        assert!(s.test_mode);
        assert_eq!(s.hit_count, 2);
        assert!(s.last_hit_ms > 0);
        engine.set_test_mode(false);
        assert!(engine.monitor.detected(1, 0.8, &tx));
        assert!(rx.try_recv().is_err());
        assert_eq!(engine.diagnostics().hit_count, 2);
        assert_eq!(engine.diagnostics().threshold, 0.25);
        assert!(engine.monitor.detected(2, 0.5, &tx));
        assert!(matches!(rx.try_recv(), Ok(WakeEvent::Detected { .. })));
        engine.set_test_mode(true);
        engine.set_test_mode(false);
        assert_eq!(engine.monitor.test_epoch.load(Ordering::Acquire), 4);
        assert_eq!(engine.diagnostics().warmup_frames, 0);
    }

    #[test]
    fn event_epoch_survives_forwarding_and_normal_dictation_switches() {
        let engine = fake_engine();
        let (tx, rx) = mpsc::channel();
        assert!(engine.monitor.detected(engine.event_epoch(), 0.8, &tx));
        let detected = rx.recv().unwrap();
        let audio = WakeEvent::Audio { epoch: engine.event_epoch(), frame: vec![0.1; 16] };
        assert!(detected.is_current(engine.event_epoch()));
        engine.set_dictation(true);
        assert!(audio.is_current(engine.event_epoch()));
        engine.set_dictation(false);
        assert!(detected.is_current(engine.event_epoch()));
        assert!(audio.is_current(engine.event_epoch()));

        engine.set_test_mode(true);
        assert!(!detected.is_current(engine.event_epoch()));
        assert!(!audio.is_current(engine.event_epoch()));
        assert!(!WakeEvent::Audio { epoch: engine.event_epoch(), frame: vec![] }.is_current(engine.event_epoch()));
        assert!(WakeEvent::Error("synthetic failure".into()).is_current(engine.event_epoch()));
        engine.set_test_mode(false);
        assert_eq!(engine.event_epoch(), 2);
        assert!(!detected.is_current(engine.event_epoch()));
        assert!(!audio.is_current(engine.event_epoch()));
        assert!(engine.monitor.detected(engine.event_epoch(), 0.7, &tx));
        assert!(rx.recv().unwrap().is_current(engine.event_epoch()));
        engine.set_dictation(true);
        assert!(WakeEvent::Audio { epoch: 2, frame: vec![0.2] }.is_current(engine.event_epoch()));
        assert!(WakeEvent::Error("synthetic failure".into()).is_current(engine.event_epoch()));
    }

    #[test]
    fn test_transition_discards_queued_audio_and_filter_tail() {
        let (tx, rx) = mpsc::sync_channel(SAMPLE_CHUNKS);
        for _ in 0..SAMPLE_CHUNKS { tx.try_send(vec![i16::MAX; 10]).unwrap(); }
        let mut resampler = Resampler::new(48_000, SAMPLE_RATE);
        let mut pending = Vec::new();
        resampler.process(&[i16::MAX; 128], &mut pending);
        assert!(resampler.started);
        let mut chunk = vec![1.0; FRAME_SAMPLES * CHUNK_FRAMES];
        reset_test_audio(&rx, &mut resampler, 48_000, &mut chunk);
        assert!(rx.try_recv().is_err());
        assert!(!resampler.started);
        assert!(chunk.iter().all(|&s| s == 0.0));
        pending.clear();
        resampler.process(&[0; 128], &mut pending);
        assert!(pending.iter().all(|&s| s == 0));
    }
}

#[cfg(test)]
mod audio_tap_regression_tests {
    use super::*;

    #[test]
    fn cancelled_tap_never_starts_device_handshake() {
        assert!(AudioTap::start_cancellable(Arc::new(AtomicBool::new(true))).is_err());
        let (tx, rx) = mpsc::sync_channel(TAP_FRAMES);
        let (hello, ready) = mpsc::channel();
        tap_main(tx, hello, Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(true)), Arc::new(AtomicBool::new(false)));
        assert!(rx.try_recv().is_err());
        assert!(ready.try_recv().is_err());
    }

    #[test]
    fn handshake_uses_absolute_deadline_and_cancel_before_queued_success() {
        let (tx, rx) = mpsc::channel();
        tx.send(Ok("synthetic".into())).unwrap();
        let stop = AtomicBool::new(false);
        assert!(wait_tap_hello(&rx, &stop, &AtomicBool::new(false), Instant::now()).is_err());
        assert!(wait_tap_hello(&rx, &stop, &AtomicBool::new(true), Instant::now() + Duration::from_secs(1)).is_err());
        assert!(wait_tap_hello(&rx, &AtomicBool::new(true), &AtomicBool::new(false), Instant::now() + Duration::from_secs(1)).is_err());
        assert_eq!(wait_tap_hello(&rx, &stop, &AtomicBool::new(false), Instant::now() + Duration::from_secs(1)).unwrap(), "synthetic");
    }

    #[test]
    fn cancelled_wake_skips_models_and_microphone() {
        let (tx, rx) = mpsc::channel();
        let _lease = WAKE_LEASE.lock().unwrap_or_else(|p| p.into_inner());
        engine_main(WakeConfig::default(), tx, Arc::new(AtomicBool::new(true)), Arc::new(AtomicU8::new(MODE_DETECT)), Arc::new(WakeMonitor::new(0.25)));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn dropping_wake_does_not_join_a_busy_worker() {
        let (release, wait) = mpsc::channel();
        let thread = std::thread::spawn(move || { let _ = wait.recv(); });
        let stop = Arc::new(AtomicBool::new(false));
        let engine = WakeEngine { stop: stop.clone(), mode: Arc::new(AtomicU8::new(MODE_DETECT)), monitor: Arc::new(WakeMonitor::new(0.25)), thread: Some(thread) };
        let (done, finished) = mpsc::channel();
        let dropper = std::thread::spawn(move || { drop(engine); let _ = done.send(()); });
        let result = finished.recv_timeout(Duration::from_secs(1));
        release.send(()).unwrap();
        dropper.join().unwrap();
        assert!(result.is_ok());
        assert!(stop.load(Ordering::Relaxed));
    }

    #[test]
    fn dropping_tap_does_not_join_a_busy_worker() {
        let (release, wait) = mpsc::channel();
        let thread = std::thread::spawn(move || { let _ = wait.recv(); });
        let (_tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let tap = AudioTap { rx, stop: stop.clone(), missing: Arc::new(AtomicBool::new(false)), thread: Some(thread), device: "synthetic".into() };
        let (done, finished) = mpsc::channel();
        let dropper = std::thread::spawn(move || { drop(tap); let _ = done.send(()); });
        let result = finished.recv_timeout(Duration::from_secs(1));
        release.send(()).unwrap();
        dropper.join().unwrap();
        assert!(result.is_ok());
        assert!(stop.load(Ordering::Relaxed));
    }

    #[test]
    fn normal_stop_cancels_waiting_handshake_without_cancelling_generation() {
        let (_tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let cancelled = AtomicBool::new(false);
        let worker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            worker_stop.store(true, Ordering::Relaxed);
        });
        let result = wait_tap_hello(&rx, &stop, &cancelled, Instant::now() + Duration::from_secs(1));
        worker.join().unwrap();
        assert!(result.unwrap_err().contains("取消"));
        assert!(!cancelled.load(Ordering::Acquire));
        assert!(AudioTap::start_with_stop(stop, Arc::new(cancelled)).is_err());
    }

    #[test]
    fn stop_joins_producer_without_losing_queued_tail() {
        let (tx, rx) = mpsc::sync_channel(TAP_FRAMES);
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let missing = Arc::new(AtomicBool::new(false));
        let worker_missing = missing.clone();
        let thread = std::thread::spawn(move || {
            assert!(push_tap_frame(&tx, &worker_missing, vec![1.0; FRAME_SAMPLES]));
            while !stopped.load(Ordering::Relaxed) {
                std::thread::yield_now();
            }
            assert!(push_tap_frame(&tx, &worker_missing, vec![2.0; 17]));
        });
        let mut tap = AudioTap { rx, stop, missing, thread: Some(thread), device: "synthetic".into() };
        assert!(tap.stop_capture());
        assert!(!tap.is_alive());
        assert!(!tap.has_missing_audio());
        assert_eq!(tap.next_frame(Duration::ZERO).unwrap().len(), FRAME_SAMPLES);
        assert_eq!(tap.next_frame(Duration::ZERO).unwrap(), vec![2.0; 17]);
        assert!(tap.next_frame(Duration::ZERO).is_none());
        assert!(tap.stop_capture());
    }

    #[test]
    fn slow_consumer_has_bounded_fifo_and_full_queue_does_not_block_stop() {
        let (tx, rx) = mpsc::sync_channel(TAP_FRAMES);
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let missing = Arc::new(AtomicBool::new(false));
        let worker_missing = missing.clone();
        let (ready, filled) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            for i in 0..TAP_FRAMES * 4 {
                assert!(push_tap_frame(&tx, &worker_missing, vec![i as f32; FRAME_SAMPLES]));
            }
            ready.send(()).unwrap();
            while !stopped.load(Ordering::Relaxed) { std::thread::yield_now(); }
            assert!(push_tap_frame(&tx, &worker_missing, vec![-1.0; 17]));
        });
        let mut tap = AudioTap { rx, stop, missing, thread: Some(thread), device: "synthetic".into() };
        let full = filled.recv_timeout(Duration::from_secs(2));
        let stopped = tap.stop_capture();
        assert!(full.is_ok());
        assert!(stopped);
        assert!(tap.has_missing_audio());
        for i in 0..TAP_FRAMES {
            assert_eq!(tap.next_frame(Duration::ZERO).unwrap(), vec![i as f32; FRAME_SAMPLES]);
        }
        assert!(tap.next_frame(Duration::ZERO).is_none());
        let (tx, rx) = mpsc::sync_channel(1);
        drop(rx);
        assert!(!push_tap_frame(&tx, &AtomicBool::new(false), vec![0.0]));
    }

    #[test]
    fn explicit_stop_times_out_without_claiming_busy_worker_is_dead() {
        let (release, wait) = mpsc::channel();
        let (_tx, rx) = mpsc::sync_channel(TAP_FRAMES);
        let stop = Arc::new(AtomicBool::new(false));
        let thread = std::thread::spawn(move || { let _ = wait.recv(); });
        let mut tap = AudioTap { rx, stop: stop.clone(), missing: Arc::new(AtomicBool::new(false)), thread: Some(thread), device: "synthetic".into() };
        let (done, finished) = mpsc::channel();
        let stopper = std::thread::spawn(move || {
            let start = Instant::now();
            let stopped = tap.stop_capture();
            let _ = done.send((stopped, tap.is_alive(), tap.has_missing_audio(), start.elapsed()));
            tap
        });
        let result = finished.recv_timeout(Duration::from_secs(2));
        // 即使回归为无限 join，也先放开假驱动再断言，测试不遗留挂起线程。
        release.send(()).unwrap();
        let mut tap = stopper.join().unwrap();
        let (stopped, alive, missing, elapsed) = result.expect("stop exceeded bounded wait");
        assert!(!stopped && alive && missing);
        assert!(elapsed >= TAP_STOP_TIMEOUT && elapsed < Duration::from_secs(2));
        assert!(stop.load(Ordering::Relaxed));
        assert!(tap.stop_capture());
        assert!(!tap.is_alive());
        assert!(tap.has_missing_audio());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(resampler: &mut Resampler, input: &[i16]) -> Vec<i16> {
        let mut out = Vec::new();
        resampler.process(input, &mut out);
        out
    }

    #[test]
    fn same_rate_is_one_to_one() {
        let mut r = Resampler::new(16_000, 16_000);
        let input = vec![1000i16; 160];
        let out = run(&mut r, &input);
        // 首样本用于起相位，少一个是正常的；其余逐一对应。
        assert_eq!(out.len(), input.len() - 1);
        assert!(out.iter().all(|&s| (s - 1000).abs() <= 1));
    }

    #[test]
    fn integer_decimation_ratio_and_dc_passthrough() {
        let mut r = Resampler::new(48_000, 16_000);
        // 直流信号过低通还是直流：3:1 抽取，幅度不变。
        let input = vec![2000i16; 480];
        let out = run(&mut r, &input);
        assert!(
            (out.len() as i32 - 160).abs() <= 2,
            "480 样本 3:1 抽取应得 ~160，实得 {}",
            out.len()
        );
        assert!(
            out.iter().skip(10).all(|&s| (s - 2000).abs() < 100),
            "直流过抽取后幅度跑偏"
        );
    }

    #[test]
    fn upsample_interpolates_on_the_ramp() {
        let mut r = Resampler::new(16_000, 48_000);
        // 斜坡信号 1:3 上采样：点数约 3 倍，且输出仍落在斜坡上（单调）。
        let input: Vec<i16> = (0..160).map(|i| i * 10).collect();
        let out = run(&mut r, &input);
        assert!(
            (out.len() as i32 - 477).abs() <= 6,
            "160 样本 1:3 上采样应得 ~477，实得 {}",
            out.len()
        );
        assert!(out.windows(2).all(|w| w[1] >= w[0]), "斜坡插值必须单调");
    }

    #[test]
    fn zero_input_rate_does_not_hang_or_explode() {
        // 病态驱动上报 0Hz：按不重采样兜底，绝不能死循环 / OOM。
        let mut r = Resampler::new(0, 16_000);
        let out = run(&mut r, &vec![7i16; 100]);
        assert_eq!(out.len(), 99);
    }
}
