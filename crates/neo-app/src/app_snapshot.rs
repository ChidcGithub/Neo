
//! 离屏渲染：把主界面渲成 PNG，落到 `docs-pri/screens/`。
//!
//! 这些不是"必须通过"的断言型测试 —— 它们的价值是**让人能看见界面**。
//! 跑 `cargo test -p neo-app -- --nocapture` 之后直接看 `docs-pri/screens/*.png`。
//!
//! 之所以能这么做，是因为 [`NeoApp::install`] 只依赖 `egui::Context`：
//! 测试与真机走完全相同的字体装配与主题构建路径。
//!
//! # 测试与数据库
//!
//! install 在测试构建中使用线程独立的显式临时数据库路径，不修改进程环境。

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use egui::Vec2;

use super::NeoApp;
use crate::state::{AppState, ChatMessage, Role, Stage, StreamSource, ToolMeta, ToolState};
use neo_theme::{Distance, ThemeMode};

fn isolate_db() {
    let _ = super::test_db_path();
}

/// 只清理当前测试线程的库，不会删除并行测试的数据库。
fn fresh_db() {
    let _ = std::fs::remove_file(super::test_db_path());
}

/// 一次性生效的初始化回调（只允许取用一次）。
type Setup = Box<dyn FnOnce(&mut NeoApp)>;

/// 输出目录：`<workspace>/docs-pri/screens`。
fn out_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs-pri/screens");
    std::fs::create_dir_all(&dir).expect("无法创建 docs-pri/screens");
    dir.canonicalize().unwrap_or(dir)
}

/// 渲染一帧并落盘。
fn shoot(name: &str, size: Vec2, setup: impl FnOnce(&mut NeoApp) + 'static) -> PathBuf {
    shoot_steps(name, size, 2, setup)
}

/// 指定帧数的版本。持续重绘的界面（脉动点、鲸鱼游动）会让
/// [`Harness::run`] 撞上 max_steps 上限 —— 它假设界面最终会静止。
fn shoot_steps(
    name: &str,
    size: Vec2,
    steps: usize,
    setup: impl FnOnce(&mut NeoApp) + 'static,
) -> PathBuf {
    isolate_db();
    let app: RefCell<Option<NeoApp>> = RefCell::new(None);
    let setup: RefCell<Option<Setup>> = RefCell::new(Some(Box::new(setup)));

    let mut harness = egui_kittest::Harness::builder()
        .with_size(size)
        .wgpu()
        .build_ui(|ui| {
            let mut slot = app.borrow_mut();
            let Some(app) = slot.as_mut() else {
                // 首帧：只装配（字体 / 主题 / 品牌纹理），不绘制。
                fresh_db();
                let mut fresh = NeoApp::install(ui.ctx());
                // ⚠️ 快照必须从**干净空态**开始：`install` 会从共享的测试库里
                // 恢复「上次会话」并把 stage 切成对话态 —— 于是标称 hero 的用例
                // 实际画的是对话视图（顶右出现「新对话」的加号而不是主题钮）。
                // 这里把会话状态清空；需要对话的用例自己在 setup 里 seed。
                fresh.state.messages.clear();
                fresh.state.active_session = None;
                fresh.state.stage = Stage::Hero;
                fresh.state.pending_persist = 0;
                if let Some(f) = setup.borrow_mut().take() {
                    f(&mut fresh);
                }
                *slot = Some(fresh);
                ui.ctx().request_repaint();
                return;
            };
            app.tick(ui.ctx().clone());
            app.render(ui);
        });

    harness.run_steps(steps);

    if name == "25-commonmark-conversation" {
        let (viewport, content, offset) = harness.ctx.data(|data| {
            data.get_temp::<(egui::Rect, Vec2, Vec2)>(egui::Id::new("neo-test-thread-geometry"))
                .unwrap()
        });
        assert!(
            content.y < viewport.height(),
            "short conversation must fit: {content:?} in {viewport:?}"
        );
        assert!(offset.y.abs() < 1.0, "no phantom bottom scroll: {offset:?}");
        let visible = harness.output().shapes.iter().any(|shape| {
            if let egui::Shape::Text(t) = &shape.shape {
                t.galley.text() == "目录与文件清单" && shape.clip_rect.contains(t.pos)
            } else {
                false
            }
        });
        assert!(
            visible,
            "table heading must actually be visible, not only stored in message state"
        );
    }

    let image = harness
        .render()
        .unwrap_or_else(|e| panic!("离屏渲染 {name} 失败: {e}"));
    let path = out_dir().join(format!("{name}.png"));
    image.save(&path).expect("PNG 落盘失败");
    println!("[snapshot] {} × {} → {}", size.x, size.y, path.display());
    path
}

/// 填一段演示对话（含流式结束后的助手回复）。
fn seed_dialogue(app: &mut NeoApp) {
    app.state.draft = "帮我用板书的结构讲解「楞次定律」".to_owned();
    app.state.submit();
    let demo = crate::state::demo_reply("楞次定律", app.state.model_display());
    app.state.start_generation(StreamSource::Demo {
        text: demo,
        cursor: 0,
    });
    // 一次泵完，不依赖时间推进。
    while app.state.pump() {}
    app.state.messages[1].meta = "DeepSeek-V3.2 · 1.4s".to_owned();
}

/// 搭一个「对话态、输入卡已聚焦」的 Harness。
///
/// 返回 `(harness, app, ctx)`：`app` 通过 `Rc` 共享，测试体在跑完帧后读它断言；
/// `ctx` 已把焦点请求到输入卡（下一帧 `m.focused()` 生效）。
fn input_harness(
    draft: &'static str,
) -> (
    egui_kittest::Harness<'static>,
    Rc<RefCell<Option<NeoApp>>>,
    egui::Context,
) {
    let app: Rc<RefCell<Option<NeoApp>>> = Rc::new(RefCell::new(None));
    let ctx_slot: Rc<RefCell<Option<egui::Context>>> = Rc::new(RefCell::new(None));
    let (app2, ctx2) = (app.clone(), ctx_slot.clone());
    let mut harness = egui_kittest::Harness::builder()
        .with_size(Vec2::new(1600.0, 1000.0))
        .wgpu()
        .build_ui(move |ui| {
            *ctx2.borrow_mut() = Some(ui.ctx().clone());
            let mut slot = app2.borrow_mut();
            if slot.is_none() {
                // 首帧只装配，不绘制（字体 `set_fonts` 下一帧才生效）。
                fresh_db();
                let mut fresh = NeoApp::install(ui.ctx());
                // 断言型测试要与既有持久化隔离：丢弃 store 走纯内存，
                // 并清掉 install 恢复出来的上次会话，从空白对话开始。
                fresh.store = None;
                fresh.state.messages.clear();
                fresh.state.active_session = None;
                fresh.state.pending_persist = 0;
                fresh.state.generating = false;
                fresh.state.wants_demo_reply = false;
                fresh.state.stage = Stage::Conversation;
                fresh.state.draft = draft.to_owned();
                *slot = Some(fresh);
                ui.ctx().request_repaint();
                return;
            }
            let app = slot.as_mut().unwrap();
            app.tick(ui.ctx().clone());
            app.render(ui);
        });
    harness.run_steps(2);
    let ctx = ctx_slot.borrow().clone().unwrap();
    ctx.memory_mut(|m| m.request_focus(egui::Id::new(crate::ui::COMPOSER_ID)));
    (harness, app, ctx)
}

fn visual_attachment(name: &str, image: bool) -> crate::attachments::Attachment {
    use base64::Engine;
    let image_url = image.then(|| {
        let mut bytes = std::io::Cursor::new(Vec::new());
        let image = image::RgbImage::from_fn(240, 150, |x, y| {
            if (x / 30 + y / 30) % 2 == 0 {
                image::Rgb([76, 126, 208])
            } else {
                image::Rgb([196, 220, 252])
            }
        });
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes.get_ref())
        )
    });
    crate::attachments::Attachment {
        name: name.into(),
        kind: if image { "image" } else { "document" }.into(),
        bytes: 254_810,
        text: "课堂学习资料".into(),
        image_url,
        warning: (!image).then(|| "仅提取文字；不含嵌入图片和复杂排版".into()),
    }
}

#[test]
fn attachment_draft_and_sent_snapshots() {
    for (name, mode, size, sent) in [
        (
            "19-attachments-dark-1080p",
            ThemeMode::Dark,
            Vec2::new(1920.0, 1080.0),
            false,
        ),
        (
            "20-attachments-light-narrow",
            ThemeMode::Light,
            Vec2::new(1024.0, 768.0),
            false,
        ),
        (
            "21-attachments-conversation",
            ThemeMode::Dark,
            Vec2::new(1920.0, 1080.0),
            true,
        ),
    ] {
        shoot(name, size, move |app| {
            app.store = None;
            app.state.theme_mode = mode;
            app.state.distance = Distance::Standard;
            for (name, image) in [
                ("磁通量变化题图.png", true),
                ("高中物理必修第三册课堂讲解与练习资料.docx", false),
                ("课堂实验演示课件.pptx", false),
            ] {
                app.state
                    .add_attachment(visual_attachment(name, image))
                    .unwrap();
            }
            app.state.draft = "请对照题图和课件，整理这节课的重点".into();
            if sent {
                app.state.submit();
                app.state.messages.push(ChatMessage::new(
                    Role::Assistant,
                    "已收到附件。图片将通过视觉通道发送，文档文字则随这条消息提供给模型。",
                ));
            } else if mode == ThemeMode::Light {
                app.state.attachment_error =
                    Some("损坏的旧课件.ppt：无法读取文件记录，请另存为 PPTX 后重试".into());
            }
        });
    }
}

#[test]
fn attachment_remove_button_really_removes_last_item() {
    isolate_db();
    let (mut harness, app, ctx) = input_harness("");
    app.borrow_mut()
        .as_mut()
        .unwrap()
        .state
        .add_attachment(visual_attachment("题图.png", true))
        .unwrap();
    harness.run_steps(2);
    let rect = app
        .borrow()
        .as_ref()
        .map(|a| {
            ctx.data(|data| {
                data.get_temp::<egui::Rect>(egui::Id::new("neo-test-first-attachment-remove"))
            })
            .unwrap_or_else(|| panic!("附件移除按钮没有绘制：{}", a.state.draft_attachments.len()))
        })
        .unwrap();
    let pos = rect.center();
    harness
        .input_mut()
        .events
        .push(egui::Event::PointerMoved(pos));
    for pressed in [true, false] {
        harness.input_mut().events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
        harness.run_steps(1);
    }
    let borrowed = app.borrow();
    let state = &borrowed.as_ref().unwrap().state;
    assert!(state.draft_attachments.is_empty());
    assert!(!state.can_submit());
}

/// 中文输入法选词那一帧里按 Enter，不应把消息发出去。
#[test]
fn enter_during_ime_commit_does_not_send() {
    isolate_db();
    let (mut harness, app, _ctx) = input_harness("");
    // 同帧塞入：输入法提交「你好」+ 一个 Enter（模拟 winit 未过滤掉的那一下）。
    harness
        .input_mut()
        .events
        .push(egui::Event::Ime(egui::ImeEvent::Commit("你好".to_owned())));
    harness.input_mut().events.push(egui::Event::Key {
        key: egui::Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    });
    harness.run_steps(1);

    let app = app.borrow();
    let app = app.as_ref().unwrap();
    assert!(
        app.state.messages.is_empty(),
        "IME 选词的 Enter 不该发送，实际发了 {:?}",
        app.state
            .messages
            .iter()
            .map(|m| &m.content)
            .collect::<Vec<_>>()
    );
    assert!(
        app.state.draft.contains("你好"),
        "候选字应落入草稿，实测 draft={:?}",
        app.state.draft
    );
}

/// 英文直接输入（无输入法事件）时，Enter 照常发送。
#[test]
fn enter_without_ime_sends() {
    isolate_db();
    let (mut harness, app, _ctx) = input_harness("hello");
    harness.input_mut().events.push(egui::Event::Key {
        key: egui::Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    });
    harness.run_steps(1);

    let app = app.borrow();
    let app = app.as_ref().unwrap();
    assert_eq!(
        app.state.messages.len(),
        1,
        "无输入法事件时 Enter 应正常发送"
    );
    assert_eq!(app.state.messages[0].content, "hello");
    assert!(app.state.draft.is_empty());
}

#[test]
fn modal_blocks_enter_and_background_clicks() {
    isolate_db();
    let (mut harness, app, _) = input_harness("不要发送这个草稿");
    app.borrow_mut().as_mut().unwrap().state.show_settings = true;
    harness.run_steps(2);
    harness.input_mut().events.push(egui::Event::Key {
        key: egui::Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    });
    // 左侧「新对话」：即使点击真实命中区，也不能操作模态框后的界面。
    let pos = egui::pos2(100.0, 90.0);
    harness
        .input_mut()
        .events
        .push(egui::Event::PointerMoved(pos));
    for pressed in [true, false] {
        harness.input_mut().events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
        harness.run_steps(1);
    }
    let borrowed = app.borrow();
    let state = &borrowed.as_ref().unwrap().state;
    assert!(state.messages.is_empty());
    assert_eq!(state.draft, "不要发送这个草稿");
    assert_eq!(state.stage, Stage::Conversation);
    assert!(state.show_settings);
}

#[test]
fn tool_confirm_long_parameters_really_scroll() {
    isolate_db();
    let (mut harness, app, ctx) = input_harness("");
    let mut message = ChatMessage::new(Role::Assistant, String::new());
    message.tool = Some(ToolMeta {
        call_id: "scroll-test".into(),
        name: "write_file".into(),
        title: "写入文件",
        risk: "write",
        preview: "写入长文本".into(),
        args: serde_json::json!({"content": "完整参数不能丢失。".repeat(1000)}),
        state: ToolState::AwaitingConfirm,
        outcome: None,
    });
    app.borrow_mut()
        .as_mut()
        .unwrap()
        .state
        .messages
        .push(message);
    harness.run_steps(4);
    let probe = egui::Id::new("neo-confirm-scroll-probe");
    let (rect, before, full_h) = ctx
        .data(|d| d.get_temp::<(egui::Rect, f32, f32)>(probe))
        .unwrap();
    assert!(full_h > rect.height());
    harness
        .input_mut()
        .events
        .push(egui::Event::PointerMoved(rect.center()));
    harness.run_steps(1);
    harness.input_mut().events.push(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: Vec2::new(0.0, -500.0),
        phase: egui::TouchPhase::Move,
        modifiers: egui::Modifiers::NONE,
    });
    harness.run_steps(5);
    let (_, after, _) = ctx
        .data(|d| d.get_temp::<(egui::Rect, f32, f32)>(probe))
        .unwrap();
    assert!(after > before, "参数区必须能实际滚动");
    assert!(app
        .borrow()
        .as_ref()
        .unwrap()
        .state
        .awaiting_tool()
        .is_some());
}

#[test]
fn disabled_components_never_register_click_sense() {
    let mut installed = false;
    let mut harness = egui_kittest::Harness::builder().build_ui(|ui| {
        if !installed {
            neo_theme::fonts::install(ui.ctx());
            installed = true;
            ui.ctx().request_repaint();
            return;
        }
        let d = neo_ui::Design::new(neo_theme::Theme::new(
            ThemeMode::Dark,
            1080.0,
            Distance::Standard,
        ));
        let button = neo_ui::Button::new("禁用").enabled(false).show(ui, &d);
        let icon = neo_ui::IconButton::new(neo_ui::Icon::Plus)
            .enabled(false)
            .show_at(ui, &d, egui::pos2(180.0, 80.0));
        let switch = neo_ui::Switch::new(true).enabled(false).show(ui, &d);
        for response in [button, icon, switch] {
            assert!(!response.sense.senses_click());
            assert!(!response.clicked());
        }
    });
    harness.run_steps(3);
}

#[test]
fn appearance_changes_persist_and_reload() {
    isolate_db();
    let (mut harness, app, _) = input_harness("");
    {
        let mut slot = app.borrow_mut();
        let app = slot.as_mut().unwrap();
        app.store = Some(neo_store::Store::open(&super::test_db_path()).unwrap());
        app.state.theme_mode = ThemeMode::Light;
        app.state.distance = Distance::Auditorium;
        app.state.show_reasoning = true;
    }
    harness.run_steps(2);
    {
        let mut slot = app.borrow_mut();
        let app = slot.as_mut().unwrap();
        let store = app.store.as_ref().unwrap();
        assert_eq!(store.setting("theme").unwrap().as_deref(), Some("light"));
        assert_eq!(
            store.setting("distance").unwrap().as_deref(),
            Some("auditorium")
        );
        app.state.theme_mode = ThemeMode::Dark;
        app.state.distance = Distance::Standard;
        app.state.show_reasoning = false;
        app.load_settings();
        assert_eq!(app.state.theme_mode, ThemeMode::Light);
        assert_eq!(app.state.distance, Distance::Auditorium);
        assert!(app.state.show_reasoning);
    }
}

#[test]
fn workspace_picker_result_and_cancel_are_nonblocking() {
    isolate_db();
    let (mut harness, app, _) = input_harness("");
    let (tx, rx) = std::sync::mpsc::channel();
    app.borrow_mut().as_mut().unwrap().workspace_picker = Some(rx);
    harness.run_steps(2); // 未返回结果时仍能绘制。
    let path = std::env::temp_dir().join("neo-workspace-result");
    tx.send(Some(path.clone())).unwrap();
    harness.run_steps(2);
    assert_eq!(
        app.borrow().as_ref().unwrap().state.workspace_dir,
        Some(path.clone())
    );
    let (tx, rx) = std::sync::mpsc::channel();
    app.borrow_mut().as_mut().unwrap().workspace_picker = Some(rx);
    tx.send(None).unwrap();
    harness.run_steps(2);
    let slot = app.borrow();
    let app = slot.as_ref().unwrap();
    assert_eq!(app.state.workspace_dir, Some(path));
    assert!(app.workspace_picker.is_none());
}

/// 造一条工具调用分片。
fn frag(id: &str, name: &str, args: &str) -> neo_llm::ToolCallFrag {
    neo_llm::ToolCallFrag {
        index: 0,
        id: Some(id.to_owned()),
        name: Some(name.to_owned()),
        args: args.to_owned(),
    }
}

/// 造一条已出结果的工具消息（用于快照）。
fn tool_msg(name: &str, args: serde_json::Value, outcome: neo_tools::Outcome) -> ChatMessage {
    let def = neo_tools::find(name);
    let title = def.map(|t| t.title).unwrap_or("工具");
    let risk = def.map(|t| t.risk.as_str()).unwrap_or("?");
    let preview = def
        .map(|t| (t.preview)(&neo_tools::Args::new(t, &args)))
        .unwrap_or_default();
    let content = neo_tools::to_model_message(&outcome);
    ChatMessage::tool_result(
        ToolMeta {
            call_id: format!("call_{name}"),
            name: name.to_owned(),
            title,
            risk,
            preview,
            args,
            state: if outcome.is_ok()
                || outcome.error.as_ref().map(|e| e.kind) == Some(neo_tools::ErrorKind::NotAllowed)
            {
                if outcome.is_ok() {
                    ToolState::Done
                } else {
                    ToolState::Denied
                }
            } else {
                ToolState::Done
            },
            outcome: Some(outcome),
        },
        content,
    )
}

/// 只读模式下写类工具被策略直接拒绝（不打扰用户），结果照实回灌给模型。
#[test]
fn read_only_tool_round_is_denied_without_asking() {
    let mut st = AppState::default();
    st.read_only = true;
    st.messages
        .push(ChatMessage::new(Role::Assistant, "我来改一下这个文件"));
    st.tool_frags = vec![frag(
        "call_1",
        "write_file",
        r#"{"path":"a.txt","content":"hi"}"#,
    )];
    st.tool_round = true;

    assert_eq!(st.begin_tool_round(), 1, "应登记一条工具消息");
    assert!(st.awaiting_tool().is_none(), "只读拒绝不该弹确认框");
    assert!(st.tools_settled(), "策略拒绝也要立刻出结果");

    let msg = st.messages.last().unwrap();
    let tool = msg.tool.as_ref().unwrap();
    assert_eq!(tool.state, ToolState::Denied);
    assert_eq!(
        tool.outcome.as_ref().unwrap().error.as_ref().unwrap().kind,
        neo_tools::ErrorKind::NotAllowed
    );
    // 回灌内容必须是可解析的协议 JSON，且带上"为什么没成"
    let v: serde_json::Value = serde_json::from_str(&msg.content).unwrap();
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["kind"], "not_allowed");
    // 助手消息要挂上 tool_calls，否则 role=tool 没有可配对的调用
    assert_eq!(st.messages[0].tool_calls.len(), 1);
}

/// 写类工具默认要用户点头；批准后才落盘。
#[test]
fn write_tool_asks_then_runs_after_approval() {
    let dir = std::env::temp_dir().join(format!("neo-tool-app-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let scope = neo_tools::Scope::new(&dir);

    let mut st = AppState::default();
    st.classroom_safe = false;
    st.messages
        .push(ChatMessage::new(Role::Assistant, String::new()));
    st.tool_frags = vec![frag(
        "call_9",
        "write_file",
        r#"{"path":"n.txt","content":"你好"}"#,
    )];
    st.tool_round = true;
    st.begin_tool_round();

    assert_eq!(st.awaiting_tool(), Some(1), "写文件必须先问");
    assert!(!st.tools_settled());
    assert!(!dir.join("n.txt").exists(), "没批准之前不该落盘");

    st.approve_tool(1);
    // 执行在后台线程：等它收工再断言。
    assert_eq!(st.spawn_ready_tools(&scope), 1);
    assert!(st.wait_tool_jobs(std::time::Duration::from_secs(10)));

    assert_eq!(std::fs::read_to_string(dir.join("n.txt")).unwrap(), "你好");
    assert!(st.tools_settled());
    let v: serde_json::Value = serde_json::from_str(&st.messages[1].content).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["tool"], "write_file");
}

/// 工具执行必须在**另一个线程**：`spawn_ready_tools` 要立刻返回，
/// 界面才不会因为一个可能要跑几分钟的命令冻住。
#[test]
fn tool_execution_does_not_block_the_caller() {
    let dir = std::env::temp_dir().join(format!("neo-tool-async-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let scope = neo_tools::Scope::new(&dir);

    let mut st = AppState::default();
    st.classroom_safe = false;
    st.messages
        .push(ChatMessage::new(Role::Assistant, String::new()));
    st.tool_frags = vec![frag(
        "call_sleep",
        "powershell",
        r#"{"command":"Start-Sleep -Seconds 2"}"#,
    )];
    st.tool_round = true;
    st.begin_tool_round();
    st.approve_tool(1);

    // 命令自己要睡 2 秒：启动必须"立刻"回来，否则就是同步执行。
    let t0 = std::time::Instant::now();
    assert_eq!(st.spawn_ready_tools(&scope), 1);
    let cost = t0.elapsed();
    assert!(
        cost < std::time::Duration::from_millis(400),
        "spawn 花了 {cost:?}，说明它在等命令返回"
    );
    assert!(st.tools_running(), "应当仍在后台跑");
    assert!(!st.tools_settled(), "还没出结果就不该算完成");
    assert!(
        st.messages[1]
            .tool
            .as_ref()
            .unwrap()
            .line()
            .contains("执行中"),
        "卡片要说明还在跑，而不是让用户以为命令没反应"
    );

    // 收工后结果落进消息，并回灌成协议 JSON
    assert!(st.wait_tool_jobs(std::time::Duration::from_secs(20)));
    assert!(st.tools_settled());
    let v: serde_json::Value = serde_json::from_str(&st.messages[1].content).unwrap();
    assert_eq!(v["data"]["exit_code"], 0);
}

/// `background: true`：立即返回、带 PID、不管进程死活。
#[test]
fn background_command_returns_immediately_with_pid() {
    let dir = std::env::temp_dir().join(format!("neo-tool-bg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let scope = neo_tools::Scope::new(&dir);

    let t0 = std::time::Instant::now();
    let out = neo_tools::dispatch(
        &scope,
        "powershell",
        &serde_json::json!({ "command": "Start-Sleep -Seconds 3", "background": true }),
    );
    assert!(out.is_ok(), "{:?}", out.error.map(|e| e.message));
    assert!(
        t0.elapsed() < std::time::Duration::from_millis(800),
        "后台模式不该等命令结束"
    );
    assert_eq!(out.data["background"], true);
    assert!(out.data["pid"].as_u64().unwrap() > 0, "要给出 PID 才能管它");
    assert_eq!(out.data["timeout_applies"], false, "后台模式超时不适用");
    assert!(out.summary.contains("后台"));
}

/// 拒绝也要回灌：模型据此换个做法，而不是干等。
#[test]
fn denied_tool_reports_back_to_model() {
    let mut st = AppState::default();
    st.classroom_safe = false;
    st.auto_approve_tools = false;
    st.messages
        .push(ChatMessage::new(Role::Assistant, String::new()));
    st.tool_frags = vec![frag(
        "call_2",
        "powershell",
        r#"{"command":"Remove-Item -Recurse -Force build"}"#,
    )];
    st.tool_round = true;
    st.begin_tool_round();
    assert_eq!(st.awaiting_tool(), Some(1));

    st.deny_tool(1);
    assert!(st.awaiting_tool().is_none());
    assert!(st.tools_settled());
    let v: serde_json::Value = serde_json::from_str(&st.messages[1].content).unwrap();
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["kind"], "not_allowed");
    assert!(v["error"]["hint"].as_str().unwrap().contains("不要重复"));
}

/// 窗口截断不能把工具块劈开（否则服务端会因为缺配对直接 400）。
#[test]
fn history_window_keeps_tool_block_intact() {
    let mut st = AppState::default();
    for i in 0..20 {
        st.messages
            .push(ChatMessage::new(Role::User, format!("问 {i}")));
        st.messages
            .push(ChatMessage::new(Role::Assistant, format!("答 {i}")));
    }
    let mut asker = ChatMessage::new(Role::Assistant, "我查一下");
    asker.tool_calls = vec![neo_llm::ToolCall {
        id: "call_read_file".into(),
        name: "read_file".into(),
        arguments: r#"{"path":"a.txt"}"#.into(),
    }];
    st.messages.push(asker);
    st.messages.push(tool_msg(
        "read_file",
        serde_json::json!({ "path": "a.txt" }),
        neo_tools::Outcome::ok(
            "read_file",
            "读取 a.txt（1 行）",
            serde_json::json!({ "content": "x" }),
        ),
    ));

    let msgs = st.api_messages(2);
    let roles: Vec<&str> = msgs.iter().map(|m| m.role.as_str()).collect();
    assert_eq!(roles[0], "system");
    // 末尾的 assistant(tool_calls) + tool 必须成对出现
    assert_eq!(roles[roles.len() - 2], "assistant");
    assert_eq!(roles[roles.len() - 1], "tool");
    assert_eq!(
        msgs[msgs.len() - 1].tool_call_id.as_deref(),
        Some("call_read_file")
    );
}

/// 工具卡片（成功 / 失败 / 待确认三态）与权限确认弹窗的快照。
#[test]
fn tool_cards_1080p() {
    let p = shoot("14-tool-cards-1080p", Vec2::new(1920.0, 1080.0), |app| {
        app.state.theme_mode = ThemeMode::Dark;
        app.state.distance = Distance::Classroom;
        app.state.workspace = Some(".".to_owned());
        // shoot 现在从空态开始（不恢复上次会话），所以进入对话态要显式写。
        app.state.stage = Stage::Conversation;
        app.state.messages.push(ChatMessage::new(
            Role::User,
            "看一下 workspace 里的说明文件，顺便编译一下",
        ));
        app.state
            .messages
            .push(ChatMessage::new(Role::Assistant, "好，我先读文件。"));
        app.state.messages.push(tool_msg(
            "read_file",
            serde_json::json!({ "path": "README.md" }),
            neo_tools::Outcome::ok(
                "read_file",
                "读取 README.md（12 行）",
                serde_json::json!({ "lines_total": 12, "content": "…" }),
            ),
        ));
        app.state.messages.push(tool_msg(
                "edit_file",
                serde_json::json!({ "path": "src/lib.rs", "old_string": "let x = 1;" }),
                neo_tools::Outcome::fail(
                    "edit_file",
                    neo_tools::ToolError::new(
                        neo_tools::ErrorKind::NotUnique,
                        "`old_string` 在 src/lib.rs 里命中 3 处（第 [12, 40, 88] 行），无法确定改哪一处",
                    )
                    .with_hint("多带几行上下文让旧文本唯一；确定要全部替换时把 `replace_all` 设为 true"),
                ),
            ));
        app.state.messages.push(tool_msg(
            "powershell",
            serde_json::json!({ "command": "cargo test" }),
            neo_tools::Outcome::fail(
                "powershell",
                neo_tools::ToolError::not_allowed("当前处于「只读」模式：写文件与执行命令已被禁用"),
            ),
        ));
        // 非零退出码：上游那一枚红色胶囊，也是教室里最常见的失败。
        app.state.messages.push(tool_msg(
            "powershell",
            serde_json::json!({ "command": "cargo test" }),
            neo_tools::Outcome::ok(
                "powershell",
                "`cargo test` 退出码 1（2310 ms）",
                serde_json::json!({ "exit_code": 1, "duration_ms": 2310, "stderr": "1 failed" }),
            ),
        ));
        // 屏幕交互自成一类（`Variant::Screen` → 标题「屏幕」）：
        // 它既不是读文件也不是跑命令，学生要能一眼区分。
        app.state.messages.push(tool_msg(
            "screenshot",
            serde_json::json!({ "x": 0, "y": 0, "width": 1280, "height": 720 }),
            neo_tools::Outcome::ok(
                "screenshot",
                "截屏 1280×720 已保存到 screenshots/shot-1.png（1.2 MB），已把图交给模型",
                serde_json::json!({
                    "path": "screenshots/shot-1.png",
                    "region": { "x": 0, "y": 0, "width": 1280, "height": 720 },
                    "image_attached": true,
                }),
            ),
        ));
        // 类 Unix 环境的那一份：同样归到 `Bash` 变体，标题一致、摘要不同 ——
        // 学生扫一眼就能看出"这次是在用 Unix 工具集"。
        app.state.messages.push(tool_msg(
            "bash",
            serde_json::json!({ "command": "grep -rn 'TODO' crates/ | wc -l" }),
            neo_tools::Outcome::ok(
                "bash",
                "`grep -rn 'TODO' crates/ | wc -l` 执行成功（86 ms）",
                serde_json::json!({
                    "exit_code": 0, "duration_ms": 86, "stdout": "3\n", "host": "gitbash-bundled"
                }),
            ),
        ));
    });
    assert!(p.is_file());
}

/// 权限确认弹窗的快照 —— 用户点头前看到的那一屏。
#[test]
fn tool_confirm_1080p() {
    let p = shoot("15-tool-confirm-1080p", Vec2::new(1920.0, 1080.0), |app| {
        app.state.theme_mode = ThemeMode::Dark;
        app.state.distance = Distance::Classroom;
        app.state.stage = Stage::Conversation;
        app.state
            .messages
            .push(ChatMessage::new(Role::User, "把构建产物清掉再重新编译"));
        let mut asker = ChatMessage::new(Role::Assistant, String::new());
        asker.tool_calls = vec![neo_llm::ToolCall {
                id: "call_ps".into(),
                name: "powershell".into(),
                arguments: r#"{"command":"Remove-Item -Recurse -Force build; cargo build --release","cwd":"."}"#.into(),
            }];
        app.state.messages.push(asker);
        app.state.tool_frags = vec![frag(
            "call_ps",
            "powershell",
            r#"{"command":"Remove-Item -Recurse -Force build; cargo build --release","cwd":"."}"#,
        )];
        app.state.classroom_safe = false;
        app.state.tool_round = true;
        app.state.begin_tool_round();
    });
    assert!(p.is_file());
}

/// 小窗完成态：只留工具流水 + 正文（markdown/LaTeX），高度自适应最后一段。
#[test]
fn miniwin_done_1080p() {
    let p = shoot_steps(
        "30-miniwin-done-1080p",
        Vec2::new(1920.0, 1080.0),
        8,
        |app| {
            app.hidden_to_tray = true;
            app.state.theme_mode = ThemeMode::Dark;
            app.state.distance = Distance::Classroom;
            app.state
                .messages
                .push(ChatMessage::new(Role::User, "总结一下楞次定律"));
            let mut reply = ChatMessage::new(
                Role::Assistant,
                "先回顾磁通量的定义与变化方式。\n\n**结论**：感应电流的效果总是阻碍磁通量的变化，\
                 即 $E = -\\frac{d\\Phi}{dt}$；判断方向用右手定则。"
                    .to_owned(),
            );
            reply.streaming = false;
            app.state.messages.push(reply);
            // 一条已完成的工具记录：覆盖工具流水行。
            let meta = crate::state::ToolMeta {
                call_id: "call_rf".into(),
                name: "read_file".into(),
                title: "查看文件",
                risk: "read",
                preview: "查看 板书设计.md".into(),
                args: serde_json::json!({"path": "板书设计.md"}),
                state: crate::state::ToolState::Done,
                outcome: Some(neo_tools::Outcome::ok(
                    "read_file",
                    "已读取 板书设计.md",
                    serde_json::json!({"bytes": 512}),
                )),
            };
            app.state
                .messages
                .push(crate::state::ChatMessage::tool_result(meta, String::new()));
            // 首帧忙（驱动驻留排程），tick 内泵完即闲 → 进入完成驻留态。
            app.state.generating = true;
        },
    );
    assert!(p.is_file());
}

/// 长参数与多行预览不应把操作按钮挤出卡片。
#[test]
fn tool_confirm_long_args_1080p() {
    let p = shoot(
        "16-tool-confirm-long-1080p",
        Vec2::new(1920.0, 1080.0),
        |app| {
            app.state.theme_mode = ThemeMode::Dark;
            app.state.distance = Distance::Classroom;
            app.state.stage = Stage::Conversation;
            let mut message = ChatMessage::new(Role::Assistant, String::new());
            message.tool = Some(ToolMeta {
                call_id: "long-confirm".into(),
                name: "powershell".into(),
                title: "PowerShell 命令",
                risk: "exec",
                preview: "将执行一条较长的 PowerShell 命令；请仔细核对参数与工作目录，然后再决定是否允许。这个描述会自动换行。".repeat(3),
                args: serde_json::json!({"command": "Get-ChildItem -Recurse -File | Where-Object { $_.Length -gt 1000000 } | Select-Object FullName, Length".repeat(8), "cwd": "D:/projects/test", "timeout": 500}),
                state: ToolState::AwaitingConfirm,
                outcome: None,
            });
            app.state.messages.push(message);
        },
    );
    assert!(p.is_file());
}

#[test]
fn tool_confirm_scroll_light_narrow() {
    let p = shoot(
        "18-tool-confirm-scroll-light",
        Vec2::new(960.0, 720.0),
        |app| {
            app.state.theme_mode = ThemeMode::Light;
            app.state.distance = Distance::Classroom;
            app.state.stage = Stage::Conversation;
            let mut message = ChatMessage::new(Role::Assistant, String::new());
            message.tool = Some(ToolMeta {
                call_id: "scroll-confirm".into(),
                name: "write_file".into(),
                title: "写入文件",
                risk: "write",
                preview: "写入教学示例文件；请滚动核对完整参数。".into(),
                args: serde_json::json!({"path": "lesson.txt", "content": "这是一段需要完整核对而不是直接截掉的长文本。".repeat(300)}),
                state: ToolState::AwaitingConfirm,
                outcome: None,
            });
            app.state.messages.push(message);
        },
    );
    assert!(p.is_file());
}

/// 窄屏两列场景卡必须各占完整行高。
#[test]
fn hero_two_columns_portrait() {
    let p = shoot(
        "17-hero-two-columns-portrait",
        Vec2::new(960.0, 1080.0),
        |app| {
            app.state.theme_mode = ThemeMode::Dark;
            app.state.distance = Distance::Standard;
        },
    );
    assert!(p.is_file());
}

/// 设置面板（侧边导航正式版）逐页渲染冒烟：五个页签各铺三帧，
/// 任何一页把面板画炸（布局溢出、组件 id 冲突、断言触发）都会在这里炸出来。
///
/// 前身是「分段控件 Id 唯一性」回归 —— 那条线上 bug 的两个控件
/// （页签分段与「显示思考过程」分段）已随正式版改版分别被导航项与开关取代，
/// 同页撞 Id 的场景不存在了，测试职责随之改为逐页冒烟。
#[test]
fn settings_panel_renders_every_tab() {
    for &(tab, name) in crate::state::SettingsTab::ALL {
        let app = Rc::new(RefCell::new(None::<NeoApp>));
        let app2 = Rc::clone(&app);
        let mut harness = egui_kittest::Harness::builder()
            .with_size(Vec2::new(1920.0, 1080.0))
            .wgpu()
            .build_ui(move |ui| {
                let mut slot = app2.borrow_mut();
                let Some(a) = slot.as_mut() else {
                    fresh_db();
                    let mut fresh = NeoApp::install(ui.ctx());
                    fresh.state.show_settings = true;
                    fresh.state.settings_tab = tab;
                    *slot = Some(fresh);
                    ui.ctx().request_repaint();
                    return;
                };
                a.tick(ui.ctx().clone());
                a.render(ui);
            });
        harness.run_steps(3);
        // 显式收尾：释放 app（含数据库连接），下一个页签重新铺。
        drop(harness);
        let _ = name;
    }
}

#[test]
fn hero_dark_1080p() {
    let p = shoot("01-hero-dark-1080p", Vec2::new(1920.0, 1080.0), |app| {
        app.state.theme_mode = ThemeMode::Dark;
        app.state.distance = Distance::Classroom;
    });
    assert!(p.is_file());
}

#[test]
fn hero_light_1080p() {
    let p = shoot("02-hero-light-1080p", Vec2::new(1920.0, 1080.0), |app| {
        app.state.theme_mode = ThemeMode::Light;
        app.state.distance = Distance::Classroom;
    });
    assert!(p.is_file());
}

#[test]
fn hero_dark_4k_far() {
    let p = shoot("03-hero-dark-4k-far", Vec2::new(3840.0, 2160.0), |app| {
        app.state.theme_mode = ThemeMode::Dark;
        app.state.distance = Distance::Auditorium;
    });
    assert!(p.is_file());
}

#[test]
fn conversation_dark_1080p() {
    let p = shoot(
        "04-conversation-dark-1080p",
        Vec2::new(1920.0, 1080.0),
        |app| {
            app.state.theme_mode = ThemeMode::Dark;
            app.state.distance = Distance::Classroom;
            seed_dialogue(app);
        },
    );
    assert!(p.is_file());
}

#[test]
fn conversation_light_1080p() {
    let p = shoot(
        "05-conversation-light-1080p",
        Vec2::new(1920.0, 1080.0),
        |app| {
            app.state.theme_mode = ThemeMode::Light;
            app.state.distance = Distance::Classroom;
            seed_dialogue(app);
        },
    );
    assert!(p.is_file());
}

/// 生成态：脉动点 + 发送位变成停止按钮。
#[test]
fn generating_1080p() {
    let p = shoot_steps("08-generating-1080p", Vec2::new(1920.0, 1080.0), 5, |app| {
        app.state.theme_mode = ThemeMode::Dark;
        app.state.distance = Distance::Classroom;
        app.state.draft = "帮我出一道例题".to_owned();
        app.state.submit();
        let demo = crate::state::demo_reply("例题", app.state.model_display());
        app.state.start_generation(StreamSource::Demo {
            text: demo,
            cursor: 0,
        });
    });
    assert!(p.is_file());
}

/// 思考过程（`show_reasoning`）的换行与缩进检查。
///
/// 这块一直没有快照 —— 于是"换行位置不对"这种问题只能等用户肉眼发现。
#[test]
fn reasoning_wrap_1080p() {
    let p = shoot("17-reasoning-1080p", Vec2::new(1920.0, 1080.0), |app| {
        app.state.theme_mode = ThemeMode::Dark;
        app.state.distance = Distance::Classroom;
        app.state.show_reasoning = true;
        app.state.draft = "讲讲楞次定律".to_owned();
        app.state.submit();
        let text = "先看题目的已知条件。导体棒在磁场中运动，回路面积变化，\
                磁通量随之变化。\n\
                根据楞次定律，感应电流的效果总要阻碍引起它的原因，\
                所以要先判断磁通量是增加还是减少，再用右手定则确定方向。";
        app.state.start_generation(StreamSource::Demo {
            text: text.to_owned(),
            cursor: 0,
        });
        while app.state.pump() {}
        app.state.messages[1].reasoning = "第一步：判断磁场方向与回路面积的变化趋势，\
                注意导体棒的有效长度是它在垂直磁场方向上的投影。第二步：用楞次定律定方向，\
                再用安培定则定电流方向。两者不要混用。"
            .to_owned();
        app.state.messages[1].content = "感应电流的方向总是**阻碍**磁通量的变化。".to_owned();
    });
    assert!(p.is_file());
}

/// 设置 → 模型页：接口地址 / 密钥 / 当前模型 / 可选模型（含刷新键）。
#[test]
fn settings_model_1080p() {
    let p = shoot(
        "18-settings-model-1080p",
        Vec2::new(1920.0, 1080.0),
        |app| {
            app.state.theme_mode = ThemeMode::Dark;
            app.state.distance = Distance::Classroom;
            app.state.show_settings = true;
            app.state.settings_tab = crate::state::SettingsTab::Model;
            // 模拟"已从模型商拉到列表"的样子（真拉取要网络，快照里不连）。
            app.state.set_models_from_provider(vec![
                "deepseek-chat".to_owned(),
                "deepseek-reasoner".to_owned(),
                "deepseek-coder".to_owned(),
            ]);
        },
    );
    assert!(p.is_file());
}

/// 公式渲染检查：行内 `$…$`、行间 `$$…$$`、上下标、分数、根号、大运算符。
#[test]
fn math_render_1080p() {
    let p = shoot("16-math-1080p", Vec2::new(1920.0, 1080.0), |app| {
        app.state.theme_mode = ThemeMode::Dark;
        app.state.distance = Distance::Classroom;
        app.state.draft = "推导一下".to_owned();
        app.state.submit();
        // 公式里全是反斜杠，用原始字符串装 LaTeX，换行仍用普通字符串，
        // 免得陷入"到底要几个反斜杠"的泥潭。
        let rich = concat!(
            "## 匀变速直线运动\n\n",
            "位移与时间的关系是 $x = v_0 t + ",
            r"\frac",
            "{1}{2} a t^2$，两边对 $t$ 求导就得到 $v = v_0 + a t$。\n\n",
            "$$",
            r"\bar{v} = \frac{v_0 + v}{2} = \frac{x}{t}",
            "$$\n\n",
            "当加速度恒定时，$",
            r"\Delta x = aT^2",
            "$（逐差法）。\n\n",
            "由牛顿第二定律 $F = ma$ 可知，质量越大惯性越强：$m = ",
            r"\frac{F}{a}",
            "$。",
        );
        app.state.start_generation(StreamSource::Demo {
            text: rich.to_owned(),
            cursor: 0,
        });
        while app.state.pump() {}
        app.state.messages[1].meta = "DeepSeek-V3.2 · 0.8s".to_owned();
    });
    assert!(p.is_file());
}

/// 有序列表的**序号**渲染：递增、非 1 起点、两位数对齐、嵌套。
///
/// 曾经每一项都渲染成「1.」—— 这张快照就是给这类回归看的。
#[test]
fn ordered_list_numbering_1080p() {
    let p = shoot(
        "19-list-numbering-1080p",
        Vec2::new(1920.0, 1080.0),
        |app| {
            app.state.theme_mode = ThemeMode::Dark;
            app.state.distance = Distance::Classroom;
            app.state.draft = "解题步骤".to_owned();
            app.state.submit();
            let rich = "### 解题步骤\n\n\
                1. 明确研究对象\n\
                2. 受力分析\n\
                3. 列出牛顿第二定律\n\
                4. 解出加速度\n\
                5. 判断方向\n\
                6. 检查单位\n\
                7. 代入数据\n\
                8. 求位移\n\
                9. 核对量级\n\
                10. 写出答案\n\
                11. 标注条件\n\n\
                从第 3 步开始数：\n\n\
                3. 甲\n\
                4. 乙\n\
                5. 丙\n\n\
                嵌套：\n\n\
                1. 甲\n   1. 甲一\n   1. 甲二\n2. 乙";
            app.state.start_generation(StreamSource::Demo {
                text: rich.to_owned(),
                cursor: 0,
            });
            while app.state.pump() {}
            app.state.messages[1].meta = "DeepSeek-V3.2 · 0.8s".to_owned();
        },
    );
    assert!(p.is_file());
}

/// Markdown 渲染检查：标题 / 列表 / 代码块 / 引用 / 行内样式。
#[test]
fn markdown_richness_1080p() {
    let p = shoot("10-markdown-1080p", Vec2::new(1920.0, 1080.0), |app| {
        app.state.theme_mode = ThemeMode::Dark;
        app.state.distance = Distance::Classroom;
        app.state.draft = "讲讲楞次定律".to_owned();
        app.state.submit();
        let rich = "## 楞次定律\n\n\
                感应电流的效果，总是**阻碍**引起它的磁通量变化。\n\n\
                - `增反减同`：磁通量增加时反向\n\
                - 来拒去留：相对运动时阻碍\n\n\
                1. 判断原磁场方向\n\
                2. 判断磁通量增减\n\
                3. 用右手定则定感应磁场\n\n\
                > 1885 年由楞次提出。\n\n\
                ```rust\n\
                fn main() {\n    println!(\"hello\");\n}\n\
                ```\n\n\
                更多内容请看 `docs-pri/design-spec.md`。";
        app.state.start_generation(StreamSource::Demo {
            text: rich.to_owned(),
            cursor: 0,
        });
        while app.state.pump() {}
        app.state.messages[1].meta = "DeepSeek-V3.2 · 0.8s".to_owned();
    });
    assert!(p.is_file());
}

#[test]
fn commonmark_conversation_regression_snapshot() {
    let path = shoot_steps(
        "25-commonmark-conversation",
        Vec2::new(1920.0, 1080.0),
        5,
        |app| {
            app.store = None;
            app.state.theme_mode = ThemeMode::Dark;
            app.state.distance = Distance::Classroom;
            app.state.draft = "整理这些文件的内容，保留表格和公式。".to_owned();
            app.state.submit();
            let text = "## 目录与文件清单\n\n| 文件 | 类型 | 大小 | 说明 |\n| :--- | :---: | ---: | :--- |\n| `LCC_cleaned/` | 文件夹 | 空 | 目录中暂无文件 |\n| 屏幕截图 132019.png | 图片 | 1.1 MB | 目标检测结果 |\n| 演示脚本.docx | Word | 18 KB | 项目演示脚本 |\n| 课程讲义.pptx | PPT | 2.4 MB | 按幻灯片顺序读取 |\n\n## 内容概览\n\n1. **检测结果**：两瓶饮料，保留类别与置信度。\n2. **课件与脚本**：直接调用文档读取工具。\n   - Word 正文和表格\n   - PPT 按页提取文字\n3. **公式示例**：$E=mc^2$，与正文正常排版。\n\n> 读取结果带分页信息；长文档按 `next_offset` 继续。";
            app.state
                .messages
                .push(crate::state::ChatMessage::new(Role::Assistant, text));
            app.state.messages.last_mut().unwrap().meta = "Markdown 与文档工具回归".to_owned();
        },
    );
    assert!(path.is_file());
}

/// 设置面板：外观页签。
#[test]
fn settings_appearance_1080p() {
    let p = shoot(
        "11-settings-appearance-1080p",
        Vec2::new(1920.0, 1080.0),
        |app| {
            app.state.theme_mode = ThemeMode::Dark;
            app.state.distance = Distance::Classroom;
            seed_dialogue(app);
            app.state.show_settings = true;
            // 这个用例叫 appearance，就该开外观页 —— 早先这里开的是模型页，
            // 于是「外观」快照里全是模型设置（同 hero 快照那次的毛病）。
            app.state.settings_tab = crate::state::SettingsTab::Appearance;
        },
    );
    assert!(p.is_file());
}

/// 组件库陈列室：把 neo-ui 的全部控件渲到一张图上，
/// 作为设计接口的"一页速览"与回归基准（docs-pri/design-kit.md 的配图）。
#[test]
fn design_kit_gallery_1080p() {
    isolate_db();
    let app: RefCell<Option<NeoApp>> = RefCell::new(None);

    let mut harness = egui_kittest::Harness::builder()
        .with_size(Vec2::new(1920.0, 1200.0))
        .wgpu()
        .build_ui(|ui| {
            let mut slot = app.borrow_mut();
            if slot.is_none() {
                // 首帧：装配字体（画廊只依赖全局字体与主题，不需要 app 状态）。
                fresh_db();
                let fresh = NeoApp::install(ui.ctx());
                *slot = Some(fresh);
                ui.ctx().request_repaint();
                return;
            }
            design_kit_gallery(ui);
        });

    harness.run_steps(3);
    let image = harness
        .render()
        .unwrap_or_else(|e| panic!("离屏渲染 design-kit 失败: {e}"));
    let path = out_dir().join("13-design-kit-1080p.png");
    image.save(&path).expect("PNG 落盘失败");
    println!("[snapshot] → {}", path.display());
    assert!(path.is_file());
}

/// 会话管理：常规行的恒显动作按钮 + 一行删除确认态。
#[test]
fn session_actions_1080p() {
    let p = shoot(
        "12-session-actions-1080p",
        Vec2::new(1920.0, 1080.0),
        |app| {
            app.state.theme_mode = ThemeMode::Dark;
            app.state.distance = Distance::Classroom;
            let seed = |app: &mut NeoApp, title: &str| -> Option<i64> {
                let store = app.store.as_ref()?;
                let id = store.create_session(title).ok()?;
                let _ = store.append_message(id, "user", "第一条消息", "", "");
                NeoApp::refresh_sessions(&mut app.state, store);
                Some(id)
            };
            let first = seed(app, "楞次定律讲解");
            let second = seed(app, "随堂测验出题");
            // 第二个是当前会话；对第一个发起删除确认。
            app.state.active_session = second;
            app.state.confirming_delete = first;
        },
    );
    assert!(p.is_file());
}

#[test]
fn display_panel_1080p() {
    let p = shoot("06-display-panel-1080p", Vec2::new(1920.0, 1080.0), |app| {
        app.state.theme_mode = ThemeMode::Dark;
        app.state.distance = Distance::Classroom;
        app.state.show_settings = true;
        app.state.settings_tab = crate::state::SettingsTab::Display;
    });
    assert!(p.is_file());
}

#[test]
fn hero_dark_1080p_near() {
    // 近距档：验证 1x 基准值下命中区扩边仍在生效（视觉尺寸保持上游比例）。
    let p = shoot(
        "07-hero-dark-1080p-near",
        Vec2::new(1920.0, 1080.0),
        |app| {
            app.state.theme_mode = ThemeMode::Dark;
            app.state.distance = Distance::Standard;
        },
    );
    assert!(p.is_file());
}

#[test]
fn readonly_notice_1080p() {
    let p = shoot(
        "09-readonly-notice-1080p",
        Vec2::new(1920.0, 1080.0),
        |app| {
            app.state.theme_mode = ThemeMode::Dark;
            app.state.distance = Distance::Classroom;
            app.state.read_only = true;
        },
    );
    assert!(p.is_file());
}

#[test]
fn hero_is_the_default_stage() {
    // 模型列表默认为空：只由模型商提供，见 `model_list_tests`
    assert!(AppState::default().models.is_empty());
    let app_state = AppState::default();
    assert_eq!(app_state.stage, Stage::Hero);
}

// ------------------------------------------------------------------
// 设计接口陈列室（docs-pri/design-kit.md 的配图）
// ------------------------------------------------------------------

/// 把 neo-ui 的控件渲满一屏：按钮族 / 图标按钮 / Chip / 分段 / 表单 /
/// 徽标 / 反馈 / 卡片 / 列表行 / 迷你对话框。
///
/// 左列 = 动作与输入，右列 = 容器与反馈；所有色值与度量都来自
/// [`neo_theme`]，本函数不出现一个裸颜色 —— 它本身就是"只用设计接口
/// 能拼出什么"的证明。
fn design_kit_gallery(ui: &mut egui::Ui) {
    use neo_ui as nui;

    let theme = neo_theme::Theme::new(ThemeMode::Dark, 1080.0, neo_theme::Distance::Standard);
    let d = nui::Design::new(theme);
    let painter = ui.painter().clone();
    let screen = ui.max_rect();
    painter.rect_filled(screen, 0.0, d.p().bg_base);

    // 标题。
    painter.text(
        egui::pos2(screen.left() + 32.0, 44.0),
        egui::Align2::LEFT_CENTER,
        "neo-ui · 设计接口陈列室",
        d.font_bold(d.t().headline),
        d.p().label_primary,
    );
    painter.text(
        egui::pos2(screen.right() - 32.0, 44.0),
        egui::Align2::RIGHT_CENTER,
        "暗色 · 教室档 · 1080p",
        d.font_mono(d.t().caption),
        d.p().label_caption,
    );

    let lx = 32.0; // 左列起点
    let rx = 976.0; // 右列起点
    let w = 912.0; // 列宽
    let mut ly = 104.0;
    let mut ry = 104.0;

    // ---- 左列 1：按钮族 ----
    ly = section(&painter, &d, lx, ly, w, "按钮 · Button");
    let slot = w / 5.0;
    type BtnFx = fn(nui::Button<'static>) -> nui::Button<'static>;
    let variants: [(&str, BtnFx); 5] = [
        ("主操作", |b| b.primary()),
        ("次级", |b| b.elevated()),
        ("幽灵", |b| b.ghost()),
        ("危险", |b| b.danger()),
        ("反色", |b| b.contrast()),
    ];
    for (i, (label, mk)) in variants.into_iter().enumerate() {
        let r =
            egui::Rect::from_min_size(egui::pos2(lx + slot * i as f32, ly), Vec2::new(slot, 44.0));
        nui::at(ui, r, |ui| {
            mk(nui::Button::new(label)).show(ui, &d);
        });
    }
    ly += 52.0;
    // 尺寸 / 禁用 / 加载。
    let states: [(&str, BtnFx); 5] = [
        ("小号", |b| b.primary().small()),
        ("中号", |b| b.primary()),
        ("大号", |b| b.primary().large()),
        ("禁用", |b| b.primary().enabled(false)),
        ("加载中", |b| b.primary().loading(true)),
    ];
    for (i, (label, mk)) in states.into_iter().enumerate() {
        let r =
            egui::Rect::from_min_size(egui::pos2(lx + slot * i as f32, ly), Vec2::new(slot, 48.0));
        nui::at(ui, r, |ui| {
            mk(nui::Button::new(label))
                .id_salt(("gk-btn2", i))
                .show(ui, &d);
        });
    }
    ly += 64.0;

    // ---- 左列 2：图标按钮 ----
    ly = section(&painter, &d, lx, ly, w, "图标按钮 · IconButton");
    type IBtnFx = fn(nui::IconButton) -> nui::IconButton;
    let ibtns: [(nui::Icon, IBtnFx); 8] = [
        (nui::Icon::Plus, |b| b.ghost()),
        (nui::Icon::Cog, |b| b.elevated()),
        (nui::Icon::Folder, |b| b.floating()),
        (nui::Icon::Trash, |b| b.danger()),
        (nui::Icon::ArrowUp, |b| b.accent()),
        (nui::Icon::Checklist, |b| b.subtle()),
        // 主题图标成对出现：新月是双圆相减（尖角对齐交点），太阳是圆环 + 8 道光。
        (nui::Icon::Moon, |b| b.elevated()),
        (nui::Icon::Sun, |b| b.elevated()),
    ];
    for (i, (icon, mk)) in ibtns.into_iter().enumerate() {
        mk(nui::IconButton::new(icon))
            .id_salt(("gk-ibtn", i))
            .show_at(ui, &d, egui::pos2(lx + 26.0 + 52.0 * i as f32, ly + 18.0));
    }
    ly += 56.0;

    // ---- 左列 3：Chip 与分段 ----
    ly = section(&painter, &d, lx, ly, w, "胶囊与分段 · Chip / Segmented");
    let mut cx = lx;
    for (label, chevron, active) in [
        ("Plan", false, true),
        ("只读", false, false),
        ("DeepSeek-R1", true, false),
    ] {
        let cw = nui::Chip::width(&painter, &d, label, chevron);
        let r = egui::Rect::from_min_size(egui::pos2(cx, ly), Vec2::new(cw, d.m().chip_h()));
        nui::Chip::new(label)
            .chevron(chevron)
            .active(active)
            .id_salt(("gk-chip", label))
            .show_at(ui, &d, r);
        cx += cw + 12.0;
    }
    ly += d.m().chip_h() + 10.0;
    let seg = egui::Rect::from_min_size(egui::pos2(lx, ly), Vec2::new(360.0, 40.0));
    nui::at(ui, seg, |ui| {
        nui::Segmented::new(&["近距", "教室", "远距"], 1).show(ui, &d, 360.0);
    });
    ly += 56.0;

    // ---- 左列 4：表单 ----
    ly = section(
        &painter,
        &d,
        lx,
        ly,
        w,
        "表单 · TextField / Switch / FieldRow",
    );
    let mut draft = String::new();
    let field = egui::Rect::from_min_size(egui::pos2(lx, ly), Vec2::new(420.0, 40.0));
    nui::at(ui, field, |ui| {
        nui::TextField::new(&mut draft)
            .hint("输入课堂问题…")
            .id_salt("gk-field")
            .show(ui, &d, 420.0);
    });
    let mut secret = "sk-...".to_owned();
    let field2 = egui::Rect::from_min_size(egui::pos2(lx + 440.0, ly), Vec2::new(300.0, 40.0));
    nui::at(ui, field2, |ui| {
        nui::TextField::new(&mut secret)
            .secret(true)
            .id_salt("gk-secret")
            .show(ui, &d, 300.0);
    });
    ly += 52.0;
    // 开关。
    let sw = egui::Rect::from_min_size(egui::pos2(lx, ly), nui::Switch::size(&d));
    nui::Switch::new(true).id_salt("gk-sw1").show_at(ui, &d, sw);
    let sw2 = egui::Rect::from_min_size(egui::pos2(lx + 64.0, ly), nui::Switch::size(&d));
    nui::Switch::new(false)
        .id_salt("gk-sw2")
        .show_at(ui, &d, sw2);
    ly += 40.0;
    // 键值行。
    let row1 = egui::Rect::from_min_size(egui::pos2(lx, ly), Vec2::new(w, 24.0));
    nui::at(ui, row1, |ui| {
        nui::FieldRow::new("最终倍率", "1.28 ×").show(ui, &d, w);
    });
    ly += 26.0;
    let row2 = egui::Rect::from_min_size(egui::pos2(lx, ly), Vec2::new(w, 24.0));
    nui::at(ui, row2, |ui| {
        nui::FieldRow::new("数据库", r"%APPDATA%\Neo\neo.db").show(ui, &d, w);
    });
    ly += 46.0;

    // ---- 左列 5：反馈 ----
    ly = section(&painter, &d, lx, ly, w, "反馈 · Badge / Spinner");
    let mut bx = lx;
    for (text, tone) in [
        ("v0.2.0", nui::BadgeTone::Neutral),
        ("R1", nui::BadgeTone::Accent),
        ("已保存", nui::BadgeTone::Success),
        ("低电量", nui::BadgeTone::Warn),
        ("连接失败", nui::BadgeTone::Danger),
    ] {
        let b = nui::Badge::new(text).tone(tone);
        let bw = b.width(ui, &d);
        b.show_at(
            ui,
            &d,
            egui::Rect::from_min_size(egui::pos2(bx, ly + 2.0), Vec2::new(bw, 20.0)),
        );
        bx += bw + 10.0;
    }
    nui::Spinner::new().show_painter(
        &painter,
        &d,
        egui::Rect::from_min_size(egui::pos2(bx + 12.0, ly - 4.0), Vec2::splat(28.0)),
        d.p().label_tertiary,
    );
    ly += 44.0;

    // ---- 右列 2：列表行 ----
    ry = section(&painter, &d, rx, ry, w, "列表 · NavItem / ListRow / 确认条");
    let lw = 420.0;
    let nav =
        egui::Rect::from_min_size(egui::pos2(rx, ry), Vec2::new(lw, nui::NavItem::height(&d)));
    nui::at(ui, nav, |ui| {
        nui::NavItem::new("新对话", nui::Icon::Plus)
            .id_salt("gk-nav")
            .show(ui, &d, lw);
    });
    ry += nui::NavItem::height(&d) + 8.0;
    let row_h = nui::ListRow::height(&d);
    let r1 = egui::Rect::from_min_size(egui::pos2(rx, ry), Vec2::new(lw, row_h));
    nui::at(ui, r1, |ui| {
        nui::ListRow::new(1, "楞次定律讲解", "14:32")
            .active(true)
            .show_normal(ui, &d, lw);
    });
    ry += row_h + 8.0;
    let r2 = egui::Rect::from_min_size(egui::pos2(rx, ry), Vec2::new(lw, row_h));
    nui::at(ui, r2, |ui| {
        nui::ListRow::new(2, "随堂测验出题", "13:58").show_normal(ui, &d, lw);
    });
    ry += row_h + 8.0;
    let r3 = egui::Rect::from_min_size(egui::pos2(rx, ry), Vec2::new(lw, row_h));
    nui::list::confirm_row(ui, &d, r3, 3, &nui::list::ConfirmBar::new("删除这条会话？"));
    ry += row_h + 24.0;

    // ---- 右列 3：迷你对话框（Panel + 按钮的组合，即 Modal 的构成）----
    ry = section(
        &painter,
        &d,
        rx,
        ry,
        w,
        "对话框 · Modal 的构成（Panel + Button）",
    );
    let dlg_w = 420.0;
    let dlg_h = 148.0;
    let dlg = egui::Rect::from_min_size(egui::pos2(rx, ry), Vec2::new(dlg_w, dlg_h));
    let body = nui::Panel::new().paint(ui, &d, dlg, 20.0);
    painter.text(
        egui::pos2(body.left(), body.top() + 10.0),
        egui::Align2::LEFT_CENTER,
        "删除会话",
        d.font_bold(d.t().label + 2.0),
        d.p().label_primary,
    );
    painter.text(
        egui::pos2(body.left(), body.top() + 38.0),
        egui::Align2::LEFT_CENTER,
        "「楞次定律讲解」及其消息将被移除。",
        d.font(d.t().body),
        d.p().label_secondary,
    );
    let btn_y = dlg.bottom() - 56.0;
    nui::at(
        ui,
        egui::Rect::from_min_size(
            egui::pos2(dlg.right() - 96.0 - 12.0 - 88.0, btn_y),
            Vec2::new(88.0, 36.0),
        ),
        |ui| {
            nui::Button::new("取消").ghost().full_width().show(ui, &d);
        },
    );
    nui::at(
        ui,
        egui::Rect::from_min_size(egui::pos2(dlg.right() - 96.0, btn_y), Vec2::new(96.0, 36.0)),
        |ui| {
            nui::Button::new("删除").danger().full_width().show(ui, &d);
        },
    );
    ry += dlg_h + 28.0;

    // ---- 右列 4：Toast ----
    ry = section(&painter, &d, rx, ry, w, "浮动提示 · Toast");
    nui::toast_at(
        ui,
        &d,
        nui::ToastKind::Success,
        "已保存到本机数据库",
        egui::pos2(rx + 200.0, ry + 24.0),
    );
    nui::toast_at(
        ui,
        &d,
        nui::ToastKind::Error,
        "连接已中断",
        egui::pos2(rx + 200.0, ry + 76.0),
    );

    let _ = ly;
    let _ = ry;

    /// 画一个小节标题并推进游标。
    fn section(
        painter: &egui::Painter,
        d: &nui::Design,
        x: f32,
        y: f32,
        w: f32,
        title: &str,
    ) -> f32 {
        nui::section_label(
            painter,
            d,
            egui::Rect::from_min_size(egui::pos2(x, y), egui::Vec2::new(w, 18.0)),
            title,
        );
        y + 30.0
    }
}

// ------------------------------------------------------------------
// 模型列表：默认为空之后的各种现场
// ------------------------------------------------------------------

/// 启动恢复缓存但不隐式联网，即使用户已关闭安全模式。
#[test]
fn safety_startup_loads_saved_models_without_refresh() {
    isolate_db();
    fresh_db();

    {
        let store = neo_store::Store::open(&super::test_db_path()).expect("测试库");
        store.set_setting("api_base", "http://127.0.0.1:9").unwrap();
        store.set_setting("api_key", "sk-test").unwrap();
        store.set_setting("classroom_safe", "0").unwrap();
        store
            .set_setting("models", "deepseek-chat\ndeepseek-coder")
            .unwrap();
        store.set_setting("model", "deepseek-coder").unwrap();
    }

    let ctx = egui::Context::default();
    let app = NeoApp::install(&ctx);

    assert_eq!(app.state.models.len(), 2, "上次拉到的列表要读回来");
    assert_eq!(app.state.model_id(), "deepseek-coder", "选中项按 id 恢复");
    assert!(
        app.state.model_fetch.is_none(),
        "启动只恢复缓存，刷新需要用户明确授权"
    );
}

/// 配好了密钥却还没有模型列表：输入卡上方那条提示。
///
/// 这是**空列表最常见的现场**（首次启动，或上次拉取失败）——
/// 界面必须能画，而且要说清"怎么才有模型"。
#[test]
fn no_model_notice_1080p() {
    let p = shoot(
        "20-no-model-notice-1080p",
        Vec2::new(1920.0, 1080.0),
        |app| {
            app.state.theme_mode = ThemeMode::Dark;
            app.state.distance = Distance::Classroom;
            // 有密钥、没有模型 —— 正是"该去拉一次"的状态
            app.state.api_key = "sk-demo".to_owned();
        },
    );
    assert!(p.is_file());
}

/// 模型页的"还没拉到"现场：显示什么、怎么引导。
#[test]
fn settings_model_empty_1080p() {
    let p = shoot(
        "21-settings-model-empty-1080p",
        Vec2::new(1920.0, 1080.0),
        |app| {
            app.state.theme_mode = ThemeMode::Dark;
            app.state.distance = Distance::Classroom;
            app.state.show_settings = true;
            app.state.settings_tab = crate::state::SettingsTab::Model;
            // 密钥留空：hint 会引导"先填密钥再刷新"
            app.state.api_base = "https://api.deepseek.com".to_owned();
        },
    );
    assert!(p.is_file());
}

// ------------------------------------------------------------------
// 审查截图：供 docs-pri/screens 肉眼核对（与断言型审查互补）
// ------------------------------------------------------------------

/// 悬停态的会话行 / 设置开关 / 确认窗按钮，与静止态同图对比用。
/// 这是「是否有 rect 变化」的人工兜底 —— 断言型测试盯的是探针矩形，
/// 截图盯的是肉眼可见的填充 / 描边 / 位移。
#[test]
fn audit_hover_states_1080p() {
    let p = shoot(
        "audit-01-sidebar-hover-dark",
        Vec2::new(1920.0, 1080.0),
        |app| {
            app.state.theme_mode = ThemeMode::Dark;
            app.state.distance = Distance::Classroom;
            let seed = |app: &mut NeoApp, title: &str| -> Option<i64> {
                let store = app.store.as_ref()?;
                let id = store.create_session(title).ok()?;
                let _ = store.append_message(id, "user", "第一条消息", "", "");
                NeoApp::refresh_sessions(&mut app.state, store);
                Some(id)
            };
            let first = seed(app, "楞次定律讲解");
            let second = seed(app, "随堂测验出题");
            app.state.active_session = second;
            app.state.confirming_delete = first;
        },
    );
    assert!(p.is_file());

    let p = shoot(
        "audit-02-settings-light",
        Vec2::new(1920.0, 1080.0),
        |app| {
            app.state.theme_mode = ThemeMode::Light;
            app.state.distance = Distance::Classroom;
            app.state.show_settings = true;
            app.state.settings_tab = crate::state::SettingsTab::Appearance;
        },
    );
    assert!(p.is_file());

    let p = shoot("audit-03-confirm-dark", Vec2::new(1920.0, 1080.0), |app| {
        app.state.theme_mode = ThemeMode::Dark;
        app.state.distance = Distance::Classroom;
        app.state.stage = Stage::Conversation;
        let mut message = ChatMessage::new(Role::Assistant, String::new());
        message.tool = Some(ToolMeta {
            call_id: "audit-confirm".into(),
            name: "write_file".into(),
            title: "写入文件",
            risk: "write",
            preview: "写入教学示例文件 lesson.txt".into(),
            args: serde_json::json!({"path": "lesson.txt", "content": "楞次定律示例"}),
            state: ToolState::AwaitingConfirm,
            outcome: None,
        });
        app.state.messages.push(message);
    });
    assert!(p.is_file());
}
