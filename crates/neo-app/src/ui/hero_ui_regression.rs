
    use super::*;
    use crate::ui::composer::ui_regression::{context, probe};

    #[test]
    fn english_i18n_welcome_and_composer_fit() {
        crate::i18n::with_language(crate::i18n::Language::EnUs, || {
            assert_eq!(tr("今天想在课堂上做点什么？"), "What would you like to do in class today?");
            welcome_column_wraps_and_keeps_workspace_and_send_inside_viewport();
        });
    }

    #[test]
    fn welcome_column_wraps_and_keeps_workspace_and_send_inside_viewport() {
        for mode in [neo_theme::ThemeMode::Light, neo_theme::ThemeMode::Dark] {
            for (width, height, scale) in [
                (320.0, 600.0, 1.0),
                (360.0, 640.0, 0.85),
                (480.0, 720.0, 1.25),
                (768.0, 1024.0, 1.6),
                (1280.0, 720.0, 1.0),
                (1920.0, 1080.0, 1.25),
                (3840.0, 2160.0, 2.8),
            ] {
                let ctx = context();
                let theme =
                    neo_theme::Theme::from_metrics(mode, neo_theme::Metrics::from_scale(scale));
                theme.apply(&ctx);
                let whale = crate::brand::WhaleMark::cached(&ctx);
                let skin = Skin::new(theme, &whale);
                let area = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(width, height));
                let mut state = AppState::default();
                state.workspace = Some("long-workspace-path/".repeat(30));
                for _ in 0..3 {
                    let mut output = ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(area),
                            ..Default::default()
                        },
                        |ui| {
                            draw(ui, &skin, area, &mut state);
                        },
                    );
                    // 纯布局测试不上传 GPU；显式消费本帧纹理增量，遵守 egui 帧生命周期。
                    output.textures_delta.clear();
                    let mut found_title = false;
                    for shape in output.shapes {
                        if let egui::Shape::Text(text) = shape.shape {
                            if text.galley.job.text == tr("今天想在课堂上做点什么？") {
                                found_title = true;
                                let bounds = Rect::from_min_size(text.pos, text.galley.size());
                                assert!(area.contains_rect(bounds), "{mode:?} {width}: {bounds:?}");
                            }
                        }
                    }
                    assert!(found_title, "欢迎标题必须实际绘制");
                }
                let chip: Rect = probe(&ctx, "neo-hero-workspace-probe");
                let (send, _): (Rect, Rect) = probe(&ctx, "neo-composer-send-probe");
                assert!(area.contains_rect(chip));
                assert!(area.contains_rect(send));
                assert!(chip.bottom() < send.top());
                // 矩形坐标相减会有亚像素舍入，容差不改变触控目标尺寸。
                assert!(chip.height() + 0.01 >= skin.m().hit_target(0.0));
            }
        }
    }
