//! 悬浮入口集成测试：无原生窗口、麦克风或真实 STT 模型。
use super::*;

fn app() -> (egui::Context, NeoApp, std::sync::mpsc::Receiver<SttCmd>) {
    let ctx = egui::Context::default();
    let mut app = NeoApp::install(&ctx);
    app.state.floating_enabled = true;
    app.state.classroom_safe = false;
    app.applied_classroom_safe = false;
    app.state.wake_enabled = false;
    let (tx, rx) = std::sync::mpsc::channel();
    app.stt_tx = Some(tx);
    (ctx, app, rx)
}

#[test]
fn floating_setting_defaults_off_and_roundtrips() {
    let ctx = egui::Context::default();
    let mut app = NeoApp::install(&ctx);
    assert!(!app.state.floating_enabled);
    assert!(app.floating.is_none());
    app.state.floating_enabled = true;
    assert!(app.persist_preferences());
    assert!(app.saved_floating_enabled);
    app.state.floating_enabled = false;
    app.load_settings();
    assert!(app.state.floating_enabled);
    for value in ["0", "broken", "", "1"] {
        app.store.as_ref().unwrap().set_setting("floating_enabled", value).unwrap();
        app.load_settings();
        assert_eq!(app.state.floating_enabled, value == "1");
    }
}

#[test]
fn floating_wake_reuses_dictation_without_enabling_voice_wake() {
    let (ctx, mut app, rx) = app();
    app.handle_floating_wake(&ctx, true);
    assert!(app.dictating && app.manual_dictation);
    assert!(app.dictation_since.is_some());
    assert!(!app.state.wake_enabled);
    assert!(matches!(rx.try_recv(), Ok(SttCmd::Reset(epoch)) if epoch == app.dictation_epoch));
    let epoch = app.dictation_epoch;
    app.handle_floating_wake(&ctx, true);
    assert_eq!(app.dictation_epoch, epoch, "重复单击不得重置正在听写的会话");
    assert!(rx.try_recv().is_err());
    app.cancel_dictation();
    assert!(!app.dictating && !app.manual_dictation);
    assert!(app.manual_audio.is_none());
    assert!(!current_dictation(app.dictating, app.dictation_epoch, epoch));
}

#[test]
fn floating_admission_respects_safety_busy_and_disabled_states() {
    for case in 0..8 {
        let (ctx, mut app, rx) = app();
        match case {
            0 => app.state.classroom_safe = true,
            1 => app.state.floating_enabled = false,
            2 => app.state.generating = true,
            3 => app.state.tool_round = true,
            4 => app.state.wake_test.requested = true,
            5 => app.exit_blocked = true,
            6 => app.quitting = true,
            _ => { ctx.data_mut(|d| d.insert_temp(egui::Id::new("neo-desktop-suspended"), true)); }
        }
        app.handle_floating_wake(&ctx, true);
        assert!(!app.dictating && !app.manual_dictation, "case {case}");
        assert!(rx.try_recv().is_err());
        assert!(app.manual_audio.is_none());
    }
}

#[test]
fn floating_missing_overlay_or_stt_uses_existing_input_fallback() {
    for missing_overlay in [false, true] {
        let (ctx, mut app, rx) = app();
        if !missing_overlay { app.stt_tx = None; }
        app.hidden_to_tray = true;
        app.handle_floating_wake(&ctx, !missing_overlay);
        assert!(!app.hidden_to_tray);
        assert!(!app.dictating && !app.manual_dictation);
        assert!(app.manual_audio.is_none());
        assert!(rx.try_recv().is_err());
    }
}

#[test]
fn disabling_floating_cancels_manual_but_not_voice_dictation() {
    let (ctx, mut app, _) = app();
    app.handle_floating_wake(&ctx, true);
    app.state.floating_enabled = false;
    app.sync_floating(&ctx);
    assert!(!app.dictating && !app.manual_dictation);
    app.handle_wake_detected(&ctx, 0.9, true);
    assert!(app.dictating && !app.manual_dictation);
    app.sync_floating(&ctx);
    assert!(app.dictating, "关闭悬浮按钮不能打断语音唤醒的听写");
}

#[test]
fn queued_voice_events_cannot_duplicate_manual_audio_or_restart_session() {
    let (ctx, mut app, stt) = app();
    app.handle_floating_wake(&ctx, true);
    let _ = stt.try_recv();
    let epoch = app.dictation_epoch;
    app.state.wake_enabled = true;
    let (tx, rx) = std::sync::mpsc::channel();
    app.wake_rx = Some(rx);
    tx.send(neo_wake::WakeEvent::Detected { score: 1.0, epoch: app.wake_epoch }).unwrap();
    tx.send(neo_wake::WakeEvent::Audio { frame: vec![0.1; 1280], epoch: app.wake_epoch }).unwrap();
    app.poll_wake_events(&ctx, false);
    assert_eq!(app.dictation_epoch, epoch);
    assert!(app.dictating && app.manual_dictation);
    assert!(stt.try_recv().is_err());
    app.fail_wake("模拟语音唤醒设备错误".into());
    assert!(app.dictating && app.manual_dictation, "独立采集不受唤醒词引擎故障影响");
}
