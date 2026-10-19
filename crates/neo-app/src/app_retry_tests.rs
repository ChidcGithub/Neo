//! 传输类接口失败的自动重试回归；模型入口替换为内存通道，不联网。
use super::*;

type StreamStart = fn(neo_llm::Config, Vec<neo_llm::Msg>, Vec<serde_json::Value>) -> neo_llm::Stream;

thread_local! {
    static START_CALLS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// 前若干次调用按脚本失败，之后返回正常完成流。
struct ScriptedStreams(StreamStart);

impl ScriptedStreams {
    fn fail_first_then_done(failures: u32, error: &str) -> Self {
        FAILURES.with(|slot| slot.set(failures));
        FAIL_MESSAGE.with(|slot| *slot.borrow_mut() = error.to_owned());
        fn scripted(_: neo_llm::Config, _: Vec<neo_llm::Msg>, _: Vec<serde_json::Value>) -> neo_llm::Stream {
            START_CALLS.with(|calls| calls.set(calls.get() + 1));
            let (tx, rx) = std::sync::mpsc::channel();
            let used = FAILURES.with(|slot| {
                let left = slot.get();
                slot.set(left.saturating_sub(1));
                left
            });
            if used > 0 {
                tx.send(neo_llm::Event::Failed(FAIL_MESSAGE.with(|slot| slot.borrow().clone()))).unwrap();
            } else {
                tx.send(neo_llm::Event::Done { tool_calls: false }).unwrap();
            }
            neo_llm::Stream::new_for_test(rx)
        }
        Self(STREAM_START.with(|slot| slot.replace(scripted)))
    }
}

impl Drop for ScriptedStreams {
    fn drop(&mut self) {
        STREAM_START.with(|slot| slot.set(neo_llm::start_with_tools));
        FAILURES.with(|slot| slot.set(0));
    }
}

thread_local! {
    static FAILURES: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    static FAIL_MESSAGE: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

fn tick_once(app: &mut NeoApp, ctx: &egui::Context) {
    ctx.begin_pass(egui::RawInput::default());
    app.tick(ctx.clone());
    let mut output = ctx.end_pass();
    output.textures_delta.clear();
}

fn streaming_app(ctx: &egui::Context) -> NeoApp {
    let mut app = NeoApp::install(ctx);
    app.store = None;
    app.state.new_session();
    app.state.auto_check_updates = false;
    app.state.api_key = "retry-test".into();
    app.state.restore_models("deepseek-chat", Some("deepseek-chat"));
    app.state.draft = "讲一下楞次定律".into();
    NeoApp::send_input(&mut app.state, None);
    app
}

fn force_due(state: &mut AppState) {
    if let Some(retry) = &mut state.retry {
        retry.due = std::time::Instant::now() - std::time::Duration::from_secs(1);
    }
}

#[test]
fn transport_failure_schedules_retry_and_reuses_bubble() {
    let _script = ScriptedStreams::fail_first_then_done(1, "接口返回 503 Service Unavailable：PRIVATE_BODY");
    let ctx = egui::Context::default();
    let mut app = streaming_app(&ctx);
    let total = app.state.messages.len();
    tick_once(&mut app, &ctx); // pump 收到 503 → 排程重试
    // 生成态保持，排程出现，气泡复用且 meta 显示重试状态。
    assert!(app.state.generating);
    let retry = app.state.retry.as_ref().expect("应排程重试");
    assert_eq!(retry.attempt, 1);
    assert!(retry.announced, "tick 应已播报一次");
    assert_eq!(app.state.messages.len(), total, "重试不得新开气泡");
    let bubble = app.state.messages.last().unwrap();
    assert!(bubble.streaming);
    assert!(bubble.content.is_empty(), "半截输出应作废");
    assert!(bubble.meta.contains("自动重试"), "meta 应显示重试状态：{}", bubble.meta);
    // 到点重发：同一条流式气泡继续，等 Done 收尾。
    force_due(&mut app.state);
    tick_once(&mut app, &ctx);
    assert!(app.state.retry.as_ref().is_some_and(|r| r.fired), "重发后记录保留，标记已发起");
    assert!(app.state.stream.is_some(), "重试流应已发起");
    tick_once(&mut app, &ctx); // pump 收到 Done
    assert!(!app.state.generating);
    assert_eq!(app.state.messages.len(), total, "全程只有一条助手气泡");
    assert!(app.state.messages.last().unwrap().error.is_none());
    assert_eq!(START_CALLS.with(|calls| calls.get()), 2, "首发 + 重试共两次请求");
}

#[test]
fn fatal_failure_skips_retry_entirely() {
    let _script = ScriptedStreams::fail_first_then_done(1, "接口返回 401 Unauthorized：PRIVATE_KEY");
    let ctx = egui::Context::default();
    let mut app = streaming_app(&ctx);
    tick_once(&mut app, &ctx);
    assert!(app.state.retry.is_none(), "鉴权失败不得重试");
    assert!(!app.state.generating);
    let bubble = app.state.messages.last().unwrap();
    assert!(!bubble.streaming);
    assert!(bubble.error.as_deref().is_some_and(|e| e.contains("401")));
    assert_eq!(START_CALLS.with(|calls| calls.get()), 1);
}

#[test]
fn retry_gives_up_after_three_attempts_with_final_error() {
    let _script = ScriptedStreams::fail_first_then_done(u32::MAX, "接口返回 503 Service Unavailable：x");
    let ctx = egui::Context::default();
    let mut app = streaming_app(&ctx);
    for attempt in 1..=crate::state::MAX_STREAM_RETRIES {
        tick_once(&mut app, &ctx); // 失败 → 排程
        let retry = app.state.retry.as_ref().unwrap_or_else(|| panic!("第 {attempt} 次应排程"));
        assert_eq!(retry.attempt, attempt);
        force_due(&mut app.state);
        tick_once(&mut app, &ctx); // 重发
    }
    // 第 4 次失败超出上限：转普通错误收尾。
    tick_once(&mut app, &ctx);
    assert!(app.state.retry.is_none());
    assert!(!app.state.generating);
    assert!(app.state.messages.last().unwrap().error.is_some());
}

#[test]
fn cancel_drops_pending_retry() {
    let _script = ScriptedStreams::fail_first_then_done(1, "请求失败：connection reset");
    let ctx = egui::Context::default();
    let mut app = streaming_app(&ctx);
    tick_once(&mut app, &ctx);
    assert!(app.state.retry.is_some());
    app.state.cancel();
    assert!(app.state.retry.is_none());
    assert!(!app.state.generating);
    force_due(&mut app.state); // 已取消，即使到点也不能复活
    tick_once(&mut app, &ctx);
    assert!(app.state.retry.is_none());
    assert!(app.state.stream.is_none());
}
