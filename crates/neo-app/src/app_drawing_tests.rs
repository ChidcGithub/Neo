//! Offscreen state/transaction tests: no GUI process, microphone, capture or network.
use super::*;
use crate::drawing_manager::tests::{attach, state};
use crate::drawing_runtime::{BoardKind, Event};

fn tick_frame(app: &mut NeoApp, ctx: &egui::Context) {
    ctx.begin_pass(egui::RawInput::default());
    app.tick(ctx.clone());
    ctx.end_pass().textures_delta.clear();
}

fn app() -> (egui::Context, NeoApp) {
    let ctx = egui::Context::default();
    let mut app = NeoApp::install(&ctx);
    app.state.wake_enabled = false;
    app.state.draft.clear();
    (ctx, app)
}

fn hosted_capture(
    app: &mut NeoApp,
    ctx: &egui::Context,
) -> (
    crate::drawing_manager::tests::Fake,
    crate::drawing_capture::FakeCapture,
) {
    use crate::drawing_runtime::Permissions;
    app.state.classroom_safe = false;
    app.drawing.set_services(app.state.llm_config(), ctx);
    let f = attach(&mut app.drawing, BoardKind::Drawing, 909);
    let mut s = state(true, false, false);
    s.permissions = Permissions::new(false, true, false);
    f.emit(Event::Ready(s.clone()));
    app.drawing.poll(false);
    f.respond(s.clone());
    app.drawing.poll(false);
    f.emit(Event::HostRequest { id: "runtime:cap".into(), method: "host.capture_region".into(),
        params: serde_json::json!({"document_id":s.document_id,"page_id":s.page_id,"revision":s.revision,
            "job_id":"job-cap","user_authorized":true,"windows_hidden_confirmed":true}) });
    app.drawing.poll(false);
    let (worker, control) = crate::drawing_capture::CaptureHandle::fake(
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        ctx.clone(),
    );
    app.drawing.inject_capture(worker);
    (f, control)
}

#[test]
fn drawing_capture_two_pass_barrier_and_show_wait_for_cleanup() {
    let (ctx, mut app) = app();
    let (fake, control) = hosted_capture(&mut app, &ctx);
    app.tick_drawing_barrier(&ctx);
    assert!(app.drawing.capture_waiting(), "first pass may only hide");
    assert!(app.drawing_desktop.windows.main_avoided);
    assert!(
        !app.desktop_windows.main_avoided,
        "desktop tool state is independent"
    );
    assert!(desktop_suspended(&ctx));
    app.tick_drawing_barrier(&ctx);
    assert!(!app.drawing.capture_waiting());
    app.show_window(&ctx);
    assert!(control.is_cancelled());
    app.drawing.poll(false);
    app.tick_drawing_barrier(&ctx);
    assert!(app.drawing_desktop.windows.main_avoided);
    assert!(desktop_suspended(&ctx));
    assert!(app.desktop_show_pending);
    assert!(fake.replies().is_empty());
    control.finish(Err("cancelled".into()));
    app.drawing.poll(false);
    app.tick_drawing_barrier(&ctx);
    app.tick_desktop_barrier(&ctx);
    assert!(!app.drawing_desktop.windows.main_avoided);
    assert!(!desktop_suspended(&ctx));
    assert!(!app.desktop_show_pending);
    assert!(!app.hidden_to_tray);
    assert!(fake.replies().last().unwrap().1.is_err());
}

#[test]
fn drawing_capture_hidden_to_tray_is_preserved_and_failed_barrier_does_not_start() {
    for failure in [false, true] {
        let (ctx, mut app) = app();
        app.hidden_to_tray = true;
        let (fake, control) = hosted_capture(&mut app, &ctx);
        app.tick_drawing_barrier(&ctx);
        app.drawing_desktop.windows.fail_verify = failure;
        app.tick_drawing_barrier(&ctx);
        if failure {
            app.drawing.poll(false);
            assert!(!app.drawing.capture_active());
            assert!(fake.replies().last().unwrap().1.is_err());
        } else {
            control.finish(Ok(b"\x89PNG\r\n\x1a\n".to_vec()));
            app.drawing.poll(false);
        }
        app.tick_drawing_barrier(&ctx);
        app.tick_desktop_barrier(&ctx);
        assert!(app.hidden_to_tray);
        assert!(!app.drawing_desktop.windows.main_avoided);
        assert!(!desktop_suspended(&ctx));
    }
}

#[test]
fn drawing_capture_exit_waits_for_cleanup_before_sending_close() {
    let (ctx, mut app) = app();
    let (fake, control) = hosted_capture(&mut app, &ctx);
    app.tick_drawing_barrier(&ctx);
    app.tick_drawing_barrier(&ctx);
    app.finish_exit(&ctx);
    app.drawing.poll(false);
    assert!(app.drawing_exit_pending);
    assert!(!app.quitting);
    assert!(control.is_cancelled());
    assert!(!fake.sent().iter().any(|r| r.1 == "close"));
    app.tick_drawing_barrier(&ctx);
    assert!(app.drawing_desktop.started.is_some());
    control.finish(Err("cancelled".into()));
    app.drawing.poll(false);
    app.tick_drawing_barrier(&ctx);
    assert!(fake.sent().iter().any(|r| r.1 == "close"));
    assert!(app.drawing_desktop.started.is_none());
    assert!(
        !app.quitting,
        "still waiting for actual runtime process exit"
    );
}

#[test]
fn drawing_install_is_lazy_and_missing_runtime_notice_does_not_wake() {
    let (ctx, mut app) = app();
    assert!(app.drawing.closed());
    app.open_board(BoardKind::Drawing, &ctx);
    assert!(app.drawing.closed());
    assert!(!app.pending_toasts.is_empty());
    assert!(!app.dictating);
    assert!(app.manual_audio.is_none());
}

#[test]
fn drawing_exact_voice_dispatch_preserves_conversation_and_never_wakes() {
    for text in [
        "打开黑板",
        "打开画板",
        "打开白板",
        "open blackboard",
        "open drawing",
        "open whiteboard",
    ] {
        let (ctx, mut app) = app();
        let active = app.state.active_session;
        assert!(app.handle_board_dictation(text, &ctx));
        assert!(app.state.draft.is_empty());
        assert_eq!(app.state.active_session, active);
        assert!(!app.dictating);
        assert!(app.manual_audio.is_none());
    }
}

#[test]
fn drawing_voice_does_not_consume_prose_draft_or_busy_input() {
    let (ctx, mut app) = app();
    for text in [
        "请解释如何打开黑板",
        "we could open drawing tomorrow",
        "打开黑板然后讲课",
    ] {
        assert!(!app.handle_board_dictation(text, &ctx));
    }
    for draft in ["existing", " "] {
        app.state.draft = draft.into();
        assert!(!app.handle_board_dictation("打开黑板", &ctx));
        assert_eq!(app.state.draft, draft);
    }
    app.state.draft.clear();
    app.state.generating = true;
    assert!(!app.handle_board_dictation("打开黑板", &ctx));
    app.state.generating = false;
    app.state.attachment_picker_open = true;
    assert!(!app.handle_board_dictation("打开黑板", &ctx));
}

#[test]
fn drawing_desktop_admission_reports_block_and_cannot_start() {
    let (ctx, mut app) = app();
    let gate = app.state.desktop_execution.as_ref().unwrap();
    gate.active.store(1, std::sync::atomic::Ordering::Release);
    assert!(app.handle_board_dictation("打开黑板", &ctx));
    assert!(app.drawing.closed());
    assert_eq!(app.pending_toasts.len(), 1);
    assert!(!app.dictating);
}

fn desktop_request(
    app: &NeoApp,
) -> (
    crate::state::DesktopRequest,
    std::sync::mpsc::Receiver<Result<(), String>>,
) {
    app.state
        .desktop_execution
        .as_ref()
        .unwrap()
        .active
        .store(1, std::sync::atomic::Ordering::Release);
    let (ack, rx) = std::sync::mpsc::channel();
    (
        crate::state::DesktopRequest {
            cancel: Default::default(),
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(30),
            ack,
        },
        rx,
    )
}

#[test]
fn drawing_desktop_rejects_starting_visible_hidden_closed_and_disconnected_boards_without_hiding_neo(
) {
    for case in 0..5 {
        for pending in [false, true] {
            let (ctx, mut app) = app();
            let fake = attach(&mut app.drawing, BoardKind::Drawing, 1);
            if case != 0 {
                fake.emit(Event::Ready(state(false, true, true)));
                app.drawing.poll(true);
                fake.respond(state(case >= 2, true, true));
                app.drawing.poll(true);
                if case == 3 {
                    fake.emit(Event::Failed("EOF".into()));
                }
                if case == 4 {
                    let mut closed = state(true, false, true);
                    closed.closed = true;
                    fake.emit(Event::StateChanged(closed));
                }
                app.drawing.poll(true);
            }
            let sent = fake.sent().len();
            let (request, rx) = desktop_request(&app);
            let cancel = request.cancel.clone();
            if pending {
                app.desktop_pending.push(request);
            } else {
                app.state
                    .desktop_execution
                    .as_ref()
                    .unwrap()
                    .requests
                    .send(request)
                    .unwrap();
            }
            app.tick_desktop_barrier(&ctx);
            assert!(
                rx.try_recv().unwrap().is_err(),
                "case={case}, pending={pending}"
            );
            assert!(cancel.load(std::sync::atomic::Ordering::Acquire));
            assert!(!app.desktop_windows.main_avoided);
            assert!(!desktop_suspended(&ctx));
            assert!(app.desktop_pending.is_empty());
            assert!(app.desktop_cancels.is_empty());
            assert!(!app.pending_toasts.is_empty());
            assert_eq!(
                fake.sent().len(),
                sent,
                "rejection must not hide/suspend/show the board"
            );
        }
    }
}

#[test]
fn drawing_desktop_hidden_to_visible_and_eof_cancel_active_immediately_until_exited() {
    let (ctx, mut app) = app();
    let fake = attach(&mut app.drawing, BoardKind::Drawing, 1);
    fake.emit(Event::Ready(state(true, true, true)));
    app.drawing.poll(true);
    fake.respond(state(true, true, true));
    app.drawing.poll(true);
    let (request, _) = desktop_request(&app);
    let cancel = request.cancel;
    app.desktop_cancels.push(cancel.clone());
    for event in [
        Event::StateChanged(state(false, true, true)),
        Event::Failed("EOF".into()),
    ] {
        cancel.store(false, std::sync::atomic::Ordering::Release);
        fake.emit(event);
        app.drawing.poll(true);
        app.tick_desktop_barrier(&ctx);
        assert!(cancel.load(std::sync::atomic::Ordering::Acquire));
        assert!(app.drawing.has_open_boards());
    }
    fake.emit(Event::Exited);
    app.drawing.poll(true);
    app.state
        .desktop_execution
        .as_ref()
        .unwrap()
        .active
        .store(0, std::sync::atomic::Ordering::Release);
    app.tick_desktop_barrier(&ctx);
    let (request, rx) = desktop_request(&app);
    app.state
        .desktop_execution
        .as_ref()
        .unwrap()
        .requests
        .send(request)
        .unwrap();
    app.tick_desktop_barrier(&ctx);
    assert!(rx.try_recv().is_err());
    app.tick_desktop_barrier(&ctx);
    assert!(
        rx.try_recv().unwrap().is_ok(),
        "only actual Exited releases the guard"
    );
    assert!(!fake.sent().iter().any(|r| r.1.starts_with("window.")));
}

#[test]
fn repeated_exit_save_failure_revokes_drawing_and_desktop_continuations() {
    for desktop_pending in [false, true] {
        let (ctx, mut app) = app();
        let fake = attach(&mut app.drawing, BoardKind::Drawing, 1);
        fake.emit(Event::Ready(state(false, false, true)));
        app.drawing.poll(true);
        fake.respond(state(false, false, true));
        app.drawing.poll(true);
        if desktop_pending {
            app.state
                .desktop_execution
                .as_ref()
                .unwrap()
                .active
                .store(1, std::sync::atomic::Ordering::Release);
        }
        app.request_exit(&ctx);
        assert!(!app.exit_blocked && !app.quitting);
        assert_eq!(app.desktop_exit_pending, desktop_pending);
        assert_eq!(app.drawing_exit_pending, !desktop_pending);
        if !desktop_pending {
            app.drawing.poll(true);
            assert_eq!(fake.sent().last().unwrap().1, "close");
            let mut closed = state(true, false, true);
            closed.closed = true;
            fake.respond(closed);
            tick_frame(&mut app, &ctx);
            assert!(app.drawing_exit_pending && !app.quitting);
        }

        app.store = None;
        app.request_exit(&ctx);
        assert!(app.exit_blocked && !app.quitting);
        assert!(!app.drawing_exit_pending && !app.desktop_exit_pending);
        fake.emit(Event::Exited);
        app.state
            .desktop_execution
            .as_ref()
            .unwrap()
            .active
            .store(0, std::sync::atomic::Ordering::Release);
        tick_frame(&mut app, &ctx);
        assert!(app.drawing.closed());
        assert!(app.exit_blocked && !app.quitting);
        // Dismissing the error must not resurrect an older exit continuation.
        app.exit_blocked = false;
        tick_frame(&mut app, &ctx);
        assert!(!app.quitting);
    }
}

#[test]
fn finish_exit_cannot_override_save_failure_even_on_discard_confirmation_screen() {
    let (ctx, mut app) = app();
    app.store = None;
    app.request_exit(&ctx);
    for confirm in [false, true] {
        app.confirm_discard = confirm;
        app.finish_exit(&ctx);
        assert!(app.exit_blocked && !app.quitting);
        assert!(!app.drawing_exit_pending && !app.desktop_exit_pending);
    }
}

#[test]
fn drawing_exit_never_discards_board_and_waits_for_process_exit() {
    let (ctx, mut app) = app();
    let fake = attach(&mut app.drawing, BoardKind::Drawing, 1);
    fake.emit(Event::Ready(state(false, true, true)));
    app.drawing.poll(true);
    fake.respond(state(false, true, true));
    app.drawing.poll(true);
    app.finish_exit(&ctx);
    assert!(!app.quitting);
    assert!(app.drawing_exit_pending);
    app.drawing.poll(true);
    fake.error("unsaved_changes");
    tick_frame(&mut app, &ctx);
    assert!(!app.quitting);
    assert!(!app.drawing_exit_pending);
    assert!(!app.drawing.closed());
    assert_eq!(fake.sent().last().unwrap().1, "show");
    fake.respond(state(false, false, true));
    app.drawing.poll(true);
    app.finish_exit(&ctx);
    app.drawing.poll(true);
    let mut closed = state(true, false, true);
    closed.closed = true;
    fake.respond(closed);
    tick_frame(&mut app, &ctx);
    assert!(!app.quitting);
    fake.emit(Event::Exited);
    tick_frame(&mut app, &ctx);
    assert!(app.quitting);
    assert!(!fake
        .sent()
        .iter()
        .any(|request| request.2.get("discard_unsaved").is_some()));
}
