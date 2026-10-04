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
//! 逐工具的契约见 `docs-pri/tools.md`；两个 shell 工具（Windows 与类 Unix）
//! 共用执行骨架，类 Unix 那一份走**随包提供的 Git Bash 运行时**
//! （`tools/fetch_runtime.py` 下载，见 `tools/README.md`）。

// 发行版是纯 GUI 进程：不挂靠控制台，双击启动不会先闪一个黑色命令行窗口。
// debug 构建保留控制台（eprintln 日志与 panic 信息直接可见）。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod attachments;
mod brand;
mod class;
mod diagnostics;
mod drawing_agent;
mod drawing_capture;
mod drawing_commands;
mod drawing_manager;
mod drawing_objects;
mod drawing_runtime;
mod floating;
mod graphics;
mod i18n;
mod manual_audio;
mod notify;
mod startup;
mod state;
mod ui;
mod updates;

fn window_limits(supported: &eframe::wgpu::Limits) -> eframe::wgpu::Limits {
    // 不把 eframe 默认的 8192 变成硬件门槛；保留设备可用的完整 2D
    // 尺寸范围，供最大化、跨屏拖动及辅助视口使用。
    let mut limits = eframe::wgpu::Limits::default().or_worse_values_from(supported);
    limits.max_texture_dimension_2d = supported.max_texture_dimension_2d;
    limits
}

fn window_wgpu_setup() -> eframe::egui_wgpu::WgpuSetupCreateNew {
    let mut setup = eframe::egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    // 与覆盖层统一使用 DX12；部分 Intel Vulkan 驱动在枚举时会访问违例，无法靠 Rust 错误回退。
    setup.instance_descriptor.backends = eframe::wgpu::Backends::DX12;
    setup.device_descriptor = std::sync::Arc::new(|adapter| eframe::wgpu::DeviceDescriptor {
        label: Some("Neo window device"),
        required_limits: window_limits(&adapter.limits()),
        ..Default::default()
    });
    setup
}

fn main() {
    // 探测必须早于单实例锁、splash 及任何业务线程。
    if let Some(code) = graphics::probe_command() {
        std::process::exit(code);
    }
    let _startup = match startup::begin() {
        Ok(Some(guard)) => guard,
        Ok(None) => return,
        Err(_) => {
            // startup::begin exposes only a string, not a concrete error chain.
            diagnostics::record(
                diagnostics::Level::Error,
                "startup",
                "startup initialization failed",
            );
            startup::fatal("instance", "startup initialization failed");
            return;
        }
    };
    let selection = match graphics::select(&_startup) {
        Ok(selection) => selection,
        Err(reason) => {
            graphics::startup_failed(&reason.to_string());
            return;
        }
    };
    let backend = selection.backend;
    // 窗口图标与托盘图标共用同一张鲸鱼位图（品牌蓝、方形内接居中）。
    let (rgba, width, height) = brand::whale_rgba(64);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(i18n::tr("Neo — 教室大屏 AI 助手"))
            .with_inner_size([1600.0, 1000.0])
            .with_min_inner_size([1024.0, 640.0])
            // 一体机是固定安装的，直接最大化铺满，避免老师还要拖窗口边缘。
            .with_maximized(true)
            .with_icon(egui::IconData {
                rgba,
                width,
                height,
            }),
        ..graphics::options(backend)
    };

    if let Err(error) = eframe::run_native(
        "Neo",
        options,
        Box::new(move |cc| {
            let mut app = app::NeoApp::install(&cc.egui_ctx);
            // 语音唤醒（"Hi, Neo"）只进真实客户端：离屏测试不碰麦克风。
            // 模型还没训练好时引擎会回一条 Error，界面降级为无唤醒照常工作。
            app.start_wake(&cc.egui_ctx);
            // 全屏跑马灯（独立窗口线程，初始隐藏）与 STT 线程（预载模型）：
            // 唤醒后跑马灯亮起直接听写，主界面不露面。离屏测试两者都不起。
            if backend == graphics::Backend::Dx12 {
                app.start_overlay();
            }
            app.start_stt(&cc.egui_ctx);
            app.start_floating(&cc.egui_ctx);
            // 系统托盘：关窗转后台运行，托盘菜单提供「显示主界面 / 退出」。
            // 装配失败只打日志降级为无托盘，不挡启动。
            app.start_tray();
            Ok(Box::new(selection.track(app)))
        }),
    ) {
        diagnostics::record_error("startup", "native window initialization failed", &error);
        // 不在可能已损坏的进程内重建渲染器；pending 让下次启动尝试后备。
        graphics::startup_failed("native window initialization failed");
    }
}

#[cfg(test)]
#[path = "main_graphics_tests.rs"]
mod graphics_tests;
