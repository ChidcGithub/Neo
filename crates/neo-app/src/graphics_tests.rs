
    use super::*;

    #[test]
    fn internal_command_is_strict_and_never_accepts_vulkan() {
        let parse = |args: &[&str]| parse_probe(&args.iter().map(OsString::from).collect::<Vec<_>>());
        assert_eq!(parse(&[]), None);
        assert_eq!(parse(&[PROBE_ARG, "dx12"]), Some(Ok(Backend::Dx12)));
        assert_eq!(parse(&[PROBE_ARG, "glow"]), Some(Ok(Backend::Glow)));
        for args in [vec![PROBE_ARG], vec![PROBE_ARG, "vulkan"], vec![PROBE_ARG, "glow", "extra"], vec!["extra", PROBE_ARG, "dx12"]] {
            assert_eq!(parse(&args), Some(Err(())));
        }
    }

    #[test]
    fn invalid_cache_probes_dx12_then_glow_and_stays_pending() {
        for text in ["", "broken", "{}", r#"{"version":"old","ready":"dx12","pending":null,"failed":[]}"#] {
            let mut cache = Cache::decode(text);
            let mut calls = Vec::new();
            let chosen = select_with(&mut cache, |backend| {
                calls.push(backend);
                backend == Backend::Glow
            }, |_| Ok(())).unwrap();
            assert_eq!(calls, [Backend::Dx12, Backend::Glow]);
            assert_eq!(chosen, Backend::Glow);
            assert_eq!(cache.pending, Some(chosen));
            assert_eq!(cache.ready, None);
        }
    }

    #[test]
    fn successful_cache_skips_probe_but_arms_recovery() {
        let mut cache = Cache::decode("");
        cache.ready = Some(Backend::Dx12);
        assert_eq!(select_with(&mut cache, |_| panic!("cached"), |_| Ok(())).unwrap(), Backend::Dx12);
        assert_eq!(cache.pending, Some(Backend::Dx12));
        assert_eq!(cache.ready, None);
        // 模拟缓存后端在主窗口首帧前崩溃：下次只尝试后备。
        let mut cache = Cache::decode(&serde_json::to_string(&cache).unwrap());
        assert_eq!(select_with(&mut cache, |backend| {
            assert_eq!(backend, Backend::Glow);
            true
        }, |_| Ok(())).unwrap(), Backend::Glow);
        // 后备也未完成启动：以后快速失败，而非反复卡死。
        assert!(select_with(&mut cache, |_| panic!("failed backend retried"), |_| Ok(())).is_err());
    }

    #[test]
    fn both_failed_probes_are_not_repeated() {
        let mut cache = Cache::decode("");
        let mut calls = 0;
        assert!(select_with(&mut cache, |_| { calls += 1; false }, |_| Ok(())).is_err());
        assert_eq!(calls, 2);
        let mut cache = Cache::decode(&serde_json::to_string(&cache).unwrap());
        assert!(select_with(&mut cache, |_| panic!("retry"), |_| Ok(())).is_err());
    }

    #[test]
    fn unwritable_pending_marker_stops_before_driver_start() {
        let mut cache = Cache::decode("");
        assert!(select_with(&mut cache, |_| panic!("unsafe start"), |_| Err(io::Error::other("read only"))).is_err());
    }

    #[test]
    fn cache_paths_never_use_working_directory() {
        let absolute = std::env::temp_dir();
        let path = cache_path(Some(absolute.clone().into_os_string()), Some(absolute.clone().into_os_string()));
        assert_eq!(path, Some(absolute.join("graphics.json")));
        assert_eq!(cache_path(Some("relative".into()), Some(absolute.clone().into_os_string())), Some(absolute.join("Neo/graphics.json")));
        assert_eq!(cache_path(Some("".into()), None), None);
    }

    #[test]
    fn only_our_root_render_receipt_confirms_a_frame() {
        let ctx = egui::Context::default();
        let mut frame = FirstFrame::default();
        for (viewport_id, user_data, expected) in [
            (egui::ViewportId::ROOT, egui::UserData::default(), false),
            (egui::ViewportId::from_hash_of("child"), egui::UserData::new(FrameReceipt), false),
            (egui::ViewportId::ROOT, egui::UserData::new(FrameReceipt), true),
        ] {
            ctx.begin_pass(egui::RawInput::default());
            frame.request(&ctx);
            assert!(!frame.poll(&ctx), "UI passes alone are not rendering proof");
            ctx.end_pass().textures_delta.clear();
            ctx.begin_pass(egui::RawInput {
                events: vec![egui::Event::Screenshot {
                    viewport_id, user_data,
                    image: Arc::new(egui::ColorImage::new([2, 2], vec![egui::Color32::BLUE; 4])),
                }],
                ..Default::default()
            });
            assert_eq!(frame.poll(&ctx), expected);
            ctx.end_pass().textures_delta.clear();
        }
    }

    #[test]
    fn hidden_business_waits_for_receipt_in_both_logic_and_ui() {
        use eframe::App;
        #[derive(Default)]
        struct HideOnTick { logic_calls: usize, ui_calls: usize }
        impl App for HideOnTick {
            fn logic(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
                self.logic_calls += 1;
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            }
            fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
                self.ui_calls += 1;
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Visible(false));
            }
        }
        let ctx = egui::Context::default();
        let mut app = TrackedApp {
            app: HideOnTick::default(), selection: None, frame: FirstFrame::default(),
        };
        let mut frame = eframe::Frame::_new_kittest();
        for pass in 0..3 {
            let events = if pass == 2 {
                vec![egui::Event::Screenshot {
                    viewport_id: egui::ViewportId::ROOT,
                    user_data: egui::UserData::new(FrameReceipt),
                    image: Arc::new(egui::ColorImage::new([2, 2], vec![egui::Color32::BLUE; 4])),
                }]
            } else { vec![] };
            ctx.begin_pass(egui::RawInput { events, ..Default::default() });
            app.logic(&ctx, &mut frame);
            let mut ui = egui::Ui::new(ctx.clone(), egui::Id::new("graphics-test"), egui::UiBuilder::new());
            egui::CentralPanel::default().show(&mut ui, |ui| app.ui(ui, &mut frame));
            let mut output = ctx.end_pass();
            let commands = &output.viewport_output[&egui::ViewportId::ROOT].commands;
            assert_eq!(commands.iter().any(|cmd| matches!(cmd, egui::ViewportCommand::Screenshot(_))), pass == 0);
            assert_eq!(commands.iter().any(|cmd| matches!(cmd, egui::ViewportCommand::Visible(false))), pass == 2);
            assert_eq!(app.app.logic_calls, usize::from(pass == 2));
            assert_eq!(app.app.ui_calls, usize::from(pass == 2));
            output.textures_delta.clear();
        }
        // 隐藏后的 logic-only 路径即使不再收到 Screenshot，也必须继续业务 tick。
        ctx.begin_pass(egui::RawInput::default());
        app.logic(&ctx, &mut frame);
        assert_eq!(app.app.logic_calls, 2);
        ctx.end_pass().textures_delta.clear();
    }

    #[test]
    fn lost_receipt_retries_at_bounded_intervals_then_stops_on_success() {
        let ctx = egui::Context::default();
        let start = Instant::now();
        let mut frame = FirstFrame::default();
        for (elapsed, expected) in [
            (Duration::ZERO, true),
            (SCREENSHOT_RETRY - Duration::from_millis(1), false),
            (SCREENSHOT_RETRY, true),
            (SCREENSHOT_RETRY + Duration::from_millis(1), false),
        ] {
            ctx.begin_pass(egui::RawInput::default());
            frame.request_at(&ctx, start + elapsed);
            assert!(!frame.received);
            let mut output = ctx.end_pass();
            assert_eq!(output.viewport_output[&egui::ViewportId::ROOT].commands.iter()
                .filter(|cmd| matches!(cmd, egui::ViewportCommand::Screenshot(_))).count(), usize::from(expected));
            output.textures_delta.clear();
        }
        ctx.begin_pass(egui::RawInput {
            events: vec![egui::Event::Screenshot {
                viewport_id: egui::ViewportId::ROOT,
                user_data: egui::UserData::new(FrameReceipt),
                image: Arc::new(egui::ColorImage::new([2, 2], vec![egui::Color32::BLUE; 4])),
            }],
            ..Default::default()
        });
        assert!(frame.poll_at(&ctx, start + SCREENSHOT_RETRY * 2));
        // 成功是终态，随后经过 15 秒也不再重试或超时。
        frame.request_at(&ctx, start + FRAME_TIMEOUT * 2);
        assert!(frame.received && !frame.timed_out);
        let mut output = ctx.end_pass();
        assert!(!output.viewport_output[&egui::ViewportId::ROOT].commands.iter()
            .any(|cmd| matches!(cmd, egui::ViewportCommand::Screenshot(_))));
        output.textures_delta.clear();
    }

    #[test]
    fn receipt_timeout_closes_without_business_or_clearing_pending() {
        use eframe::App;
        struct NoBusiness;
        impl App for NoBusiness {
            fn logic(&mut self, _: &egui::Context, _: &mut eframe::Frame) { panic!("business logic"); }
            fn ui(&mut self, _: &mut egui::Ui, _: &mut eframe::Frame) { panic!("business ui"); }
        }
        let ctx = egui::Context::default();
        let start = Instant::now() - FRAME_TIMEOUT;
        let mut cache = Cache::decode("");
        cache.pending = Some(Backend::Dx12);
        let mut app = TrackedApp {
            app: NoBusiness,
            selection: Some(Selection {
                backend: Backend::Dx12,
                path: std::env::temp_dir().join("neo-graphics-must-not-write.json"),
                cache,
            }),
            frame: FirstFrame { started: Some(start), ..Default::default() },
        };
        ctx.begin_pass(egui::RawInput::default());
        assert!(!app.frame.poll_at(&ctx, start + FRAME_TIMEOUT - Duration::from_millis(1)));
        assert!(!app.frame.timed_out);
        let mut frame = eframe::Frame::_new_kittest();
        app.logic(&ctx, &mut frame);
        assert!(app.frame.timed_out);
        let mut ui = egui::Ui::new(ctx.clone(), egui::Id::new("graphics-timeout"), egui::UiBuilder::new());
        app.ui(&mut ui, &mut frame);
        let mut output = ctx.end_pass();
        let commands = &output.viewport_output[&egui::ViewportId::ROOT].commands;
        assert!(commands.iter().any(|cmd| matches!(cmd, egui::ViewportCommand::Close)));
        assert!(!commands.iter().any(|cmd| matches!(cmd, egui::ViewportCommand::Screenshot(_))));
        output.textures_delta.clear();
        // 迟到回执不能重新放行业务或把 pending 改成成功。
        ctx.begin_pass(egui::RawInput {
            events: vec![egui::Event::Screenshot {
                viewport_id: egui::ViewportId::ROOT,
                user_data: egui::UserData::new(FrameReceipt),
                image: Arc::new(egui::ColorImage::new([2, 2], vec![egui::Color32::BLUE; 4])),
            }],
            ..Default::default()
        });
        app.logic(&ctx, &mut frame);
        assert!(!app.frame.received);
        let cache = &app.selection.as_ref().unwrap().cache;
        assert_eq!(cache.pending, Some(Backend::Dx12));
        assert_eq!(cache.ready, None);
        ctx.end_pass().textures_delta.clear();
    }

    #[test]
    fn options_select_explicit_renderers() {
        assert_eq!(options(Backend::Dx12).renderer, eframe::Renderer::Wgpu);
        assert_eq!(options(Backend::Glow).renderer, eframe::Renderer::Glow);
    }
