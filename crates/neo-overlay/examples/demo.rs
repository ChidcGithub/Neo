//! 实机冒烟：显示 12 秒全屏跑马灯，模拟音频电平起伏。

fn main() {
    let mut h = neo_overlay::start().expect("overlay 初始化失败（需要支持 DX12 的 GPU）");
    h.show();
    let t0 = std::time::Instant::now();
    while t0.elapsed().as_secs() < 12 {
        let s = t0.elapsed().as_secs_f32();
        // 模拟说话时的电平起伏
        let lvl = ((s * 2.0).sin() * 0.5 + 0.5) * 0.6;
        h.set_level(lvl);
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    h.hide();
    std::thread::sleep(std::time::Duration::from_millis(500));
    h.shutdown();
    println!("ok");
}
