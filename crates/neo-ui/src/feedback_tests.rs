use super::*;

#[test]
fn inline_notice_wraps_full_text_and_keeps_id_across_resize() {
    for mode in [neo_theme::ThemeMode::Light, neo_theme::ThemeMode::Dark] {
        for scale in [0.85, 1.0, 1.75, 2.8] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            let theme = neo_theme::Theme::from_metrics(mode, neo_theme::Metrics::from_scale(scale));
            theme.apply(&ctx);
            let d = Design::new(theme);
            let text = "完整错误与建议 LONG_UNBROKEN_TOKEN_\n".repeat(12);
            let mut last_id = None;
            for width in [240.0, 320.0, 560.0] {
                let mut response = None;
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width, 6000.0),
                        )),
                        ..Default::default()
                    },
                    |ui| {
                        ui.set_max_width(width - 16.0);
                        response = Some(
                            InlineNotice::new("persistent", &text)
                                .tone(NoticeTone::Error)
                                .show(ui, &d),
                        );
                    },
                );
                output.textures_delta.clear();
                let response = response.unwrap();
                assert!(response.rect.right() <= width);
                if let Some(id) = last_id {
                    assert_eq!(id, response.id);
                }
                last_id = Some(response.id);
                let tree = output
                    .platform_output
                    .accesskit_update
                    .as_ref()
                    .expect("AccessKit tree");
                assert!(tree
                    .nodes
                    .iter()
                    .any(|(_, node)| node.value() == Some(text.as_str())));
                assert!(output
                    .shapes
                    .iter()
                    .any(|shape| matches!(&shape.shape, egui::Shape::Text(t)
                    if t.galley.text() == text && !t.galley.elided && t.galley.rows.len() > 12)));
            }
        }
    }
}

#[test]
fn toast_wrapped_long_text_is_bounded_and_stacks_without_overlap() {
    let ctx = egui::Context::default();
    let d = Design::new(neo_theme::Theme::new(
        neo_theme::ThemeMode::Dark,
        1080.0,
        neo_theme::Distance::Standard,
    ));
    let mut rects = Vec::new();
    ctx.begin_pass(egui::RawInput::default());
    egui::Area::new(egui::Id::new("toast-wrap-test")).show(&ctx, |ui| {
        ui.spacing_mut().item_spacing.y = 10.0;
        for text in [
            "saved".to_owned(),
            "long text 中文 😀\n".repeat(100),
            "X".repeat(512),
        ] {
            rects.push(toast_wrapped(ui, &d, ToastKind::Warning, &text, 260.0).rect);
        }
    });
    let mut output = ctx.end_pass();
    output.textures_delta.clear();
    assert!(rects[1].height() > rects[0].height());
    for rect in &rects {
        assert!(rect.width() <= 260.0);
        assert!(
            rect.height() <= d.t().label * 4.5 + d.m().s(20.0) + 6.0,
            "height={}, label={}, scale={}",
            rect.height(),
            d.t().label,
            d.m().scale()
        );
    }
    for pair in rects.windows(2) {
        assert!(pair[1].top() >= pair[0].bottom() + 9.0);
    }
}

/// toast 浮在所有内容之上：底色必须是层级阶梯的最上层 `surface_3`。
#[test]
fn toast_paints_surface_3() {
    for mode in [neo_theme::ThemeMode::Light, neo_theme::ThemeMode::Dark] {
        let ctx = egui::Context::default();
        let theme = neo_theme::Theme::from_metrics(mode, neo_theme::Metrics::from_scale(1.0));
        theme.apply(&ctx);
        let d = Design::new(theme);

        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            toast(ui, &d, ToastKind::Info, "已保存");
        });
        output.textures_delta.clear();

        let want = d.p().surface_3;
        let painted = output.shapes.iter().any(|clipped| {
            matches!(
                &clipped.shape,
                egui::Shape::Path(path) if path.fill == want
            )
        });
        assert!(painted, "{mode:?}: toast 底应为 surface_3 {want:?}");
    }
}
