
    use super::*;

    pub fn context() -> egui::Context {
        let ctx = egui::Context::default();
        let mut fonts = egui::FontDefinitions::default();
        let proportional = fonts.families[&egui::FontFamily::Proportional].clone();
        fonts.families.insert(neo_theme::fonts::bold(), proportional.clone());
        fonts.families.insert(neo_theme::fonts::mono(), proportional);
        ctx.set_fonts(fonts);
        ctx
    }

    pub fn frame(
        ctx: &egui::Context,
        size: Vec2,
        events: Vec<egui::Event>,
        draw: impl FnMut(&mut Ui, &Skin<'_>),
    ) -> egui::FullOutput {
        frame_themed(ctx, size, events, neo_theme::ThemeMode::Light, draw)
    }

    pub fn frame_themed(
        ctx: &egui::Context,
        size: Vec2,
        events: Vec<egui::Event>,
        mode: neo_theme::ThemeMode,
        mut draw: impl FnMut(&mut Ui, &Skin<'_>),
    ) -> egui::FullOutput {
        let theme = neo_theme::Theme::new(mode, 1080.0, neo_theme::Distance::Standard);
        theme.apply(ctx);
        let whale = crate::brand::WhaleMark::cached(ctx);
        let mut output = ctx.run_ui(egui::RawInput {
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, size)),
            events,
            ..Default::default()
        }, |ui| draw(ui, &Skin::new(theme, &whale)));
        output.textures_delta.clear();
        output
    }

    pub fn pointer(pos: egui::Pos2, pressed: bool) -> Vec<egui::Event> {
        vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton {
            pos, button: egui::PointerButton::Primary, pressed,
            modifiers: egui::Modifiers::NONE,
        }]
    }

    pub fn probe<T: Clone + Send + Sync + 'static>(ctx: &egui::Context, key: impl std::hash::Hash + std::fmt::Debug) -> T {
        ctx.data(|data| data.get_temp::<T>(egui::Id::new(key)).unwrap())
    }

    #[test]
    fn scaled_composer_keyboard_respects_disabled_send_and_tool_stop() {
        for mode in [neo_theme::ThemeMode::Light, neo_theme::ThemeMode::Dark] {
            for scale in [0.85, 1.0, 1.75, 2.8] {
                for width in [240.0, 320.0, 560.0] {
                    let ctx = context();
                    let theme = neo_theme::Theme::from_metrics(mode, neo_theme::Metrics::from_scale(scale));
                    theme.apply(&ctx);
                    let whale = crate::brand::WhaleMark::cached(&ctx);
                    let skin = Skin::new(theme, &whale);
                    let mut state = AppState::default();
                    let mut outcome = Outcome::default();
                    for busy in [false, true] {
                        state.tool_open = busy;
                        for step in 0..4 {
                            if step == 3 { ctx.memory_mut(|m| m.request_focus(neo_ui::hash_id("neo-composer-send"))); }
                            let mut output = ctx.run_ui(egui::RawInput {
                                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(width, 1600.0))),
                                events: if step == 3 { vec![egui::Event::Key { key: egui::Key::Enter,
                                    physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }] } else { vec![] },
                                ..Default::default()
                            }, |ui| {
                                let rect = Rect::from_min_size(egui::pos2(8.0, 8.0),
                                    Vec2::new(width - 16.0, block_height(ui, &skin, &state, width - 16.0, false)));
                                outcome = draw(ui, &skin, rect, &mut state, false);
                            });
                            output.textures_delta.clear();
                            let (send, clip): (Rect, Rect) = probe(&ctx, "neo-composer-send-probe");
                            assert!(clip.contains_rect(send));
                            assert!(send.left() >= 8.0 && send.right() <= width - 8.0);
                        }
                        assert!(!outcome.send, "空草稿不得通过键盘发送");
                        assert_eq!(outcome.stop, busy, "工具审批期间停止键须可用 {width}/{scale}");
                        assert!(state.draft.is_empty());
                    }
                }
            }
        }
    }

    #[test]
    fn long_model_keeps_send_and_stop_visible_and_clickable() {
        for mode in [neo_theme::ThemeMode::Light, neo_theme::ThemeMode::Dark] {
        let frame = |ctx: &egui::Context, size, events, draw: &mut dyn FnMut(&mut Ui, &Skin<'_>)| {
            frame_themed(ctx, size, events, mode, draw)
        };
        for width in [240.0, 320.0, 480.0] {
            for busy in 0..4 {
                let ctx = context();
                let mut state = AppState::default();
                state.draft = "ready".into();
                state.models = vec![crate::state::ModelDef::new("long-model-name-".repeat(80), "test", false)];
                state.generating = busy == 1;
                state.tool_open = busy == 2;
                state.tool_round = busy == 3;
                let mut outcome = Outcome::default();
                let mut render = |ui: &mut Ui, skin: &Skin<'_>| {
                    let rect = Rect::from_min_size(egui::pos2(8.0, 8.0),
                        Vec2::new(width - 16.0, block_height(ui, skin, &state, width - 16.0, false)));
                    outcome = draw(ui, skin, rect, &mut state, false);
                };
                for _ in 0..3 {
                    frame(&ctx, Vec2::new(width, 400.0), vec![], &mut render);
                }
                let output = frame(&ctx, Vec2::new(width, 400.0), vec![], &mut render);
                let (send, clip): (Rect, Rect) = probe(&ctx, "neo-composer-send-probe");
                assert!(clip.contains_rect(send), "width={width}, {send:?}, {clip:?}");
                assert!(send.right() <= width - 8.0);
                if width == 480.0 {
                    let (chip, chip_clip): (Rect, Rect) = probe(&ctx, "neo-composer-model-probe");
                    assert!(chip.contains_rect(chip_clip));
                    assert!(chip.right() < send.left(), "chip={chip:?}, send={send:?}");
                    assert!(output.shapes.iter().any(|clipped| {
                        if let egui::Shape::Text(text) = &clipped.shape {
                            text.galley.job.text.starts_with("long-model")
                                && chip.contains_rect(clipped.clip_rect)
                        } else { false }
                    }), "model text paint must remain clipped to its Chip::show_at rect");
                }
                frame(&ctx, Vec2::new(width, 400.0), pointer(send.center(), true), &mut render);
                frame(&ctx, Vec2::new(width, 400.0), pointer(send.center(), false), &mut render);
                assert_eq!(outcome.send, busy == 0, "width={width}, busy={busy}");
                assert_eq!(outcome.stop, busy != 0, "width={width}, busy={busy}");
            }
        }
        }
    }
