use super::*;

/// InlineNotice 必须有细边框（不是无边框的纯色块）。
#[test]
fn inline_notice_has_thin_border() {
    for mode in [neo_theme::ThemeMode::Light, neo_theme::ThemeMode::Dark] {
        let ctx = egui::Context::default();
        neo_theme::fonts::install(&ctx);
        let theme = neo_theme::Theme::from_metrics(mode, neo_theme::Metrics::from_scale(1.0));
        theme.apply(&ctx);
        let d = Design::new(theme);

        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            InlineNotice::new("border-test", "测试提示").show(ui, &d);
        });
        output.textures_delta.clear();

        // egui::Frame 的 stroke 画出来是 Rect shape（带 stroke），不是 Path。
        let has_border = output.shapes.iter().any(|clipped| {
            matches!(
                &clipped.shape,
                egui::Shape::Rect(r) if r.stroke.width > 0.0
            )
        });
        assert!(has_border, "{mode:?}: InlineNotice 应画出细边框");
    }
}

/// InlineNotice 的矩形在 idle/hover 之间不变（hover 只改视觉不改几何）。
#[test]
fn inline_notice_rect_stable_across_hover() {
    for mode in [neo_theme::ThemeMode::Light, neo_theme::ThemeMode::Dark] {
        let ctx = egui::Context::default();
        neo_theme::fonts::install(&ctx);
        let theme = neo_theme::Theme::from_metrics(mode, neo_theme::Metrics::from_scale(1.0));
        theme.apply(&ctx);
        let d = Design::new(theme);

        let idle_rect = {
            let mut rect = None;
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                rect = Some(
                    InlineNotice::new("stab", "稳定提示")
                        .tone(NoticeTone::Info)
                        .show(ui, &d)
                        .rect,
                );
            });
            output.textures_delta.clear();
            rect.expect("idle rect")
        };

        let hover_input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            events: vec![egui::Event::PointerMoved(idle_rect.center())],
            ..Default::default()
        };
        let hover_rect = {
            let mut rect = None;
            let mut output = ctx.run_ui(hover_input, |ui| {
                rect = Some(
                    InlineNotice::new("stab", "稳定提示")
                        .tone(NoticeTone::Info)
                        .show(ui, &d)
                        .rect,
                );
            });
            output.textures_delta.clear();
            rect.expect("hover rect")
        };
        assert_eq!(
            idle_rect, hover_rect,
            "{mode:?}: hover 改变了 InlineNotice 矩形"
        );
    }
}

/// toast 淡入动画：新 toast 的 alpha 从 0 渐变到 1，已完成的 toast 直接为 1。
/// reduced motion（alpha 写入为 1.0）时无渐变。
#[test]
fn toast_fade_in_alpha_progresses() {
    let ctx = egui::Context::default();
    let theme = neo_theme::Theme::from_metrics(
        neo_theme::ThemeMode::Dark,
        neo_theme::Metrics::from_scale(1.0),
    );
    theme.apply(&ctx);
    let d = Design::new(theme);

    // 第一帧（t=0）：投递一条 toast，alpha 应该从 0 开始。
    let mut tq = toasts(&d);
    tq.add(Toast::new().kind(ToastKind::Info).text("测试"));
    let mut output = ctx.run_ui(
        egui::RawInput {
            time: Some(0.0),
            ..Default::default()
        },
        |ui| {
            tq.show(ui);
        },
    );
    output.textures_delta.clear();

    // 读取淡入 alpha：第一帧 animate_value_with_time 刚写入 0→1，
    // 返回的值应该接近 0（刚开始渐变）。
    let alpha: f32 = ctx
        .data(|d| d.get_temp(egui::Id::new("neo-toast-fade-alpha")))
        .unwrap_or(1.0);
    assert!(
        alpha < 0.99,
        "第一帧 alpha 应接近 0（淡入中），实际 {alpha}"
    );

    // 多跑几帧让动画完成（每帧推进 16ms，共推进约 480ms 远超 150ms 淡入时长）。
    for frame in 1..=30 {
        let mut tq2 = toasts(&d);
        // 不再投递新 toast（队列已有），只推进时间。
        let mut output = ctx.run_ui(
            egui::RawInput {
                time: Some(frame as f64 * 0.016),
                ..Default::default()
            },
            |ui| {
                tq2.show(ui);
            },
        );
        output.textures_delta.clear();
    }

    let alpha: f32 = ctx
        .data(|d| d.get_temp(egui::Id::new("neo-toast-fade-alpha")))
        .unwrap_or(1.0);
    assert!(
        (alpha - 1.0).abs() < 0.01,
        "动画完成后 alpha 应为 1.0，实际 {alpha}"
    );
}

/// toast 矩形尺寸不因淡入动画而改变。
#[test]
fn toast_rect_stable_during_fade() {
    let ctx = egui::Context::default();
    let theme = neo_theme::Theme::from_metrics(
        neo_theme::ThemeMode::Dark,
        neo_theme::Metrics::from_scale(1.0),
    );
    theme.apply(&ctx);
    let d = Design::new(theme);

    // 直接调用 toast() 绘制（不经过队列），验证绘制函数本身的 rect 稳定性。
    let mut rects = Vec::new();
    for _ in 0..3 {
        let mut rect = None;
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            rect = Some(toast(ui, &d, ToastKind::Success, "已保存").rect);
        });
        output.textures_delta.clear();
        rects.push(rect.expect("toast rect"));
    }
    for (i, r) in rects.iter().enumerate() {
        assert_eq!(rects[0], *r, "帧 {i}: toast 矩形变了");
    }
}

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
