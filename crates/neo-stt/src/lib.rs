//! neo-stt：本地语音转写引擎
//!
//! - Silero VAD：从 16kHz 单声道 f32 流中切出语音段（断句）
//! - SenseVoice-Small（int8）：语音段离线转写为文本（自带标点与 ITN）
//!
//! 全部推理在本进程内完成（sherpa-onnx 静态链接），无网络依赖。
//! 引擎不是线程安全的调用者约定：请在同一个专用线程内驱动它。

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use sherpa_onnx::{
    OfflineRecognizer, OfflineRecognizerConfig, OfflineSenseVoiceModelConfig,
    SileroVadModelConfig, VadModelConfig, VoiceActivityDetector,
};

/// 引擎输入采样率（VAD 与 SenseVoice 均按 16kHz 设计）
pub const SAMPLE_RATE: i32 = 16_000;

/// Silero VAD 一次推理的消费窗口（样本数）。
/// sherpa-onnx 的 VAD 实现对单次 accept_waveform 的大块输入处理不完整
/// （5s 一次喂入只检出 0.32s），官方示例按窗口逐块喂——这里在引擎内部
/// 缓冲对齐，调用方可以喂任意长度。
const VAD_WINDOW: usize = 512;

/// 模型路径与推理参数
#[derive(Debug, Clone)]
pub struct SttConfig {
    /// SenseVoice int8 模型（model.int8.onnx）
    pub sense_voice_model: PathBuf,
    /// SenseVoice 词表（tokens.txt）
    pub tokens: PathBuf,
    /// Silero VAD 模型（silero_vad.onnx）
    pub vad_model: PathBuf,
    /// 语言标记："auto" / "zh" / "en" / "ja" / "ko" / "yue"
    pub language: String,
    /// SenseVoice 解码线程数（i5 上 4 线程即可跑满实时倍率）
    pub asr_threads: i32,
}

impl Default for SttConfig {
    fn default() -> Self {
        // 模型目录优先级：NEO_STT_MODEL_DIR 环境变量 > exe 同级 assets-stt/
        // （release 打包布局）> crate 自带 assets/（开发布局，约 240MB，不进版本库）
        let root = std::env::var_os("NEO_STT_MODEL_DIR")
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .or_else(|| {
                std::env::current_exe()
                    .ok()
                    .and_then(|exe| exe.parent().map(|d| d.join("assets-stt")))
                    .filter(|p| p.is_dir())
            })
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("assets"));
        Self {
            sense_voice_model: root.join("sense-voice").join("model.int8.onnx"),
            tokens: root.join("sense-voice").join("tokens.txt"),
            vad_model: root.join("vad").join("silero_vad.onnx"),
            language: "auto".into(),
            asr_threads: 4,
        }
    }
}

/// 一次听写会话：VAD 持续断句，每句交给 SenseVoice 转写
pub struct SttEngine {
    vad: VoiceActivityDetector,
    recognizer: OfflineRecognizer,
    /// accept_waveform 的未消费余数（凑满一个 VAD 窗口才喂下去）
    feed: Mutex<Vec<f32>>,
}

impl SttEngine {
    /// 加载模型并创建引擎。失败时返回可读错误（通常是模型文件缺失/损坏）。
    pub fn create(cfg: &SttConfig) -> Result<Self, String> {
        for (label, p) in [
            ("SenseVoice 模型", &cfg.sense_voice_model),
            ("SenseVoice 词表", &cfg.tokens),
            ("VAD 模型", &cfg.vad_model),
        ] {
            if !p.is_file() {
                return Err(format!("{label}不存在：{}", p.display()));
            }
        }

        let vad_cfg = VadModelConfig {
            silero_vad: SileroVadModelConfig {
                model: Some(cfg.vad_model.to_string_lossy().into_owned()),
                threshold: 0.5,
                // 停顿超过 0.5s 视为一句说完（语音指令场景的端点检测）
                min_silence_duration: 0.5,
                // 短于 0.25s 的响动（咳嗽/点击声）不算语音
                min_speech_duration: 0.25,
                window_size: VAD_WINDOW as i32,
                // 单句最长 20s：超时强制切段，避免长独白迟迟不出结果
                max_speech_duration: 20.0,
            },
            ten_vad: Default::default(),
            sample_rate: SAMPLE_RATE,
            num_threads: 1,
            provider: Some("cpu".into()),
            debug: std::env::var_os("NEO_STT_DEBUG").is_some(),
        };
        // 段缓冲 30s：足够排下连续说出的多个语音段
        let vad = VoiceActivityDetector::create(&vad_cfg, 30.0)
            .ok_or_else(|| "VAD 初始化失败（模型文件可能不兼容）".to_string())?;

        let mut rec_cfg = OfflineRecognizerConfig::default();
        rec_cfg.model_config.sense_voice = OfflineSenseVoiceModelConfig {
            model: Some(cfg.sense_voice_model.to_string_lossy().into_owned()),
            language: Some(cfg.language.clone()),
            use_itn: true,
        };
        rec_cfg.model_config.tokens = Some(cfg.tokens.to_string_lossy().into_owned());
        rec_cfg.model_config.num_threads = cfg.asr_threads;
        let recognizer = OfflineRecognizer::create(&rec_cfg)
            .ok_or_else(|| "SenseVoice 初始化失败（模型文件可能不兼容）".to_string())?;

        Ok(Self {
            vad,
            recognizer,
            feed: Mutex::new(Vec::with_capacity(VAD_WINDOW * 2)),
        })
    }

    /// 喂入一段 16kHz 单声道 f32 音频（长度任意，建议 20~100ms 一块）
    pub fn accept_waveform(&self, samples: &[f32]) {
        let mut feed = self.feed.lock().unwrap();
        feed.extend_from_slice(samples);
        while feed.len() >= VAD_WINDOW {
            self.vad.accept_waveform(&feed[..VAD_WINDOW]);
            feed.drain(..VAD_WINDOW);
        }
    }

    /// 当前是否正处于一段语音之中（用于 UI 的「正在听」状态）
    pub fn speech_active(&self) -> bool {
        self.vad.detected()
    }

    /// 取出一段已说完的语音（16kHz f32）。没有完整段时返回 None。
    pub fn take_segment(&self) -> Option<Vec<f32>> {
        let seg = self.vad.front()?;
        let samples = seg.samples().to_vec();
        self.vad.pop();
        Some(samples)
    }

    /// 结束输入：把缓冲中未说完的尾巴也冲刷成一个语音段
    pub fn flush(&self) {
        let mut feed = self.feed.lock().unwrap();
        if !feed.is_empty() {
            // 补零凑满一个窗口喂下去，让末尾不满 32ms 的尾音也参与断句
            feed.resize(VAD_WINDOW, 0.0);
            self.vad.accept_waveform(&feed);
            feed.clear();
        }
        self.vad.flush();
    }

    /// 丢弃全部 VAD 状态与段缓冲（开始新一轮听写前调用）
    pub fn reset(&self) {
        self.feed.lock().unwrap().clear();
        self.vad.clear();
        self.vad.reset();
    }

    /// 转写一段 16kHz 单声道 f32 语音，返回文本（已做 ITN 与标点）。
    /// 空语音/无法识别时返回空串。
    pub fn transcribe(&self, samples: &[f32]) -> Result<String, String> {
        if samples.is_empty() {
            return Ok(String::new());
        }
        let stream = self.recognizer.create_stream();
        stream.accept_waveform(SAMPLE_RATE, samples);
        self.recognizer.decode(&stream);
        let result = stream
            .get_result()
            .ok_or_else(|| "SenseVoice 解码失败".to_string())?;
        Ok(result.text.trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 冒烟测试：加载全部模型，验证转写链路通畅 + VAD 不把静音当语音。
    /// 需要 assets 模型就位（约 240MB），平时跳过：
    /// `cargo test -p neo-stt -- --ignored`
    #[test]
    #[ignore = "需要 240MB 模型文件，手动运行"]
    fn smoke_load_and_decode_silence() {
        let engine = SttEngine::create(&SttConfig::default()).expect("引擎创建失败");
        let silence = vec![0.0f32; (SAMPLE_RATE / 2) as usize];

        // 转写链路必须通畅。注意：SenseVoice 对纯静音会产生幻听（如"嗯。"），
        // 这是模型固有行为——实际产品中静音根本到不了这里（VAD 已拦住）。
        let _ = engine.transcribe(&silence).expect("转写失败");

        // VAD 必须把静音判为「无语音」：0 检出、0 分段
        engine.reset();
        engine.accept_waveform(&silence);
        engine.flush();
        assert!(!engine.speech_active(), "静音不应被 VAD 判为语音");
        assert!(
            engine.take_segment().is_none(),
            "静音不应切出语音段"
        );
    }

    #[test]
    fn default_config_paths_exist_or_env_override() {
        let cfg = SttConfig::default();
        // 只验证路径解析逻辑本身，不要求模型已下载
        assert!(cfg.sense_voice_model.ends_with("model.int8.onnx"));
        assert!(cfg.tokens.ends_with("tokens.txt"));
        assert!(cfg.vad_model.ends_with("silero_vad.onnx"));
    }

    /// 真实语音转写：NEO_STT_TEST_WAV 指向一个 16kHz 单声道 i16 PCM WAV 时，
    /// 走「VAD 切段 → 转写」完整链路并打印文本（断言只要求非空，内容人工看）。
    /// 生成测试音频（PowerShell + SAPI）：
    ///   Add-Type -AssemblyName System.Speech
    ///   $f = New-Object System.Speech.AudioFormat.SpeechAudioFormatInfo(16000,'Sixteen','Mono')
    ///   $s = New-Object System.Speech.Synthesis.SpeechSynthesizer
    ///   $s.SetOutputToWaveFile('speech.wav', $f); $s.Speak('你好'); $s.Dispose()
    #[test]
    #[ignore = "需要 NEO_STT_TEST_WAV 指向测试音频"]
    fn transcribe_real_speech_wav() {
        let Some(wav) = std::env::var_os("NEO_STT_TEST_WAV") else {
            return;
        };
        let samples = read_wav_16k_mono_i16(Path::new(&wav));
        let engine = SttEngine::create(&SttConfig::default()).expect("引擎创建失败");

        // 走完整会话链路：任意长度一次喂入（引擎内部按 VAD 窗口对齐）→ 逐段转写 → 拼接
        engine.reset();
        engine.accept_waveform(&samples);
        engine.flush();
        let mut text = String::new();
        while let Some(seg) = engine.take_segment() {
            eprintln!("[neo-stt] 语音段 {:.2}s（{} 样本）", seg.len() as f32 / SAMPLE_RATE as f32, seg.len());
            text.push_str(&engine.transcribe(&seg).expect("转写失败"));
        }
        eprintln!("[neo-stt] 转写结果：{text:?}");
        assert!(!text.is_empty(), "真实语音应转出文本");
    }

    /// 极简 WAV 解析：只接受 16kHz 单声道 i16 PCM（测试音频我们自己生成，格式已知）
    fn read_wav_16k_mono_i16(path: &Path) -> Vec<f32> {
        let data = std::fs::read(path).unwrap_or_else(|e| panic!("读 {} 失败：{e}", path.display()));
        assert!(data.len() >= 44, "WAV 太小");
        assert_eq!(&data[0..4], b"RIFF");
        assert_eq!(&data[8..12], b"WAVE");
        let channels = u16::from_le_bytes([data[22], data[23]]);
        let sample_rate = u32::from_le_bytes([data[24], data[25], data[26], data[27]]);
        let bits = u16::from_le_bytes([data[34], data[35]]);
        assert_eq!((channels, sample_rate, bits), (1, 16_000, 16), "只支持 16kHz mono i16");
        // 找到 data 块（44 字节定长头部是理想情况，稳妥起见扫一遍）
        let mut pos = 12;
        while pos + 8 <= data.len() {
            let tag = &data[pos..pos + 4];
            let size = u32::from_le_bytes([
                data[pos + 4],
                data[pos + 5],
                data[pos + 6],
                data[pos + 7],
            ]) as usize;
            if tag == b"data" {
                return data[pos + 8..pos + 8 + size]
                    .chunks_exact(2)
                    .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
                    .collect();
            }
            pos += 8 + size + (size & 1); // 块按 2 字节对齐
        }
        panic!("WAV 里没有 data 块");
    }
}
