
    use super::*;

    fn theme() -> Theme {
        Theme::new(ThemeMode::Dark, 1080.0, neo_theme::Distance::Standard)
    }

    #[test]
    fn toast_deadlines_expire_without_renewal_or_main_window() {
        let now = Instant::now();
        let mut queue = Queue::default();
        let mut pending = vec![
            (ToastKind::Info, "old".into(), now),
            (
                ToastKind::Success,
                "live".into(),
                now + Duration::from_secs(2),
            ),
        ];
        assert!(queue.advance(&mut pending, now));
        assert!(pending.is_empty());
        assert_eq!(queue.entries.len(), 1);
        for _ in 0..20 {
            assert!(!queue.advance(&mut pending, now + Duration::from_secs(1)));
            assert_eq!(queue.entries[0].2, now + Duration::from_secs(2));
        }
        assert!(queue.advance(&mut pending, now + Duration::from_secs(2)));
        assert!(queue.entries.is_empty());
    }

    #[test]
    fn toast_burst_is_bounded_and_unicode_safe() {
        let now = Instant::now();
        let mut queue = Queue::default();
        let mut pending = (0..100)
            .map(|i| {
                (
                    ToastKind::Info,
                    format!("{i}:{}", "长😀".repeat(1000)),
                    now + Duration::from_secs(2),
                )
            })
            .collect();
        queue.advance(&mut pending, now);
        assert_eq!(queue.entries.len(), MAX_TOASTS);
        assert!(queue.entries[0].1.starts_with("97:"));
        assert!(queue.entries[2].1.starts_with("99:"));
        assert!(queue
            .entries
            .iter()
            .all(|(_, text, _)| text.chars().count() == MAX_CHARS && text.ends_with('…')));
        assert!(pending.is_empty() && pending.capacity() <= MAX_TOASTS * 4);
    }

    #[test]
    fn toast_snapshot_only_changes_for_content_or_appearance() {
        let ctx = Context::default();
        let now = Instant::now();
        let screen = primary_screen(&ctx);
        let mut win = ToastWin::default();
        let mut pending = vec![(ToastKind::Info, "live".into(), now + Duration::from_secs(2))];
        assert!(win.update(&mut pending, theme(), screen, now));
        let before = win.snapshot.clone().unwrap();
        assert!(!win.update(&mut pending, theme(), screen, now));
        assert!(Arc::ptr_eq(&before, win.snapshot.as_ref().unwrap()));
        let mut light = theme();
        light.mode = ThemeMode::Light;
        assert!(win.update(&mut pending, light, screen, now));
        assert_eq!(win.queue.entries[0].2, before.entries[0].2);
        win.update(&mut pending, light, screen, now + Duration::from_secs(2));
        assert!(win.snapshot.is_none());
    }

    #[test]
    fn toast_target_geometry_is_not_virtual_desktop_center() {
        let primary = Rect::from_min_size(Pos2::ZERO, Vec2::new(1920.0, 1080.0));
        let virtual_desktop = Rect::from_min_max(Pos2::new(-1280.0, -200.0), primary.max);
        let rect = stack_rect(primary, theme(), 3);
        assert!(primary.contains_rect(rect));
        assert_eq!(rect.center().x, 960.0);
        assert_ne!(rect.center().x, virtual_desktop.center().x);
        let moved = primary.translate(Vec2::new(200.0, 100.0));
        assert_eq!(
            stack_rect(moved, theme(), 3),
            rect.translate(Vec2::new(200.0, 100.0))
        );
    }

    #[test]
    fn toast_child_expiry_wakes_root_and_suspension_skips_callback() {
        for all_expired in [false, true] {
            let ctx = Context::default();
            ctx.set_embed_viewports(false);
            let mut win = ToastWin::default();
            let now = Instant::now();
            let rect = stack_rect(primary_screen(&ctx), theme(), 2);
            let snapshot = Arc::new(Snapshot {
                entries: vec![
                    (ToastKind::Info, "expired".into(), now - Duration::from_secs(1)),
                    (ToastKind::Info, "other".into(), if all_expired { now } else { now + Duration::from_secs(60) }),
                ],
                theme: theme(),
                rect,
            });
            ctx.begin_pass(egui::RawInput::default());
            win.register_fallback(&ctx, Some(snapshot.clone()));
            let mut output = ctx.end_pass();
            output.textures_delta.clear();
            let callback = output.viewport_output[&viewport_id()].viewport_ui_cb.as_ref().unwrap();
            let render = || {
                let mut input = egui::RawInput {
                    viewport_id: viewport_id(),
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, rect.size())),
                    ..Default::default()
                };
                input.viewports.insert(viewport_id(), egui::ViewportInfo {
                    parent: Some(ViewportId::ROOT), ..Default::default()
                });
                let mut output = ctx.run_ui(input, |ui| callback(ui));
                output.textures_delta.clear();
                output
            };
            let repaint_ids = Arc::new(std::sync::Mutex::new(Vec::new()));
            let ids = repaint_ids.clone();
            ctx.set_request_repaint_callback(move |info| ids.lock().unwrap().push(info.viewport_id));
            for _ in 0..3 {
                ctx.run_ui(egui::RawInput::default(), |_| {}).textures_delta.clear();
            }
            repaint_ids.lock().unwrap().clear();
            ctx.data_mut(|d| d.insert_temp(egui::Id::new("neo-desktop-suspended"), true));
            let output = render();
            assert!(output.shapes.iter().all(|s| matches!(s.shape, egui::Shape::Noop)));
            assert!(!repaint_ids.lock().unwrap().contains(&ViewportId::ROOT));
            ctx.data_mut(|d| d.insert_temp(egui::Id::new("neo-desktop-suspended"), false));
            let output = render();
            assert!(repaint_ids.lock().unwrap().contains(&ViewportId::ROOT));
            assert_eq!(output.viewport_output[&viewport_id()].commands.iter().any(|cmd| {
                matches!(cmd, ViewportCommand::OuterPosition(pos) if *pos == OFFSCREEN)
            }), all_expired);
            for suspended in [true, false] {
                ctx.data_mut(|d| d.insert_temp(egui::Id::new("neo-desktop-suspended"), suspended));
                ctx.begin_pass(egui::RawInput::default());
                win.register_fallback(&ctx, Some(snapshot.clone()));
                let mut output = ctx.end_pass();
                output.textures_delta.clear();
                let viewport = &output.viewport_output[&viewport_id()];
                assert_eq!(viewport.builder.visible, Some(!suspended));
                assert_eq!(viewport.builder.mouse_passthrough, Some(true));
                assert_eq!(viewport.builder.transparent, Some(true));
                assert_eq!(viewport.builder.active, Some(false));
                assert!(viewport.commands.iter().any(|cmd| matches!(cmd, ViewportCommand::Visible(v) if *v == !suspended)));
                assert!(!viewport.commands.iter().any(|cmd| matches!(cmd, ViewportCommand::Close | ViewportCommand::Focus)));
            }
        }
    }

    #[test]
    fn toast_overlay_loss_keeps_deadline_and_registers_passive_fallback_each_frame() {
        let ctx = Context::default();
        ctx.set_embed_viewports(false);
        let mut win = ToastWin::default();
        win.overlay_key = Some(123); // fake 原后端，禁止创建真实 overlay。
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut pending = vec![(ToastKind::Info, "后台提示".into(), deadline)];
        for _ in 0..3 {
            let mut input = egui::RawInput::default();
            input
                .viewports
                .get_mut(&ViewportId::ROOT)
                .unwrap()
                .minimized = Some(true);
            ctx.begin_pass(input);
            win.tick(&ctx, &mut pending, theme(), None);
            let mut output = ctx.end_pass();
            output.textures_delta.clear();
            let viewport = &output.viewport_output[&viewport_id()];
            assert!(viewport.viewport_ui_cb.is_some());
            assert_eq!(viewport.builder.mouse_passthrough, Some(true));
            assert_eq!(viewport.builder.active, Some(false));
            assert_eq!(viewport.builder.transparent, Some(true));
            assert_eq!(win.queue.entries[0].2, deadline);
            assert!(!viewport.commands.iter().any(|c| matches!(
                c,
                ViewportCommand::Visible(_) | ViewportCommand::Focus | ViewportCommand::Close
            )));
        }
        ctx.begin_pass(egui::RawInput::default());
        win.register_fallback(&ctx, None); // 模拟 overlay 恢复：旧窗口休眠。
        let mut output = ctx.end_pass();
        output.textures_delta.clear();
        assert_eq!(
            output.viewport_output[&viewport_id()].builder.position,
            Some(OFFSCREEN)
        );
        assert_eq!(
            output.viewport_output[&viewport_id()].builder.inner_size,
            Some(Vec2::splat(1.0))
        );
        assert_eq!(win.queue.entries[0].2, deadline);
    }
