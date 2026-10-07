
//! 启动恢复：不依赖 egui，直接驱动存储与状态。
use super::NeoApp;
use crate::state::{AppState, Stage};
use neo_store::Store;

fn temp_store(tag: &str) -> Store {
    let dir = std::env::temp_dir().join(format!("neo-restore-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    Store::open(&dir.join("neo.db")).expect("打开测试库")
}

// SQLite URI 只读连接：使用真实数据库写错误，不用空 Store 冒充失败。
fn readonly_store(path: &std::path::Path) -> Store {
    let path = path
        .to_string_lossy()
        .replace('\\', "/")
        .replace('%', "%25")
        .replace(' ', "%20")
        .replace('#', "%23")
        .replace('?', "%3F");
    Store::open(std::path::Path::new(&format!("file:{path}?mode=ro")))
        .expect("打开真实 SQLite 只读连接")
}

#[test]
fn startup_preferences_roundtrip_and_failed_writes_keep_exit_barrier() {
    let ctx = egui::Context::default();
    let mut app = NeoApp::install(&ctx);
    assert!(!app.state.silent_startup_errors);
    assert!(app.state.auto_check_updates);
    app.state.silent_startup_errors = true;
    app.state.auto_check_updates = false;
    assert!(app.persist_preferences());
    assert!(app.saved_silent_startup_errors);
    assert!(!app.saved_auto_check_updates);
    app.state.silent_startup_errors = false;
    app.state.auto_check_updates = true;
    app.load_settings();
    assert!(app.state.silent_startup_errors);
    assert!(!app.state.auto_check_updates);
    for value in ["broken", "", "false", "1", "0"] {
        let store = app.store.as_ref().unwrap();
        store.set_setting("silent_startup_errors", value).unwrap();
        store.set_setting("auto_check_updates", value).unwrap();
        app.load_settings();
        assert_eq!(app.state.silent_startup_errors, value == "1");
        assert_eq!(app.state.auto_check_updates, value != "0");
    }
    app.state.silent_startup_errors = true;
    app.state.auto_check_updates = false;
    assert!(app.persist_preferences());
    app.store = Some(readonly_store(&super::test_db_path()));
    app.state.silent_startup_errors = false;
    app.state.auto_check_updates = true;
    assert!(!app.persist_preferences());
    assert!(app.saved_silent_startup_errors);
    assert!(!app.saved_auto_check_updates);
    assert!(app.state.preferences_unsaved);
    app.request_exit(&ctx);
    assert!(app.exit_blocked && !app.quitting);
    app.store = Some(Store::open(&super::test_db_path()).unwrap());
    assert!(app.persist_preferences());
    assert!(!app.saved_silent_startup_errors);
    assert!(app.saved_auto_check_updates);
    assert!(!app.state.preferences_unsaved);
}

#[test]
fn startup_silent_policy_requires_both_preferences() {
    let mut state = AppState::default();
    for safe in [false, true] {
        for silent in [false, true] {
            state.classroom_safe = safe;
            state.silent_startup_errors = silent;
            let mut received = None;
            state.sync_startup_policy(|classroom, enabled| received = Some((classroom, enabled)));
            assert_eq!(received, Some((safe, safe && silent)));
        }
    }
}

#[test]
fn startup_update_schedule_delays_once_and_throttles_manual_requests() {
    use std::time::{Duration, Instant};
    let now = Instant::now();
    let mut schedule = super::UpdateSchedule::default();
    assert_eq!(schedule.request_due(now, true, false, false), None);
    assert_eq!(
        schedule.request_due(now + Duration::from_secs(2), true, false, false),
        None
    );
    assert_eq!(
        schedule.request_due(now + Duration::from_secs(3), true, false, false),
        Some(true)
    );
    assert_eq!(
        schedule.request_due(now + Duration::from_secs(30), true, false, false),
        None
    );
    assert_eq!(
        schedule.request_due(now + Duration::from_secs(12), true, true, false),
        None
    );
    assert_eq!(
        schedule.request_due(now + Duration::from_secs(13), true, true, false),
        Some(false)
    );
    assert_eq!(
        schedule.request_due(now + Duration::from_secs(23), true, true, true),
        None
    );
    assert_eq!(
        schedule.request_due(now + Duration::from_secs(23), true, true, false),
        Some(false)
    );
}

#[test]
fn startup_update_disable_cancels_pending_automatic_check() {
    use std::time::{Duration, Instant};
    let now = Instant::now();
    for initially_enabled in [false, true] {
        let mut schedule = super::UpdateSchedule::default();
        assert_eq!(
            schedule.request_due(now, initially_enabled, false, false),
            None
        );
        assert_eq!(
            schedule.request_due(now + Duration::from_secs(1), false, false, false),
            None
        );
        assert_eq!(
            schedule.request_due(now + Duration::from_secs(30), true, false, false),
            None
        );
        assert_eq!(
            schedule.request_due(now + Duration::from_secs(31), false, true, true),
            None
        );
        assert!(schedule.last_request.is_none());
        assert_eq!(
            schedule.request_due(now + Duration::from_secs(31), false, true, false),
            Some(false)
        );
    }
}

#[test]
fn startup_update_poll_schedules_idle_repaint_and_manual_survives_disable() {
    use crate::updates::Status;
    use std::time::{Duration, Instant};
    let ctx = egui::Context::default();
    let mut app = NeoApp::install(&ctx);
    for _ in 0..3 {
        ctx.begin_pass(egui::RawInput::default());
        app.poll_updates(&ctx);
        let mut output = ctx.end_pass();
        output.textures_delta.clear();
        assert!(
            output.viewport_output[&egui::ViewportId::ROOT].repaint_delay <= Duration::from_secs(3)
        );
        assert_eq!(app.state.update_status, Status::Idle);
    }
    app.update_schedule.ready_at = Some(Instant::now() - Duration::from_secs(1));
    app.poll_updates(&ctx);
    assert_eq!(app.state.update_status, Status::Checking);
    assert!(app.update_schedule.auto_inflight);
    app.state.auto_check_updates = false;
    app.poll_updates(&ctx);
    assert_eq!(app.state.update_status, Status::Idle);
    app.update_schedule.last_request = Some(Instant::now() - Duration::from_secs(10));
    app.state.update_check_requested = true;
    app.poll_updates(&ctx);
    assert_eq!(app.state.update_status, Status::Checking);
    assert!(!app.update_schedule.auto_inflight);
    app.accept_update_status(Status::UpToDate);
    app.poll_updates(&ctx);
    assert_eq!(app.state.update_status, Status::UpToDate);
    assert!(app.pending_toasts.is_empty());
}

#[test]
fn startup_update_cancelled_worker_retains_one_manual_until_completion_and_throttle() {
    use crate::updates::Status;
    use std::time::{Duration, Instant};
    for completion_secs in [5, 15] {
        let ctx = egui::Context::default();
        let mut app = NeoApp::install(&ctx);
        let now = Instant::now();
        let stale = Status::Available {
            version: "9.0.0".into(),
            url: "https://github.com/ChidcGithub/Neo/releases/tag/v9.0.0".into(),
        };
        let old_worker = app.update_checker.request_controlled(&ctx, stale);
        app.state.update_status = Status::Checking;
        app.update_schedule.last_request = Some(now);
        app.update_schedule.auto_done = true;
        app.update_schedule.auto_inflight = true;
        app.state.auto_check_updates = false;
        for _ in 0..10 {
            app.state.update_check_requested = true;
            app.poll_updates_at(&ctx, now + Duration::from_secs(1));
            assert!(app.update_checker.is_running());
            assert!(app.update_schedule.manual_pending);
            assert_eq!(app.state.update_status, Status::Idle);
            assert_eq!(app.update_schedule.last_request, Some(now));
            assert_eq!(app.update_schedule.next_deadline(now, true), None);
        }
        for _ in 0..3 {
            ctx.begin_pass(egui::RawInput::default());
            let mut output = ctx.end_pass();
            output.textures_delta.clear();
        }
        let repaints = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let received = repaints.clone();
        ctx.set_request_repaint_callback(move |_| {
            received.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        });
        old_worker();
        assert!(repaints.load(std::sync::atomic::Ordering::Relaxed) > 0);
        assert!(!app.update_checker.is_running());
        let completed_at = now + Duration::from_secs(completion_secs);
        app.poll_updates_at(&ctx, completed_at);
        if completion_secs < 10 {
            let deadline = now + Duration::from_secs(10);
            assert!(app.update_schedule.manual_pending);
            assert_eq!(app.state.update_status, Status::Idle);
            assert_eq!(
                app.update_schedule.next_deadline(completed_at, false),
                Some(deadline)
            );
            // 没有任何新点击，空闲帧仍明确安排节流到期的重绘。
            for _ in 0..3 {
                ctx.begin_pass(egui::RawInput::default());
                app.poll_updates_at(&ctx, completed_at);
                let mut output = ctx.end_pass();
                output.textures_delta.clear();
                assert!(
                    output.viewport_output[&egui::ViewportId::ROOT].repaint_delay
                        <= Duration::from_secs(5)
                );
            }
            app.poll_updates_at(&ctx, deadline);
        }
        assert!(!app.update_schedule.manual_pending);
        assert!(!app.update_schedule.auto_inflight);
        assert_eq!(app.state.update_status, Status::Checking);
        assert_eq!(
            app.update_schedule.last_request,
            Some(now + Duration::from_secs(completion_secs.max(10)))
        );
        app.poll_updates_at(&ctx, now + Duration::from_secs(30));
        assert_eq!(app.state.update_status, Status::UpToDate);
        assert_eq!(app.update_schedule.next_deadline(now, false), None);
        assert!(
            app.pending_toasts.is_empty(),
            "discarded automatic result must not notify"
        );
    }
}

#[test]
fn startup_update_manual_throttle_retains_click_without_another_click() {
    use std::time::{Duration, Instant};
    let now = Instant::now();
    let mut schedule = super::UpdateSchedule::default();
    assert_eq!(schedule.request_due(now, false, true, false), Some(false));
    assert_eq!(
        schedule.request_due(now + Duration::from_secs(1), false, true, false),
        None
    );
    assert_eq!(
        schedule.next_deadline(now, false),
        Some(now + Duration::from_secs(10))
    );
    assert_eq!(
        schedule.request_due(now + Duration::from_secs(10), false, false, false),
        Some(false)
    );
    assert_eq!(
        schedule.request_due(now + Duration::from_secs(20), true, false, false),
        None
    );
    assert!(!schedule.manual_pending);
}

#[test]
fn startup_update_close_and_exit_cancel_pending_manual_even_if_save_blocks() {
    use crate::updates::Status;
    for close in [false, true] {
        let ctx = egui::Context::default();
        let mut app = NeoApp::install(&ctx);
        let worker = app
            .update_checker
            .request_controlled(&ctx, Status::UpToDate);
        app.state.update_status = Status::Checking;
        app.update_schedule.manual_pending = true;
        app.state.update_check_requested = true;
        app.store = None;
        if close {
            app.handle_close(&ctx);
        } else {
            app.request_exit(&ctx);
        }
        assert!(!app.update_schedule.manual_pending);
        assert!(!app.state.update_check_requested);
        assert!(app.update_schedule.auto_done);
        assert!(app.update_checker.is_running());
        worker();
        app.poll_updates(&ctx);
        assert_eq!(app.state.update_status, Status::Idle);
        assert!(app.pending_toasts.is_empty());
        app.exit_blocked = false;
        app.quitting = false;
        app.poll_updates(&ctx);
        assert_eq!(app.state.update_status, Status::Idle);
    }
}

#[test]
fn startup_update_results_only_notify_available_once_and_do_not_exit() {
    use crate::updates::Status;
    let ctx = egui::Context::default();
    let mut app = NeoApp::install(&ctx);
    app.state.silent_startup_errors = true;
    app.update_schedule.auto_inflight = true;
    app.accept_update_status(Status::Failed("不可落盘的原始错误".into()));
    assert!(app.pending_toasts.is_empty());
    assert!(!app.quitting && !app.exit_blocked);
    app.update_schedule.auto_inflight = true;
    app.accept_update_status(Status::UpToDate);
    assert!(app.pending_toasts.is_empty());
    let available = Status::Available {
        version: "9.0.0".into(),
        url: "https://github.com/ChidcGithub/Neo/releases/tag/v9.0.0".into(),
    };
    for _ in 0..2 {
        app.update_schedule.auto_inflight = true;
        app.accept_update_status(available.clone());
    }
    assert_eq!(app.pending_toasts.len(), 1);
    app.pending_toasts.clear();
    app.state.auto_check_updates = false;
    app.update_schedule.auto_inflight = true;
    app.state.update_status = Status::Checking;
    app.poll_updates(&ctx);
    assert_eq!(app.state.update_status, Status::Idle);
    assert!(!app.update_schedule.auto_inflight);
    assert!(app.pending_toasts.is_empty());
}

#[test]
fn startup_duplicate_queues_toast_even_when_main_window_hidden() {
    let ctx = egui::Context::default();
    let mut app = NeoApp::install(&ctx);
    app.hidden_to_tray = true;
    app.queue_duplicate(false);
    assert!(app.pending_toasts.is_empty());
    app.queue_duplicate(true);
    assert_eq!(app.pending_toasts.len(), 1);
    assert_eq!(app.pending_toasts[0].1, "Neo 已经在运行");
    assert!(app.hidden_to_tray);
    assert!(!app.quitting);
}

#[test]
fn dictation_input_conflicts_preserve_text_attachments_and_session() {
    use crate::state::{ChatMessage, Role};
    use std::time::{Duration, Instant};
    for last_done in [
        None,
        Some(Instant::now()),
        Some(Instant::now() - Duration::from_secs(301)),
    ] {
        for draft in ["", "  \n", "  尚未提交的文字  "] {
            for attachment in [false, true] {
                if draft.is_empty() && !attachment {
                    continue;
                }
                let mut state = AppState::default();
                state.active_session = Some(42);
                state.messages.push(ChatMessage::new(Role::User, "原会话"));
                state.draft = draft.into();
                if attachment {
                    state.add_attachment(sample_attachment(true)).unwrap();
                }
                let attachments = serde_json::to_string(&state.draft_attachments).unwrap();
                let epoch = state.session_epoch;
                assert!(!NeoApp::prepare_dictation_input(
                    &mut state,
                    None,
                    " 新语音 ",
                    last_done
                ));
                let separator = if draft.is_empty() { "" } else { "\n\n" };
                assert_eq!(
                    state.draft,
                    format!("{draft}{separator}【语音转写 · 待确认】\n新语音")
                );
                assert_eq!(
                    serde_json::to_string(&state.draft_attachments).unwrap(),
                    attachments
                );
                assert_eq!(state.active_session, Some(42));
                assert_eq!(state.session_epoch, epoch);
                assert_eq!(state.messages.len(), 1);
                assert_eq!(state.messages[0].content, "原会话");
                assert_eq!(state.pending_persist, 0);
                assert!(!state.generating && !state.wants_demo_reply && state.stream.is_none());
            }
        }
    }
}

#[test]
fn dictation_input_picker_in_flight_never_switches_or_auto_sends() {
    let mut state = AppState::default();
    state.attachment_picker_open = true;
    state.attachment_status = Some("正在选择附件…".into());
    state.active_session = Some(42);
    assert!(!NeoApp::prepare_dictation_input(
        &mut state,
        None,
        "讲解这道题",
        None
    ));
    assert!(state.attachment_picker_open);
    assert_eq!(state.attachment_status.as_deref(), Some("正在选择附件…"));
    assert_eq!(state.active_session, Some(42));
    // 模拟稍后导入完成：语音依然只是待确认草稿，不因附件到达而触发发送。
    state.attachment_picker_open = false;
    state.add_attachment(sample_attachment(false)).unwrap();
    assert_eq!(state.draft, "【语音转写 · 待确认】\n讲解这道题");
    assert_eq!(state.draft_attachments.len(), 1);
    assert!(state.messages.is_empty());
    assert!(!state.generating && !state.wants_demo_reply);
}

#[test]
fn dictation_input_empty_editor_resumes_only_within_five_minutes() {
    use crate::state::{ChatMessage, Role};
    use std::time::{Duration, Instant};
    for (last_done, resume) in [
        (None, false),
        (Some(Instant::now() - Duration::from_secs(299)), true),
        (Some(Instant::now() - Duration::from_secs(300)), false),
        (Some(Instant::now() - Duration::from_secs(301)), false),
    ] {
        let mut state = AppState::default();
        state.messages.push(ChatMessage::new(Role::User, "原会话"));
        state.active_session = Some(42);
        let epoch = state.session_epoch;
        assert!(NeoApp::prepare_dictation_input(
            &mut state,
            None,
            "  那第二问呢  ",
            last_done
        ));
        assert_eq!(state.active_session, resume.then_some(42));
        assert_eq!(state.session_epoch, epoch + u64::from(!resume));
        assert_eq!(state.messages.len(), usize::from(resume));
        assert_eq!(state.draft, "那第二问呢");
        // submit 仅落纯状态，不经过真实发送、演示生成或任何网络。
        assert!(state.submit());
        let message = state.messages.last().unwrap();
        assert_eq!(message.content, "那第二问呢");
        assert!(message.attachments.is_empty());
        assert!(!state.generating && !state.wants_demo_reply && state.stream.is_none());
    }
}

#[test]
fn dictation_input_recent_completion_without_messages_starts_new_session() {
    let mut state = AppState::default();
    state.active_session = Some(42);
    let epoch = state.session_epoch;
    assert!(NeoApp::prepare_dictation_input(
        &mut state,
        None,
        "新问题",
        Some(std::time::Instant::now())
    ));
    assert_eq!(state.active_session, None);
    assert_eq!(state.session_epoch, epoch + 1);
    assert_eq!(state.draft, "新问题");
    assert!(state.messages.is_empty());
}

#[test]
fn dictation_input_readonly_failure_keeps_both_sources_and_history() {
    use crate::state::{ChatMessage, Role};
    let store = temp_store("dictation-readonly");
    let id = store.create_session("原会话").unwrap();
    let path = std::env::temp_dir()
        .join(format!(
            "neo-restore-dictation-readonly-{}",
            std::process::id()
        ))
        .join("neo.db");
    drop(store);
    let store = readonly_store(&path);
    for conflict in [false, true] {
        let mut state = AppState::default();
        state.active_session = Some(id);
        state
            .messages
            .push(ChatMessage::new(Role::User, "尚未保存的消息"));
        if conflict {
            state.draft = "原草稿".into();
            state.add_attachment(sample_attachment(false)).unwrap();
        }
        assert!(!NeoApp::prepare_dictation_input(
            &mut state,
            Some(&store),
            "识别文本",
            None
        ));
        assert_eq!(state.active_session, Some(id));
        assert_eq!(state.messages[0].content, "尚未保存的消息");
        assert_eq!(state.pending_persist, 0);
        assert!(state.draft.contains("识别文本"));
        if conflict {
            assert!(state.draft.starts_with("原草稿\n\n"));
            assert_eq!(state.draft_attachments.len(), 1);
        } else {
            assert!(state.attachment_error.is_some());
        }
        assert!(store.messages(id).unwrap().is_empty());
        assert!(!state.generating && !state.wants_demo_reply);
    }
}

#[test]
fn dictation_input_send_rejections_keep_transcript_without_stream() {
    // 缺模型与超预算都在真实请求之前拒绝；不启动发送线程。
    for budget_failure in [false, true] {
        let mut state = AppState::default();
        state.context_tokens = 32 * 1024;
        state.api_key = "test-key".into();
        let text = if budget_failure {
            state.restore_models("deepseek-chat", Some("deepseek-chat"));
            "教学正文".repeat(20_000)
        } else {
            "识别文本".into()
        };
        assert!(NeoApp::prepare_dictation_input(
            &mut state, None, &text, None
        ));
        NeoApp::send_input(&mut state, None);
        assert_eq!(state.draft, text);
        assert!(state.attachment_error.is_some());
        assert!(state.messages.is_empty());
        assert!(!state.generating && !state.wants_demo_reply && state.stream.is_none());
    }
}

#[test]
fn dictation_input_append_save_failure_restores_voice_draft() {
    let store = temp_store("dictation-append");
    let path = std::env::temp_dir()
        .join(format!(
            "neo-restore-dictation-append-{}",
            std::process::id()
        ))
        .join("neo.db");
    drop(store);
    let store = readonly_store(&path);
    let mut state = AppState::default();
    // 空会话准备阶段无需写库，提交后的首次保存失败必须退回语音草稿。
    assert!(NeoApp::prepare_dictation_input(
        &mut state,
        Some(&store),
        "识别文本",
        None
    ));
    NeoApp::send_input(&mut state, Some(&store));
    assert_eq!(state.draft, "识别文本");
    assert!(state.messages.is_empty());
    assert!(state.attachment_error.is_some());
    assert!(!state.generating && !state.wants_demo_reply && state.stream.is_none());
}

#[test]
fn dictation_input_fake_channel_accepts_only_current_nonempty_transcripts() {
    let (tx, rx) = std::sync::mpsc::channel();
    tx.send(super::SttResult::Transcript(6, Ok("旧结果".into())))
        .unwrap();
    tx.send(super::SttResult::Transcript(7, Ok(" \n ".into())))
        .unwrap();
    tx.send(super::SttResult::Transcript(7, Ok("第一句".into())))
        .unwrap();
    tx.send(super::SttResult::Transcript(8, Ok("第二句".into())))
        .unwrap();
    drop(tx);
    let mut state = AppState::default();
    state.draft = "原草稿".into();
    for result in rx {
        if let super::SttResult::Transcript(epoch, Ok(text)) = result {
            if super::current_dictation(true, 7, epoch) {
                assert!(!NeoApp::prepare_dictation_input(
                    &mut state, None, &text, None
                ));
            }
        }
    }
    assert_eq!(state.draft, "原草稿\n\n【语音转写 · 待确认】\n第一句");
    assert!(!NeoApp::prepare_dictation_input(
        &mut state,
        None,
        "再次唤醒",
        None
    ));
    assert_eq!(
        state.draft,
        "原草稿\n\n【语音转写 · 待确认】\n第一句\n\n【语音转写 · 待确认】\n再次唤醒"
    );
    assert!(state.messages.is_empty());
    assert!(!state.generating && !state.wants_demo_reply);
}

#[test]
fn safety_sqlite_readonly_blocks_close_and_tray_then_retry_saves() {
    use crate::state::{ChatMessage, Role, StreamSource};
    for tray in [false, true] {
        let ctx = egui::Context::default();
        let mut app = NeoApp::install(&ctx);
        app.state.new_session();
        let path = super::test_db_path();
        let store = app.store.as_ref().unwrap();
        store.set_setting("classroom_safe", "0").unwrap();
        let id = store.create_session("original").unwrap();
        app.saved_classroom_safe = false;
        app.state.active_session = Some(id);
        app.state
            .messages
            .push(ChatMessage::new(Role::User, "keep"));
        app.state.start_generation(StreamSource::Demo {
            text: "partial".into(),
            cursor: 0,
        });
        app.state.pump();
        drop(app.store.take());
        app.store = Some(readonly_store(&path));
        assert!(app
            .store
            .as_ref()
            .unwrap()
            .set_setting("probe", "fail")
            .is_err());
        app.hidden_to_tray = tray;
        let mut input = egui::RawInput::default();
        if !tray {
            input
                .viewports
                .get_mut(&egui::ViewportId::ROOT)
                .unwrap()
                .events
                .push(egui::ViewportEvent::Close);
        }
        let mut output = ctx.run_ui(input, |_| {
            if tray {
                app.handle_tray_menu(&ctx, &app.tray_quit_id.clone());
            } else {
                app.tick(ctx.clone());
            }
        });
        output.textures_delta.clear();
        let commands = &output.viewport_output[&egui::ViewportId::ROOT].commands;
        assert!(commands
            .iter()
            .any(|command| matches!(command, egui::ViewportCommand::CancelClose)));
        assert!(!commands
            .iter()
            .any(|command| matches!(command, egui::ViewportCommand::Close)));
        assert!(!app.quitting);
        assert!(app.exit_blocked);
        assert!(!app.hidden_to_tray);
        assert!(!app.state.generating);
        assert!(app.state.messages.iter().all(|message| !message.streaming));
        assert_eq!(app.state.pending_persist, 0);
        assert!(app.state.preferences_unsaved);
        assert!(!app.saved_classroom_safe);
        assert_eq!(
            app.store
                .as_ref()
                .unwrap()
                .setting("classroom_safe")
                .unwrap()
                .as_deref(),
            Some("0")
        );
        assert!(app.store.as_ref().unwrap().messages(id).unwrap().is_empty());
        // 等待下一帧不会偷偷放行或恢复生成。
        app.tick(ctx.clone());
        assert!(app.exit_blocked && !app.quitting && !app.state.generating);
        NeoApp::rename_session(&mut app.state, app.store.as_ref(), id, "retry title");
        assert_eq!(app.state.renaming, Some(id));
        assert_eq!(app.state.rename_draft, "retry title");
        assert!(app
            .state
            .attachment_error
            .as_deref()
            .unwrap()
            .contains("重命名未保存"));
        assert_eq!(
            app.store
                .as_ref()
                .unwrap()
                .sessions()
                .unwrap()
                .into_iter()
                .find(|row| row.id == id)
                .unwrap()
                .title,
            "original"
        );
        drop(app.store.take());
        app.store = Some(Store::open(&path).unwrap());
        app.request_exit(&ctx);
        assert!(app.quitting && !app.exit_blocked);
        assert!(!app.state.preferences_unsaved);
        assert!(app.saved_classroom_safe);
        assert_eq!(
            app.store.as_ref().unwrap().messages(id).unwrap().len(),
            app.state.messages.len()
        );
        assert_eq!(
            app.store
                .as_ref()
                .unwrap()
                .setting("classroom_safe")
                .unwrap()
                .as_deref(),
            Some("1")
        );
    }
}

#[test]
fn safety_class_exit_retries_all_four_cards_and_index_failures() {
    struct HomeGuard(Option<std::ffi::OsString>, std::path::PathBuf);
    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match &self.0 {
                Some(old) => std::env::set_var("NEO_HOME", old),
                None => std::env::remove_var("NEO_HOME"),
            }
            let _ = std::fs::remove_dir_all(&self.1);
        }
    }
    let path = std::env::temp_dir().join(format!("neo-exit-class-{}", std::process::id()));
    std::fs::create_dir_all(&path).unwrap();
    let _home = HomeGuard(std::env::var_os("NEO_HOME"), path.clone());
    std::env::set_var("NEO_HOME", &path);
    std::fs::write(path.join("class"), b"blocked").unwrap();
    let ctx = egui::Context::default();
    let mut app = NeoApp::install(&ctx);
    app.class.seed_pending_summaries();
    app.request_exit(&ctx);
    assert!(app.exit_blocked && !app.quitting);
    assert_eq!(app.class.pending_saves(), 4);
    // 安全模式停用/重复退出也不能覆盖或丢掉队列。
    app.state.classroom_safe = true;
    app.class.tick(&ctx, &app.state, false);
    assert_eq!(app.class.pending_saves(), 4);
    std::fs::remove_file(path.join("class")).unwrap();
    std::fs::create_dir(path.join("memories.json")).unwrap();
    app.request_exit(&ctx);
    assert!(app.exit_blocked && !app.quitting);
    assert_eq!(app.class.pending_saves(), 4);
    assert_eq!(
        app.class.presenting().unwrap().save_state,
        crate::class::SaveState::IndexFailed
    );
    let date = app.class.presenting().unwrap().date.clone();
    assert_eq!(neo_tools::classlog::load_day(&date).len(), 4);
    app.request_exit(&ctx);
    assert_eq!(neo_tools::classlog::load_day(&date).len(), 4);
    std::fs::remove_dir(path.join("memories.json")).unwrap();
    app.request_exit(&ctx);
    assert!(app.quitting && !app.exit_blocked);
    assert_eq!(app.class.pending_saves(), 0);
    assert_eq!(neo_tools::classlog::load_day(&date).len(), 4);
    let memories: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path.join("memories.json")).unwrap()).unwrap();
    assert_eq!(memories.as_array().unwrap().len(), 4);
}

#[test]
fn safety_budget_rejection_keeps_draft_attachments_and_does_not_start_stream() {
    let mut state = AppState::default();
    state.context_tokens = 32 * 1024;
    state.api_key = "test-key".into();
    state.restore_models("deepseek-chat", Some("deepseek-chat"));
    state.draft = format!("  {}  ", "教学正文".repeat(20_000));
    state.add_attachment(sample_attachment(false)).unwrap();
    let draft = state.draft.clone();
    assert!(state.can_call_real());
    NeoApp::send_input(&mut state, None);
    assert_eq!(state.draft, draft);
    assert_eq!(state.draft_attachments.len(), 1);
    assert!(state.messages.is_empty());
    assert!(!state.generating && state.stream.is_none());
    assert!(state.attachment_error.as_deref().unwrap().contains("预算"));
}

#[test]
fn safety_memory_only_exit_requires_explicit_discard() {
    let ctx = egui::Context::default();
    let mut app = NeoApp::install(&ctx);
    app.store = None;
    app.request_exit(&ctx);
    assert!(app.exit_blocked && !app.quitting && !app.confirm_discard);
    app.confirm_discard_exit(&ctx);
    assert!(app.exit_blocked && !app.quitting);
    app.confirm_discard = true;
    app.tick(ctx.clone());
    assert!(!app.quitting);
    app.confirm_discard_exit(&ctx);
    assert!(app.quitting && !app.exit_blocked);
}

#[test]
fn ui_safety_exit_discard_is_two_step_and_error_is_visible() {
    let pointer = |pos, pressed| {
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    };
    let ctx = egui::Context::default();
    let mut app = NeoApp::install(&ctx);
    app.store = None;
    app.state.classroom_safe = !app.saved_classroom_safe;
    app.request_exit(&ctx);
    let size = egui::vec2(900.0, 600.0);
    let draw = |app: &mut NeoApp, events| {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                events,
                ..Default::default()
            },
            |_| app.exit_dialog(&ctx),
        );
        output.textures_delta.clear();
        output
    };
    draw(&mut app, vec![]);
    let output = draw(&mut app, vec![]);
    let text_rect = |output: &egui::FullOutput, needle: &str| {
        output
            .shapes
            .iter()
            .find_map(|shape| {
                if let egui::Shape::Text(text) = &shape.shape {
                    if text.galley.job.text.contains(needle) {
                        return Some(egui::Rect::from_min_size(text.pos, text.galley.size()));
                    }
                }
                None
            })
            .unwrap_or_else(|| panic!("缺少可见提示：{needle}"))
    };
    text_rect(&output, "安全偏好尚未保存");
    text_rect(&output, "纯内存模式");
    text_rect(&output, "重试保存并退出");
    let discard = text_rect(&output, "退出不保存…");
    draw(&mut app, pointer(discard.center(), true));
    draw(&mut app, pointer(discard.center(), false));
    assert!(app.confirm_discard && !app.quitting);
    draw(&mut app, vec![]);
    let output = draw(&mut app, vec![]);
    let confirm = text_rect(&output, "确认丢弃并退出");
    draw(&mut app, pointer(confirm.center(), true));
    let output = draw(&mut app, pointer(confirm.center(), false));
    assert!(app.quitting);
    assert!(output.viewport_output[&egui::ViewportId::ROOT]
        .commands
        .iter()
        .any(|command| matches!(command, egui::ViewportCommand::Close)));
}

fn wake_test_app(ctx: &egui::Context) -> NeoApp {
    let mut app = NeoApp::install(ctx);
    app.state.classroom_safe = false;
    app.applied_classroom_safe = false;
    app.state.show_settings = true;
    app.state.settings_tab = crate::state::SettingsTab::WakeTest;
    app.state.wake_enabled = true;
    app.state.wake_test.requested = true;
    app
}

#[test]
fn wake_test_admission_blocks_safety_disabled_tasks_and_dictation() {
    for case in 0..9 {
        let ctx = egui::Context::default();
        let mut app = wake_test_app(&ctx);
        match case {
            0 => app.state.classroom_safe = true,
            1 => app.state.wake_enabled = false,
            2 => app.state.generating = true,
            3 => app.state.tool_open = true,
            4 => app.state.tool_round = true,
            5 => app.dictating = true,
            6 => app.wake_broken = true,
            7 => app.state.compaction_resume = true,
            _ => app.exit_blocked = true,
        }
        app.sync_wake_test(true); // 只注入引擎可用性，不创建硬件引擎。
        assert!(!app.state.wake_test.running && !app.state.wake_test.requested);
        assert!(app.wake.is_none());
    }
}

#[test]
fn wake_test_leaving_stops_and_keeps_errors_without_resuming() {
    for case in 0..5 {
        let ctx = egui::Context::default();
        let mut app = wake_test_app(&ctx);
        assert!(app.sync_wake_test(true));
        assert!(app.state.wake_test.running);
        app.state.wake_test.error = Some("resource unavailable".into());
        match case {
            0 => app.state.settings_tab = crate::state::SettingsTab::General,
            1 => app.state.show_settings = false,
            2 => app.state.classroom_safe = true,
            3 => app.hidden_to_tray = true,
            _ => app.state.wake_test.requested = false,
        }
        assert!(app.sync_wake_test(true));
        assert!(!app.state.wake_test.running && !app.state.wake_test.requested);
        assert_eq!(
            app.state.wake_test.error.as_deref(),
            Some("resource unavailable")
        );
        app.state.show_settings = true;
        app.state.settings_tab = crate::state::SettingsTab::WakeTest;
        app.state.classroom_safe = false;
        app.hidden_to_tray = false;
        assert!(!app.sync_wake_test(true));
    }
}

#[test]
fn wake_test_queued_events_never_start_dictation_or_tasks_and_errors_latch() {
    let ctx = egui::Context::default();
    let mut app = wake_test_app(&ctx);
    let (tx, rx) = std::sync::mpsc::channel();
    let (stt_tx, stt_rx) = std::sync::mpsc::channel();
    app.stt_tx = Some(stt_tx);
    app.wake_rx = Some(rx);
    app.state.draft = "untouched".into();
    for stopping in [false, true] {
        tx.send(neo_wake::WakeEvent::Detected {
            epoch: app.wake_epoch,
            score: 0.99,
        })
        .unwrap();
        tx.send(neo_wake::WakeEvent::Audio {
            epoch: app.wake_epoch,
            frame: vec![0.1; 16],
        })
        .unwrap();
        app.state.wake_test.requested = !stopping;
        let suppress = app.sync_wake_test(true);
        assert!(suppress);
        app.poll_wake_events(&ctx, suppress);
        assert!(!app.dictating && !app.state.generating && app.state.messages.is_empty());
        assert_eq!(app.state.draft, "untouched");
        assert!(stt_rx.try_recv().is_err());
        assert!(app.pending_toasts.is_empty());
    }
    tx.send(neo_wake::WakeEvent::Error("school resource missing".into()))
        .unwrap();
    app.poll_wake_events(&ctx, true);
    assert!(app.wake_broken && app.state.wake_test.broken);
    assert_eq!(
        app.state.wake_test.error.as_deref(),
        Some("school resource missing")
    );
    app.state.wake_enabled = false;
    ctx.set_embed_viewports(false);
    ctx.begin_pass(egui::RawInput::default());
    app.tick(ctx.clone()); // 复用开关的故障解锁，测试构建不会开麦克风。
    ctx.end_pass().textures_delta.clear();
    assert!(!app.wake_broken);
    app.state.wake_enabled = true;
    ctx.begin_pass(egui::RawInput::default());
    app.tick(ctx.clone());
    ctx.end_pass().textures_delta.clear();
    assert!(app.wake.is_none() && !app.state.wake_test.requested);
    assert_eq!(
        app.state.wake_test.error.as_deref(),
        Some("school resource missing")
    );
}

#[test]
fn wake_test_delayed_forwarder_rejects_old_detection_and_audio_after_exit() {
    use neo_wake::WakeEvent;
    use std::sync::mpsc;
    use std::time::Duration;

    let ctx = egui::Context::default();
    let mut app = wake_test_app(&ctx);
    let (engine_tx, engine_rx) = mpsc::channel();
    let (ui_tx, ui_rx) = mpsc::channel();
    let (held_tx, held_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (sent_tx, sent_rx) = mpsc::channel();
    let forward_tx = ui_tx.clone();
    let forwarder = std::thread::spawn(move || {
        for event in engine_rx {
            held_tx.send(()).unwrap();
            if release_rx.recv().is_err() {
                break;
            }
            forward_tx.send(event).unwrap();
            sent_tx.send(()).unwrap();
        }
    });
    app.wake_rx = Some(ui_rx);
    let (stt_tx, stt_rx) = mpsc::channel();
    app.stt_tx = Some(stt_tx);
    app.state.draft = "untouched".into();
    engine_tx
        .send(WakeEvent::Detected {
            epoch: app.wake_epoch,
            score: 0.99,
        })
        .unwrap();
    engine_tx
        .send(WakeEvent::Audio {
            epoch: app.wake_epoch,
            frame: vec![0.1; 16],
        })
        .unwrap();
    held_rx.recv_timeout(Duration::from_secs(2)).unwrap();

    // 转发线程已收到旧命中，但尚未写入 UI 队列；完整进出测试并排空两次。
    let suppress = app.sync_wake_test(true);
    assert!(suppress);
    app.poll_wake_events(&ctx, suppress);
    app.state.wake_test.requested = false;
    let suppress = app.sync_wake_test(true);
    assert!(suppress);
    app.poll_wake_events(&ctx, suppress);
    assert_eq!(app.wake_epoch, 2);
    assert!(!app.sync_wake_test(true));
    app.hidden_to_tray = true;
    release_tx.send(()).unwrap();
    sent_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    app.poll_wake_events(&ctx, false);
    assert!(app.hidden_to_tray);
    assert!(!app.dictating && app.pending_toasts.is_empty());

    // 独立验证 Audio 没有进入处理器：无覆盖层时，误消费会取消此哨兵听写。
    held_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    app.dictating = true;
    app.dictation_epoch = 7;
    release_tx.send(()).unwrap();
    sent_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    app.poll_wake_events(&ctx, false);
    assert!(app.dictating && app.hidden_to_tray);
    assert_eq!(app.dictation_epoch, 7);
    assert!(stt_rx.try_recv().is_err());
    assert!(!app.state.generating && app.state.messages.is_empty());
    assert_eq!(app.state.draft, "untouched");
    drop(engine_tx);
    forwarder.join().unwrap();

    // 同一接收端仍接受新代事件，不是永久封禁或额外排空队列。
    ui_tx
        .send(WakeEvent::Audio {
            epoch: app.wake_epoch,
            frame: vec![0.2; 16],
        })
        .unwrap();
    app.poll_wake_events(&ctx, false);
    assert!(!app.dictating);
    assert!(matches!(stt_rx.try_recv(), Ok(super::SttCmd::Reset(8))));
    app.hidden_to_tray = true;
    ui_tx
        .send(WakeEvent::Detected {
            epoch: app.wake_epoch,
            score: 0.8,
        })
        .unwrap();
    app.poll_wake_events(&ctx, false);
    assert!(!app.hidden_to_tray);
    assert!(!app.pending_toasts.is_empty());
    ui_tx
        .send(WakeEvent::Error("delayed engine failure".into()))
        .unwrap();
    app.poll_wake_events(&ctx, false);
    assert!(app.wake_broken);
    assert_eq!(
        app.state.wake_test.error.as_deref(),
        Some("delayed engine failure")
    );
}

#[test]
fn toast_app_hidden_and_exit_blocked_still_deliver() {
    for blocked in [false, true] {
        let ctx = egui::Context::default();
        ctx.set_embed_viewports(false);
        let mut app = NeoApp::install(&ctx);
        app.hidden_to_tray = !blocked;
        app.exit_blocked = blocked;
        app.pending_toasts.push((
            neo_ui::ToastKind::Warning,
            "notice".into(),
            std::time::Instant::now() + std::time::Duration::from_secs(4),
        ));
        ctx.begin_pass(egui::RawInput::default());
        app.tick(ctx.clone());
        let mut output = ctx.end_pass();
        output.textures_delta.clear();
        assert!(app.pending_toasts.is_empty());
        let toast = &output.viewport_output[&egui::ViewportId::from_hash_of("neo-toastwin")];
        assert!(toast.viewport_ui_cb.is_some());
        assert!(toast.builder.inner_size.unwrap().x > 1.0);
    }
}

#[test]
fn safety_send_without_models_only_opens_settings() {
    let mut state = AppState::default();
    state.api_base = "http://127.0.0.1:9".into();
    state.api_key = "test-key".into();
    state.draft = "keep draft".into();
    NeoApp::send_input(&mut state, None);
    assert!(state.model_fetch.is_none());
    assert!(state.show_settings);
    assert_eq!(state.draft, "keep draft");
    assert!(state.messages.is_empty());
}

#[test]
fn safety_settings_missing_corrupt_and_failed_save() {
    let ctx = egui::Context::default();
    let mut app = NeoApp::install(&ctx);
    assert!(app.state.classroom_safe);
    for value in ["broken", "", "false", "1", "0"] {
        app.store
            .as_ref()
            .unwrap()
            .set_setting("classroom_safe", value)
            .unwrap();
        app.load_settings();
        assert_eq!(app.state.classroom_safe, value != "0");
    }
    app.state.wake_enabled = false;
    app.state.class_enabled = false;
    let store = app.store.take();
    app.tick(ctx.clone());
    assert!(app.saved_classroom_safe);
    app.store = store;
    app.tick(ctx.clone());
    assert!(!app.saved_classroom_safe);
    assert_eq!(
        app.store
            .as_ref()
            .unwrap()
            .setting("classroom_safe")
            .unwrap()
            .as_deref(),
        Some("0")
    );
    app.state.set_classroom_safe(true);
    app.tick(ctx);
    assert!(app.saved_classroom_safe);
}

#[test]
fn context_settings_persist_restore_and_invalid_fallback() {
    let ctx = egui::Context::default();
    let mut app = NeoApp::install(&ctx);
    app.store = Some(temp_store("context-config"));
    app.load_settings();
    assert_eq!(app.state.context_tokens, 1_000_000);
    app.state.context_tokens = 65_536;
    assert!(app.persist_preferences());
    app.state.context_tokens = 8192;
    app.load_settings();
    assert_eq!(app.state.context_tokens, 65_536);
    assert_eq!(app.saved_context_tokens, 65_536);
    app.store
        .as_ref()
        .unwrap()
        .set_setting("context_tokens", "corrupt")
        .unwrap();
    app.load_settings();
    assert_eq!(app.state.context_tokens, 1_000_000);
    app.state.context_tokens = 32_768;
    app.store = None;
    assert!(!app.persist_preferences());
    assert_eq!(app.saved_context_tokens, 1_000_000);
    assert!(app.state.preferences_unsaved);
}

#[test]
fn safety_hot_switch_invalidates_dictation_without_render() {
    let ctx = egui::Context::default();
    let mut app = NeoApp::install(&ctx);
    app.applied_classroom_safe = false;
    app.dictating = true;
    app.dictation_epoch = 5;
    let (tx, rx) = std::sync::mpsc::channel();
    app.stt_tx = Some(tx);
    let (wake_tx, wake_rx) = std::sync::mpsc::channel();
    app.wake_rx = Some(wake_rx);
    app.tick(ctx);
    assert!(!app.dictating);
    assert_eq!(app.dictation_epoch, 6);
    assert!(matches!(rx.try_recv(), Ok(super::SttCmd::Reset(6))));
    assert!(wake_tx
        .send(neo_wake::WakeEvent::Audio {
            epoch: 0,
            frame: vec![]
        })
        .is_err());
    assert!(!super::current_dictation(
        app.dictating,
        app.dictation_epoch,
        5
    ));
}

#[test]
fn safety_overlay_failure_summary_never_exposes_system_details() {
    for (reason, category) in [
        ("CreateWindowExW 失败 secret-path", "原生窗口"),
        ("创建 neo-overlay 线程失败: secret-path", "线程启动或等待"),
        ("GPU adapter secret-path", "渲染设备或其他初始化阶段"),
    ] {
        let summary = super::overlay_failure_summary(reason);
        assert!(summary.contains(category));
        assert!(summary.contains("独立窗口"));
        assert!(!summary.contains("secret-path"));
    }
}

#[test]
fn safety_overlay_unavailable_stops_audio_and_wake_uses_visible_input() {
    let ctx = egui::Context::default();
    let mut app = NeoApp::install(&ctx);
    let (tx, rx) = std::sync::mpsc::channel();
    app.stt_tx = Some(tx);
    app.dictating = true;
    app.dictation_epoch = 7;
    app.hidden_to_tray = true;
    app.state.draft = "keep draft".into();
    app.check_overlay_health(&ctx);
    assert!(!app.dictating && !app.hidden_to_tray);
    assert!(matches!(rx.try_recv(), Ok(super::SttCmd::Reset(8))));
    assert_eq!(app.state.draft, "keep draft");
    assert!(!super::current_dictation(
        app.dictating,
        app.dictation_epoch,
        7
    ));
    app.hidden_to_tray = true;
    app.on_wake_detected(&ctx, 0.9);
    assert!(!app.dictating && !app.hidden_to_tray);
    assert!(rx.try_recv().is_err());
    app.dictating = true;
    app.on_dictation_audio(&ctx, vec![0.5; 16]);
    assert!(!app.dictating);
    assert!(matches!(rx.try_recv(), Ok(super::SttCmd::Reset(9))));
    assert!(rx.try_recv().is_err());
    app.overlay_attempted = true;
    for _ in 0..10 {
        app.start_overlay();
    }
    assert!(app.overlay.is_none());
}

fn window_request_input(minimized: bool, occluded: bool) -> egui::RawInput {
    let mut input = egui::RawInput {
        focused: false,
        ..Default::default()
    };
    let root = input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap();
    root.minimized = Some(minimized);
    root.occluded = Some(occluded);
    root.focused = Some(false);
    input
}

fn assert_window_shown_once(commands: &[egui::ViewportCommand]) {
    use egui::ViewportCommand as Cmd;
    assert_eq!(
        commands
            .iter()
            .filter(|c| matches!(c, Cmd::Visible(true)))
            .count(),
        1
    );
    assert_eq!(
        commands
            .iter()
            .filter(|c| matches!(c, Cmd::Minimized(false)))
            .count(),
        1
    );
    assert_eq!(
        commands.iter().filter(|c| matches!(c, Cmd::Focus)).count(),
        1
    );
}

fn assert_no_window_show(commands: &[egui::ViewportCommand]) {
    assert!(commands.iter().all(|c| !matches!(
        c,
        egui::ViewportCommand::Visible(true)
            | egui::ViewportCommand::Minimized(false)
            | egui::ViewportCommand::Focus
    )));
}

#[test]
fn explicit_show_requests_restore_minimized_occluded_and_visible_unfocused_root() {
    for (minimized, occluded) in [(true, false), (false, true), (false, false)] {
        for wake in [false, true] {
            let ctx = egui::Context::default();
            let mut app = NeoApp::install(&ctx);
            app.state.generating = true;
            assert!(!app.hidden_to_tray && !app.desktop_windows.main_avoided);
            ctx.begin_pass(window_request_input(minimized, occluded));
            if wake {
                app.on_wake_detected(&ctx, 0.9);
                assert!(ctx.memory(|m| m.has_focus(egui::Id::new(crate::ui::COMPOSER_ID))));
            } else {
                app.handle_show_signal(&ctx, 0);
                app.handle_show_signal(&ctx, 42);
                app.handle_show_signal(&ctx, 42);
            }
            let mut output = ctx.end_pass();
            output.textures_delta.clear();
            assert_window_shown_once(&output.viewport_output[&egui::ViewportId::ROOT].commands);
            assert!(
                app.state.generating && !app.state.round_cancelled,
                "唤回普通后台窗口不能取消非桌面任务"
            );
            assert!(!app.dictating);
            if !wake {
                // 后续轮询（包括零值）不能重新消费已处理的请求。
                ctx.begin_pass(window_request_input(minimized, occluded));
                app.handle_show_signal(&ctx, 0);
                app.handle_show_signal(&ctx, 42);
                let mut output = ctx.end_pass();
                output.textures_delta.clear();
                assert_no_window_show(&output.viewport_output[&egui::ViewportId::ROOT].commands);
                ctx.begin_pass(window_request_input(minimized, occluded));
                app.handle_show_signal(&ctx, 43);
                let mut output = ctx.end_pass();
                output.textures_delta.clear();
                assert_window_shown_once(&output.viewport_output[&egui::ViewportId::ROOT].commands);
            }
        }
    }
}

#[test]
fn dictation_wake_never_shows_root_or_cancels_desktop_task() {
    for (minimized, occluded, tray, avoided) in [
        (true, false, false, false),
        (false, true, false, false),
        (false, false, false, false),
        (true, false, true, false),
        (true, false, false, true),
    ] {
        let ctx = egui::Context::default();
        let mut app = NeoApp::install(&ctx);
        app.hidden_to_tray = tray;
        app.desktop_windows.main_avoided = avoided;
        app.state.generating = true;
        let (tx, rx) = std::sync::mpsc::channel();
        app.stt_tx = Some(tx);
        ctx.begin_pass(window_request_input(minimized, occluded));
        app.handle_wake_detected(&ctx, 0.9, true);
        let mut output = ctx.end_pass();
        output.textures_delta.clear();
        assert_no_window_show(&output.viewport_output[&egui::ViewportId::ROOT].commands);
        assert!(app.dictating && app.dictation_since.is_some());
        assert!(matches!(rx.try_recv(), Ok(super::SttCmd::Reset(1))));
        assert!(app.state.generating && !app.state.round_cancelled);
        assert_eq!(app.hidden_to_tray, tray);
        assert_eq!(app.desktop_windows.main_avoided, avoided);
        assert!(!app.desktop_show_pending);
    }
}

#[test]
fn fallback_wake_during_dictation_shows_root_only_once() {
    let ctx = egui::Context::default();
    let mut app = NeoApp::install(&ctx);
    let (tx, rx) = std::sync::mpsc::channel();
    app.stt_tx = Some(tx);
    app.dictating = true;
    app.hidden_to_tray = true;
    ctx.begin_pass(window_request_input(true, false));
    app.on_wake_detected(&ctx, 0.9);
    let mut output = ctx.end_pass();
    output.textures_delta.clear();
    assert_window_shown_once(&output.viewport_output[&egui::ViewportId::ROOT].commands);
    assert!(!app.dictating && !app.hidden_to_tray);
    assert!(matches!(rx.try_recv(), Ok(super::SttCmd::Reset(1))));
    assert!(rx.try_recv().is_err());
}

#[test]
fn desktop_barrier_hide_revokes_pending_show_without_replaying_signal() {
    use std::sync::atomic::Ordering;
    let ctx = egui::Context::default();
    let mut app = NeoApp::install(&ctx);
    let (cancel, rx) = desktop_request(
        &app,
        std::time::Instant::now() + std::time::Duration::from_secs(10),
    );
    ctx.begin_pass(window_request_input(false, false));
    app.tick_desktop_barrier(&ctx);
    app.handle_show_signal(&ctx, 42);
    assert!(cancel.load(Ordering::Acquire));
    assert!(app.desktop_show_pending && app.state.round_cancelled);
    // 已处理信号不能再次取消；直接重复显示也合并到同一个待显示请求。
    app.state.round_cancelled = false;
    app.handle_show_signal(&ctx, 42);
    app.show_window(&ctx);
    assert!(!app.state.round_cancelled);
    app.hide_to_tray(&ctx);
    assert!(app.hidden_to_tray && !app.desktop_show_pending);
    let mut output = ctx.end_pass();
    output.textures_delta.clear();
    assert_no_window_show(&output.viewport_output[&egui::ViewportId::ROOT].commands);
    app.state
        .desktop_execution
        .as_ref()
        .unwrap()
        .active
        .store(0, Ordering::Release);
    for _ in 0..2 {
        ctx.begin_pass(window_request_input(true, false));
        app.tick_desktop_barrier(&ctx);
        app.handle_show_signal(&ctx, 42);
        let mut output = ctx.end_pass();
        output.textures_delta.clear();
        assert_no_window_show(&output.viewport_output[&egui::ViewportId::ROOT].commands);
        assert!(app.hidden_to_tray && !app.desktop_show_pending);
        assert!(!app.desktop_windows.main_avoided);
    }
    assert!(rx.try_recv().unwrap().is_err());
    // 真正的新请求仍能再次打开，不能永久锁住显示。
    ctx.begin_pass(window_request_input(true, false));
    app.handle_show_signal(&ctx, 43);
    let mut output = ctx.end_pass();
    output.textures_delta.clear();
    assert_window_shown_once(&output.viewport_output[&egui::ViewportId::ROOT].commands);
    assert!(!app.hidden_to_tray);
}

fn desktop_request(
    app: &NeoApp,
    deadline: std::time::Instant,
) -> (
    std::sync::Arc<std::sync::atomic::AtomicBool>,
    std::sync::mpsc::Receiver<Result<(), String>>,
) {
    let gate = app.state.desktop_execution.as_ref().unwrap();
    gate.active.store(1, std::sync::atomic::Ordering::Release);
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (ack, rx) = std::sync::mpsc::channel();
    gate.requests
        .send(crate::state::DesktopRequest {
            cancel: cancel.clone(),
            deadline,
            ack,
        })
        .unwrap();
    (cancel, rx)
}

#[test]
fn desktop_barrier_visible_main_auto_avoids_without_tray_and_holds_round() {
    let ctx = egui::Context::default();
    let mut app = NeoApp::install(&ctx);
    app.state.tool_open = true;
    let (_, rx) = desktop_request(
        &app,
        std::time::Instant::now() + std::time::Duration::from_secs(10),
    );
    app.tick_desktop_barrier(&ctx);
    assert!(rx.try_recv().is_err(), "必须等下一帧验证");
    assert!(app.desktop_windows.main_avoided);
    assert!(!app.hidden_to_tray);
    assert!(app.tray.is_none());
    app.tick_desktop_barrier(&ctx);
    assert!(rx.try_recv().unwrap().is_ok());
    app.state
        .desktop_execution
        .as_ref()
        .unwrap()
        .active
        .store(0, std::sync::atomic::Ordering::Release);
    app.tick_desktop_barrier(&ctx);
    assert!(
        app.desktop_windows.main_avoided,
        "同轮观察与点击之间不恢复主窗"
    );
    assert!(
        !super::desktop_suspended(&ctx),
        "主窗暂避不能让打断确认整轮隐形"
    );
    app.state.tool_open = false;
    ctx.begin_pass(egui::RawInput::default());
    app.tick_desktop_barrier(&ctx);
    assert!(!app.desktop_windows.main_avoided);
    assert!(!super::desktop_suspended(&ctx));
    let mut output = ctx.end_pass();
    output.textures_delta.clear();
    assert!(output
        .viewport_output
        .values()
        .flat_map(|v| &v.commands)
        .all(|c| !matches!(c, egui::ViewportCommand::Focus)));
}

#[test]
fn desktop_barrier_auxiliary_ui_resumes_between_leases_without_cancel_or_main_restore() {
    use std::sync::atomic::Ordering;
    let ctx = egui::Context::default();
    let mut app = NeoApp::install(&ctx);
    app.state.generating = true;
    for _ in 0..2 {
        let (cancel, rx) = desktop_request(
            &app,
            std::time::Instant::now() + std::time::Duration::from_secs(10),
        );
        app.tick_desktop_barrier(&ctx);
        assert!(super::desktop_suspended(&ctx));
        app.tick_desktop_barrier(&ctx);
        assert!(rx.try_recv().unwrap().is_ok());
        assert!(super::desktop_suspended(&ctx));

        app.state
            .desktop_execution
            .as_ref()
            .unwrap()
            .active
            .store(0, Ordering::Release);
        app.tick_desktop_barrier(&ctx);
        assert!(!super::desktop_suspended(&ctx));

        assert!(app.desktop_windows.main_avoided);
        assert!(!cancel.load(Ordering::Acquire));
        assert!(!app.state.round_cancelled);
        assert!(app.state.generating);
    }
    app.state.generating = false;
    app.tick_desktop_barrier(&ctx);
    assert!(!app.desktop_windows.main_avoided);
}

#[test]
fn desktop_barrier_auto_avoided_main_drives_miniwin_without_tray() {
    let ctx = egui::Context::default();
    ctx.set_embed_viewports(false);
    let mut app = NeoApp::install(&ctx);
    app.state.tool_open = true;
    let (_, rx) = desktop_request(
        &app,
        std::time::Instant::now() + std::time::Duration::from_secs(10),
    );
    app.tick_desktop_barrier(&ctx);
    app.tick_desktop_barrier(&ctx);
    assert!(rx.try_recv().unwrap().is_ok());
    app.state
        .desktop_execution
        .as_ref()
        .unwrap()
        .active
        .store(0, std::sync::atomic::Ordering::Release);
    ctx.begin_pass(egui::RawInput::default());
    app.tick(ctx.clone());
    let mut output = ctx.end_pass();
    output.textures_delta.clear();
    assert!(!app.hidden_to_tray);
    assert!(app.desktop_windows.main_avoided);
    assert!(!super::desktop_suspended(&ctx));
    let mini = &output.viewport_output[&egui::ViewportId::from_hash_of("neo-miniwin")];
    assert!(
        mini.builder
            .inner_size
            .is_some_and(|size| size.x > 1.0 && size.y > 1.0),
        "自动暂避与托盘隐藏都要开启迷你窗"
    );
    assert!(!app.state.round_cancelled);
}

#[test]
fn desktop_barrier_manual_show_cancels_and_waits_for_uia() {
    let ctx = egui::Context::default();
    let mut app = NeoApp::install(&ctx);
    let (cancel, rx) = desktop_request(
        &app,
        std::time::Instant::now() + std::time::Duration::from_secs(10),
    );
    app.tick_desktop_barrier(&ctx);
    app.show_window(&ctx);
    assert!(cancel.load(std::sync::atomic::Ordering::Acquire));
    app.tick_desktop_barrier(&ctx);
    assert!(rx.try_recv().unwrap().is_err());
    assert!(app.desktop_windows.main_avoided);
    assert!(app.desktop_show_pending);
    app.state
        .desktop_execution
        .as_ref()
        .unwrap()
        .active
        .store(0, std::sync::atomic::Ordering::Release);
    app.tick_desktop_barrier(&ctx);
    assert!(!app.desktop_windows.main_avoided);
    assert!(!app.desktop_show_pending);
}

#[test]
fn desktop_barrier_verify_failure_restores_even_during_round() {
    let ctx = egui::Context::default();
    let mut app = NeoApp::install(&ctx);
    app.state.tool_open = true;
    let (_, rx) = desktop_request(
        &app,
        std::time::Instant::now() + std::time::Duration::from_secs(10),
    );
    app.tick_desktop_barrier(&ctx);
    app.desktop_windows.fail_verify = true;
    app.tick_desktop_barrier(&ctx);
    assert!(rx.try_recv().unwrap().is_err());
    app.state
        .desktop_execution
        .as_ref()
        .unwrap()
        .active
        .store(0, std::sync::atomic::Ordering::Release);
    app.tick_desktop_barrier(&ctx);
    assert!(!app.desktop_windows.main_avoided);
}

#[test]
fn desktop_barrier_stale_deadline_and_approval_do_not_hide() {
    for pending_approval in [false, true] {
        let ctx = egui::Context::default();
        let mut app = NeoApp::install(&ctx);
        let deadline = if pending_approval {
            let mut meta = crate::state::ToolMeta::restored("click");
            meta.state = crate::state::ToolState::AwaitingConfirm;
            app.state
                .messages
                .push(crate::state::ChatMessage::tool_result(meta, String::new()));
            std::time::Instant::now() + std::time::Duration::from_secs(10)
        } else {
            std::time::Instant::now()
        };
        let (_, rx) = desktop_request(&app, deadline);
        app.tick_desktop_barrier(&ctx);
        assert!(rx.try_recv().unwrap().is_err());
        assert!(!app.desktop_windows.main_avoided);
        assert!(!super::desktop_suspended(&ctx));
    }
}

#[test]
fn desktop_barrier_exit_waits_and_tray_state_is_preserved() {
    for tray in [false, true] {
        let ctx = egui::Context::default();
        let mut app = NeoApp::install(&ctx);
        app.hidden_to_tray = tray;
        let (cancel, rx) = desktop_request(
            &app,
            std::time::Instant::now() + std::time::Duration::from_secs(10),
        );
        app.tick_desktop_barrier(&ctx);
        assert_eq!(app.desktop_windows.main_avoided, !tray);
        app.finish_exit(&ctx);
        assert!(app.desktop_exit_pending);
        assert!(cancel.load(std::sync::atomic::Ordering::Acquire));
        app.tick_desktop_barrier(&ctx);
        assert!(rx.try_recv().unwrap().is_err());
        app.state
            .desktop_execution
            .as_ref()
            .unwrap()
            .active
            .store(0, std::sync::atomic::Ordering::Release);
        app.tick_desktop_barrier(&ctx);
        assert!(!app.desktop_exit_pending);
        assert!(!app.desktop_windows.main_avoided);
        assert_eq!(app.hidden_to_tray, tray);
    }
}

#[test]
fn desktop_barrier_pending_deadline_never_admits() {
    let ctx = egui::Context::default();
    let mut app = NeoApp::install(&ctx);
    let (_, rx) = desktop_request(
        &app,
        std::time::Instant::now() + std::time::Duration::from_secs(10),
    );
    app.tick_desktop_barrier(&ctx);
    app.desktop_pending[0].deadline = std::time::Instant::now();
    app.tick_desktop_barrier(&ctx);
    assert!(rx.try_recv().unwrap().is_err());
    app.state
        .desktop_execution
        .as_ref()
        .unwrap()
        .active
        .store(0, std::sync::atomic::Ordering::Release);
    app.tick_desktop_barrier(&ctx);
    assert!(!app.desktop_windows.main_avoided);
}

#[test]
fn desktop_barrier_fallback_only_visible_nonminimized_windows_obstruct() {
    // TOPMOST 不参与判断；原本隐藏/最小化的窗口不阻挡桌面工具。
    assert!(!super::desktop_window_obstructs(false, false));
    assert!(!super::desktop_window_obstructs(false, true));
    assert!(!super::desktop_window_obstructs(true, true));
    assert!(super::desktop_window_obstructs(true, false));
}

#[test]
fn desktop_barrier_render_does_not_verify_in_same_logic_pass() {
    let ctx = egui::Context::default();
    ctx.set_embed_viewports(false);
    let mut app = NeoApp::install(&ctx);
    app.store = None;
    let mut output = ctx.run_ui(egui::RawInput::default(), |_| {});
    output.textures_delta.clear();
    let (_, rx) = desktop_request(
        &app,
        std::time::Instant::now() + std::time::Duration::from_secs(10),
    );
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
        app.tick(ui.ctx().clone());
        assert_eq!(app.desktop_pending.len(), 1);
        app.render(ui);
        assert_eq!(app.desktop_pending.len(), 1);
        assert!(
            rx.try_recv().is_err(),
            "render must not advance the hide/verify barrier"
        );
    });
    output.textures_delta.clear();
    app.tick_desktop_barrier(&ctx);
    assert!(rx.try_recv().unwrap().is_ok());
}

#[test]
fn desktop_barrier_discard_pass_cannot_ack_before_native_commands() {
    let ctx = egui::Context::default();
    let mut app = NeoApp::install(&ctx);
    let (_, rx) = desktop_request(
        &app,
        std::time::Instant::now() + std::time::Duration::from_secs(10),
    );
    let mut passes = 0;
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
        passes += 1;
        app.tick_desktop_barrier(ui.ctx());
        assert_eq!(app.desktop_pending.len(), 1);
        assert!(rx.try_recv().is_err());
        if ui.ctx().current_pass_index() == 0 {
            ui.ctx()
                .request_discard("exercise the synchronous layout retry");
        }
    });
    output.textures_delta.clear();
    assert_eq!(passes, 2);
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
        app.tick_desktop_barrier(ui.ctx());
    });
    output.textures_delta.clear();
    assert!(rx.try_recv().unwrap().is_ok());
}

#[test]
fn desktop_barrier_fallback_visibility_only_changes_on_edges() {
    let ctx = egui::Context::default();
    let viewport = egui::ViewportId::from_hash_of("fake-miniwin");
    for suspended in [false, true, true, false] {
        ctx.begin_pass(egui::RawInput::default());
        ctx.data_mut(|data| data.insert_temp(egui::Id::new("neo-desktop-suspended"), suspended));
        assert_eq!(super::desktop_viewport(&ctx, viewport), suspended);
        let mut output = ctx.end_pass();
        output.textures_delta.clear();
        for command in output.viewport_output.values().flat_map(|v| &v.commands) {
            if let egui::ViewportCommand::Visible(visible) = command {
                assert_eq!(*visible, !suspended);
            }
            assert!(!matches!(command, egui::ViewportCommand::Focus));
        }
    }
}

#[test]
fn safety_dictation_rejects_previous_generation() {
    assert!(super::current_dictation(true, 3, 3));
    assert!(!super::current_dictation(true, 3, 1));
    assert!(!super::current_dictation(false, 3, 3));
}

#[test]
fn safety_cancel_dictation_invalidates_queued_results_and_resets_once() {
    let ctx = egui::Context::default();
    let mut app = NeoApp::install(&ctx);
    let (tx, rx) = std::sync::mpsc::channel();
    app.stt_tx = Some(tx);
    app.dictating = true;
    app.dictation_epoch = 7;
    app.dictation_since = Some(std::time::Instant::now());
    app.cancel_dictation();
    assert!(!app.dictating);
    assert!(app.dictation_since.is_none());
    assert_eq!(app.dictation_epoch, 8);
    assert!(matches!(rx.try_recv(), Ok(super::SttCmd::Reset(8))));
    assert!(!super::current_dictation(
        app.dictating,
        app.dictation_epoch,
        7
    ));
    app.cancel_dictation();
    assert_eq!(app.dictation_epoch, 8);
    assert!(matches!(
        rx.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
    app.dictating = true;
    app.dictation_epoch += 1;
    assert!(!super::current_dictation(
        app.dictating,
        app.dictation_epoch,
        7
    ));
    assert!(app.wake.is_none());
}

#[test]
fn safety_session_switch_stabilizes_and_preserves_unsaved_messages() {
    use crate::state::{ChatMessage, Role, StreamSource};
    let store = temp_store("switch-barrier");
    let target = store.create_session("target").unwrap();
    let mut state = AppState::default();
    state.messages.push(ChatMessage::new(Role::User, "keep"));
    state.start_generation(StreamSource::Demo {
        text: "partial".into(),
        cursor: 0,
    });
    state.pump();
    assert!(NeoApp::prepare_session_change(&mut state, Some(&store)));
    assert!(!state.generating);
    let old = state.active_session.unwrap();
    assert_eq!(store.messages(old).unwrap().len(), state.messages.len());
    state.messages.push(ChatMessage::new(Role::User, "unsaved"));
    store.delete_session(old).unwrap();
    assert!(!NeoApp::open_session(&mut state, &store, target));
    assert_eq!(state.active_session, Some(old));
    assert_eq!(state.messages.last().unwrap().content, "unsaved");
    assert!(!state.generating);
}

#[test]
fn safety_save_failure_blocks_spawn_and_recovers_without_livelock() {
    use crate::state::{ChatMessage, Role, ToolMeta, ToolState};
    let store = temp_store("save-gate");
    let mut state = AppState::default();
    let missing = store.create_session("removed").unwrap();
    store.delete_session(missing).unwrap();
    state.active_session = Some(missing);
    state
        .messages
        .push(ChatMessage::new(Role::Assistant, "request"));
    let mut meta = ToolMeta::restored("write_file");
    meta.state = ToolState::Running;
    state
        .messages
        .push(ChatMessage::tool_result(meta, String::new()));
    state.tool_open = true;
    assert!(!NeoApp::advance_tools(&mut state, Some(&store)));
    assert!(!state.tools_running());
    assert_eq!(
        state.messages[1].tool.as_ref().unwrap().state,
        ToolState::Running
    );
    state.active_session = None;
    state.plan_mode = true;
    assert!(NeoApp::advance_tools(&mut state, Some(&store)));
    assert!(state.tools_settled());
    assert_eq!(state.pending_persist, state.messages.len());
    let rows = store.messages(state.active_session.unwrap()).unwrap();
    assert_eq!(rows.len(), 2);
    assert!(!state.messages[1].content.is_empty());
}

#[test]
fn safety_pending_tool_is_not_a_save_failure_but_settled_result_is() {
    use crate::state::{ChatMessage, Role, ToolMeta, ToolState};
    let store = temp_store("pending-versus-failed");
    let mut state = AppState::default();
    state
        .messages
        .push(ChatMessage::new(Role::Assistant, "request"));
    let mut meta = ToolMeta::restored("ask_user");
    meta.state = ToolState::AwaitingConfirm;
    state
        .messages
        .push(ChatMessage::tool_result(meta, String::new()));
    state.tool_open = true;
    assert!(NeoApp::advance_tools(&mut state, Some(&store)));
    assert_eq!(state.pending_persist, 1);
    assert!(!state.tools_settled());
    let id = state.active_session.unwrap();
    store.delete_session(id).unwrap();
    state.answer_question(1, Some("answer".into()));
    for _ in 0..2 {
        assert!(!NeoApp::advance_tools(&mut state, Some(&store)));
        assert!(state.tool_open);
        assert!(state.tools_settled());
        assert_eq!(state.pending_persist, 1);
    }
    assert!(!NeoApp::prepare_session_change(&mut state, Some(&store)));
    assert_eq!(state.messages.len(), 2);
    assert!(state.messages[1].content.contains("answer"));
}

#[test]
fn safety_approved_batch_rechecks_plan_before_spawning() {
    use crate::state::{ChatMessage, Role, ToolState};
    let store = temp_store("approved-plan");
    let mut state = AppState::default();
    state.classroom_safe = false;
    state
        .messages
        .push(ChatMessage::new(Role::Assistant, "request"));
    for (index, (name, args)) in [
        (
            "write_file",
            serde_json::json!({"path":"must-not-write", "content":"x"}),
        ),
        (
            "powershell",
            serde_json::json!({"command":"echo must-not-run"}),
        ),
        (
            "web_search",
            serde_json::json!({"query":"test", "open_browser":true}),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        state.tool_frags.push(neo_llm::ToolCallFrag {
            index,
            id: Some(format!("call-{index}")),
            name: Some(name.into()),
            args: args.to_string(),
        });
    }
    state.begin_tool_round();
    state.approve_all_awaiting();
    for message in &state.messages[1..] {
        assert_eq!(message.tool.as_ref().unwrap().state, ToolState::Running);
    }
    state.plan_mode = true;
    assert!(NeoApp::advance_tools(&mut state, Some(&store)));
    assert!(!state.tools_running());
    assert!(state.tools_settled());
    assert_eq!(state.pending_persist, state.messages.len());
    for message in &state.messages[1..] {
        assert_eq!(message.tool.as_ref().unwrap().state, ToolState::Denied);
        assert!(!message.content.is_empty());
    }
    assert_eq!(
        store.messages(state.active_session.unwrap()).unwrap().len(),
        4
    );
}

#[test]
fn safety_deleting_other_session_keeps_current_round_running() {
    use crate::state::{ChatMessage, Role, StreamSource};
    let store = temp_store("delete-other");
    let other = store.create_session("other").unwrap();
    let mut state = AppState::default();
    state.messages.push(ChatMessage::new(Role::User, "keep"));
    assert!(NeoApp::persist_ready(&mut state, Some(&store)));
    let current = state.active_session;
    state.start_generation(StreamSource::Demo {
        text: "partial".into(),
        cursor: 0,
    });
    state.pump();
    let epoch = state.session_epoch;
    assert!(NeoApp::delete_session(&mut state, &store, other));
    assert!(state.generating);
    assert!(!state.round_cancelled);
    assert_eq!(state.active_session, current);
    assert_eq!(state.session_epoch, epoch);
    assert_eq!(state.messages[1].content, "pa");
    assert!(NeoApp::delete_session(&mut state, &store, current.unwrap()));
    assert!(state.messages.is_empty());
    assert!(!state.generating);
    assert_ne!(state.session_epoch, epoch);
}

#[test]
fn safety_delete_current_does_not_discard_failed_save() {
    use crate::state::{ChatMessage, Role};
    let store = temp_store("delete-failed");
    let id = store.create_session("gone").unwrap();
    store.delete_session(id).unwrap();
    let mut state = AppState::default();
    state.active_session = Some(id);
    state.messages.push(ChatMessage::new(Role::User, "unsaved"));
    assert!(!NeoApp::delete_session(&mut state, &store, id));
    assert_eq!(state.active_session, Some(id));
    assert_eq!(state.messages[0].content, "unsaved");
    assert_eq!(state.pending_persist, 0);
}

#[test]
fn safety_test_databases_are_thread_local() {
    let own = super::test_db_path();
    let other = std::thread::spawn(super::test_db_path).join().unwrap();
    assert_ne!(own, other);
    assert_eq!(own, super::test_db_path());
}

#[test]
fn restores_last_active_session() {
    let store = temp_store("ok");
    let keep = store.create_session("留着的").unwrap();
    store.append_message(keep, "user", "你好", "", "").unwrap();
    store.create_session("别的").unwrap();
    store
        .set_setting("active_session", &keep.to_string())
        .unwrap();

    let mut state = AppState::default();
    NeoApp::refresh_sessions(&mut state, &store);
    NeoApp::restore_last_session(&mut state, &store);

    assert_eq!(state.active_session, Some(keep));
    assert_eq!(state.stage, Stage::Conversation);
    assert_eq!(state.messages.len(), 1);
    assert_eq!(state.messages[0].content, "你好");
    assert_eq!(
        state.pending_persist,
        state.messages.len(),
        "历史消息不应重复落库"
    );
    state.auto_approve_tools = true;
    NeoApp::open_session(&mut state, &store, keep);
    assert!(!state.auto_approve_tools, "切换会话必须清除临时授权");
    state.auto_approve_tools = true;
    let title = state.current_title();
    state.new_session();
    assert!(!state.auto_approve_tools, "新会话必须重新询问授权");
    assert_eq!(title, "留着的");
    NeoApp::open_session(&mut state, &store, keep);
    // 顶栏标题读会话行。
    assert_eq!(state.current_title(), "留着的");
}

fn sample_attachment(image: bool) -> crate::attachments::Attachment {
    crate::attachments::Attachment {
        name: if image {
            "题图.png"
        } else {
            "课堂资料.docx"
        }
        .into(),
        kind: if image { "image" } else { "document" }.into(),
        bytes: 32,
        text: "课堂资料正文".into(),
        image_url: image.then(|| "data:image/png;base64,SAMPLE".into()),
        warning: None,
    }
}

thread_local! {
    static CAPTURED_REQUESTS: std::cell::RefCell<Vec<Vec<neo_llm::Msg>>> = const { std::cell::RefCell::new(Vec::new()) };
    static INCOMING_MESSAGES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

struct MockStream;

impl MockStream {
    fn install() -> Self {
        crate::state::API_MESSAGE_BUILDS.with(|count| count.set(0));
        CAPTURED_REQUESTS.with(|requests| requests.borrow_mut().clear());
        super::STREAM_START.with(|start| {
            start.set(|cfg, mut messages, tools| {
                // 与通用入口一样，续轮也必须预算；只发送本地 channel 事件。
                INCOMING_MESSAGES.with(|count| count.set(messages.len()));
                let result = neo_llm::budget_messages(&cfg, &mut messages, &tools);
                CAPTURED_REQUESTS.with(|requests| requests.borrow_mut().push(messages));
                let (tx, rx) = std::sync::mpsc::channel();
                tx.send(match result {
                    Ok(()) => neo_llm::Event::Done { tool_calls: false },
                    Err(error) => neo_llm::Event::Failed(error),
                })
                .unwrap();
                neo_llm::Stream::new_for_test(rx)
            })
        });
        Self
    }
}

impl Drop for MockStream {
    fn drop(&mut self) {
        super::STREAM_START.with(|start| start.set(neo_llm::start_with_tools));
        CAPTURED_REQUESTS.with(|requests| requests.borrow_mut().clear());
    }
}

fn synthetic_send_state(image: bool) -> AppState {
    use crate::state::{ChatMessage, Role};
    let mut state = AppState::default();
    state.context_tokens = 32 * 1024;
    state.api_key = "synthetic-test".into();
    state.restore_models("deepseek-chat", Some("deepseek-chat"));
    state
        .messages
        .push(ChatMessage::new(Role::User, "历史问题"));
    state
        .messages
        .push(ChatMessage::new(Role::Assistant, "历史回答"));
    state.draft = "  分析课堂资料  ".into();
    let mut attachment = sample_attachment(image);
    if image {
        use base64::Engine;
        // 纯合成 PNG 头及载荷，只供预算验证，不调用解码器或模型。
        let mut bytes = vec![0; 768 * 1024];
        bytes[..24].copy_from_slice(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01");
        attachment.image_url = Some(format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        ));
    }
    state.add_attachment(attachment).unwrap();
    state
}

#[test]
fn send_reuse_synthetic_build_count() {
    for image in [false, true] {
        let _mock = MockStream::install();
        let mut state = synthetic_send_state(image);
        // 含图片也低于80%压缩阈值；本测试只验证正常发送的构建复用。
        state.context_tokens = 64 * 1024;
        NeoApp::send_input(&mut state, None);
        assert!(state.generating);
        let builds = crate::state::API_MESSAGE_BUILDS.with(|count| count.get());
        assert_eq!(builds, 1);
        CAPTURED_REQUESTS.with(|requests| {
                let requests = requests.borrow();
                assert_eq!(requests.len(), 1);
                let messages = &requests[0];
                assert_eq!(messages.len(), 4, "image={image}; first={}", messages[0].content);
                assert!(messages[0].content.contains("课堂安全模式限制"));
                assert!(messages[3].content.contains("课堂资料正文"));
                let image_bytes: usize = messages.iter().flat_map(|m| &m.images).map(String::len).sum();
                assert_eq!(image_bytes, if image { 1024 * 1024 + 22 } else { 0 });
                println!("synthetic image={image}: api_messages={builds}, image_bytes_per_build={image_bytes}");
            });
        state.pump();
        assert!(!state.generating);
        assert!(state.messages.last().unwrap().error.is_none());
    }
}

#[test]
fn send_reuse_compacts_instead_of_silently_trimming() {
    let _mock = MockStream::install();
    super::STREAM_START.with(|start| {
        start.set(|cfg, mut messages, tools| {
            assert!(tools.is_empty(), "摘要必须是独立无工具请求");
            neo_llm::budget_messages(&cfg, &mut messages, &tools).unwrap();
            assert!(messages[1].content.contains(&"a".repeat(20_000)));
            CAPTURED_REQUESTS.with(|requests| requests.borrow_mut().push(messages));
            let (tx, rx) = std::sync::mpsc::channel();
            tx.send(neo_llm::Event::Delta {
                content: "旧目标与已完成回答".into(),
                reasoning: String::new(),
            })
            .unwrap();
            tx.send(neo_llm::Event::Done { tool_calls: false }).unwrap();
            neo_llm::Stream::new_for_test(rx)
        })
    });
    let store = temp_store("compaction-roundtrip");
    let mut state = synthetic_send_state(false);
    state.messages[0].content = "a".repeat(20_000);
    NeoApp::send_input(&mut state, Some(&store));
    assert!(state.compaction.is_some() && state.generating);
    assert!(state.stream.is_none());
    assert!(state.poll_compaction());
    assert!(state.compaction_resume && state.checkpoint_dirty);
    assert!(NeoApp::persist_ready(&mut state, Some(&store)));
    let id = state.active_session.unwrap();
    assert!(store.checkpoint(id).unwrap().is_some());
    let expected = state.api_messages(24);
    assert_eq!(expected.len(), 3);
    assert_eq!(expected[1].role, neo_llm::Role::User);
    assert!(expected[1].content.contains("旧目标与已完成回答"));
    assert!(expected[2].content.contains("分析课堂资料"));
    assert_eq!(state.messages[0].content.len(), 20_000, "摘要不改本地历史");
    let mut restored = AppState::default();
    assert!(NeoApp::open_session(&mut restored, &store, id));
    assert!(restored
        .checkpoint
        .as_ref()
        .unwrap()
        .valid(&restored.messages));
    assert_eq!(restored.messages.len(), 3);
    assert_eq!(restored.api_messages(24)[1].content, expected[1].content);
}

#[test]
fn send_reuse_compaction_large_history_is_not_mistaken_for_latest_input() {
    let _mock = MockStream::install();
    let mut state = synthetic_send_state(false);
    state.messages[0].content = "x".repeat(neo_llm::MAX_REQUEST_BYTES + 1);
    let draft = state.draft.clone();
    NeoApp::send_input(&mut state, None);
    assert_eq!(state.draft, draft);
    assert_eq!(state.messages.len(), 2);
    assert_eq!(
        state.messages[0].content.len(),
        neo_llm::MAX_REQUEST_BYTES + 1
    );
    assert!(state
        .attachment_error
        .as_ref()
        .unwrap()
        .contains("旧历史无法"));
    assert!(!state.generating);
    CAPTURED_REQUESTS.with(|requests| assert!(requests.borrow().is_empty()));
}

#[test]
fn compaction_tool_protocol_survives_sqlite_reopen_with_uncovered_results() {
    use crate::state::{ChatMessage, Role};
    let store = temp_store("compaction-tool-protocol");
    let path = std::env::temp_dir()
        .join(format!(
            "neo-restore-compaction-tool-protocol-{}",
            std::process::id()
        ))
        .join("neo.db");
    let mut state = synthetic_send_state(false);
    state.messages[0].content = "a".repeat(20_000);
    assert!(state.submit());
    let mut checkpoint = state
        .compaction_plan(&state.api_messages(24))
        .unwrap()
        .unwrap()
        .checkpoint;
    checkpoint.summary = "旧历史摘要".into();
    state.checkpoint = Some(checkpoint);
    state.checkpoint_dirty = true;
    state
        .messages
        .push(ChatMessage::new(Role::Assistant, "需要确认"));
    let arguments = r#"{ "question": "保留原始参数?", "options": ["是", "否"] }"#;
    state.tool_frags = vec![neo_llm::ToolCallFrag {
        index: 0,
        id: Some("persist-call".into()),
        name: Some("ask_user".into()),
        args: arguments.into(),
    }];
    state.begin_tool_round();
    state.answer_question(4, Some("是".into()));
    assert!(NeoApp::persist_ready(&mut state, Some(&store)));
    let id = state.active_session.unwrap();
    let expected = state.messages[4].content.clone();
    drop(store);
    let store = Store::open(&path).unwrap();
    let mut restored = AppState::default();
    assert!(NeoApp::open_session(&mut restored, &store, id));
    assert!(restored.checkpoint.is_some());
    assert_eq!(restored.messages[3].tool_calls[0].arguments, arguments);
    let mut messages = restored.api_messages(24);
    neo_llm::budget_messages(
        &restored.llm_config(),
        &mut messages,
        &neo_tools::tool_declarations(),
    )
    .unwrap();
    assert_eq!(
        messages.last().unwrap().tool_call_id.as_deref(),
        Some("persist-call")
    );
    assert_eq!(messages.last().unwrap().content, expected);
    drop(store);
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn compaction_crash_after_call_persist_uses_same_unknown_result_as_context() {
    use crate::state::{ChatMessage, Role, ToolMeta, ToolState};
    let store = temp_store("crash-tool-intent");
    let mut state = AppState::default();
    state.context_tokens = 32 * 1024;
    state.messages.push(ChatMessage::new(Role::User, "旧目标"));
    let mut assistant = ChatMessage::new(Role::Assistant, "a".repeat(16_000));
    assistant.tool_calls.push(neo_llm::ToolCall {
        id: "crash-call".into(),
        name: "write_file".into(),
        arguments: "{ \"path\": \"audit.txt\", \"content\": \"x\" }".into(),
    });
    state.messages.push(assistant);
    let mut meta = ToolMeta::restored("write_file");
    meta.call_id = "crash-call".into();
    meta.state = ToolState::Running;
    state
        .messages
        .push(ChatMessage::tool_result(meta, String::new()));
    assert!(NeoApp::persist_ready(&mut state, Some(&store)));
    let id = state.active_session.unwrap();
    assert_eq!(state.pending_persist, 2);
    assert_eq!(
        store.messages(id).unwrap().len(),
        2,
        "崩溃边界只持久化调用意图"
    );
    state
        .messages
        .push(ChatMessage::new(Role::User, "继续分析，不重跑工具"));
    assert!(
        state
            .compaction_plan(&state.api_messages(24))
            .unwrap()
            .is_none(),
        "真实 running 不得压缩"
    );
    let mut restored = AppState::default();
    restored.context_tokens = 32 * 1024;
    assert!(NeoApp::open_session(&mut restored, &store, id));
    restored
        .messages
        .push(ChatMessage::new(Role::User, "继续分析，不重跑工具"));
    let mut messages = restored.api_messages(24);
    restored.context_tokens = neo_llm::context_usage(
        &restored.llm_config(),
        &messages,
        &neo_tools::tool_declarations(),
    )
    .unwrap()
        + 100;
    neo_llm::budget_messages(
        &restored.llm_config(),
        &mut messages,
        &neo_tools::tool_declarations(),
    )
    .unwrap();
    let result = messages
        .iter()
        .find(|m| m.role == neo_llm::Role::Tool)
        .unwrap();
    assert!(result.content.contains("执行状态未知"));
    assert!(!result.content.contains("未执行"));
    let plan = restored.compaction_plan(&messages).unwrap().unwrap();
    let history: serde_json::Value = serde_json::from_str(&plan.messages[1].content).unwrap();
    let result_in_summary = history
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "tool")
        .unwrap();
    assert_eq!(result_in_summary["content"], result.content);
    assert_eq!(result_in_summary["tool_call_id"], "crash-call");
    let mut checkpoint = plan.checkpoint;
    checkpoint.summary = "历史调用执行状态未知，不得自动重跑".into();
    assert!(checkpoint.valid(&restored.messages));
    assert!(!restored.tools_running());
    assert_eq!(
        restored.spawn_ready_tools(&neo_tools::Scope::new(std::env::temp_dir())),
        0
    );
    assert_eq!(restored.messages.len(), 3, "未知结果仅存在于请求副本");
    assert_eq!(store.messages(id).unwrap().len(), 2);
}

#[test]
fn uia_restore_session_isolation_rejects_old_reference_and_late_publication() {
    use neo_tools::tools::screen_uia;
    let store = temp_store("uia-session-isolation");
    let first = store.create_session("第一会话").unwrap();
    let second = store.create_session("第二会话").unwrap();
    let mut state = AppState::default();
    assert!(NeoApp::open_session(&mut state, &store, first));
    let scope = neo_tools::Scope::new(std::env::temp_dir());
    let element = screen_uia::ScreenElement {
        id: 1,
        role: "Button",
        name: "合成按钮".into(),
        label: "合成按钮".into(),
        enabled: true,
        foreground: true,
        actionable: true,
        rect: [0, 0, 10, 10],
        ..Default::default()
    };
    for switch in [false, true] {
        let id = screen_uia::cache_store_checked(
            std::slice::from_ref(&element),
            &scope,
            screen_uia::cache_generation(),
        )
        .unwrap();
        let generation = screen_uia::cache_generation();
        assert_eq!(screen_uia::cache_snapshot().unwrap().0, id);
        if switch {
            assert!(NeoApp::open_session(&mut state, &store, second));
        } else {
            state.cancel();
        }
        assert!(screen_uia::cache_consume(&id, 1).is_none());
        assert!(screen_uia::cache_store_checked(std::slice::from_ref(&element), &scope, generation).is_err());
        assert!(screen_uia::cache_snapshot().is_none());
    }
}

#[test]
fn screenshot_restore_session_switch_clears_only_outbound_reference_and_keeps_audit() {
    use crate::state::{ChatMessage, Role, ToolMeta};
    let store = temp_store("screenshot-wire-restore");
    let mut state = AppState::default();
    state
        .messages
        .push(ChatMessage::new(Role::User, "查看图片"));
    let mut assistant = ChatMessage::new(Role::Assistant, "观察");
    assistant.tool_calls.push(neo_llm::ToolCall {
        id: "image-call".into(),
        name: "screenshot".into(),
        arguments: "{}".into(),
    });
    state.messages.push(assistant);
    let mut meta = ToolMeta::restored("screenshot");
    meta.call_id = "image-call".into();
    let content = neo_tools::Outcome::ok(
        "screenshot",
        "已附图，已把图交给模型",
        serde_json::json!({
            "path":"audit.png", "image_attached":true, "screenshot_id":"old-shot",
            "image_space":{"width":2,"height":3}, "image_to_desktop":{"offset_x":-100}
        }),
    )
    .to_model_json(usize::MAX);
    let mut result = ChatMessage::tool_result(meta, content.clone());
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(2, 3)
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    result
        .images
        .push(neo_tools::tools::view_image::model_image(png.get_ref(), false).unwrap());
    state.messages.push(result);
    let live = state.api_messages(24);
    assert_eq!(live.last().unwrap().content, content);
    let live_wire = neo_llm::request_body(&state.llm_config(), &live, vec![]);
    assert!(live_wire["messages"].as_array().unwrap().last().unwrap()["content"].is_array());
    assert!(NeoApp::persist_ready(&mut state, Some(&store)));
    let id = state.active_session.unwrap();
    let other = store.create_session("其他会话").unwrap();
    assert!(NeoApp::open_session(&mut state, &store, other));
    assert!(NeoApp::open_session(&mut state, &store, id));
    assert_eq!(state.messages[2].content, content);
    assert!(state.messages[2].images.is_empty());
    let mut messages = state.api_messages(24);
    neo_llm::budget_messages(
        &state.llm_config(),
        &mut messages,
        &neo_tools::tool_declarations(),
    )
    .unwrap();
    let wire = neo_llm::request_body(&state.llm_config(), &messages, vec![]);
    let tool = wire["messages"].as_array().unwrap().last().unwrap();
    assert_eq!(tool["tool_call_id"], "image-call");
    let outbound: serde_json::Value =
        serde_json::from_str(tool["content"].as_str().unwrap()).unwrap();
    assert_eq!(outbound["data"]["image_attached"], false);
    assert!(outbound["data"]["screenshot_id"].is_null());
    assert!(outbound["data"].get("image_to_desktop").is_none());
    assert!(outbound["data"].get("image_space").is_none());
    assert_eq!(outbound["data"]["path"], "audit.png");
    assert_eq!(outbound["historical_image"], true);
    assert!(outbound.get("historical_image_audit").is_none());
    assert!(!tool["content"].as_str().unwrap().contains("old-shot"));
    assert!(outbound["summary"]
        .as_str()
        .unwrap()
        .contains("未随本次请求提供"));
    assert_eq!(store.messages(id).unwrap()[2].content, content);

    state.messages.push(ChatMessage::new(
        Role::User,
        "继续分析历史，但不得凭旧截图操作",
    ));
    state.context_tokens = neo_llm::context_usage(
        &state.llm_config(),
        &state.api_messages(24),
        &neo_tools::tool_declarations(),
    )
    .unwrap();
    let plan = state
        .compaction_plan(&state.api_messages(24))
        .unwrap()
        .unwrap();
    assert!(plan.messages.iter().all(|m| m.images.is_empty()));
    let history: serde_json::Value = serde_json::from_str(&plan.messages[1].content).unwrap();
    let compacted: serde_json::Value =
        serde_json::from_str(history[2]["content"].as_str().unwrap()).unwrap();
    assert_eq!(compacted, outbound);
    assert!(!plan.messages[1].content.contains("old-shot"));
    assert!(!plan.messages[1].content.contains("image_to_desktop"));
    assert_eq!(state.messages[2].content, content);
    let mut checkpoint = plan.checkpoint;
    checkpoint.summary = "历史图片未提供，后续操作需重新观察".into();
    assert!(checkpoint.valid(&state.messages));
    state.checkpoint = Some(checkpoint);
    state.checkpoint_dirty = true;
    assert!(NeoApp::persist_ready(&mut state, Some(&store)));
    assert!(NeoApp::open_session(&mut state, &store, id));
    assert!(state.checkpoint.as_ref().unwrap().valid(&state.messages));
    assert_eq!(state.messages[2].content, content);
    assert_eq!(store.messages(id).unwrap()[2].content, content);
}

#[test]
fn context_legacy_tool_results_restore_as_low_trust_without_fabricated_calls() {
    let store = temp_store("context-legacy-tool");
    let id = store.create_session("旧工具记录").unwrap();
    store
        .append_message(id, "assistant", "读取结果", "", "")
        .unwrap();
    store
        .append_message(id, "tool", "历史结果不能消失", "", "read_file · 读取完成")
        .unwrap();
    let mut state = AppState::default();
    assert!(NeoApp::open_session(&mut state, &store, id));
    let mut messages = state.api_messages(24);
    assert!(messages.iter().all(|m| m.tool_calls.is_empty()));
    let result = messages.last().unwrap();
    assert_eq!(result.role, neo_llm::Role::User);
    assert!(result.content.contains("低信任历史工具结果"));
    assert!(result.content.contains("历史结果不能消失"));
    neo_llm::budget_messages(
        &state.llm_config(),
        &mut messages,
        &neo_tools::tool_declarations(),
    )
    .unwrap();
}

#[test]
fn task_tool_limit_500th_completes_and_persists_but_never_continues() {
    use crate::state::{ChatMessage, Role, ToolState};
    for batch in [1, 3] {
        let _mock = MockStream::install();
        let store = temp_store(&format!("task-tool-limit-{batch}"));
        let mut state = synthetic_send_state(false);
        assert!(state.submit());
        state.task_tool_calls = 499;
        state
            .messages
            .push(ChatMessage::new(Role::Assistant, "确认"));
        state.tool_frags = (0..batch)
            .map(|index| neo_llm::ToolCallFrag {
                index,
                id: Some(format!("limit-{index}")),
                name: Some("ask_user".into()),
                args: r#"{"question":"继续?"}"#.into(),
            })
            .collect();
        assert_eq!(state.begin_tool_round(), batch);
        assert!(state.task_limit_reached);
        assert_eq!(
            state.messages[4].tool.as_ref().unwrap().state,
            ToolState::AwaitingConfirm
        );
        state.answer_question(4, Some("第500次完成".into()));
        assert!(state.messages[4]
            .tool
            .as_ref()
            .unwrap()
            .outcome
            .as_ref()
            .unwrap()
            .is_ok());
        for msg in &state.messages[5..] {
            assert_eq!(msg.tool.as_ref().unwrap().state, ToolState::Denied);
        }
        assert!(NeoApp::advance_tools(&mut state, Some(&store)));
        let rows = store.messages(state.active_session.unwrap()).unwrap();
        assert_eq!(rows.len(), state.messages.len());
        assert!(rows[4].content.contains("第500次完成"));
        assert_eq!(rows[4].tool_call_id, "limit-0");
        let mut messages = state.api_messages(24);
        neo_llm::budget_messages(
            &state.llm_config(),
            &mut messages,
            &neo_tools::tool_declarations(),
        )
        .unwrap();
        NeoApp::start_real_stream(&mut state);
        assert!(!state.generating);
        CAPTURED_REQUESTS.with(|requests| assert!(requests.borrow().is_empty()));
    }
}

#[test]
fn task_tool_limit_500th_read_dispatches_after_save_and_persists() {
    use crate::state::{ChatMessage, Role, ToolState};
    let _mock = MockStream::install();
    let store = temp_store("task-tool-limit-read");
    let dir = std::env::temp_dir().join(format!(
        "neo-restore-task-tool-limit-read-{}",
        std::process::id()
    ));
    std::fs::write(dir.join("input.txt"), "第500次读取成功").unwrap();
    let mut state = synthetic_send_state(false);
    assert!(state.submit());
    state.task_tool_calls = 499;
    state
        .messages
        .push(ChatMessage::new(Role::Assistant, "读取"));
    state.tool_frags = vec![neo_llm::ToolCallFrag {
        index: 0,
        id: Some("last-read".into()),
        name: Some("read_file".into()),
        args: r#"{"path":"input.txt"}"#.into(),
    }];
    state.begin_tool_round();
    assert!(state.task_limit_reached);
    assert_eq!(
        state.messages[4].tool.as_ref().unwrap().state,
        ToolState::Running
    );
    assert!(NeoApp::persist_ready(&mut state, Some(&store)));
    assert_eq!(state.pending_persist, 4);
    assert_eq!(state.spawn_ready_tools(&neo_tools::Scope::new(&dir)), 1);
    assert!(state.wait_tool_jobs(std::time::Duration::from_secs(5)));
    assert!(state.messages[4]
        .tool
        .as_ref()
        .unwrap()
        .outcome
        .as_ref()
        .unwrap()
        .is_ok());
    assert!(NeoApp::persist_ready(&mut state, Some(&store)));
    assert!(store.messages(state.active_session.unwrap()).unwrap()[4]
        .content
        .contains("第500次读取成功"));
    NeoApp::start_real_stream(&mut state);
    CAPTURED_REQUESTS.with(|requests| assert!(requests.borrow().is_empty()));
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn compaction_restore_rejects_corrupt_checkpoint_without_losing_history() {
    let store = temp_store("compaction-corrupt");
    let mut state = synthetic_send_state(false);
    state.messages[0].content = "a".repeat(20_000);
    assert!(state.submit());
    assert!(NeoApp::persist_ready(&mut state, Some(&store)));
    let plan = state
        .compaction_plan(&state.api_messages(24))
        .unwrap()
        .unwrap();
    let mut checkpoint = plan.checkpoint;
    checkpoint.summary = "历史摘要".into();
    checkpoint.fingerprint ^= 1;
    let id = state.active_session.unwrap();
    store
        .save_checkpoint(
            id,
            checkpoint.covered,
            &serde_json::to_string(&checkpoint).unwrap(),
        )
        .unwrap();
    let mut restored = AppState::default();
    assert!(NeoApp::open_session(&mut restored, &store, id));
    assert!(restored.checkpoint.is_none());
    assert!(restored
        .compaction_status
        .as_ref()
        .unwrap()
        .contains("校验失败"));
    assert_eq!(restored.messages.len(), 3);
    assert_eq!(restored.api_messages(24)[1].content.len(), 20_000);
}

#[test]
fn compaction_checkpoint_save_failure_holds_resume_and_cancel_stops_it() {
    let store = temp_store("compaction-readonly");
    let mut state = synthetic_send_state(false);
    state.messages[0].content = "a".repeat(20_000);
    assert!(state.submit());
    assert!(NeoApp::persist_ready(&mut state, Some(&store)));
    let plan = state
        .compaction_plan(&state.api_messages(24))
        .unwrap()
        .unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    state.start_compaction(plan, neo_llm::Stream::new_for_test(rx));
    tx.send(neo_llm::Event::Delta {
        content: "完整摘要".into(),
        reasoning: String::new(),
    })
    .unwrap();
    tx.send(neo_llm::Event::Done { tool_calls: false }).unwrap();
    state.poll_compaction();
    drop(store);
    let path = std::env::temp_dir()
        .join(format!(
            "neo-restore-compaction-readonly-{}",
            std::process::id()
        ))
        .join("neo.db");
    let store = readonly_store(&path);
    assert!(!NeoApp::persist_ready(&mut state, Some(&store)));
    assert!(state.compaction_resume && state.checkpoint_dirty);
    state.draft = "不能越过保存屏障".into();
    assert!(!state.can_submit());
    state.cancel();
    assert!(!state.compaction_resume);
    assert_eq!(state.messages.len(), 3);
    assert!(store
        .checkpoint(state.active_session.unwrap())
        .unwrap()
        .is_none());
}

#[test]
fn compaction_tick_resumes_once_after_checkpoint_save_and_limit_stops_continuation() {
    let _mock = MockStream::install();
    super::STREAM_START.with(|start| {
        start.set(|_, messages, tools| {
            let summary = tools.is_empty();
            CAPTURED_REQUESTS.with(|requests| requests.borrow_mut().push(messages));
            let (tx, rx) = std::sync::mpsc::channel();
            if summary {
                tx.send(neo_llm::Event::Delta {
                    content: "历史整理完成".into(),
                    reasoning: String::new(),
                })
                .unwrap();
            }
            tx.send(neo_llm::Event::Done { tool_calls: false }).unwrap();
            neo_llm::Stream::new_for_test(rx)
        })
    });
    let ctx = egui::Context::default();
    let mut app = NeoApp::install(&ctx);
    app.store = Some(temp_store("compaction-tick"));
    app.state = synthetic_send_state(false);
    app.state.messages[0].content = "a".repeat(20_000);
    NeoApp::send_input(&mut app.state, app.store.as_ref());
    assert!(app.state.compaction.is_some());
    app.tick(ctx.clone());
    assert!(!app.state.checkpoint_dirty);
    assert!(app
        .store
        .as_ref()
        .unwrap()
        .checkpoint(app.state.active_session.unwrap())
        .unwrap()
        .is_some());
    app.tick(ctx.clone());
    app.tick(ctx);
    CAPTURED_REQUESTS.with(|requests| assert_eq!(requests.borrow().len(), 2));
    assert!(!app.state.generating && !app.state.compaction_resume);
    app.state.task_tool_calls = 500;
    app.state.task_limit_reached = true;
    NeoApp::start_real_stream(&mut app.state);
    CAPTURED_REQUESTS.with(|requests| assert_eq!(requests.borrow().len(), 2));
    assert!(app.state.attachment_error.as_ref().unwrap().contains("500"));
}

#[test]
fn compaction_config_change_after_completion_cancels_automatic_resume() {
    let _mock = MockStream::install();
    let ctx = egui::Context::default();
    let mut app = NeoApp::install(&ctx);
    app.store = Some(temp_store("compaction-config-change"));
    app.state = synthetic_send_state(false);
    app.state.messages[0].content = "a".repeat(20_000);
    assert!(app.state.submit());
    assert!(NeoApp::persist_ready(&mut app.state, app.store.as_ref()));
    let plan = app
        .state
        .compaction_plan(&app.state.api_messages(24))
        .unwrap()
        .unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    app.state
        .start_compaction(plan, neo_llm::Stream::new_for_test(rx));
    tx.send(neo_llm::Event::Delta {
        content: "历史摘要".into(),
        reasoning: String::new(),
    })
    .unwrap();
    tx.send(neo_llm::Event::Done { tool_calls: false }).unwrap();
    assert!(app.state.poll_compaction());
    assert!(app.state.compaction_resume);
    app.state.context_tokens += 1024;
    app.tick(ctx);
    assert!(!app.state.compaction_resume && !app.state.generating);
    assert!(app
        .state
        .compaction_status
        .as_ref()
        .unwrap()
        .contains("自动续轮已取消"));
    CAPTURED_REQUESTS.with(|requests| assert!(requests.borrow().is_empty()));
    assert_eq!(app.state.messages.len(), 3);
}

#[test]
fn send_reuse_save_barriers_preserve_draft_and_attachments() {
    use crate::state::{ChatMessage, Role};
    let store = temp_store("send-reuse-readonly");
    let id = store.create_session("原会话").unwrap();
    let path = std::env::temp_dir()
        .join(format!(
            "neo-restore-send-reuse-readonly-{}",
            std::process::id()
        ))
        .join("neo.db");
    drop(store);
    let store = readonly_store(&path);
    for before_submit in [true, false] {
        let _mock = MockStream::install();
        let mut state = synthetic_send_state(true);
        state.messages.clear();
        state.active_session = Some(id);
        if before_submit {
            state
                .messages
                .push(ChatMessage::new(Role::User, "未保存历史"));
            state.stage = Stage::Conversation;
        }
        let draft = state.draft.clone();
        let attachments = serde_json::to_string(&state.draft_attachments).unwrap();
        NeoApp::send_input(&mut state, Some(&store));
        // 原语义：首屏障失败不触碰草稿，提交后的保存失败恢复已 trim 的正文。
        assert_eq!(
            state.draft,
            if before_submit {
                draft.as_str()
            } else {
                draft.trim()
            }
        );
        assert_eq!(
            serde_json::to_string(&state.draft_attachments).unwrap(),
            attachments
        );
        assert_eq!(state.messages.len(), usize::from(before_submit));
        assert_eq!(state.pending_persist, 0);
        assert_eq!(
            state.stage,
            if before_submit {
                Stage::Conversation
            } else {
                Stage::Hero
            }
        );
        assert_eq!(state.active_session, before_submit.then_some(id));
        assert!(state.attachment_error.is_some());
        assert!(!state.generating && !state.wants_demo_reply && state.stream.is_none());
        assert!(store.messages(id).unwrap().is_empty());
        CAPTURED_REQUESTS.with(|requests| assert!(requests.borrow().is_empty()));
        crate::state::API_MESSAGE_BUILDS
            .with(|count| assert_eq!(count.get(), usize::from(!before_submit)));
    }
}

#[test]
fn send_reuse_budget_failure_restores_original_draft_and_image() {
    let _mock = MockStream::install();
    let mut state = synthetic_send_state(true);
    state.draft = format!("  {}  ", "课堂正文".repeat(20_000));
    state.stage = Stage::Conversation;
    let draft = state.draft.clone();
    let attachments = serde_json::to_string(&state.draft_attachments).unwrap();
    NeoApp::send_input(&mut state, None);
    assert_eq!(state.draft, draft);
    assert_eq!(
        serde_json::to_string(&state.draft_attachments).unwrap(),
        attachments
    );
    assert_eq!(state.messages.len(), 2);
    assert_eq!(state.pending_persist, 2);
    assert_eq!(state.stage, Stage::Conversation);
    assert!(state.attachment_error.is_some());
    assert!(!state.generating && !state.wants_demo_reply && state.stream.is_none());
    CAPTURED_REQUESTS.with(|requests| assert!(requests.borrow().is_empty()));
    crate::state::API_MESSAGE_BUILDS.with(|count| assert_eq!(count.get(), 1));
}

#[test]
fn send_reuse_tool_round_still_checks_budget_and_pairing() {
    use crate::state::{ChatMessage, Role, ToolMeta};
    for over_budget in [false, true] {
        let _mock = MockStream::install();
        let mut state = synthetic_send_state(false);
        assert!(state.submit());
        let mut assistant = ChatMessage::new(Role::Assistant, "");
        assistant.reasoning = if over_budget {
            "推理".repeat(20_000)
        } else {
            "先读取资料".into()
        };
        assistant.tool_calls.push(neo_llm::ToolCall {
            id: "synthetic-call".into(),
            name: "read_file".into(),
            arguments: "{}".into(),
        });
        state.messages.push(assistant);
        let mut meta = ToolMeta::restored("read_file");
        meta.call_id = "synthetic-call".into();
        state
            .messages
            .push(ChatMessage::tool_result(meta, "工具结果".into()));
        NeoApp::start_real_stream(&mut state);
        crate::state::API_MESSAGE_BUILDS.with(|count| assert_eq!(count.get(), 1));
        CAPTURED_REQUESTS.with(|requests| {
            let requests = requests.borrow();
            assert_eq!(requests.len(), 1);
            if !over_budget {
                let messages = &requests[0];
                assert_eq!(messages[4].tool_calls[0].id, "synthetic-call");
                assert_eq!(messages[5].tool_call_id.as_deref(), Some("synthetic-call"));
                assert_eq!(messages[5].content, "工具结果");
            }
        });
        state.poll_compaction();
        state.pump();
        assert!(!state.generating);
        assert_eq!(state.attachment_error.is_some(), over_budget);
        assert!(state.messages.iter().any(|m| m.content == "工具结果"));
    }
}

#[test]
fn attachment_persistence_restores_payload_and_tolerates_bad_json() {
    let store = temp_store("attachments-restore");
    let mut state = AppState::default();
    state.add_attachment(sample_attachment(true)).unwrap();
    assert!(state.submit());
    state.start_generation(crate::state::StreamSource::Demo {
        text: "稍后回复".into(),
        cursor: 0,
    });
    assert!(NeoApp::persist_ready(&mut state, Some(&store)));
    assert_eq!(
        state.pending_persist, 1,
        "生成过程中必须已保存用户附件，不能保存流式占位"
    );
    let id = state.active_session.unwrap();
    let path = std::env::temp_dir()
        .join(format!(
            "neo-restore-attachments-restore-{}",
            std::process::id()
        ))
        .join("neo.db");
    drop(store);
    let store = neo_store::Store::open(&path).unwrap();
    store
        .append_message_with_attachments(id, "user", "仍可读", "", "", "invalid-json")
        .unwrap();
    let mut restored = AppState::default();
    restored.draft = "清除旧草稿".into();
    restored.add_attachment(sample_attachment(false)).unwrap();
    NeoApp::open_session(&mut restored, &store, id);
    assert!(restored.draft.is_empty() && restored.draft_attachments.is_empty());
    assert_eq!(restored.messages.len(), 2);
    assert_eq!(restored.messages[0].attachments.len(), 1);
    assert!(restored.attachment_error.as_ref().unwrap().contains("损坏"));
    let body = neo_llm::request_body(&restored.llm_config(), &restored.api_messages(24), vec![]);
    assert_eq!(
        body["messages"][1]["content"][1]["image_url"]["url"],
        "data:image/png;base64,SAMPLE"
    );
}

#[test]
fn attachment_send_without_model_preserves_draft() {
    for configured in [false, true] {
        let mut state = AppState::default();
        if configured {
            state.api_key = "test-only".into();
            state.api_base = String::new();
        }
        state.draft = "分析这份资料".into();
        state.add_attachment(sample_attachment(false)).unwrap();
        NeoApp::send_input(&mut state, None);
        assert_eq!(state.draft_attachments.len(), 1);
        assert_eq!(state.draft, "分析这份资料");
        assert!(state.messages.is_empty() && !state.wants_demo_reply);
        assert!(state.attachment_error.as_ref().unwrap().contains("未发送"));
    }
}

#[test]
fn attachment_save_failure_does_not_advance_cursor() {
    let store = temp_store("attachment-save-failure");
    let mut state = AppState::default();
    state.active_session = Some(999_999);
    state.add_attachment(sample_attachment(false)).unwrap();
    assert!(state.submit());
    assert!(!NeoApp::persist_ready(&mut state, Some(&store)));
    assert_eq!(state.pending_persist, 0);
    assert!(state.attachment_error.is_some());
}

#[test]
fn attachment_and_text_sends_reach_local_mock_model() {
    use std::io::{Read, Write};
    for mode in 0..3 {
        let store = temp_store(&format!("mock-model-{mode}"));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(error) => panic!("本地模型 accept 失败: {error}"),
                }
                assert!(std::time::Instant::now() < deadline, "模型没有收到请求");
                std::thread::sleep(std::time::Duration::from_millis(10));
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            stream
                .set_write_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut chunk = [0u8; 4096];
            let (offset, length) = loop {
                assert!(std::time::Instant::now() < deadline, "模型请求头超时");
                let count = stream.read(&mut chunk).unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&chunk[..count]);
                if let Some(pos) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&bytes[..pos]);
                    let length = header
                        .lines()
                        .find_map(|line| {
                            let (key, value) = line.split_once(':')?;
                            key.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap();
                    break (pos + 4, length);
                }
            };
            while bytes.len() < offset + length {
                assert!(std::time::Instant::now() < deadline, "模型请求体超时");
                let count = stream.read(&mut chunk).unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&chunk[..count]);
            }
            let body: serde_json::Value =
                serde_json::from_slice(&bytes[offset..offset + length]).unwrap();
            let response = "data: {\"choices\":[{\"delta\":{\"content\":\"已收到\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n";
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", response.len(), response).unwrap();
            body
        });
        let mut state = AppState::default();
        state.api_base = format!("http://{address}");
        state.api_key = "local-test".into();
        state.set_models_from_provider(vec!["test-vision".into()]);
        if mode == 0 {
            state.draft = "纯文字".into();
        } else {
            let mut attachment = sample_attachment(mode == 2);
            if mode == 2 {
                use base64::Engine;
                let mut bytes = std::io::Cursor::new(Vec::new());
                image::DynamicImage::new_rgb8(1, 1)
                    .write_to(&mut bytes, image::ImageFormat::Png)
                    .unwrap();
                attachment.bytes = bytes.get_ref().len() as u64;
                attachment.image_url = Some(format!(
                    "data:image/png;base64,{}",
                    base64::engine::general_purpose::STANDARD.encode(bytes.get_ref())
                ));
            }
            state.add_attachment(attachment).unwrap();
        }
        NeoApp::send_input(&mut state, Some(&store));
        let generating = state.generating;
        let body = server.join().unwrap();
        assert!(generating);
        assert_eq!(state.pending_persist, 1);
        let persisted = store.messages(state.active_session.unwrap()).unwrap();
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].content, state.messages[0].content);
        let user = &body["messages"][1]["content"];
        if mode == 0 {
            assert_eq!(user, "纯文字");
        } else if mode == 1 {
            assert!(user.as_str().unwrap().contains("课堂资料正文"));
        } else {
            assert_eq!(user[1]["type"], "image_url");
        }
        state.cancel();
    }
}

#[test]
fn ignores_stale_session_id() {
    let store = temp_store("stale");
    store.create_session("现存会话").unwrap();
    store.set_setting("active_session", "99999").unwrap();

    let mut state = AppState::default();
    NeoApp::refresh_sessions(&mut state, &store);
    NeoApp::restore_last_session(&mut state, &store);

    assert_eq!(state.active_session, None);
    assert_eq!(state.stage, Stage::Hero);
}
