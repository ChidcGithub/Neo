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
