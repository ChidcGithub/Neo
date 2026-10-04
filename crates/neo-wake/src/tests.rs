use super::*;

#[test]
fn release_paths_wake_models_prefer_override_and_reject_legacy() {
    let custom = Path::new("custom/wake");
    let exe = Path::new("package");
    let source = Path::new("source/neo-wake");
    let new = exe.join("resources/models/wake");
    let old = exe.join("assets");
    for (available, expected) in [
        (
            vec![custom.to_path_buf(), new.clone(), old.clone()],
            custom.to_path_buf(),
        ),
        (vec![new.clone(), old.clone()], new.clone()),
        (vec![new.clone()], new),
        (vec![old], source.join("assets")),
        (vec![], source.join("assets")),
    ] {
        assert_eq!(
            resolve_model_dir(Some(custom), Some(exe), source, |p| available
                .iter()
                .any(|v| v == p)),
            expected,
        );
    }
    assert_eq!(
        resolve_model_dir(None, None, source, |_| false),
        source.join("assets")
    );
}

#[test]
fn release_paths_wake_dll_splits_runtime_and_preserves_explicit_model_dir() {
    // 不使用默认模型路径；覆盖直接赋值 WakeConfig.model_dir 的调用方。
    let config = WakeConfig {
        model_dir: PathBuf::from("custom/wake"),
        threshold: 0.25,
        debounce: Duration::from_secs(2),
    };
    let exe = Path::new("package");
    let source = Path::new("source/neo-wake");
    let local = config.model_dir.join("onnxruntime.dll");
    let new = exe.join("runtime/onnx/onnxruntime.dll");
    let old = exe.join("assets/onnxruntime.dll");
    let dev = source.join("assets/onnxruntime.dll");
    for (available, expected) in [
        (
            vec![local.clone(), new.clone(), old.clone(), dev.clone()],
            local.clone(),
        ),
        (vec![new.clone(), old.clone(), dev.clone()], new.clone()),
        (vec![new.clone()], new.clone()),
        (vec![old.clone(), dev.clone()], dev.clone()),
        (vec![old], new.clone()),
        (vec![dev.clone()], dev),
        (vec![], new),
    ] {
        assert_eq!(
            resolve_ort_path(&config.model_dir, Some(exe), source, |p| available
                .iter()
                .any(|v| v == p)),
            expected,
        );
    }
    assert_eq!(
        resolve_ort_path(&config.model_dir, None, source, |_| false),
        local
    );
}

#[test]
fn release_paths_wake_new_models_use_separate_runtime() {
    let exe = Path::new("package");
    let source = Path::new("source/neo-wake");
    let expected_model = exe.join("resources/models/wake");
    let expected_dll = exe.join("runtime/onnx/onnxruntime.dll");
    let model = resolve_model_dir(None, Some(exe), source, |p| p == expected_model);
    assert_eq!(model, expected_model);
    assert_eq!(
        resolve_ort_path(&model, Some(exe), source, |p| p == expected_dll),
        expected_dll,
    );
}

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
    let out = run(&mut r, &[7i16; 100]);
    assert_eq!(out.len(), 99);
}
