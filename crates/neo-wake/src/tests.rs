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
    let out = run(&mut r, &[7i16; 100]);
    assert_eq!(out.len(), 99);
}
