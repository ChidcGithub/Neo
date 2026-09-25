//! # Neo
//!
//! 面向教室大屏（触控一体机）的全场景 AI 助手客户端。
//!
//! ## 这个骨架做了什么
//!
//! - **视觉语言**取自 DeepSeek Harness：同一套 `--dsw-*` 语义色板、
//!   同样的 22px 输入卡、28/34px 圆形控件、`superellipse(1.5)` 超椭圆圆角、
//!   同一只鲸鱼标志（路径直接从上游源码提取并光栅化）；
//! - **大屏适配**是 Neo 自己的：所有度量乘一个统一倍率，
//!   倍率 = 像素密度 × 观看距离系数，并在界面上可见可调；
//! - **触控**：圆形控件的视觉尺寸保持上游比例，命中区单独扩到 48pt 下限。
//!
//! ## 模块
//!
//! | 模块 | 职责 |
//! |------|------|
//! | `neo_theme` | 设计系统：色板、度量、字号、超椭圆圆角 |
//! | [`brand`] | 鲸鱼标志的解析与光栅化 |
//! | [`state`] | 全部可变状态与状态迁移 |
//! | [`ui`] | 自绘界面：侧栏 / 空态 / 对话态 / 输入卡 / 显示设置 |
//! | [`app`] | 一帧的编排与动作消费 |
//!
//! 工具集（读/写/改文件、看图、打开文件、执行命令）在 `neo-tools`，
//! 逐工具的契约见 `docs/tools.md`；两个 shell 工具（Windows 与类 Unix）
//! 共用执行骨架，类 Unix 那一份走**随包提供的 Git Bash 运行时**
//! （`tools/fetch_runtime.py` 下载，见 `tools/README.md`）。

mod app;
mod attachments;
mod brand;
mod state;
mod ui;

fn main() -> eframe::Result {
    // 窗口图标与托盘图标共用同一张鲸鱼位图（品牌蓝、方形内接居中）。
    let (rgba, width, height) = brand::whale_rgba(64);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Neo — 教室大屏 AI 助手")
            .with_inner_size([1600.0, 1000.0])
            .with_min_inner_size([1024.0, 640.0])
            // 一体机是固定安装的，直接最大化铺满，避免老师还要拖窗口边缘。
            .with_maximized(true)
            .with_icon(egui::IconData {
                rgba,
                width,
                height,
            }),
        ..Default::default()
    };

    eframe::run_native(
        "Neo",
        options,
        Box::new(|cc| {
            let mut app = app::NeoApp::install(&cc.egui_ctx);
            // 语音唤醒（"Hi, Neo"）只进真实客户端：离屏测试不碰麦克风。
            // 模型还没训练好时引擎会回一条 Error，界面降级为无唤醒照常工作。
            app.start_wake(&cc.egui_ctx);
            // 全屏跑马灯（独立窗口线程，初始隐藏）与 STT 线程（预载模型）：
            // 唤醒后跑马灯亮起直接听写，主界面不露面。离屏测试两者都不起。
            app.start_overlay();
            app.start_stt(&cc.egui_ctx);
            // 系统托盘：关窗转后台运行，托盘菜单提供「显示主界面 / 退出」。
            // 装配失败只打日志降级为无托盘，不挡启动。
            app.start_tray();
            Ok(Box::new(app))
        }),
    )
}
