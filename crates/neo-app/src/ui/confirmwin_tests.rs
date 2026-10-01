
    use super::*;
    use crate::state::{ChatMessage, ToolMeta, ToolState};

    fn request(state: &mut AppState, id: &str) {
        let mut meta = ToolMeta::restored("write_file · test");
        meta.call_id = id.into();
        meta.state = ToolState::AwaitingConfirm;
        state
            .messages
            .push(ChatMessage::tool_result(meta, String::new()));
    }

    #[test]
    fn confirm_callback_wakes_root_and_preserves_offscreen_lifecycle() {
        let ctx = Context::default();
        neo_theme::fonts::install(&ctx);
        ctx.set_embed_viewports(false);
        let mut state = AppState::default();
        let mut win = ConfirmWin::default();
        request(&mut state, "pending");
        let theme = Theme::new(neo_theme::ThemeMode::Dark, 1080.0, neo_theme::Distance::Standard);
        ctx.begin_pass(egui::RawInput::default());
        win.tick(&ctx, &mut state, theme, None);
        let mut output = ctx.end_pass();
        output.textures_delta.clear();
        let viewport = &output.viewport_output[&viewport_id()];
        let callback = viewport.viewport_ui_cb.as_ref().unwrap();
        let size = viewport.builder.inner_size.unwrap();
        let render = |events| {
            let mut input = egui::RawInput {
                viewport_id: viewport_id(),
                screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, size)),
                events,
                ..Default::default()
            };
            input.viewports.insert(viewport_id(), egui::ViewportInfo {
                parent: Some(ViewportId::ROOT), ..Default::default()
            });
            let mut output = ctx.run_ui(input, |ui| callback(ui));
            output.textures_delta.clear();
            output
        };
        render(Vec::new());
        let deny = ctx.data(|d| d.get_temp::<(egui::Rect, egui::Rect)>(
            egui::Id::new(("neo-confirm-button-probe", tools::Answer::Deny as u8))
        ).unwrap().0.center());
        let repaint_ids = Arc::new(std::sync::Mutex::new(Vec::new()));
        let ids = repaint_ids.clone();
        ctx.set_request_repaint_callback(move |info| ids.lock().unwrap().push(info.viewport_id));
        for _ in 0..3 {
            ctx.run_ui(egui::RawInput::default(), |_| {}).textures_delta.clear();
        }
        repaint_ids.lock().unwrap().clear();
        for suspended in [true, false] {
            ctx.data_mut(|d| d.insert_temp(egui::Id::new("neo-desktop-suspended"), suspended));
            render(vec![egui::Event::PointerMoved(deny)]);
            for pressed in [true, false] {
                let output = render(vec![egui::Event::PointerMoved(deny), egui::Event::PointerButton {
                    pos: deny, button: egui::PointerButton::Primary, pressed, modifiers: Default::default(),
                }]);
                if suspended {
                    assert!(output.shapes.iter().all(|s| matches!(s.shape, egui::Shape::Noop)));
                }
            }
            assert_eq!(win.answer.load(Ordering::Acquire), if suspended { 0 } else { 3 });
        }
        assert!(repaint_ids.lock().unwrap().contains(&ViewportId::ROOT));
        for suspended in [true, false] {
            ctx.data_mut(|d| d.insert_temp(egui::Id::new("neo-desktop-suspended"), suspended));
            ctx.begin_pass(egui::RawInput::default());
            win.tick(&ctx, &mut state, theme, None);
            let mut output = ctx.end_pass();
            output.textures_delta.clear();
            let viewport = &output.viewport_output[&viewport_id()];
            assert!(viewport.viewport_ui_cb.is_some());
            assert_eq!(viewport.builder.visible, Some(!suspended));
            assert_eq!(viewport.builder.position, Some(OFFSCREEN));
            assert_eq!(viewport.builder.inner_size, Some(Vec2::splat(1.0)));
            assert_eq!(viewport.builder.mouse_passthrough, Some(false));
            assert_eq!(viewport.builder.transparent, Some(false));
            assert_eq!(viewport.builder.active, Some(false));
            assert!(viewport.commands.iter().any(|cmd| matches!(cmd, ViewportCommand::Visible(v) if *v == !suspended)));
            assert!(!viewport.commands.iter().any(|cmd| matches!(cmd, ViewportCommand::Close | ViewportCommand::Focus)));
        }
        assert_eq!(state.messages[0].tool.as_ref().unwrap().state, ToolState::Denied);
    }

    #[test]
    fn safety_overlay_loss_reopens_fallback_and_invalidates_old_answer() {
        let ctx = Context::default();
        ctx.set_embed_viewports(false);
        let mut state = AppState::default();
        let mut win = ConfirmWin::default();
        request(&mut state, "pending");
        win.consume(&mut state);
        win.open = true;
        win.using_overlay = true;
        let old = win.answer.clone();
        old.store(1, Ordering::Release);
        let theme = Theme::new(neo_theme::ThemeMode::Dark, 1080.0, neo_theme::Distance::default());
        ctx.begin_pass(egui::RawInput::default());
        win.tick(&ctx, &mut state, theme, None);
        let mut output = ctx.end_pass();
        output.textures_delta.clear();
        assert_eq!(state.awaiting_tool(), Some(0));
        assert_eq!(old.load(Ordering::Acquire), CLOSED);
        assert!(!win.using_overlay && win.open);
        let viewport = output.viewport_output.get(&viewport_id()).unwrap();
        assert!(viewport.viewport_ui_cb.is_some());
        assert_eq!(viewport.builder.mouse_passthrough, Some(false));
        assert_eq!(viewport.builder.transparent, Some(false));
        assert_ne!(viewport.builder.position, Some(OFFSCREEN));
        assert!(!viewport.commands.iter().any(|cmd| matches!(cmd, ViewportCommand::Close)));
        assert!(old.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire).is_err());
        win.answer.store(3, Ordering::Release);
        win.consume(&mut state);
        assert_eq!(state.messages[0].tool.as_ref().unwrap().state, ToolState::Denied);
    }

    #[test]
    fn safety_batch_permission_and_consecutive_questions_are_isolated() {
        let mut state = AppState::default();
        let mut win = ConfirmWin::default();
        state.classroom_safe = false;
        request(&mut state, "permission");
        for id in ["question-1", "question-2"] {
            let mut meta = ToolMeta::restored("ask_user");
            meta.call_id = id.into();
            meta.args = serde_json::json!({"question":"choose", "options":"A|B"});
            meta.state = ToolState::AwaitingConfirm;
            state
                .messages
                .push(ChatMessage::tool_result(meta, String::new()));
        }
        request(&mut state, "permission-2");
        win.consume(&mut state);
        let permission = win.answer.clone();
        permission.store(2, Ordering::Release);
        win.consume(&mut state);
        assert!(state.auto_approve_tools);
        for index in [0, 3] {
            assert_eq!(
                state.messages[index].tool.as_ref().unwrap().state,
                ToolState::Running
            );
        }
        assert_eq!(state.awaiting_tool(), Some(1));
        assert_eq!(permission.load(Ordering::Acquire), CLOSED);
        assert!(permission
            .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire)
            .is_err());
        win.answer.store(2, Ordering::Release);
        win.consume(&mut state);
        assert_eq!(state.awaiting_tool(), Some(1));
        assert!(state.messages[1].content.is_empty());
        let first_question = win.answer.clone();
        first_question.store(10, Ordering::Release);
        win.consume(&mut state);
        assert_eq!(state.awaiting_tool(), Some(2));
        assert!(state.messages[1].content.contains('A'));
        assert!(first_question
            .compare_exchange(0, 11, Ordering::AcqRel, Ordering::Acquire)
            .is_err());
        win.consume(&mut state);
        assert_eq!(state.awaiting_tool(), Some(2));
        assert!(state.messages[2].content.is_empty());
        win.answer.store(11, Ordering::Release);
        win.consume(&mut state);
        assert_eq!(state.awaiting_tool(), None);
        assert!(state.messages[2].content.contains('B'));
    }

    #[test]
    fn safety_mode_rejects_batch_answer_from_previously_enabled_card() {
        let mut state = AppState::default();
        let mut win = ConfirmWin::default();
        state.classroom_safe = false;
        request(&mut state, "permission");
        win.consume(&mut state);
        let old = win.answer.clone();
        state.classroom_safe = true;
        old.store(2, Ordering::Release);
        win.consume(&mut state);
        assert_eq!(state.awaiting_tool(), Some(0));
        assert!(!state.auto_approve_tools);
        assert_eq!(old.load(Ordering::Acquire), CLOSED);
        win.answer.store(3, Ordering::Release);
        win.consume(&mut state);
        assert_eq!(state.awaiting_tool(), None);
        assert_eq!(state.messages[0].tool.as_ref().unwrap().state, ToolState::Denied);
    }

    #[test]
    fn safety_stale_card_cannot_approve_next_request_or_session() {
        let mut state = AppState::default();
        let mut win = ConfirmWin::default();
        request(&mut state, "same");
        win.consume(&mut state);
        let old = win.answer.clone();
        state.new_session();
        request(&mut state, "same");
        old.store(2, Ordering::Release);
        win.consume(&mut state);
        assert_eq!(state.awaiting_tool(), Some(0));
        assert!(!state.auto_approve_tools);
        assert_eq!(old.load(Ordering::Acquire), CLOSED);
        win.answer.store(1, Ordering::Release);
        win.consume(&mut state);
        assert_eq!(state.awaiting_tool(), None);
        request(&mut state, "next");
        old.store(2, Ordering::Release);
        win.consume(&mut state);
        assert_eq!(state.awaiting_tool(), Some(1));
        assert!(!state.auto_approve_tools);
    }
