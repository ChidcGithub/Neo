
    use super::*;

    #[test]
    fn wake_test_narrow_page_wraps_and_blocks_unsafe_start() {
        for width in [240.0, 320.0, 460.0] {
            for scale in [0.85, 1.0, 1.75, 2.8] {
                let ctx = crate::ui::composer::ui_regression::context();
                let theme = neo_theme::Theme::from_metrics(neo_theme::ThemeMode::Dark,
                    neo_theme::Metrics::from_scale(scale));
                theme.apply(&ctx);
                let whale = crate::brand::WhaleMark::cached(&ctx);
                let skin = Skin::new(theme, &whale);
                let mut state = AppState::default();
                state.wake_test.error = Some("学校模型资源缺失，请联系管理员核对部署资源与权限".repeat(3));
                state.wake_test.snapshot = Some(neo_wake::WakeDiagnostics {
                    enabled: true, test_mode: true, phase: neo_wake::WakePhase::Ready,
                    device_name: Some("Windows 默认输入设备名称很长的学校教室麦克风".repeat(3)),
                    sample_rate: Some(48_000), rms: Some(0.02), dbfs: Some(-34.0),
                    peak: Some(0.3), score: Some(0.4), score_peak_2s: Some(0.5),
                    threshold: 0.25, warmup_frames: 25, warmup_total: 25, dictating: false,
                    hit_count: 3, last_hit_ms: 0, updated_at_ms: 0, last_audio_ms: 0,
                    error: None,
                });
                let mut output = ctx.run_ui(egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 12000.0))),
                    ..Default::default()
                }, |ui| {
                    draw(ui, &skin, width - 16.0, &mut state);
                    assert!(ui.min_rect().width() <= width, "overflow {width}/{scale}");
                });
                output.textures_delta.clear();
                let (_, enabled): (egui::Rect, bool) = ctx.data(|d| d.get_temp(egui::Id::new("wake-test-button")).unwrap());
                assert!(!enabled);
                for clipped in &output.shapes {
                    if let egui::Shape::Text(text) = &clipped.shape {
                        assert!(!text.galley.elided);
                        let bounds = egui::Rect::from_min_size(text.pos, text.galley.size());
                        assert!(clipped.clip_rect.expand(1.0).contains_rect(bounds),
                            "clipped {width}/{scale}: {:?}, clip={:?}, bounds={bounds:?}",
                            text.galley.text(), clipped.clip_rect);
                    }
                }
                assert!(output.platform_output.commands.is_empty());
                assert!(!state.wake_test.requested);
            }
        }
    }
