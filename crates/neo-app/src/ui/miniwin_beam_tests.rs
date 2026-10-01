
    use super::*;

    fn fallback_input(root_scale: f32, child_scale: f32) -> egui::RawInput {
        let mut input = egui::RawInput::default();
        let root = input.viewports.get_mut(&ViewportId::ROOT).unwrap();
        root.native_pixels_per_point = Some(root_scale);
        root.monitor_size = Some(Vec2::new(2560.0, 1440.0) / root_scale);
        input.viewports.insert(viewport_id(), egui::ViewportInfo {
            native_pixels_per_point: Some(child_scale),
            monitor_size: Some(Vec2::new(1920.0, 1080.0) / child_scale),
            ..Default::default()
        });
        input
    }

    #[test]
    fn interrupt_all_task_kinds_require_confirmation_before_cancel() {
        use crate::state::{ChatMessage, ToolMeta};
        for kind in ["text", "read_file", "click", "drag", "round"] {
            for answer in [0, 2, 1] {
                let ctx = Context::default();
                ctx.set_embed_viewports(false);
                let mut state = AppState::default();
                state.generating = kind == "text";
                state.tool_open = !matches!(kind, "text" | "round");
                state.tool_round = kind == "round";
                if state.tool_open {
                    let mut meta = ToolMeta::restored(kind);
                    meta.state = ToolState::Running;
                    state.messages.push(ChatMessage::tool_result(meta, String::new()));
                }
                let mut mini = MiniWin::default();
                mini.session_epoch = state.session_epoch;
                mini.request_interrupt(&state, true, false, false);
                assert!(mini.interrupt_open, "{kind}");
                assert!(!state.round_cancelled);
                assert!(state.generating || state.tool_open || state.tool_round);
                mini.interrupt_result.store(answer, Ordering::Release);
                ctx.begin_pass(egui::RawInput::default());
                mini.tick(&ctx, &mut state,
                    Theme::new(neo_theme::ThemeMode::Dark, 1080.0, neo_theme::Distance::Standard),
                    true, None, None);
                let mut output = ctx.end_pass();
                output.textures_delta.clear();
                assert_eq!(state.round_cancelled, answer == 1, "{kind}: {answer}");
                assert_eq!(mini.interrupt_open, answer == 0);
                assert_eq!(state.generating || state.tool_open || state.tool_round, answer != 1);
            }
        }
    }

    #[test]
    fn interrupt_suspended_keeps_request_without_cancelling() {
        let ctx = Context::default();
        ctx.set_embed_viewports(false);
        let mut state = AppState::default();
        state.tool_open = true;
        let mut mini = MiniWin::default();
        mini.session_epoch = state.session_epoch;
        mini.request_interrupt(&state, true, false, false);
        let old = mini.interrupt_result.clone();
        assert_eq!(old.load(Ordering::Acquire), 0);
        for suspended in [true, false] {
            ctx.begin_pass(egui::RawInput::default());
            ctx.data_mut(|d| d.insert_temp(Id::new("neo-desktop-suspended"), suspended));
            mini.tick(&ctx, &mut state,
                Theme::new(neo_theme::ThemeMode::Dark, 1080.0, neo_theme::Distance::Standard),
                true, None, None);
            let mut output = ctx.end_pass();
            output.textures_delta.clear();
            assert!(mini.interrupt_open);
            assert!(!mini.shown, "确认不再复用纯视觉状态牌");
            assert_eq!(mini.interrupt_viewport.is_some(), !suspended, "只在桌面安全时展示确认");
            assert!(state.tool_open);
            assert!(!state.round_cancelled);
            assert_eq!(old.load(Ordering::Acquire), u8::MAX);
        }
        assert_eq!(mini.interrupt_result.load(Ordering::Acquire), 0);
        assert!(!Arc::ptr_eq(&old, &mini.interrupt_result));
    }

    #[test]
    fn interrupt_excludes_confirmation_question_cards_idle_and_synthetic() {
        use crate::state::{ChatMessage, ToolMeta};
        for name in ["write_file", "ask_user"] {
            let mut state = AppState::default();
            state.tool_open = true;
            let mut meta = ToolMeta::restored(name);
            meta.state = ToolState::AwaitingConfirm;
            state.messages.push(ChatMessage::tool_result(meta, String::new()));
            let mut mini = MiniWin::default();
            mini.request_interrupt(&state, true, false, false);
            assert!(!mini.interrupt_open, "{name}");
        }
        for (busy, armed, on_card, synthetic) in [
            (false, true, false, false),
            (true, false, false, false),
            (true, true, true, false), // 真实交互弹窗。
            (true, true, false, true),
        ] {
            let mut state = AppState::default();
            state.generating = busy;
            let mut mini = MiniWin::default();
            mini.request_interrupt(&state, armed, on_card, synthetic);
            assert!(!mini.interrupt_open);
            assert!(!state.round_cancelled);
        }
    }

    #[test]
    fn interrupt_sampling_keeps_click_position_and_consumes_ignored_edges() {
        let card = Rect::from_min_size(Pos2::new(600.0, 20.0), Vec2::new(640.0, 460.0));
        let mut input = GlobalInput::default();
        input.sample(Some((card.center(), true)), 2000, 0);
        input.sample(Some((Pos2::new(500.0, 900.0), false)), 2016, 0);
        assert!(card.contains(input.edge.unwrap().1));
        assert!(!card.contains(input.cursor.unwrap()));
        let mut mini = MiniWin::default();
        assert!(mini.consume_click(input, 2100));
        assert!(!mini.consume_click(input, 2110));
        input.sample(Some((Pos2::ZERO, true)), 2200, 0);
        assert!(!mini.consume_click(input, 2450));
        assert_eq!(mini.lmb_edge_consumed, 2200);
    }

    #[test]
    fn interrupt_synthetic_edge_cannot_escape_window_during_delayed_tick_or_drag() {
        let mut input = GlobalInput::default();
        input.sample(Some((Pos2::ZERO, true)), 1999, 1000);
        input.sample(Some((Pos2::ZERO, false)), 2010, 1000);
        let mut mini = MiniWin::default();
        assert!(!mini.consume_click(input, 2050));
        assert!(input.edge.is_none());
        input.sample(Some((Pos2::ZERO, true)), 2999, 2000);
        input.sample(Some((Pos2::ZERO, true)), 3100, 2000);
        assert!(input.edge.is_none(), "合成拖动不能在排除窗口过期后变成新沿");
        input.sample(Some((Pos2::ZERO, false)), 3110, 2000);
        input.sample(Some((Pos2::ZERO, true)), 3120, 2000);
        assert!(mini.consume_click(input, 3200), "窗口外的真实新点击可确认打断");
        assert!(!synthetic_click_at(1000, 0));
        assert!(synthetic_click_at(1000, 1001));
        assert!(!synthetic_click_at(2000, 1000));
    }

    #[test]
    fn interrupt_foreground_text_shows_confirmation_and_only_explicit_yes_cancels() {
        for answer in [0, 2, 1] {
            let ctx = Context::default();
            ctx.set_embed_viewports(false);
            let mut state = AppState::default();
            state.generating = true;
            let mut mini = MiniWin::default();
            mini.session_epoch = state.session_epoch;
            let mut input = GlobalInput::default();
            assert!(!mini.task_click(&state, input, 2000));
            input.sample(Some((Pos2::new(900.0, 700.0), true)), 2010, 0);
            let clicked = mini.task_click(&state, input, 2010);
            assert!(clicked);
            mini.request_interrupt(&state, clicked, false, false);
            for result in [0, answer] {
                mini.interrupt_result.store(result, Ordering::Release);
                ctx.begin_pass(egui::RawInput::default());
                mini.tick(&ctx, &mut state,
                    Theme::new(neo_theme::ThemeMode::Dark, 1080.0, neo_theme::Distance::Standard),
                    false, None, None);
                let mut output = ctx.end_pass();
                output.textures_delta.clear();
                assert!(!mini.shown, "前台不显示后台状态牌");
                assert_eq!(mini.interrupt_viewport.is_some(), result == 0);
                assert_eq!(state.round_cancelled, result == 1);
                assert_eq!(state.generating, result != 1);
            }
        }
    }

    #[test]
    fn interrupt_start_and_approval_clicks_are_consumed_before_arming() {
        use crate::state::{ChatMessage, ToolMeta};
        let mut state = AppState::default();
        let mut mini = MiniWin::default();
        let mut input = GlobalInput::default();
        input.sample(Some((Pos2::ZERO, true)), 1000, 0);
        assert!(!mini.task_click(&state, input, 1000), "闲时点击不能触发");
        state.generating = true;
        assert!(!mini.task_click(&state, input, 1010), "发送点击不能在任务开始后重放");
        assert!(!mini.task_click(&state, input, 1020));
        input.sample(Some((Pos2::ZERO, false)), 1030, 0);
        input.sample(Some((Pos2::ZERO, true)), 1040, 0);
        assert!(mini.task_click(&state, input, 1040));

        for name in ["write_file", "ask_user"] {
            let mut meta = ToolMeta::restored(name);
            meta.state = ToolState::AwaitingConfirm;
            state.messages.push(ChatMessage::tool_result(meta, String::new()));
            assert!(!mini.task_click(&state, input, 1050));
            input.sample(Some((Pos2::ZERO, false)), 1060, 0);
            input.sample(Some((Pos2::ZERO, true)), 1070, 0);
            state.messages.clear();
            assert!(!mini.task_click(&state, input, 1070), "审批/提问回答点击不能触发");
            assert!(!mini.task_click(&state, input, 1080));
        }
        // 首次采样已在任务开始后，也必须丢弃启动沿。
        let mut fresh = MiniWin::default();
        assert!(!fresh.task_click(&state, input, 1080));
    }

    #[test]
    fn interrupt_hook_real_click_during_injection_waits_for_safe_confirmation() {
        let ctx = Context::default();
        ctx.set_embed_viewports(false);
        let mut state = AppState::default();
        state.tool_open = true;
        let slot = Arc::new(mouse_hook::ClickSlot::default());
        let mut mini = MiniWin::default();
        mini.session_epoch = state.session_epoch;
        mini.mock_clicks = Some(slot.clone());
        assert_eq!(mini.poll_task_click(&state), (None, false, false));
        slot.mock_down(1, 0, 50, 50); // 模型按下不触发、不占序号。
        assert_eq!(mini.poll_task_click(&state), (None, false, false));
        slot.mock_down(0, 0, 900, 700); // 无需等注入后 1s，也无需松开模型拖动。
        slot.mock_down(1, 0, 50, 50); // 后续注入不能覆盖真实事件位置。
        assert_eq!(slot.latest().unwrap().sequence, 1);
        let theme = Theme::new(neo_theme::ThemeMode::Dark, 1080.0, neo_theme::Distance::Standard);
        for suspended in [true, false] {
            ctx.begin_pass(egui::RawInput::default());
            ctx.data_mut(|d| d.insert_temp(Id::new("neo-desktop-suspended"), suspended));
            mini.tick(&ctx, &mut state, theme, false, None, None);
            let mut output = ctx.end_pass();
            output.textures_delta.clear();
            assert!(mini.interrupt_open);
            assert!(!mini.shown);
            assert_eq!(mini.interrupt_viewport.is_some(), !suspended);
            assert!(state.tool_open);
            assert!(!state.round_cancelled);
        }
        assert_eq!(mini.physical_sequence, 1);
        let (_, repeated, synthetic) = mini.poll_task_click(&state);
        assert!(!repeated);
        assert!(!synthetic, "hook 路径不能再套用 1s 时间戳屏蔽");
    }

    #[test]
    fn interrupt_hook_sequences_exclude_send_approval_and_question_clicks() {
        use crate::state::{ChatMessage, ToolMeta};
        let mut state = AppState::default();
        let slot = Arc::new(mouse_hook::ClickSlot::default());
        let mut mini = MiniWin::default();
        mini.mock_clicks = Some(slot.clone());
        slot.mock_down(0, 0, 10, 20);
        assert!(!mini.poll_task_click(&state).1);
        state.generating = true;
        assert!(!mini.poll_task_click(&state).1, "发送沿已经消费");
        for name in ["write_file", "ask_user"] {
            let mut meta = ToolMeta::restored(name);
            meta.state = ToolState::AwaitingConfirm;
            state.messages.push(ChatMessage::tool_result(meta, String::new()));
            slot.mock_down(0, 0, 10, 20);
            assert!(!mini.poll_task_click(&state).1);
            state.messages.clear();
            slot.mock_down(0, 0, 10, 20);
            assert!(!mini.poll_task_click(&state).1, "恢复首帧消费审批/回答点击");
            assert!(!mini.poll_task_click(&state).1);
            slot.mock_down(0, 0, 10, 20);
            assert!(mini.poll_task_click(&state).1, "同毫秒/同位置的新真实点击靠序号区分");
        }
        mini.input_armed = false; // 切会话时重置监听；旧事件仍不可重放。
        assert!(!mini.poll_task_click(&state).1);
        assert!(!mini.poll_task_click(&state).1);
    }

    #[test]
    fn interrupt_hook_unavailable_uses_fallback_without_installing_in_tests() {
        let mut mini = MiniWin::default();
        assert!(mini.hook_click().is_none());
        assert!(mini.hook_attempted);
        assert!(mini.mouse_hook.is_none(), "测试构建绝不能安装真实 hook");
        assert!(mini.hook_click().is_none());
        let mut state = AppState::default();
        state.generating = true;
        let mut input = GlobalInput::default();
        assert!(!mini.task_click(&state, input, 1000));
        input.sample(Some((Pos2::ZERO, true)), 1010, 1000);
        assert!(!mini.task_click(&state, input, 1010));
        input.sample(Some((Pos2::ZERO, false)), 2010, 1000);
        input.sample(Some((Pos2::ZERO, true)), 2020, 1000);
        assert!(mini.task_click(&state, input, 2020), "fallback sampling remains available");
    }

    #[test]
    fn interrupt_hook_class_card_consumes_event_at_original_position() {
        let ctx = Context::default();
        ctx.set_embed_viewports(false);
        let mut state = AppState::default();
        state.generating = true;
        let slot = Arc::new(mouse_hook::ClickSlot::default());
        let mut mini = MiniWin::default();
        mini.session_epoch = state.session_epoch;
        mini.mock_clicks = Some(slot.clone());
        mini.poll_task_click(&state);
        slot.mock_down(0, 0, 100, 100);
        let theme = Theme::new(neo_theme::ThemeMode::Dark, 1080.0, neo_theme::Distance::Standard);
        for card in [Some(Rect::from_min_size(Pos2::ZERO, Vec2::splat(400.0))), None] {
            ctx.begin_pass(egui::RawInput::default());
            mini.tick(&ctx, &mut state, theme, false, card, None);
            let mut output = ctx.end_pass();
            output.textures_delta.clear();
            assert!(!mini.interrupt_open, "关闭课堂卡后不能重放卡片点击");
        }
        assert_eq!(mini.physical_sequence, 1);
    }

    #[test]
    fn interrupt_class_client_hit_test_stays_in_global_physical_pixels() {
        for (root_ppp, zoom) in [(1.0, 1.0), (2.0, 1.25), (1.25, 2.0)] {
            for (card, click, excluded) in [
                // 滑入中的实际物理客户区，不是屏幕上方的布局终点。
                (Some(Rect::from_min_size(Pos2::new(1200.0, -400.0), Vec2::new(960.0, 690.0))), Pos2::new(1300.0, 50.0), true),
                (Some(Rect::from_min_size(Pos2::new(1200.0, -400.0), Vec2::new(960.0, 690.0))), Pos2::new(1300.0, 400.0), false),
                (Some(Rect::from_min_size(Pos2::new(-1800.0, 40.0), Vec2::new(640.0, 460.0))), Pos2::new(-1700.0, 100.0), true),
                (None, Pos2::new(1300.0, 50.0), false), // 关闭、挂起或未创建。
            ] {
                let ctx = Context::default();
                ctx.set_embed_viewports(false);
                ctx.set_zoom_factor(zoom);
                ctx.run_ui(egui::RawInput::default(), |_| {}).textures_delta.clear();
                let mut state = AppState::default();
                state.generating = true;
                let slot = Arc::new(mouse_hook::ClickSlot::default());
                let mut mini = MiniWin::default();
                mini.session_epoch = state.session_epoch;
                mini.mock_clicks = Some(slot.clone());
                mini.poll_task_click(&state);
                slot.mock_down(0, 0, click.x as i32, click.y as i32);
                ctx.begin_pass(fallback_input(root_ppp, 1.5));
                mini.tick(&ctx, &mut state, theme(), false, card, None);
                ctx.end_pass().textures_delta.clear();
                assert_eq!(mini.interrupt_open, !excluded, "root={root_ppp}, zoom={zoom}, click={click:?}");
                assert_eq!(mini.physical_sequence, 1);
                assert!(!state.round_cancelled);
            }
        }
    }

    #[test]
    fn interrupt_real_buttons_are_bounded_and_dialog_is_opaque() {
        for height in [768.0, 1080.0, 2160.0] {
            let theme = Theme::new(neo_theme::ThemeMode::Dark, height, neo_theme::Distance::Standard);
            let builder = interrupt_builder(theme, Vec2::new(1366.0, height));
            assert_eq!(builder.transparent, Some(false));
            assert_eq!(builder.mouse_passthrough, Some(false));
            assert_eq!(builder.active, Some(false));
            let rect = Rect::from_min_size(Pos2::ZERO, builder.inner_size.unwrap());
            let ctx = Context::default();
            neo_theme::fonts::install(&ctx);
            let answer = AtomicU8::new(0);
            let mut output = ctx.run_ui(egui::RawInput { screen_rect: Some(rect), ..Default::default() }, |ui| {
                paint_interrupt(ui, theme, 1, &answer);
            });
            output.textures_delta.clear();
            let buttons = [1u8, 2].map(|value| ctx.data(|d|
                d.get_temp::<Rect>(Id::new(("neo-interrupt-button", value))).unwrap()));
            assert!(buttons.iter().all(|button| rect.contains_rect(*button)));
            assert!(!buttons[0].intersects(buttons[1]));
            assert_eq!(answer.load(Ordering::Acquire), 0);
            for open in [false, true] {
                assert_eq!(MiniWin::builder(rect.size(), open).mouse_passthrough, Some(true));
            }
        }
    }

    fn theme() -> Theme {
        Theme::new(neo_theme::ThemeMode::Dark, 1080.0, neo_theme::Distance::Standard)
    }

    fn tick_mini(ctx: &Context, mini: &mut MiniWin, state: &mut AppState) -> egui::FullOutput {
        ctx.begin_pass(egui::RawInput::default());
        mini.tick(ctx, state, theme(), false, None, None);
        let mut output = ctx.end_pass();
        output.textures_delta.clear();
        output
    }

    #[test]
    fn interrupt_callback_real_click_wakes_root_and_suspension_preserves_receipt() {
        for value in [1u8, 2] {
            let ctx = Context::default();
            neo_theme::fonts::install(&ctx);
            ctx.set_embed_viewports(false);
            let mut state = AppState::default();
            state.generating = true;
            let mut mini = MiniWin::default();
            mini.request_interrupt(&state, true, false, false);
            let output = tick_mini(&ctx, &mut mini, &mut state);
            let id = mini.interrupt_viewport.unwrap();
            let viewport = &output.viewport_output[&id];
            let callback = viewport.viewport_ui_cb.as_ref().unwrap();
            let size = viewport.builder.inner_size.unwrap();
            let repaint_ids = Arc::new(Mutex::new(Vec::new()));
            let ids = repaint_ids.clone();
            ctx.set_request_repaint_callback(move |info| ids.lock().unwrap().push(info.viewport_id));
            let render = |events| {
                let mut input = egui::RawInput {
                    viewport_id: id,
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
                    events,
                    ..Default::default()
                };
                input.viewports.insert(id, egui::ViewportInfo { parent: Some(ViewportId::ROOT), ..Default::default() });
                let mut output = ctx.run_ui(input, |ui| callback(ui));
                output.textures_delta.clear();
            };
            render(Vec::new());
            // egui 合并已挂起的立即重绘；先消费 root 初始帧，才能观察 child 的唤醒。
            for _ in 0..3 {
                ctx.run_ui(egui::RawInput::default(), |_| {}).textures_delta.clear();
            }
            repaint_ids.lock().unwrap().clear();
            let button = ctx.data(|d| d.get_temp::<Rect>(Id::new(("neo-interrupt-button", value))).unwrap());
            // 正文也属于真实不透明客户区，但不是按钮，不能提交答案。
            for position in [Pos2::new(30.0, 30.0), button.center()] {
                for pressed in [true, false] {
                    render(vec![egui::Event::PointerMoved(position), egui::Event::PointerButton {
                        pos: position, button: egui::PointerButton::Primary, pressed, modifiers: Default::default(),
                    }]);
                }
                assert_eq!(mini.interrupt_result.load(Ordering::Acquire), if position == button.center() { value } else { 0 });
            }
            assert!(repaint_ids.lock().unwrap().contains(&ViewportId::ROOT));
            ctx.data_mut(|d| d.insert_temp(Id::new("neo-desktop-suspended"), true));
            render(Vec::new()); // root 消费前的 child 暂避不能覆盖有效回执。
            assert_eq!(mini.interrupt_result.load(Ordering::Acquire), value);
            tick_mini(&ctx, &mut mini, &mut state);
            assert_eq!(state.round_cancelled, value == 1);
            assert!(!mini.interrupt_open);
        }
    }

    #[test]
    fn interrupt_suspension_replaces_viewport_and_rejects_old_callback_clicks() {
        let ctx = Context::default();
        ctx.set_embed_viewports(false);
        let mut state = AppState::default();
        state.generating = true;
        let mut mini = MiniWin::default();
        mini.request_interrupt(&state, true, false, false);
        tick_mini(&ctx, &mut mini, &mut state);
        let old_id = mini.interrupt_viewport.unwrap();
        let old = mini.interrupt_result.clone();
        ctx.data_mut(|d| d.insert_temp(Id::new("neo-desktop-suspended"), true));
        tick_mini(&ctx, &mut mini, &mut state);
        assert!(mini.interrupt_viewport.is_none());
        assert!(old.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire).is_err());
        ctx.data_mut(|d| d.insert_temp(Id::new("neo-desktop-suspended"), false));
        tick_mini(&ctx, &mut mini, &mut state);
        assert_ne!(mini.interrupt_viewport.unwrap(), old_id);
        assert!(!Arc::ptr_eq(&old, &mini.interrupt_result));
        assert_eq!(mini.interrupt_result.load(Ordering::Acquire), 0);
        assert!(!state.round_cancelled);
    }

    #[test]
    fn interrupt_task_epoch_rejects_old_receipt_and_start_click_but_tool_feedback_keeps_it() {
        use crate::state::{ChatMessage, StreamSource};
        for mode in 0..5 {
            let ctx = Context::default();
            ctx.set_embed_viewports(false);
            let mut state = AppState::default();
            state.draft = "same input".into();
            assert!(state.submit());
            state.start_generation(StreamSource::Demo { text: "reply".into(), cursor: 0 });
            let slot = Arc::new(mouse_hook::ClickSlot::default());
            let mut mini = MiniWin::default();
            mini.mock_clicks = Some(slot.clone());
            mini.poll_task_click(&state);
            mini.request_interrupt(&state, true, false, false);
            let old = mini.interrupt_result.clone();
            old.store(1, Ordering::Release);
            match mode {
                0 => {
                    state.cancel();
                    state.draft = "same input".into();
                    assert!(state.submit());
                    state.start_generation(StreamSource::Demo { text: "reply".into(), cursor: 0 });
                }
                1 => {
                    // 同消息原地重试；mini 两次 tick 之间没有观察到空闲态。
                    state.cancel();
                    let history = format!("{:?}", state.messages);
                    state.begin_task();
                    assert_eq!(format!("{:?}", state.messages), history);
                    state.start_generation(StreamSource::Demo { text: "retry".into(), cursor: 0 });
                }
                2 => state.session_epoch += 1,
                3 => {
                    state.end_stream_for_test(None);
                    state.messages.push(ChatMessage::new(Role::Tool, "feedback"));
                    state.start_generation(StreamSource::Demo { text: "continued".into(), cursor: 0 });
                }
                _ => {
                    // 同任务的历史整理不应误作新任务，身份只认代号。
                    state.messages[0].content = "edited history".into();
                    state.messages.insert(0, ChatMessage::new(Role::Assistant, "history"));
                }
            }
            slot.mock_down(0, 0, 900, 700);
            tick_mini(&ctx, &mut mini, &mut state);
            assert_eq!(state.round_cancelled, mode >= 3);
            assert!(!mini.interrupt_open);
            assert_eq!(old.load(Ordering::Acquire), u8::MAX);
            assert!(!mini.poll_task_click(&state).1, "启动点击不得重放");
        }
    }

    #[test]
    fn interrupt_task_epoch_rejects_suspended_retry_receipts_and_old_tokens() {
        use crate::state::StreamSource;
        for answer in [0u8, 1, 2] {
            let ctx = Context::default();
            ctx.set_embed_viewports(false);
            let mut state = AppState::default();
            state.draft = "unchanged".into();
            assert!(state.submit());
            state.start_generation(StreamSource::Demo { text: "reply".into(), cursor: 0 });
            let mut mini = MiniWin::default();
            mini.request_interrupt(&state, true, false, false);
            tick_mini(&ctx, &mut mini, &mut state);
            let old_id = mini.interrupt_viewport.unwrap();
            let old = mini.interrupt_result.clone();
            old.store(answer, Ordering::Release);
            ctx.data_mut(|d| d.insert_temp(Id::new("neo-desktop-suspended"), true));
            state.cancel();
            state.begin_task();
            state.start_generation(StreamSource::Demo { text: "retry".into(), cursor: 0 });
            tick_mini(&ctx, &mut mini, &mut state);
            assert!(state.generating && !state.round_cancelled);
            assert!(!mini.interrupt_open);
            assert_eq!(old.load(Ordering::Acquire), u8::MAX);
            assert!(old.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire).is_err());
            ctx.data_mut(|d| d.insert_temp(Id::new("neo-desktop-suspended"), false));
            mini.request_interrupt(&state, true, false, false);
            tick_mini(&ctx, &mut mini, &mut state);
            assert_ne!(mini.interrupt_viewport.unwrap(), old_id);
            assert!(!Arc::ptr_eq(&mini.interrupt_result, &old));
            mini.interrupt_result.store(1, Ordering::Release);
            tick_mini(&ctx, &mut mini, &mut state);
            assert!(state.round_cancelled, "新任务自己的回执仍有效");
        }
    }

    #[test]
    fn interrupt_backend_switch_preserves_valid_answer() {
        let ctx = Context::default();
        ctx.set_embed_viewports(false);
        let mut state = AppState::default();
        state.generating = true;
        let mut mini = MiniWin::default();
        mini.request_interrupt(&state, true, false, false);
        tick_mini(&ctx, &mut mini, &mut state);
        let id = mini.interrupt_viewport;
        mini.interrupt_result.store(1, Ordering::Release);
        mini.switch_backend(&ctx, true);
        assert_eq!(mini.interrupt_viewport, id);
        assert_eq!(mini.interrupt_result.load(Ordering::Acquire), 1);
        tick_mini(&ctx, &mut mini, &mut state);
        assert!(state.round_cancelled);
    }

    #[test]
    fn flash_fallback_suspension_hides_and_skips_stale_callback() {
        let ctx = Context::default();
        ctx.set_embed_viewports(false);
        let mut flash = ShotFlash::default();
        flash.shot_seen = neo_tools::tools::screen::SCREENSHOT_AT.load(Ordering::Relaxed);
        flash.flashing_since = Some(Instant::now() - Duration::from_millis(100));
        ctx.begin_pass(egui::RawInput::default());
        flash.tick(&ctx, None);
        let mut output = ctx.end_pass();
        output.textures_delta.clear();
        let callback = output.viewport_output[&flash_viewport_id()].viewport_ui_cb.clone().unwrap();
        assert!(flash.shown);
        ctx.data_mut(|d| d.insert_temp(Id::new("neo-desktop-suspended"), true));
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| callback(ui));
        output.textures_delta.clear();
        assert!(output.shapes.iter().all(|shape| matches!(shape.shape, egui::epaint::Shape::Noop)));
        ctx.begin_pass(egui::RawInput::default());
        flash.tick(&ctx, None);
        let mut output = ctx.end_pass();
        output.textures_delta.clear();
        let viewport = &output.viewport_output[&flash_viewport_id()];
        assert_eq!(viewport.builder.visible, Some(false));
        assert_eq!(viewport.builder.inner_size, Some(Vec2::splat(1.0)));
        assert_eq!(viewport.builder.position, Some(OFFSCREEN));
        assert!(!flash.shown);
    }

    #[test]
    fn flash_long_capture_keeps_pending_until_resume_then_plays_full_duration() {
        let now = Instant::now();
        let region = neo_tools::tools::screen::Rect { x: -1280, y: 40, width: 640, height: 480 };
        let mut flash = ShotFlash::default();
        assert!(!flash.advance(now, (1, Some(region)), true));
        for elapsed in [450, 900, 5000] {
            assert!(!flash.advance(now + Duration::from_millis(elapsed), (1, None), true));
            assert!(flash.flash_at.is_some());
            assert!(flash.flashing_since.is_none(), "挂起时不能消耗动画时长");
        }
        let resumed = now + Duration::from_secs(6);
        assert!(flash.advance(resumed, (1, None), false));
        assert_eq!(flash.flashing_since, Some(resumed));
        assert_eq!(flash.region, Some(region));
        assert!(flash.advance(resumed + Duration::from_millis(449), (1, None), false));
        assert!(!flash.advance(resumed + Duration::from_millis(451), (1, None), false));
        assert!(!flash.advance(resumed + Duration::from_secs(5), (1, None), false), "同一信号不重播");
    }

    #[test]
    fn flash_expired_before_non_capture_barrier_never_replays_but_new_signal_survives() {
        let now = Instant::now();
        for elapsed in [449, 450, 451, 5000] {
            for new_signal in [false, true] {
                let mut flash = ShotFlash::default();
                flash.advance(now - FLASH_DELAY, (1, None), false);
                assert!(flash.advance(now, (1, None), false));
                let generation = flash.frame_generation.load(Ordering::Acquire);
                // 无中间 tick：模拟动画到期后才收到下一个桌面租约。
                let signal = if new_signal { 2 } else { 1 };
                assert!(!flash.advance(now + Duration::from_millis(elapsed), (signal, None), true));
                assert!(flash.flashing_since.is_none());
                assert!(flash.frame_generation.load(Ordering::Acquire) > generation);
                let should_resume = elapsed < 450 || new_signal;
                assert_eq!(flash.flash_at.is_some(), should_resume);
                assert_eq!(flash.advance(now + Duration::from_secs(10), (signal, None), false), should_resume);
                assert!(!flash.advance(now + Duration::from_secs(11), (signal, None), false));
            }
        }
    }

    #[test]
    fn flash_fast_capture_and_late_signal_keep_minimum_delay() {
        let now = Instant::now();
        for suspended in [false, true] {
            let mut flash = ShotFlash::default();
            assert!(!flash.advance(now, (1, None), suspended));
            assert!(!flash.advance(now + Duration::from_millis(449), (1, None), false));
            assert!(flash.advance(now + FLASH_DELAY, (1, None), false));
            assert_eq!(flash.flashing_since, Some(now + FLASH_DELAY));
        }
        let mut idle = ShotFlash::default();
        assert!(!idle.advance(now, (0, None), true));
        assert!(!idle.advance(now + Duration::from_secs(10), (0, None), false));
    }

    #[test]
    fn flash_consecutive_capture_replaces_region_and_interrupts_previous_flash() {
        let now = Instant::now();
        let first = neo_tools::tools::screen::Rect { x: 0, y: 0, width: 1920, height: 1080 };
        let latest = neo_tools::tools::screen::Rect { x: -900, y: -100, width: 600, height: 400 };
        let mut flash = ShotFlash::default();
        flash.advance(now, (1, Some(first)), false);
        assert!(flash.advance(now + FLASH_DELAY, (1, None), false));
        let next = now + Duration::from_millis(500);
        assert!(!flash.advance(next, (2, Some(latest)), true));
        assert!(flash.flashing_since.is_none());
        assert!(!flash.advance(next + Duration::from_secs(5), (2, None), true));
        let resumed = next + Duration::from_secs(6);
        assert!(flash.advance(resumed, (2, None), false));
        assert_eq!(flash.region, Some(latest));
        assert_eq!(flash.flashing_since, Some(resumed));
        // 非截图桌面租约打断正在播放的闪光也应保留反馈，而不是隐藏着耗尽。
        assert!(!flash.advance(resumed + POLL, (2, None), true));
        assert!(flash.advance(resumed + Duration::from_secs(3), (2, None), false));
        assert_eq!(flash.flashing_since, Some(resumed + Duration::from_secs(3)));
    }

    fn assert_flash_callback_lifecycle(
        ctx: &Context,
        clock: &FlashClock,
        resumed: Instant,
        mut draw: impl FnMut(&mut egui::Ui),
    ) {
        // 只推进回调时钟，不 tick ROOT；真实 deferred/card 必须自己保持动画。
        for millis in [100, 200, 350, 400, 450, 451, 1000] {
            clock.set(resumed + Duration::from_millis(millis));
            // 消耗 egui 初始化及上一帧遗留的即时重绘，避免假阳性。
            for pass in 0..5 {
                let mut output = ctx.run_ui(egui::RawInput::default(), |ui| draw(ui));
                output.textures_delta.clear();
                if millis < 450 {
                    assert!(output.shapes.iter().any(|shape| match &shape.shape {
                        egui::epaint::Shape::Rect(rect) => rect.fill.a() > 0 || rect.stroke.color.a() > 0,
                        _ => false,
                    }), "{millis}ms 必须实际画出非透明的闪光，不能只检查 viewport builder");
                    assert!(output.viewport_output[&ViewportId::ROOT].repaint_delay <= POLL,
                        "没有 ROOT tick 时，回调仍须持续重绘");
                } else {
                    assert!(output.shapes.iter().all(|shape| matches!(shape.shape, egui::epaint::Shape::Noop)));
                    if pass == 4 {
                        assert!(output.viewport_output[&ViewportId::ROOT].repaint_delay > POLL,
                            "到期回调不应无限请求动画帧");
                    }
                }
            }
        }
    }

    #[test]
    fn flash_fallback_resume_rejects_old_frame_and_preserves_hidden_root() {
        let ctx = Context::default();
        ctx.set_embed_viewports(false);
        let mut flash = ShotFlash::default();
        let now = Instant::now();
        let region = neo_tools::tools::screen::Rect { x: 40, y: 40, width: 400, height: 300 };
        flash.advance(now, (1, Some(region)), false);
        ctx.begin_pass(egui::RawInput::default());
        flash.tick_at(&ctx, None, now + FLASH_DELAY, (1, None));
        let mut output = ctx.end_pass();
        output.textures_delta.clear();
        let old = output.viewport_output[&flash_viewport_id()].viewport_ui_cb.clone().unwrap();
        ctx.data_mut(|d| d.insert_temp(Id::new("neo-desktop-suspended"), true));
        ctx.begin_pass(egui::RawInput::default());
        flash.tick_at(&ctx, None, now + Duration::from_secs(1), (2, Some(region)));
        let mut output = ctx.end_pass();
        output.textures_delta.clear();
        assert_eq!(output.viewport_output[&flash_viewport_id()].builder.visible, Some(false));
        ctx.data_mut(|d| d.insert_temp(Id::new("neo-desktop-suspended"), false));
        ctx.begin_pass(egui::RawInput::default());
        flash.tick_at(&ctx, None, now + Duration::from_secs(5), (2, None));
        let mut output = ctx.end_pass();
        output.textures_delta.clear();
        let viewport = &output.viewport_output[&flash_viewport_id()];
        let fresh = viewport.viewport_ui_cb.clone().unwrap();
        assert_eq!(viewport.builder.visible, Some(true));
        assert!(viewport.commands.iter().any(|cmd| matches!(cmd, ViewportCommand::Visible(true))));
        assert!(viewport.builder.inner_size.is_some_and(|size| size.x > 1.0 && size.y > 1.0));
        assert!(!output.viewport_output[&ViewportId::ROOT].commands.iter().any(|cmd|
            matches!(cmd, ViewportCommand::Visible(true) | ViewportCommand::Minimized(false) | ViewportCommand::Focus)));
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| old(ui));
        output.textures_delta.clear();
        assert!(output.shapes.iter().all(|shape| matches!(shape.shape, egui::epaint::Shape::Noop)),
            "恢复屏障后旧截图回调仍必须失效");
        let resumed = now + Duration::from_secs(5);
        assert_flash_callback_lifecycle(&ctx, &flash.clock, resumed, |ui| fresh(ui));
        ctx.begin_pass(egui::RawInput::default());
        flash.tick_at(&ctx, None, resumed + Duration::from_secs(1), (2, None));
        let mut output = ctx.end_pass();
        output.textures_delta.clear();
        let viewport = &output.viewport_output[&flash_viewport_id()];
        assert_eq!(viewport.builder.inner_size, Some(Vec2::splat(1.0)));
        assert_eq!(viewport.builder.position, Some(OFFSCREEN));
        assert!(!flash.shown && flash.flash_at.is_none() && flash.flashing_since.is_none());
    }

    #[test]
    fn flash_overlay_card_does_not_replay_suspended_or_retired_frame() {
        let ctx = Context::default();
        let mut flash = ShotFlash::default();
        let now = Instant::now();
        flash.advance(now - FLASH_DELAY, (1, None), false);
        assert!(flash.advance(now, (1, None), false));
        flash.clock.set(now + Duration::from_millis(100));
        let mut card = flash.card(&ctx, [0.0, 0.0, 640.0, 480.0], flash.flashing_since.unwrap());
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| (card.draw)(ui));
        output.textures_delta.clear();
        assert!(output.shapes.iter().any(|shape| !matches!(shape.shape, egui::epaint::Shape::Noop)));
        ctx.data_mut(|d| d.insert_temp(Id::new("neo-desktop-suspended"), true));
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| (card.draw)(ui));
        output.textures_delta.clear();
        assert!(output.shapes.iter().all(|shape| matches!(shape.shape, egui::epaint::Shape::Noop)));
        assert!(!flash.advance(now + Duration::from_millis(110), (1, None), true));
        ctx.data_mut(|d| d.insert_temp(Id::new("neo-desktop-suspended"), false));
        assert!(flash.advance(now + Duration::from_secs(5), (1, None), false));
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| (card.draw)(ui));
        output.textures_delta.clear();
        assert!(output.shapes.iter().all(|shape| matches!(shape.shape, egui::epaint::Shape::Noop)));
        let resumed = now + Duration::from_secs(5);
        let mut fresh = flash.card(&ctx, [0.0, 0.0, 640.0, 480.0], flash.flashing_since.unwrap());
        assert_flash_callback_lifecycle(&ctx, &flash.clock, resumed, |ui| (fresh.draw)(ui));
        assert!(!flash.advance(resumed + Duration::from_secs(1), (1, None), false));
        assert!(flash.flash_at.is_none() && flash.flashing_since.is_none());
    }

    #[test]
    fn interrupt_hidden_preserves_answers_but_session_change_invalidates_them() {
        for mode in 0..3 {
            let ctx = Context::default();
            ctx.set_embed_viewports(false);
            let mut state = AppState::default();
            state.generating = true;
            let mut mini = MiniWin::default();
            mini.session_epoch = state.session_epoch;
            mini.request_interrupt(&state, true, false, false);
            let old = mini.interrupt_result.clone();
            old.store(1, Ordering::Release);
            if mode == 0 { mini.shot_hide_until = Some(Instant::now() + SHOT_HIDE); }
            if mode == 1 { state.session_epoch += 1; }
            ctx.begin_pass(egui::RawInput::default());
            if mode == 2 { ctx.data_mut(|d| d.insert_temp(Id::new("neo-desktop-suspended"), true)); }
            mini.tick(&ctx, &mut state, Theme::new(neo_theme::ThemeMode::Dark, 1080.0, neo_theme::Distance::Standard), mode != 0, None, None);
            let mut output = ctx.end_pass();
            output.textures_delta.clear();
            assert_eq!(old.load(Ordering::Acquire), u8::MAX);
            assert!(old.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire).is_err());
            assert_eq!(state.generating, mode == 1);
            assert_eq!(state.round_cancelled, mode != 1);
        }
    }

    #[test]
    fn interrupt_passive_actual_home_and_away_do_not_exclude_desktop_clicks() {
        for (root_scale, child_scale) in [(1.5, 1.0), (1.0, 1.5)] {
            for position in [Pos2::new(1600.0, 40.0), Pos2::new(40.0, 40.0), Pos2::new(800.0, 40.0)] {
                let ctx = Context::default();
                ctx.set_embed_viewports(false);
                let mut state = AppState::default();
                state.generating = true;
                let slot = Arc::new(mouse_hook::ClickSlot::default());
                let mut mini = MiniWin::default();
                mini.mock_clicks = Some(slot.clone());
                mini.shown = true;
                mini.poll_task_click(&state);
                *mini.layer_rect.lock().unwrap() = [position.x - 10.0, position.y - 10.0, 340.0, 96.0];
                slot.mock_down(0, 0, position.x as i32, position.y as i32);
                ctx.begin_pass(fallback_input(root_scale, child_scale));
                FallbackGeometry::new(FallbackGeometry::screen(&ctx), position / child_scale).store(&ctx);
                mini.tick(&ctx, &mut state,
                    Theme::new(neo_theme::ThemeMode::Dark, 1080.0, neo_theme::Distance::Standard), true, None, None);
                let mut output = ctx.end_pass();
                output.textures_delta.clear();
                assert!(mini.interrupt_open, "穿透卡片不能吞掉桌面点击");
                assert!(mini.interrupt_viewport.is_some());
                assert!(!state.round_cancelled);
            }
        }
    }

    #[test]
    fn fallback_scale_changes_refresh_cache_and_position_commands() {
        let ctx = Context::default();
        ctx.set_embed_viewports(false);
        let pos = Pos2::new(-1200.0, -200.0);
        for (scale, expect_move) in [(1.0, true), (1.0, false),
            (1.5, true), (1.5, false), (1.0, true)] {
            ctx.begin_pass(fallback_input(2.0, scale));
            ctx.show_viewport_deferred(viewport_id(), ViewportBuilder::default(), |_, _| {});
            let screen = FallbackGeometry::screen(&ctx);
            FallbackGeometry::new(screen, pos).store(&ctx);
            let cached = ctx.data(|d| d.get_temp::<FallbackGeometry>(Id::new("neo-miniwin-pos"))).unwrap();
            assert_eq!(cached.position, pos);
            assert_eq!(cached.screen.ppp, scale);
            let mut output = ctx.end_pass();
            output.textures_delta.clear();
            let moved = output.viewport_output.get(&viewport_id()).is_some_and(|v|
                v.commands.iter().any(|c| matches!(c, ViewportCommand::OuterPosition(p) if *p == pos)));
            assert_eq!(moved, expect_move);
        }
        // egui zoom 也属于完整 ppp，不能只记录系统 DPI。
        ctx.set_zoom_factor(1.25);
        let mut input = fallback_input(1.0, 1.5);
        input.viewports.get_mut(&viewport_id()).unwrap().monitor_size = Some(Vec2::new(1920.0, 1080.0) / 1.875);
        ctx.begin_pass(input);
        ctx.show_viewport_deferred(viewport_id(), ViewportBuilder::default(), |_, _| {});
        let screen = FallbackGeometry::screen(&ctx);
        assert_eq!(screen.ppp, 1.875);
        FallbackGeometry::new(screen, pos).store(&ctx);
        assert_eq!(ctx.data(|d| d.get_temp::<FallbackGeometry>(Id::new("neo-miniwin-pos")))
            .unwrap().screen.ppp, 1.875);
        let mut output = ctx.end_pass();
        output.textures_delta.clear();
        assert!(output.viewport_output[&viewport_id()].commands.iter()
            .any(|c| matches!(c, ViewportCommand::OuterPosition(p) if *p == pos)));
    }

    #[test]
    fn fallback_missing_child_uses_cached_or_primary_geometry_never_root() {
        let ctx = Context::default();
        let mut input = fallback_input(1.5, 1.0);
        input.viewports.remove(&viewport_id());
        ctx.begin_pass(input);
        let screen = FallbackGeometry::screen(&ctx);
        assert_eq!(screen.ppp, 1.0);
        assert_eq!(screen.monitor, Vec2::new(1920.0, 1080.0));
        let child = ScreenGeometry::from_physical(Vec2::new(1920.0, 1080.0), 1.25);
        FallbackGeometry::new(child, Pos2::new(100.0, 20.0)).store(&ctx);
        assert_eq!(FallbackGeometry::screen(&ctx).ppp, 1.25);
        let mut output = ctx.end_pass();
        output.textures_delta.clear();
    }

    #[test]
    fn mixed_dpi_overlay_geometry_uses_primary_scale_once() {
        for (primary_scale, secondary_scale) in [(1.0, 1.5), (1.5, 1.0)] {
            let primary = ScreenGeometry::from_physical(Vec2::new(1920.0, 1080.0), primary_scale);
            let secondary = ScreenGeometry::from_physical(Vec2::new(2560.0, 1440.0), secondary_scale);

            let home = Pos2::new(primary.monitor.x - 356.0, 16.0);
            let card = Rect::from_min_size(home, Vec2::new(340.0, 96.0));
            let physical = card.center() * primary_scale;
            assert!(card.contains(primary.point(physical)));
            assert_ne!(primary.point(physical), secondary.point(physical));
            assert_eq!(primary.monitor * primary_scale, Vec2::new(1920.0, 1080.0));
        }
    }

    #[test]
    fn flash_negative_origin_is_global_for_overlay_local_for_fallback() {
        let physical = neo_tools::tools::screen::Rect { x: -1200, y: -150, width: 600, height: 300 };
        let origin = Pos2::new(-1500.0, -300.0);
        for scale in [1.0, 1.5] {
            let geometry = ScreenGeometry::from_physical(Vec2::new(1920.0, 1080.0), scale);
            let overlay = geometry.rect(physical, Pos2::ZERO);
            let fallback = geometry.rect(physical, origin);
            assert_eq!(overlay.min, Pos2::new(-1200.0, -150.0) / scale);
            assert_eq!(fallback.min, Pos2::new(300.0, 150.0) / scale);
            assert_eq!(overlay.size(), Vec2::new(600.0, 300.0) / scale);
            assert_eq!(fallback.translate(geometry.point(origin).to_vec2()), overlay);
        }
    }

    #[test]
    fn backend_switch_resets_positions_but_preserves_local_beam_cache() {
        let ctx = Context::default();
        let mut input = egui::RawInput::default();
        let root = input.viewports.get_mut(&ViewportId::ROOT).unwrap();
        root.native_pixels_per_point = Some(1.5);
        root.monitor_size = Some(Vec2::new(1700.0, 960.0));
        ctx.begin_pass(input);
        let fallback = screen_geometry(&ctx, false);
        let overlay = screen_geometry(&ctx, true);
        assert_eq!(fallback.monitor, Vec2::new(1700.0, 960.0));
        assert_eq!(fallback.point(Pos2::new(150.0, 300.0)), Pos2::new(100.0, 200.0));
        assert_eq!(overlay.monitor, Vec2::new(1920.0, 1080.0));
        assert_eq!(overlay.point(Pos2::new(150.0, 300.0)), Pos2::new(150.0, 300.0));
        let mut win = MiniWin::default();
        let key = BeamKey::new(Vec2::new(340.0, 96.0), 18.0);
        let mut cache = BeamGeometry::default();
        assert!(cache.ensure(key));
        for using_overlay in [true, false, true] {
            win.shown = true;
            win.legacy_sent = key.size;
            *win.layer_rect.lock().unwrap() = [100.0, 20.0, 340.0, 96.0];
            FallbackGeometry::new(fallback, Pos2::new(100.0, 20.0)).store(&ctx);
            win.switch_backend(&ctx, using_overlay);
            assert!(!win.shown);
            assert_eq!(win.legacy_sent, Vec2::ZERO);
            assert_eq!(*win.layer_rect.lock().unwrap(), [0.0; 4]);
            assert!(ctx.data(|d| d.get_temp::<FallbackGeometry>(Id::new("neo-miniwin-pos"))).is_none());
            assert!(!cache.ensure(key));
        }
        let mut output = ctx.end_pass();
        output.textures_delta.clear();
    }

    #[test]
    fn beam_cache_invalidates_only_geometry_inputs() {
        let mut cache = BeamGeometry::default();
        let mut key = BeamKey::new(Vec2::new(340.0, 96.0), 18.0);
        assert!(cache.ensure(key));
        assert!(!cache.ensure(key));
        key.size.y += 1.0;
        assert!(cache.ensure(key));
        key.size.x += 1.0;
        assert!(cache.ensure(key));
        key.radius += 1.0;
        assert!(cache.ensure(key));
        key.segments += 1;
        assert!(cache.ensure(key));
        key.superellipse += 0.1;
        assert!(cache.ensure(key));
        assert!(!cache.ensure(key));
    }

    #[test]
    fn beam_local_geometry_matches_absolute_reference_and_wraps() {
        for size in [Vec2::new(340.0, 96.0), Vec2::new(340.0, 273.5), Vec2::new(510.0, 144.0)] {
            for radius in [0.0, 18.0, 27.0] {
                let mut cache = BeamGeometry::default();
                cache.ensure(BeamKey::new(size, radius));
                for origin in [Pos2::ZERO, Pos2::new(-1280.0, 16.0), Pos2::new(1523.25, 8.75)] {
                    let pts = neo_theme::squircle::squircle_points(
                        Rect::from_min_size(origin, size).shrink(0.5), radius,
                        neo_theme::HARNESS_SUPERELLIPSE, neo_theme::squircle::DEFAULT_SEGMENTS);
                    let mut cum = vec![0.0];
                    for i in 0..pts.len() {
                        cum.push(cum[i] + pts[i].distance(pts[(i + 1) % pts.len()]));
                    }
                    let total = *cum.last().unwrap();
                    for now in [0.0, 0.3, 1.7, 2.39999, 2.4, 23.0] {
                        let head = ((now / 2.4_f64).fract() as f32) * total;
                        let tail = total * 0.30;
                        for (i, p) in cache.tail_points(now).iter().enumerate() {
                            let s = (head - tail + tail * (i as f32 / 28.0)).rem_euclid(total);
                            let idx = match cum.binary_search_by(|c| c.partial_cmp(&s).unwrap()) {
                                Ok(i) => i,
                                Err(i) => i.saturating_sub(1),
                            }.min(pts.len() - 1);
                            let t = ((s - cum[idx]) / (cum[idx + 1] - cum[idx]).max(f32::EPSILON)).clamp(0.0, 1.0);
                            let reference = pts[idx] + (pts[(idx + 1) % pts.len()] - pts[idx]) * t;
                            assert!((*p + origin.to_vec2()).distance(reference) < 0.002);
                        }
                    }
                }
                assert!(cache.at(-cache.total * 0.25).distance(cache.at(cache.total * 0.75)) < 0.001);
            }
        }
    }

    #[test]
    fn beam_synthetic_frames_reuse_buffers_and_reduce_rebuilds() {
        const FRAMES: usize = 6000;
        let key = BeamKey::new(Vec2::new(340.0, 180.0), 18.0);
        let start = Instant::now();
        for frame in 0..FRAMES {
            let mut geometry = BeamGeometry::default();
            geometry.ensure(std::hint::black_box(key));
            let head = ((frame as f64 / 60.0 / 2.4).fract() as f32) * geometry.total;
            let tail = geometry.total * 0.30;
            for i in 0..28 {
                std::hint::black_box(geometry.at(head - tail + tail * (i as f32 / 28.0)));
                std::hint::black_box(geometry.at(head - tail + tail * ((i + 1) as f32 / 28.0)));
            }
        }
        let uncached = start.elapsed();
        let start = Instant::now();
        let mut cache = BeamGeometry::default();
        let mut rebuilds = usize::from(cache.ensure(key));
        let buffers = (cache.points.as_ptr(), cache.cumulative.as_ptr());
        let capacities = (cache.points.capacity(), cache.cumulative.capacity());
        for frame in 0..FRAMES {
            // 模拟避让平移，局部尺寸不变。
            let rect = Rect::from_min_size(Pos2::new(frame as f32 * 0.25, 16.0), key.size);
            rebuilds += usize::from(cache.ensure(BeamKey::new(rect.size(), key.radius)));
            let points = cache.tail_points(std::hint::black_box(frame as f64 / 60.0));
            std::hint::black_box(points.map(|p| p + rect.min.to_vec2()));
            assert_eq!((cache.points.as_ptr(), cache.cumulative.as_ptr()), buffers);
            assert_eq!((cache.points.capacity(), cache.cumulative.capacity()), capacities);
        }
        assert_eq!(rebuilds, 1);
        eprintln!("{FRAMES} 合成帧：几何重建 {FRAMES} -> {rebuilds}，端点查找 {} -> {}；缓存命中不分配几何 Vec；未缓存 {:?}，缓存 {:?}（仅本机合成 CPU 路径）",
            FRAMES * 56, FRAMES * 29, uncached, start.elapsed());
    }
