//! 语音唤醒引擎：复刻 livekit-wakeword 的推理链路。
//!
//! 链路（与 Python 版逐一对应）：
//!   麦克风 16kHz i16 → 1280 样本（80ms）一帧 → 25 帧（2s）滑窗
//!   → melspectrogram.onnx（输出 x/10+2）→ 76/8 滑窗 → embedding_model.onnx
//!   → 最后 16 个 96 维 embedding → hi_neo.onnx → score
//!   → score >= threshold 且过 debounce → WakeEvent::Detected

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{mpsc, Arc, OnceLock};
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
            model_dir: Path::new(env!("CARGO_MANIFEST_DIR")).join("assets"),
            // 默认 0.2：合成训练集上能到 0.5，但真实麦克风 + 真人嗓音实测
            // 只有 0.31~0.58，而静音/噪声 ≤0.04——0.2 在两侧都留有 5 倍余量。
            // 调试期可用 NEO_WAKE_THRESHOLD=0.3 之类临时压阈值试灵敏度，
            // 不必改代码重新构建。
            threshold: std::env::var("NEO_WAKE_THRESHOLD")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.2),
            debounce: Duration::from_secs(2),
        }
    }
}

#[derive(Debug, Clone)]
pub enum WakeEvent {
    Detected { score: f32 },
    /// 听写模式下转发的 16kHz 单声道 f32 音频块（每块 80ms / 1280 样本）
    Audio(Vec<f32>),
    /// 引擎线程遇到无法恢复的错误后退出（模型缺失 / 无麦克风 / 推理失败）
    Error(String),
}

// 引擎工作模式（AtomicU8 编码）
const MODE_DETECT: u8 = 0; // 常规：只跑唤醒词检测
const MODE_DICTATE: u8 = 1; // 唤醒后听写：转发音频帧，暂停唤醒检测（省 CPU、防复读）

pub struct WakeEngine {
    stop: Arc<AtomicBool>,
    mode: Arc<AtomicU8>,
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
        let thread = std::thread::Builder::new()
            .name("neo-wake".into())
            .spawn(move || engine_main(config, tx, stop2, mode2))
            .expect("spawn neo-wake thread");
        (
            Self {
                stop,
                mode,
                thread: Some(thread),
            },
            rx,
        )
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
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

// ---------------- 引擎线程 ----------------

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
) {
    let mut models = match load_models(&config.model_dir) {
        Ok(m) => m,
        Err(e) => {
            let _ = tx.send(WakeEvent::Error(e));
            return;
        }
    };

    let (sample_tx, sample_rx) = mpsc::channel::<Vec<i16>>();
    let err_tx = tx.clone();
    let stream = match build_stream(sample_tx, err_tx) {
        Ok(s) => s,
        Err(e) => {
            let _ = tx.send(WakeEvent::Error(e));
            return;
        }
    };
    if let Err(e) = stream.stream.play() {
        let _ = tx.send(WakeEvent::Error(format!("start mic stream: {e}")));
        return;
    }

    let mut resampler = Resampler::new(stream.sample_rate, SAMPLE_RATE);
    let mut pending: Vec<i16> = Vec::with_capacity(FRAME_SAMPLES * 2);
    let mut frames: VecDeque<Vec<f32>> = VecDeque::with_capacity(CHUNK_FRAMES);
    let mut last_fire = Instant::now() - config.debounce * 2;
    let mut chunk = vec![0f32; FRAME_SAMPLES * CHUNK_FRAMES];

    // NEO_WAKE_DEBUG=1 打开遥测：每 2s 报一次「峰值得分 + 输入电平」，
    // 用来区分「麦克风没声 / 有声但得分低 / 得分够但被防抖拦下」。
    let debug = std::env::var_os("NEO_WAKE_DEBUG").is_some();
    if debug {
        eprintln!(
            "[neo-wake] 采集设备：{} @ {}Hz（阈值 {:.2}）",
            stream.device_name, stream.sample_rate, config.threshold
        );
    }
    let mut stat_t = Instant::now();
    let mut stat_peak = 0f32;
    let mut stat_pow = 0f64;
    let mut stat_n = 0usize;
    let mut mode_seen = MODE_DETECT;

    while !stop.load(Ordering::Relaxed) {
        // 模式切换：清缓冲 + 重置防抖。
        // 回到检测模式时，滑窗需要约 2s 重新填满，期间不会误触发；
        // 听写残留的唤醒词尾音也不会在切回后立刻再烧一次。
        let m = mode.load(Ordering::Relaxed);
        if m != mode_seen {
            mode_seen = m;
            frames.clear();
            pending.clear();
            last_fire = Instant::now();
            if debug {
                eprintln!("[neo-wake] 模式切换 → {}", if m == MODE_DICTATE { "听写" } else { "检测" });
            }
        }
        // 放在收样之前：即使麦克风完全没有数据到达，周期报告也照常输出
        //（「电平 -inf、峰值 0」本身就是关键诊断信息）。
        if debug && stat_t.elapsed() >= Duration::from_secs(2) {
            let rms = if stat_n > 0 {
                (stat_pow / stat_n as f64).sqrt()
            } else {
                0.0
            };
            let db = 20.0 * (rms / 32768.0).max(1e-6).log10();
            eprintln!("[neo-wake] 峰值分 {stat_peak:.3} | 输入电平 {db:.1} dBFS（近 2s）");
            stat_t = Instant::now();
            stat_peak = 0.0;
            stat_pow = 0.0;
            stat_n = 0;
        }
        let raw = match sample_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(v) => v,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if debug {
            stat_pow += raw.iter().map(|&s| (s as f64) * (s as f64)).sum::<f64>();
            stat_n += raw.len();
        }
        resampler.process(&raw, &mut pending);

        while pending.len() >= FRAME_SAMPLES {
            let frame: Vec<f32> = pending
                .drain(..FRAME_SAMPLES)
                .map(|s| s as f32 / 32768.0)
                .collect();

            // 听写模式：帧直接转发给上层（VAD/STT），不跑唤醒推理
            if mode_seen == MODE_DICTATE {
                if tx.send(WakeEvent::Audio(frame)).is_err() {
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
                    if debug {
                        stat_peak = stat_peak.max(score);
                    }
                    if score >= config.threshold && last_fire.elapsed() >= config.debounce {
                        last_fire = Instant::now();
                        if debug {
                            eprintln!("[neo-wake] 触发！score={score:.3}");
                        }
                        // 检测后清空缓冲，与 Python listener 的 pause 行为一致
                        frames.clear();
                        pending.clear();
                        if tx.send(WakeEvent::Detected { score }).is_err() {
                            return;
                        }
                    }
                }
                Err(e) => {
                    let _ = tx.send(WakeEvent::Error(format!("inference: {e}")));
                    return;
                }
            }
        }
    }
    // stream 随作用域结束而 drop，采集回调停止
}

// ---------------- 模型加载与推理 ----------------

static ORT_INIT: OnceLock<Result<(), String>> = OnceLock::new();

fn init_ort(dir: &Path) -> Result<(), String> {
    ORT_INIT
        .get_or_init(|| {
            let dll = dir.join("onnxruntime.dll");
            // commit() 返回 bool：false 表示 environment 已配置过（幂等，可忽略）
            ort::init_from(&dll)
                .map_err(|e| format!("load {}: {e}", dll.display()))?
                .with_name("neo-wake")
                .commit();
            Ok(())
        })
        .clone()
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
    let t = mel_shape[2] as usize;
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
    Ok(score[0])
}

// ---------------- 音频采集 ----------------

struct AudioStream {
    stream: cpal::Stream,
    /// 实际采集采样率（可能不是 16000，需要重采样）
    sample_rate: u32,
    /// 设备名（启动遥测用）
    device_name: String,
}

fn build_stream(
    tx: mpsc::Sender<Vec<i16>>,
    err_tx: mpsc::Sender<WakeEvent>,
) -> Result<AudioStream, String> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| "no input device".to_string())?;

    let device_name = device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| "?".into());

    let err_cb = move |e: cpal::Error| {
        match e.kind() {
            // Xrun（缓冲区欠载/过载）与 DeviceChanged（默认设备切换、流自动跟随）
            // 都是瞬态事件，流仍在继续采集——启动期 CPU 尖峰很容易触发一次 Xrun，
            // 误当致命错误会让唤醒整局失效，这里只记录不上报。
            cpal::ErrorKind::Xrun | cpal::ErrorKind::DeviceChanged => {
                eprintln!("[neo-wake] mic stream 瞬态事件（流仍存活，忽略）: {e}");
            }
            _ => {
                let _ = err_tx.send(WakeEvent::Error(format!("mic stream: {e}")));
            }
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
        if let Ok(stream) = device.build_input_stream(
            want_16k,
            move |d: &[i16], _| {
                let _ = tx16.send(d.to_vec());
            },
            err_cb.clone(),
            None,
        ) {
            return Ok(AudioStream {
                stream,
                sample_rate: SAMPLE_RATE,
                device_name,
            });
        }
    }

    // 退回设备默认格式，自己混单声道 + 转 i16，重采样交给引擎线程
    let def = device.default_input_config().map_err(|e| e.to_string())?;
    let ch = def.channels() as usize;
    let sample_rate = def.sample_rate();
    let config: cpal::StreamConfig = def.clone().into();

    fn mix_mono<F: Fn(&f32) -> i16>(
        data: &[f32],
        ch: usize,
        conv: &F,
    ) -> Vec<i16> {
        data.chunks_exact(ch).map(|f| conv(&f[0])).collect()
    }

    let stream = match def.sample_format() {
        cpal::SampleFormat::I16 => device
            .build_input_stream(
                config.clone(),
                move |d: &[i16], _| {
                    let mono = if ch == 1 {
                        d.to_vec()
                    } else {
                        d.chunks_exact(ch)
                            .map(|f| (f.iter().map(|&s| s as i32).sum::<i32>() / ch as i32) as i16)
                            .collect()
                    };
                    let _ = tx.send(mono);
                },
                err_cb.clone(),
                None,
            )
            .map_err(|e| e.to_string())?,
        cpal::SampleFormat::F32 => device
            .build_input_stream(
                config.clone(),
                move |d: &[f32], _| {
                    let mono = mix_mono(d, ch, &|s| (s.clamp(-1.0, 1.0) * 32767.0) as i16);
                    let _ = tx.send(mono);
                },
                err_cb.clone(),
                None,
            )
            .map_err(|e| e.to_string())?,
        cpal::SampleFormat::U16 => device
            .build_input_stream(
                config,
                move |d: &[u16], _| {
                    let mono: Vec<i16> = d
                        .chunks_exact(ch)
                        .map(|f| (f[0] as i32 - 32768) as i16)
                        .collect();
                    let _ = tx.send(mono);
                },
                err_cb.clone(),
                None,
            )
            .map_err(|e| e.to_string())?,
        f => return Err(format!("unsupported sample format: {f:?}")),
    };
    Ok(AudioStream {
        stream,
        sample_rate,
        device_name,
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
