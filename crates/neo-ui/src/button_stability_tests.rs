//! 矩形稳定性测试：锁定所有按钮变体在 idle/hover/press/release 四态下
//! 的 `Response.rect` 逐字节相等，以及相邻控件位置不漂移。
//!
//! 这些测试确保：
//! - 自绘 Button 的悬停/按下反馈**只改颜色**，不改几何
//! - IconButton / Chip / Segmented 的矩形在所有状态下不变
//! - Segmented 切换选中段时各段矩形不动
//! - 一个按钮的交互状态不会推移相邻控件

use super::*;

/// 从指针位置构造一次 RawInput。
fn pointer_input(pos: egui::Pos2, button_down: bool) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(800.0, 600.0),
        )),
        events: vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: button_down,
                modifiers: egui::Modifiers::NONE,
            },
        ],
        ..Default::default()
    }
}

/// 只有指针移动（无点击）的 RawInput。
fn hover_input(pos: egui::Pos2) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(800.0, 600.0),
        )),
        events: vec![egui::Event::PointerMoved(pos)],
        ..Default::default()
    }
}

/// 无任何输入（idle 态）的 RawInput。
fn idle_input() -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(800.0, 600.0),
        )),
        ..Default::default()
    }
}

fn setup_ctx(mode: neo_theme::ThemeMode, scale: f32) -> (egui::Context, Design) {
    let ctx = egui::Context::default();
    neo_theme::fonts::install(&ctx);
    let theme = neo_theme::Theme::from_metrics(mode, neo_theme::Metrics::from_scale(scale));
    theme.apply(&ctx);
    let d = Design::new(theme);
    (ctx, d)
}

/// 四态矩形相等断言。
fn assert_rect_eq(idle: &Rect, state: &Rect, label: &str) {
    assert_eq!(
        idle, state,
        "{label}: rect 在交互状态间变化了\n  idle:  {idle:?}\n  state: {state:?}"
    );
}

/// 渲染按钮并返回 rect（使用预设的 Design）。
fn render_with_design(
    ctx: &egui::Context,
    input: egui::RawInput,
    d: &Design,
    show: impl Fn(&mut Ui, &Design) -> Response,
) -> Rect {
    let mut rect = None;
    let mut output = ctx.run_ui(input, |ui| {
        rect = Some(show(ui, d).rect);
    });
    output.textures_delta.clear();
    rect.expect("button rect not captured")
}

// -----------------------------------------------------------------------
// Button 变体：四态 rect 稳定
// -----------------------------------------------------------------------

#[test]
fn button_primary_rect_stable_across_states() {
    for mode in [neo_theme::ThemeMode::Light, neo_theme::ThemeMode::Dark] {
        for scale in [0.85, 1.0, 1.75] {
            let (ctx, d) = setup_ctx(mode, scale);
            let btn_pos = egui::pos2(200.0, 100.0);

            let idle_rect = render_with_design(&ctx, idle_input(), &d, |ui, d| {
                Button::new("主操作").id_salt("stab-primary").show(ui, d)
            });

            let hover_rect = render_with_design(&ctx, hover_input(btn_pos), &d, |ui, d| {
                Button::new("主操作").id_salt("stab-primary").show(ui, d)
            });
            assert_rect_eq(&idle_rect, &hover_rect, "primary hover");

            let press_rect = render_with_design(&ctx, pointer_input(btn_pos, true), &d, |ui, d| {
                Button::new("主操作").id_salt("stab-primary").show(ui, d)
            });
            assert_rect_eq(&idle_rect, &press_rect, "primary press");

            let release_rect =
                render_with_design(&ctx, pointer_input(btn_pos, false), &d, |ui, d| {
                    Button::new("主操作").id_salt("stab-primary").show(ui, d)
                });
            assert_rect_eq(&idle_rect, &release_rect, "primary release");
        }
    }
}

#[test]
fn button_elevated_rect_stable_across_states() {
    let (ctx, d) = setup_ctx(neo_theme::ThemeMode::Light, 1.0);
    let btn_pos = egui::pos2(200.0, 100.0);

    let idle = render_with_design(&ctx, idle_input(), &d, |ui, d| {
        Button::new("次级操作")
            .id_salt("stab-elev")
            .elevated()
            .show(ui, d)
    });

    for (name, input) in [
        ("hover", hover_input(btn_pos)),
        ("press", pointer_input(btn_pos, true)),
        ("release", pointer_input(btn_pos, false)),
    ] {
        let rect = render_with_design(&ctx, input, &d, |ui, d| {
            Button::new("次级操作")
                .id_salt("stab-elev")
                .elevated()
                .show(ui, d)
        });
        assert_rect_eq(&idle, &rect, &format!("elevated {name}"));
    }
}

#[test]
fn button_ghost_rect_stable_across_states() {
    let (ctx, d) = setup_ctx(neo_theme::ThemeMode::Dark, 1.0);
    let btn_pos = egui::pos2(200.0, 100.0);

    let idle = render_with_design(&ctx, idle_input(), &d, |ui, d| {
        Button::new("幽灵")
            .id_salt("stab-ghost")
            .ghost()
            .show(ui, d)
    });

    for (name, input) in [
        ("hover", hover_input(btn_pos)),
        ("press", pointer_input(btn_pos, true)),
        ("release", pointer_input(btn_pos, false)),
    ] {
        let rect = render_with_design(&ctx, input, &d, |ui, d| {
            Button::new("幽灵")
                .id_salt("stab-ghost")
                .ghost()
                .show(ui, d)
        });
        assert_rect_eq(&idle, &rect, &format!("ghost {name}"));
    }
}

#[test]
fn button_danger_rect_stable_across_states() {
    let (ctx, d) = setup_ctx(neo_theme::ThemeMode::Light, 1.0);
    let btn_pos = egui::pos2(200.0, 100.0);

    let idle = render_with_design(&ctx, idle_input(), &d, |ui, d| {
        Button::new("删除")
            .id_salt("stab-danger")
            .danger()
            .show(ui, d)
    });

    for (name, input) in [
        ("hover", hover_input(btn_pos)),
        ("press", pointer_input(btn_pos, true)),
        ("release", pointer_input(btn_pos, false)),
    ] {
        let rect = render_with_design(&ctx, input, &d, |ui, d| {
            Button::new("删除")
                .id_salt("stab-danger")
                .danger()
                .show(ui, d)
        });
        assert_rect_eq(&idle, &rect, &format!("danger {name}"));
    }
}

#[test]
fn button_contrast_rect_stable_across_states() {
    let (ctx, d) = setup_ctx(neo_theme::ThemeMode::Dark, 1.0);
    let btn_pos = egui::pos2(200.0, 100.0);

    let idle = render_with_design(&ctx, idle_input(), &d, |ui, d| {
        Button::new("反色")
            .id_salt("stab-contrast")
            .contrast()
            .show(ui, d)
    });

    for (name, input) in [
        ("hover", hover_input(btn_pos)),
        ("press", pointer_input(btn_pos, true)),
        ("release", pointer_input(btn_pos, false)),
    ] {
        let rect = render_with_design(&ctx, input, &d, |ui, d| {
            Button::new("反色")
                .id_salt("stab-contrast")
                .contrast()
                .show(ui, d)
        });
        assert_rect_eq(&idle, &rect, &format!("contrast {name}"));
    }
}

#[test]
fn button_with_icon_rect_stable_across_states() {
    let (ctx, d) = setup_ctx(neo_theme::ThemeMode::Light, 1.0);
    let btn_pos = egui::pos2(200.0, 100.0);

    let idle = render_with_design(&ctx, idle_input(), &d, |ui, d| {
        Button::new("图标")
            .id_salt("stab-icon")
            .icon(Icon::Trash)
            .show(ui, d)
    });

    for (name, input) in [
        ("hover", hover_input(btn_pos)),
        ("press", pointer_input(btn_pos, true)),
        ("release", pointer_input(btn_pos, false)),
    ] {
        let rect = render_with_design(&ctx, input, &d, |ui, d| {
            Button::new("图标")
                .id_salt("stab-icon")
                .icon(Icon::Trash)
                .show(ui, d)
        });
        assert_rect_eq(&idle, &rect, &format!("icon-btn {name}"));
    }
}

// -----------------------------------------------------------------------
// IconButton：四态 rect 稳定
// -----------------------------------------------------------------------

#[test]
fn icon_button_rect_stable_across_states() {
    type IconButtonStyle = (&'static str, fn(IconButton) -> IconButton);
    let styles: Vec<IconButtonStyle> = vec![
        ("ghost", IconButton::ghost),
        ("elevated", IconButton::elevated),
        ("floating", IconButton::floating),
        ("danger", IconButton::danger),
        ("accent", IconButton::accent),
        ("subtle", IconButton::subtle),
    ];

    for (style_name, style_fn) in styles {
        let (ctx, d) = setup_ctx(neo_theme::ThemeMode::Light, 1.0);
        let btn_pos = egui::pos2(200.0, 100.0);

        let idle = render_with_design(&ctx, idle_input(), &d, |ui, d| {
            style_fn(IconButton::new(Icon::Pen).id_salt("stab-ib")).show(ui, d)
        });

        for (name, input) in [
            ("hover", hover_input(btn_pos)),
            ("press", pointer_input(btn_pos, true)),
            ("release", pointer_input(btn_pos, false)),
        ] {
            let rect = render_with_design(&ctx, input, &d, |ui, d| {
                style_fn(IconButton::new(Icon::Pen).id_salt("stab-ib")).show(ui, d)
            });
            assert_rect_eq(&idle, &rect, &format!("icon-btn-{style_name} {name}"));
        }
    }
}

// -----------------------------------------------------------------------
// Chip：四态 rect 稳定
// -----------------------------------------------------------------------

#[test]
fn chip_rect_stable_across_states() {
    let (ctx, d) = setup_ctx(neo_theme::ThemeMode::Light, 1.0);
    let chip_pos = egui::pos2(200.0, 100.0);

    let idle = render_with_design(&ctx, idle_input(), &d, |ui, d| {
        Chip::new("模型").id_salt("stab-chip").show(ui, d)
    });

    for (name, input) in [
        ("hover", hover_input(chip_pos)),
        ("press", pointer_input(chip_pos, true)),
        ("release", pointer_input(chip_pos, false)),
    ] {
        let rect = render_with_design(&ctx, input, &d, |ui, d| {
            Chip::new("模型").id_salt("stab-chip").show(ui, d)
        });
        assert_rect_eq(&idle, &rect, &format!("chip {name}"));
    }
}

// -----------------------------------------------------------------------
// 相邻控件不动：一个按钮交互时旁边的按钮不推移
// -----------------------------------------------------------------------

#[test]
fn adjacent_buttons_do_not_shift_on_interaction() {
    let (ctx, d) = setup_ctx(neo_theme::ThemeMode::Light, 1.0);
    let btn1_pos = egui::pos2(150.0, 100.0);

    let mut all_rects: Vec<(&str, (Rect, Rect, Rect))> = Vec::new();

    for (name, input) in [
        ("idle", idle_input()),
        ("hover-first", hover_input(btn1_pos)),
        ("press-first", pointer_input(btn1_pos, true)),
        ("release-first", pointer_input(btn1_pos, false)),
    ] {
        let mut rects = None;
        let mut output = ctx.run_ui(input, |ui| {
            ui.horizontal_wrapped(|ui| {
                let r1 = Button::new("操作一").id_salt("adj-1").show(ui, &d).rect;
                let r2 = Button::new("操作二").id_salt("adj-2").show(ui, &d).rect;
                let r3 = Button::new("操作三").id_salt("adj-3").show(ui, &d).rect;
                rects = Some((r1, r2, r3));
            });
        });
        output.textures_delta.clear();
        all_rects.push((name, rects.expect("rects not captured")));
    }

    let (idle_name, (i1, i2, i3)) = &all_rects[0];
    for (name, (r1, r2, r3)) in &all_rects[1..] {
        assert_eq!(i1, r1, "按钮1 {idle_name} vs {name}");
        assert_eq!(i2, r2, "按钮2 {idle_name} vs {name}");
        assert_eq!(i3, r3, "按钮3 {idle_name} vs {name}");
    }
}

// -----------------------------------------------------------------------
// Segmented：段切换选中时 rect 不变
// -----------------------------------------------------------------------

#[test]
fn segmented_rect_stable_on_selection_change() {
    let (ctx, d) = setup_ctx(neo_theme::ThemeMode::Light, 1.0);
    let width = 300.0;
    let opts: &[&str] = &["选项A", "选项B", "选项C"];

    let mut all_rects: Vec<Rect> = Vec::new();
    for sel in 0..3 {
        let mut captured: Option<Rect> = None;
        let mut output = ctx.run_ui(idle_input(), |ui| {
            let (rect, _) =
                ui.allocate_exact_size(Vec2::new(width, d.m().s(32.0)), egui::Sense::hover());
            Segmented::new(opts, sel).show(ui, &d, width);
            captured = Some(rect);
        });
        output.textures_delta.clear();
        all_rects.push(captured.expect("rect not captured"));
    }

    for (i, r) in all_rects.iter().enumerate() {
        assert_eq!(all_rects[0], *r, "Segmented 整体 rect 在选中段 {i} 时变了");
    }
}

#[test]
fn segmented_below_button_stable_on_selection_change() {
    let (ctx, d) = setup_ctx(neo_theme::ThemeMode::Light, 1.0);
    let width = 300.0;
    let opts: &[&str] = &["选项A", "选项B", "选项C"];

    let mut below_rects = Vec::new();
    for sel in 0..3 {
        let mut below_rect = None;
        let mut output = ctx.run_ui(idle_input(), |ui| {
            ui.vertical(|ui| {
                Segmented::new(opts, sel).show(ui, &d, width);
                below_rect = Some(
                    Button::new("下方按钮")
                        .id_salt("below-seg")
                        .show(ui, &d)
                        .rect,
                );
            });
        });
        output.textures_delta.clear();
        below_rects.push(below_rect.expect("below rect not captured"));
    }

    for (i, r) in below_rects.iter().enumerate() {
        assert_eq!(
            below_rects[0], *r,
            "Segmented 选中段 {i} 时下方按钮位置变了"
        );
    }
}

// -----------------------------------------------------------------------
// 综合：所有变体在暗色 + 亮色主题下 rect 稳定
// -----------------------------------------------------------------------

#[test]
fn all_button_variants_rect_stable_both_themes() {
    for mode in [neo_theme::ThemeMode::Light, neo_theme::ThemeMode::Dark] {
        let (ctx, d) = setup_ctx(mode, 1.0);
        let btn_pos = egui::pos2(200.0, 100.0);

        type ButtonVariant = (&'static str, Box<dyn Fn(&mut Ui, &Design) -> Response>);
        let variants: Vec<ButtonVariant> = vec![
            (
                "primary",
                Box::new(|ui, d| Button::new("P").id_salt("v-p").primary().show(ui, d)),
            ),
            (
                "elevated",
                Box::new(|ui, d| Button::new("E").id_salt("v-e").elevated().show(ui, d)),
            ),
            (
                "ghost",
                Box::new(|ui, d| Button::new("G").id_salt("v-g").ghost().show(ui, d)),
            ),
            (
                "danger",
                Box::new(|ui, d| Button::new("D").id_salt("v-d").danger().show(ui, d)),
            ),
            (
                "contrast",
                Box::new(|ui, d| Button::new("C").id_salt("v-c").contrast().show(ui, d)),
            ),
        ];

        for (name, show_fn) in &variants {
            let idle = render_with_design(&ctx, idle_input(), &d, |ui, d| show_fn(ui, d));

            for (state_name, input) in [
                ("hover", hover_input(btn_pos)),
                ("press", pointer_input(btn_pos, true)),
            ] {
                let rect = render_with_design(&ctx, input, &d, |ui, d| show_fn(ui, d));
                assert_rect_eq(&idle, &rect, &format!("{mode:?}/{name}/{state_name}"));
            }
        }
    }
}
