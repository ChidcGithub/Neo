
    use super::*;

    fn geometry_input(rect: Option<Rect>, native_ppp: Option<f32>, root_ppp: f32) -> egui::RawInput {
        let mut input = egui::RawInput::default();
        input.viewports.get_mut(&ViewportId::ROOT).unwrap().native_pixels_per_point = Some(root_ppp);
        input.viewports.insert(viewport_id(), egui::ViewportInfo {
            inner_rect: rect,
            native_pixels_per_point: native_ppp,
            ..Default::default()
        });
        input
    }

    #[test]
    fn visible_client_rect_uses_child_dpi_zoom_and_reported_animation_position() {
        for (root_ppp, child_ppp, zoom) in [(1.0, 2.0, 1.0), (2.0, 1.0, 1.25), (1.25, 1.5, 2.0)] {
            let ctx = Context::default();
            ctx.set_zoom_factor(zoom);
            ctx.run_ui(egui::RawInput::default(), |_| {}).textures_delta.clear();
            let win = ClassWin { open: true, ..Default::default() };
            // 原生采样从屏外滑入；包括副屏负坐标，不得换成布局终点。
            for y in [-450.0, -200.0, 20.0] {
                let rect = Rect::from_min_size(Pos2::new(-900.0, y), Vec2::new(640.0, 460.0));
                ctx.begin_pass(geometry_input(Some(rect), Some(child_ppp), root_ppp));
                let expected = Rect::from_min_max(rect.min * (child_ppp * zoom), rect.max * (child_ppp * zoom));
                assert_eq!(win.visible_rect_physical(&ctx), Some(expected));
                // 待执行命令不是原生已到达的位置。
                ctx.send_viewport_cmd_to(viewport_id(), ViewportCommand::OuterPosition(Pos2::new(100.0, 20.0)));
                assert_eq!(win.visible_rect_physical(&ctx), Some(expected));
                ctx.end_pass().textures_delta.clear();
            }
        }
    }

    #[test]
    fn visible_client_rect_rejects_suspended_closed_uncreated_and_dormant_viewports() {
        let ctx = Context::default();
        ctx.set_embed_viewports(false);
        let mut win = ClassWin { open: true, ..Default::default() };
        let rect = Rect::from_min_size(Pos2::new(600.0, 20.0), Vec2::new(640.0, 460.0));
        ctx.begin_pass(egui::RawInput::default());
        assert_eq!(win.visible_rect_physical(&ctx), None, "未创建不得用 target_rect 兜底");
        ctx.end_pass().textures_delta.clear();
        for (rect, ppp) in [
            (None, Some(1.0)), (Some(rect), None), (Some(rect), Some(0.0)),
            (Some(rect), Some(f32::NAN)),
            (Some(Rect::from_min_size(OFFSCREEN, Vec2::splat(1.0))), Some(2.0)),
        ] {
            ctx.begin_pass(geometry_input(rect, ppp, 1.0));
            assert_eq!(win.visible_rect_physical(&ctx), None);
            ctx.end_pass().textures_delta.clear();
        }
        for suspended in [false, true, false] {
            ctx.begin_pass(geometry_input(Some(rect), Some(1.0), 2.0));
            ctx.data_mut(|d| d.insert_temp(egui::Id::new("neo-desktop-suspended"), suspended));
            assert_eq!(win.visible_rect_physical(&ctx), (!suspended).then_some(rect));
            ctx.end_pass().textures_delta.clear();
        }
        for minimized in [true, false] {
            let mut input = geometry_input(Some(rect), Some(1.0), 2.0);
            let viewport = input.viewports.get_mut(&viewport_id()).unwrap();
            if minimized { viewport.minimized = Some(true); } else { viewport.occluded = Some(true); }
            ctx.begin_pass(input);
            assert_eq!(win.visible_rect_physical(&ctx), None);
            ctx.end_pass().textures_delta.clear();
        }
        ctx.begin_pass(geometry_input(Some(rect), Some(1.0), 2.0));
        let mut monitor = ClassMonitor::default();
        let theme = Theme::new(neo_theme::ThemeMode::Dark, 1080.0, neo_theme::Distance::Standard);
        win.tick(&ctx, &mut monitor, theme, None);
        assert_eq!(win.visible_rect_physical(&ctx), None, "关闭沿立即失效，即使原生几何仍旧");
        ctx.end_pass().textures_delta.clear();
    }

    #[test]
    fn summary_uses_interactive_viewport_with_desktop_suspend() {
        let ctx = Context::default();
        ctx.set_embed_viewports(false);
        let mut monitor = ClassMonitor::default();
        monitor.seed_pending_summaries();
        let mut win = ClassWin::default();
        let theme = Theme::new(neo_theme::ThemeMode::Dark, 1080.0, neo_theme::Distance::Standard);
        for (frame, suspended) in [false, true, false].into_iter().enumerate() {
            ctx.begin_pass(egui::RawInput::default());
            ctx.data_mut(|d| d.insert_temp(egui::Id::new("neo-desktop-suspended"), suspended));
            win.tick(&ctx, &mut monitor, theme, None);
            let mut output = ctx.end_pass();
            output.textures_delta.clear();
            let viewport = &output.viewport_output[&viewport_id()];
            assert!(viewport.viewport_ui_cb.is_some());
            assert_eq!(viewport.builder.mouse_passthrough, Some(false));
            assert_eq!(viewport.builder.transparent, Some(false));
            assert_eq!(viewport.builder.active, Some(false));
            assert_eq!(viewport.builder.visible, Some(!suspended));
            if frame > 0 {
                assert!(viewport.commands.iter().any(|cmd| matches!(cmd, ViewportCommand::Visible(v) if *v == !suspended)));
            }
            assert!(!viewport.commands.iter().any(|cmd| matches!(cmd, ViewportCommand::Close | ViewportCommand::Focus)));
        }
    }

    #[test]
    fn summary_callback_finishes_slide_and_wakes_root_for_both_receipts() {
        for retry in [false, true] {
            let ctx = Context::default();
            neo_theme::fonts::install(&ctx);
            ctx.set_embed_viewports(false);
            let mut monitor = ClassMonitor::default();
            monitor.seed_pending_summaries();
            let mut win = ClassWin::default();
            let theme = Theme::new(neo_theme::ThemeMode::Dark, 1080.0, neo_theme::Distance::Standard);
            ctx.begin_pass(egui::RawInput::default());
            win.tick(&ctx, &mut monitor, theme, None);
            let mut output = ctx.end_pass();
            output.textures_delta.clear();
            let viewport = &output.viewport_output[&viewport_id()];
            let callback = viewport.viewport_ui_cb.as_ref().unwrap();
            let size = viewport.builder.inner_size.unwrap();
            let target = viewport.builder.position.unwrap();
            let render = |events| {
                let mut input = egui::RawInput {
                    viewport_id: viewport_id(),
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
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
            // 模拟回调停顿跨过动画终点，挂起期间不能推进或绘制。
            *win.open_since.lock().unwrap() = Some(Instant::now() - Duration::from_secs(1));
            ctx.data_mut(|d| d.insert_temp(egui::Id::new("neo-desktop-suspended"), true));
            let output = render(Vec::new());
            assert!(output.shapes.iter().all(|s| matches!(s.shape, egui::Shape::Noop)));
            assert!(win.open_since.lock().unwrap().is_some());
            assert!(!win.close_wanted.load(Ordering::Relaxed));
            assert!(!win.retry_wanted.load(Ordering::Relaxed));
            ctx.data_mut(|d| d.insert_temp(egui::Id::new("neo-desktop-suspended"), false));
            let output = render(Vec::new());
            assert!(output.viewport_output[&viewport_id()].commands.iter().any(|cmd| {
                matches!(cmd, ViewportCommand::OuterPosition(pos) if *pos == target)
            }));
            assert!(win.open_since.lock().unwrap().is_none());
            let button = if retry {
                output.shapes.iter().find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.job.text == "重试保存 / 索引" =>
                        Some(text.galley.rect.translate(text.pos.to_vec2()).center()),
                    _ => None,
                }).expect("重试按钮")
            } else {
                ctx.data(|d| d.get_temp::<Rect>(egui::Id::new("neo-classwin-close-probe")).unwrap().center())
            };
            let output = render(Vec::new());
            assert!(!output.viewport_output[&viewport_id()].commands.iter().any(|cmd| {
                matches!(cmd, ViewportCommand::OuterPosition(_))
            }));
            let repaint_ids = Arc::new(Mutex::new(Vec::new()));
            let ids = repaint_ids.clone();
            ctx.set_request_repaint_callback(move |info| ids.lock().unwrap().push(info.viewport_id));
            for _ in 0..3 {
                ctx.run_ui(egui::RawInput::default(), |_| {}).textures_delta.clear();
            }
            render(vec![egui::Event::PointerMoved(button)]);
            repaint_ids.lock().unwrap().clear();
            for pressed in [true, false] {
                render(vec![egui::Event::PointerMoved(button), egui::Event::PointerButton {
                    pos: button, button: egui::PointerButton::Primary, pressed, modifiers: Default::default(),
                }]);
            }
            assert_eq!(win.retry_wanted.load(Ordering::Relaxed), retry);
            assert_eq!(win.close_wanted.load(Ordering::Relaxed), !retry);
            assert!(repaint_ids.lock().unwrap().contains(&ViewportId::ROOT));
            ctx.data_mut(|d| d.insert_temp(egui::Id::new("neo-desktop-suspended"), true));
            render(Vec::new());
            assert_eq!(win.retry_wanted.load(Ordering::Relaxed), retry);
            assert_eq!(win.close_wanted.load(Ordering::Relaxed), !retry);
        }
    }

    #[test]
    fn summary_closed_viewport_stays_registered_offscreen() {
        let ctx = Context::default();
        ctx.set_embed_viewports(false);
        let mut win = ClassWin::default();
        let mut monitor = ClassMonitor::default();
        let theme = Theme::new(neo_theme::ThemeMode::Dark, 1080.0, neo_theme::Distance::Standard);
        for _ in 0..2 {
            ctx.begin_pass(egui::RawInput::default());
            win.tick(&ctx, &mut monitor, theme, None);
            let mut output = ctx.end_pass();
            output.textures_delta.clear();
            let viewport = &output.viewport_output[&viewport_id()];
            assert!(viewport.viewport_ui_cb.is_some());
            assert_eq!(viewport.builder.visible, Some(true));
            assert_eq!(viewport.builder.position, Some(OFFSCREEN));
            assert_eq!(viewport.builder.inner_size, Some(Vec2::splat(1.0)));
            assert!(!viewport.commands.iter().any(|cmd| matches!(cmd, ViewportCommand::Close | ViewportCommand::Visible(false))));
        }
    }

    #[test]
    fn failed_close_keeps_card_and_renders_explicit_retention() {
        let ctx = Context::default();
        neo_theme::fonts::install(&ctx);
        let theme = Theme::new(neo_theme::ThemeMode::Dark, 1080.0, neo_theme::Distance::Standard);
        theme.apply(&ctx);
        let whale = WhaleMark::load(&ctx);
        let skin = Skin::new(theme, &whale);
        let mut monitor = ClassMonitor::default();
        monitor.seed_pending_summaries();
        monitor.dismiss();
        let ready = monitor.presenting().unwrap();
        assert!(ready.close_blocked);
        assert_eq!(monitor.pending_saves(), 4);
        for state in [SaveState::Unsaved, SaveState::IndexFailed] {
            let status = save_status(true, state, ready.close_blocked);
            let mut output = ctx.run_ui(egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(640.0, 460.0))),
                ..Default::default()
            }, |ui| {
                paint(ui, &skin, "数学", "保留正文", false, "2026-09-28", &status, true, &AtomicBool::new(false));
            });
            output.textures_delta.clear();
            for needle in ["已保留卡片，未关闭", "重试保存 / 索引"] {
                assert!(output.shapes.iter().any(|shape| {
                    matches!(&shape.shape, egui::Shape::Text(text) if text.galley.job.text.contains(needle))
                }), "缺少可见提示：{needle}");
            }
        }
    }
