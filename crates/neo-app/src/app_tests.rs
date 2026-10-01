//! 截图反馈与桌面屏障的离屏时序回归；不启动 overlay、不抓屏、不注入输入。
use super::*;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

fn request(app: &NeoApp) -> std::sync::mpsc::Receiver<Result<(), String>> {
    let gate = app.state.desktop_execution.as_ref().unwrap();
    gate.active.fetch_add(1, Ordering::AcqRel);
    let (ack, rx) = std::sync::mpsc::channel();
    gate.requests.send(crate::state::DesktopRequest {
        cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        deadline: Instant::now() + Duration::from_secs(30),
        ack,
    }).unwrap();
    rx
}

fn frame(app: &mut NeoApp, ctx: &egui::Context, now: Instant, signal: u64) -> egui::FullOutput {
    ctx.begin_pass(egui::RawInput::default());
    app.tick_desktop_barrier(ctx);
    app.shotflash.tick_at(ctx, None, now, (signal, Some(neo_tools::tools::screen::Rect {
        x: -800, y: 0, width: 800, height: 600,
    })));
    let mut output = ctx.end_pass();
    output.textures_delta.clear();
    assert!(!output.viewport_output[&egui::ViewportId::ROOT].commands.iter().any(|cmd|
        matches!(cmd, egui::ViewportCommand::Visible(true)
            | egui::ViewportCommand::Minimized(false) | egui::ViewportCommand::Focus)),
        "截图反馈不能恢复主窗或抢焦点");
    output
}

#[test]
fn screenshot_flash_waits_for_last_lease_and_resumes_without_restoring_hidden_windows() {
    for tray in [false, true] {
        let ctx = egui::Context::default();
        ctx.set_embed_viewports(false);
        let mut app = NeoApp::install(&ctx);
        app.hidden_to_tray = tray;
        app.state.tool_open = true;
        let now = Instant::now();
        let id = egui::ViewportId::from_hash_of("neo-shotflash");
        let ack = request(&app);
        let output = frame(&mut app, &ctx, now, 0);
        assert!(ack.try_recv().is_err(), "隐藏和验证不能在同一 pass 完成");
        assert!(desktop_suspended(&ctx));
        assert_eq!(output.viewport_output[&id].builder.visible, Some(false));
        frame(&mut app, &ctx, now + Duration::from_millis(10), 0);
        assert!(ack.try_recv().unwrap().is_ok());
        // capture 在取得回执后才广播 signal；超过旧 delay + 动画时长仍不得显示。
        for millis in [20, 500, 1000, 5000] {
            let output = frame(&mut app, &ctx, now + Duration::from_millis(millis), 1);
            assert_eq!(output.viewport_output[&id].builder.visible, Some(false));
            assert_eq!(output.viewport_output[&id].builder.inner_size, Some(egui::Vec2::splat(1.0)));
        }
        // 两个租约重叠，结束一个不能恢复；与 overlay guard 的引用计数语义一致。
        let next_ack = request(&app);
        frame(&mut app, &ctx, now + Duration::from_secs(6), 2);
        frame(&mut app, &ctx, now + Duration::from_secs(7), 2);
        assert!(next_ack.try_recv().unwrap().is_ok());
        app.state.desktop_execution.as_ref().unwrap().active.fetch_sub(1, Ordering::AcqRel);
        let output = frame(&mut app, &ctx, now + Duration::from_secs(8), 2);
        assert!(desktop_suspended(&ctx));
        assert_eq!(output.viewport_output[&id].builder.visible, Some(false));
        app.state.desktop_execution.as_ref().unwrap().active.fetch_sub(1, Ordering::AcqRel);
        let resumed = now + Duration::from_secs(9);
        let output = frame(&mut app, &ctx, resumed, 2);
        assert!(!desktop_suspended(&ctx));
        let flash = &output.viewport_output[&id];
        assert_eq!(flash.builder.visible, Some(true));
        assert!(flash.builder.inner_size.is_some_and(|size| size.x > 1.0 && size.y > 1.0));
        assert!(flash.commands.iter().any(|cmd| matches!(cmd, egui::ViewportCommand::Visible(true))));
        assert_eq!(app.hidden_to_tray, tray);
        assert_eq!(app.desktop_windows.main_avoided, !tray);
        assert!(!app.state.round_cancelled);
        // 不恢复过往辅助窗口；flash 只向自己的 viewport 发送恢复命令。
        assert!(output.viewport_output.keys().all(|viewport| *viewport == id || *viewport == egui::ViewportId::ROOT));
        let output = frame(&mut app, &ctx, resumed + Duration::from_millis(451), 2);
        assert_eq!(output.viewport_output[&id].builder.inner_size, Some(egui::Vec2::splat(1.0)));
        // 下一次截图进入时必须隐藏，新的 signal 在完成后重新起播。
        let ack = request(&app);
        let output = frame(&mut app, &ctx, resumed + Duration::from_secs(1), 3);
        assert!(ack.try_recv().is_err());
        assert_eq!(output.viewport_output[&id].builder.visible, Some(false));
        frame(&mut app, &ctx, resumed + Duration::from_secs(2), 3);
        assert!(ack.try_recv().unwrap().is_ok());
        app.state.desktop_execution.as_ref().unwrap().active.store(0, Ordering::Release);
        let output = frame(&mut app, &ctx, resumed + Duration::from_secs(3), 3);
        assert!(output.viewport_output[&id].builder.inner_size.is_some_and(|size| size.x > 1.0));
        assert_eq!(app.desktop_windows.main_avoided, !tray);
        assert_eq!(app.hidden_to_tray, tray);
    }
}

#[test]
fn expired_flash_is_not_requeued_by_non_capture_lease_without_intervening_tick() {
    for new_signal in [false, true] {
        let ctx = egui::Context::default();
        ctx.set_embed_viewports(false);
        let mut app = NeoApp::install(&ctx);
        app.hidden_to_tray = true;
        app.state.tool_open = true;
        let now = Instant::now();
        let id = egui::ViewportId::from_hash_of("neo-shotflash");
        frame(&mut app, &ctx, now, 1);
        let output = frame(&mut app, &ctx, now + Duration::from_millis(450), 1);
        assert!(output.viewport_output[&id].builder.inner_size.is_some_and(|size| size.x > 1.0));
        // 不先 tick 清理旧动画，直接在播放 450ms 后进入新屏障。
        let ack = request(&app);
        let signal = if new_signal { 2 } else { 1 };
        let output = frame(&mut app, &ctx, now + Duration::from_millis(900), signal);
        assert!(ack.try_recv().is_err());
        assert_eq!(output.viewport_output[&id].builder.visible, Some(false));
        frame(&mut app, &ctx, now + Duration::from_secs(2), signal);
        assert!(ack.try_recv().unwrap().is_ok());
        app.state.desktop_execution.as_ref().unwrap().active.store(0, Ordering::Release);
        let output = frame(&mut app, &ctx, now + Duration::from_secs(3), signal);
        assert!(!desktop_suspended(&ctx));
        let size = output.viewport_output[&id].builder.inner_size.unwrap();
        assert_eq!(size.x > 1.0 && size.y > 1.0, new_signal,
            "只有新截图能重播；非截图屏障不能复活已过期的旧动画");
        assert!(app.hidden_to_tray && !app.desktop_windows.main_avoided);
        assert!(!app.state.round_cancelled);
    }
}
