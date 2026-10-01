//! 应用层日志收尾；tick 回归使用隔离测试库，模型入口替换为内存通道，不联网。
use super::*;
use crate::diagnostics::capture_for_test;

type StreamStart = fn(neo_llm::Config, Vec<neo_llm::Msg>, Vec<serde_json::Value>) -> neo_llm::Stream;

struct OfflineStreams(StreamStart);

impl OfflineStreams {
    fn install() -> Self {
        fn completed(_: neo_llm::Config, _: Vec<neo_llm::Msg>, _: Vec<serde_json::Value>) -> neo_llm::Stream {
            let (tx, rx) = std::sync::mpsc::channel();
            tx.send(neo_llm::Event::Done { tool_calls: false }).unwrap();
            neo_llm::Stream::new_for_test(rx)
        }
        Self(STREAM_START.with(|slot| slot.replace(completed)))
    }
}

impl Drop for OfflineStreams {
    fn drop(&mut self) { STREAM_START.with(|slot| slot.set(self.0)); }
}

fn tick_once(app: &mut NeoApp, ctx: &egui::Context) {
    ctx.begin_pass(egui::RawInput::default());
    app.tick(ctx.clone());
    let mut output = ctx.end_pass();
    output.textures_delta.clear();
}

fn task_events(view: &crate::diagnostics::Snapshot) -> Vec<&str> {
    view.entries.iter().filter(|entry| entry.component.as_ref() == "task")
        .map(|entry| entry.message.as_ref()).collect()
}

fn pending_compaction_resume() -> AppState {
    let mut state = AppState::default();
    state.api_key = "PRIVATE_KEY".into();
    state.restore_models("deepseek-chat", Some("deepseek-chat"));
    state.begin_task();
    state.compaction_resume = true;
    state.compaction_resume_config = Some(state.llm_config());
    state
}

#[test]
fn tick_finishes_generation_before_same_frame_submit_or_session_change() {
    let _offline = OfflineStreams::install();
    for switch_session in [false, true] {
        let ctx = egui::Context::default();
        let mut app = NeoApp::install(&ctx);
        app.state.new_session();
        app.state.classroom_safe = true;
        app.applied_classroom_safe = true;
        app.state.auto_check_updates = false;
        app.state.api_key.clear();
        app.store = None;
        let view = capture_for_test(|| {
            app.state.begin_task();
            app.state.start_generation(StreamSource::Demo { text: "好".into(), cursor: 0 });
            tick_once(&mut app, &ctx);
            assert!(!app.state.generating);
            // 与 render 的动作入口一致，中间不再运行 tick 或手动收尾。
            if switch_session {
                assert!(NeoApp::prepare_session_change(&mut app.state, None));
                app.state.new_session();
            } else {
                app.state.draft = "PRIVATE_NEXT_INPUT".into();
                NeoApp::send_input(&mut app.state, None);
                assert!(app.state.wants_demo_reply);
            }
        });
        let events = task_events(&view);
        assert_eq!(events.len(), if switch_session { 2 } else { 3 });
        assert!(events[0].contains("event=started"));
        assert!(events[1].contains("event=completed"));
        if !switch_session { assert!(events[2].contains("event=started")); }
        assert!(!events.iter().any(|event| event.contains("abandoned") || event.contains("cancel_requested")));
        assert!(!format!("{view:?}").contains("PRIVATE"));
    }
}

#[test]
fn compaction_resume_rejections_record_one_explicit_terminal_event() {
    let _offline = OfflineStreams::install();
    for (case, phase, kind) in [
        ("changed", "failed", Some("configuration")),
        ("missing_config", "failed", Some("configuration")),
        ("missing_model", "failed", Some("configuration")),
        ("unconfigured", "failed", Some("configuration")),
        ("cancelled", "cancel_requested", None),
        ("limit_flag", "rejected", Some("tool_limit")),
        ("limit_count", "rejected", Some("tool_limit")),
        ("budget", "rejected", Some("budget")),
    ] {
        let view = capture_for_test(|| {
            let mut state = pending_compaction_resume();
            match case {
                "changed" => state.api_key = "PRIVATE_CHANGED_KEY".into(),
                "missing_config" => state.compaction_resume_config = None,
                "missing_model" => {
                    state.models.clear();
                    state.compaction_resume_config = Some(state.llm_config());
                }
                "unconfigured" => {
                    state.api_key.clear();
                    state.compaction_resume_config = Some(state.llm_config());
                }
                "cancelled" => state.round_cancelled = true,
                "limit_flag" => state.task_limit_reached = true,
                "limit_count" => state.task_tool_calls = crate::state::TASK_TOOL_LIMIT,
                "budget" => {
                    state.context_tokens = 1;
                    state.compaction_resume_config = Some(state.llm_config());
                }
                _ => unreachable!(),
            }
            NeoApp::resume_compaction(&mut state, true);
            assert!(!state.compaction_resume && state.compaction_resume_config.is_none());
            assert!(!state.generating && state.stream.is_none(), "{case}");
            NeoApp::finish_idle_task_diagnostic(&mut state);
            NeoApp::resume_compaction(&mut state, true);
            state.cancel();
        });
        let events = task_events(&view);
        assert_eq!(events.len(), 2, "{case}: {events:?}");
        assert!(events[1].contains(&format!("event={phase}")), "{case}");
        if let Some(kind) = kind { assert!(events[1].contains(&format!("kind={kind}")), "{case}"); }
        assert!(!events.iter().any(|event| event.contains("event=completed")));
        assert!(!format!("{view:?}").contains("PRIVATE"));
    }
}

#[test]
fn compaction_save_barrier_keeps_parent_until_successful_resume() {
    let _offline = OfflineStreams::install();
    let view = capture_for_test(|| {
        let mut state = pending_compaction_resume();
        NeoApp::resume_compaction(&mut state, false);
        NeoApp::finish_idle_task_diagnostic(&mut state);
        assert!(state.compaction_resume && state.compaction_resume_config.is_some());
        assert!(!state.generating);
        NeoApp::resume_compaction(&mut state, true);
        assert!(state.generating && !state.compaction_resume);
        NeoApp::finish_idle_task_diagnostic(&mut state);
        assert!(!state.pump());
        NeoApp::finish_idle_task_diagnostic(&mut state);
    });
    let events = task_events(&view);
    assert_eq!(events.len(), 2);
    assert!(events[1].contains("event=completed"));
    let id = events[0].split_whitespace().find_map(|field| field.strip_prefix("id=")).unwrap();
    let model: Vec<_> = view.entries.iter().filter(|entry| entry.component.as_ref() == "model").collect();
    assert_eq!(model.len(), 2);
    assert!(model.iter().all(|entry| entry.message.contains(&format!("parent={id}"))));
}

#[test]
fn tick_keeps_resumed_generation_in_same_task_until_stream_finishes() {
    let _offline = OfflineStreams::install();
    let ctx = egui::Context::default();
    let mut app = NeoApp::install(&ctx);
    app.store = None;
    app.applied_classroom_safe = true;
    let view = capture_for_test(|| {
        app.state = pending_compaction_resume();
        app.state.auto_check_updates = false;
        tick_once(&mut app, &ctx);
        assert!(app.state.generating && !app.state.compaction_resume);
        tick_once(&mut app, &ctx);
        assert!(!app.state.generating);
        assert!(NeoApp::prepare_session_change(&mut app.state, None));
    });
    let events = task_events(&view);
    assert_eq!(events.len(), 2);
    assert!(events[1].contains("event=completed"));
    let completed = view.entries.iter().position(|entry| entry.component.as_ref() == "task"
        && entry.message.contains("event=completed")).unwrap();
    let stream_completed = view.entries.iter().position(|entry| entry.component.as_ref() == "model"
        && entry.message.contains("event=completed")).unwrap();
    assert!(stream_completed < completed, "任务不能在续轮生成之前结束");
}

#[test]
fn tick_records_compaction_resume_configuration_failure_not_completion() {
    let _offline = OfflineStreams::install();
    let ctx = egui::Context::default();
    let mut app = NeoApp::install(&ctx);
    app.store = None;
    app.applied_classroom_safe = true;
    let view = capture_for_test(|| {
        app.state = pending_compaction_resume();
        app.state.auto_check_updates = false;
        app.state.api_key = "PRIVATE_CHANGED_KEY".into();
        tick_once(&mut app, &ctx);
        tick_once(&mut app, &ctx);
    });
    let events = task_events(&view);
    assert_eq!(events.len(), 2);
    assert!(events[1].contains("event=failed") && events[1].contains("kind=configuration"));
    assert!(!app.state.compaction_resume && !app.state.generating);
    assert!(!format!("{view:?}").contains("PRIVATE"));
}

#[test]
fn idle_completion_waits_for_continuations_and_records_once_without_tray() {
    let mut state = AppState::default();
    let view = capture_for_test(|| {
        state.begin_task();
        state.wants_demo_reply = true;
        NeoApp::finish_idle_task_diagnostic(&mut state);
        state.wants_demo_reply = false;
        state.compaction_resume = true;
        NeoApp::finish_idle_task_diagnostic(&mut state);
        state.compaction_resume = false;
        state.tool_round = true;
        NeoApp::finish_idle_task_diagnostic(&mut state);
        state.tool_round = false;
        NeoApp::finish_idle_task_diagnostic(&mut state);
        NeoApp::finish_idle_task_diagnostic(&mut state);
    });
    assert_eq!(view.entries.len(), 2);
    assert!(view.entries[0].message.contains("event=started"));
    assert!(view.entries[1].message.contains("event=completed"));
    assert_eq!(view.entries[1].occurrences, 1);
}

#[test]
fn idle_failure_and_tool_limit_do_not_log_raw_error_or_false_completion() {
    let mut state = AppState::default();
    let view = capture_for_test(|| {
        state.begin_task();
        state.attachment_error = Some("PRIVATE_COMMAND PRIVATE_KEY PRIVATE_BODY".into());
        NeoApp::finish_idle_task_diagnostic(&mut state);
        state.begin_task();
        state.task_limit_reached = true;
        NeoApp::start_real_stream(&mut state);
        NeoApp::finish_idle_task_diagnostic(&mut state);
    });
    let text = format!("{view:?}");
    assert!(text.contains("event=failed"));
    assert!(text.contains("event=rejected") && text.contains("kind=tool_limit"));
    assert!(!text.contains("PRIVATE") && !text.contains("event=completed"));
}

#[test]
fn cancellation_is_not_reported_as_success_or_recorded_twice() {
    let mut state = AppState::default();
    let view = capture_for_test(|| {
        state.begin_task();
        state.cancel();
        NeoApp::finish_idle_task_diagnostic(&mut state);
    });
    assert_eq!(view.entries.len(), 2);
    assert!(view.entries[1].message.contains("event=cancel_requested"));
    assert!(!view.entries.iter().any(|e| e.message.contains("event=completed")));
}
