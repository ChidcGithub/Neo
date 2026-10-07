//! 矩形稳定性测试：锁定 TextField / Switch / FieldRow 在
//! idle / hover / focus / disabled 四态下的 `Response.rect` 逐字节相等，
//! 以及相邻控件位置不漂移。
//!
//! 对应 `button_stability_tests.rs` 的约定：
//! - 聚焦环只改**描边宽度与颜色**（`StrokeKind::Inside`，压在边界内侧），不改几何
//! - 占位符淡出没入只改文本层，不改外框
//! - 一个输入框的交互状态不会推移相邻控件

use super::*;

/// 从指针位置构造一次 RawInput（悬停或按下）。
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

/// `id_salt("salt")` 的 TextField 内部 `TextEdit` 的 Id。
fn edit_id(salt: &str) -> egui::Id {
    crate::hash_id(salt).with("edit")
}

// -----------------------------------------------------------------------
// TextField：idle / hover / focus / focus+hover 四态 rect 稳定
// -----------------------------------------------------------------------

#[test]
fn text_field_rect_stable_across_states() {
    for mode in [neo_theme::ThemeMode::Light, neo_theme::ThemeMode::Dark] {
        for scale in [0.85, 1.0, 1.75] {
            let (ctx, d) = setup_ctx(mode, scale);
            let field_pos = egui::pos2(200.0, 100.0);
            let mut value = String::new();

            let render = |input: egui::RawInput, focus: bool, value: &mut String| -> Rect {
                if focus {
                    ctx.memory_mut(|m| m.request_focus(edit_id("stab-field")));
                } else {
                    ctx.memory_mut(|m| m.surrender_focus(edit_id("stab-field")));
                }
                let mut rect = None;
                let mut output = ctx.run_ui(input, |ui| {
                    rect = Some(
                        TextField::new(value)
                            .hint("搜索")
                            .id_salt("stab-field")
                            .show(ui, &d, 280.0)
                            .rect,
                    );
                });
                output.textures_delta.clear();
                rect.expect("field rect not captured")
            };

            let idle = render(idle_input(), false, &mut value);
            let hover = render(hover_input(field_pos), false, &mut value);
            assert_rect_eq(&idle, &hover, "text-field hover");

            let focus = render(idle_input(), true, &mut value);
            assert_rect_eq(&idle, &focus, "text-field focus");

            let focus_hover = render(hover_input(field_pos), true, &mut value);
            assert_rect_eq(&idle, &focus_hover, "text-field focus+hover");
        }
    }
}

/// 聚焦环动画推进中（ring 在 0..1 之间）rect 也不许动。
#[test]
fn text_field_rect_stable_during_ring_animation() {
    let (ctx, d) = setup_ctx(neo_theme::ThemeMode::Light, 1.0);
    let mut value = String::from("内容");
    let mut rects = Vec::new();

    for frame in 0..6 {
        // 第 2 帧起聚焦：ease 从 0 渐到 1，中间每帧 ring 值都不同。
        if frame == 2 {
            ctx.memory_mut(|m| m.request_focus(edit_id("anim-field")));
        }
        let mut rect = None;
        let mut output = ctx.run_ui(idle_input(), |ui| {
            rect = Some(
                TextField::new(&mut value)
                    .id_salt("anim-field")
                    .show(ui, &d, 280.0)
                    .rect,
            );
        });
        output.textures_delta.clear();
        rects.push(rect.expect("rect not captured"));
    }

    for (i, r) in rects.iter().enumerate() {
        assert_eq!(rects[0], *r, "聚焦环动画第 {i} 帧 rect 变了");
    }
}

/// 禁用态（只感知悬停）rect 与可用态一致。
#[test]
fn text_field_disabled_rect_matches_enabled() {
    let (ctx, d) = setup_ctx(neo_theme::ThemeMode::Light, 1.0);
    let mut value = String::new();

    let render = |enabled: bool, value: &mut String| -> Rect {
        let mut rect = None;
        let mut output = ctx.run_ui(idle_input(), |ui| {
            rect = Some(
                TextField::new(value)
                    .id_salt("disabled-field")
                    .enabled(enabled)
                    .show(ui, &d, 280.0)
                    .rect,
            );
        });
        output.textures_delta.clear();
        rect.expect("rect not captured")
    };

    let enabled = render(true, &mut value);
    let disabled = render(false, &mut value);
    assert_rect_eq(&enabled, &disabled, "text-field enabled vs disabled");
}

// -----------------------------------------------------------------------
// field_frame：聚焦只改描边，绘制矩形不变
// -----------------------------------------------------------------------

#[test]
fn field_frame_paints_inside_same_rect_when_focused() {
    let (ctx, d) = setup_ctx(neo_theme::ThemeMode::Light, 1.0);
    let frame_rect = Rect::from_min_size(egui::pos2(40.0, 40.0), Vec2::new(280.0, 36.0));

    for focused in [false, true] {
        let mut output = ctx.run_ui(idle_input(), |ui| {
            field_frame(ui, &d, frame_rect, focused);
        });
        // 聚焦时描边加粗到 1.6：描边必须是 Inside（压在边界内侧），
        // 否则加粗的那 0.6pt 会画到 rect 外面、盖住相邻控件。
        // （`Shape::visual_bounding_rect` 对 Inside 描边也一律外扩 width/2，
        // 是保守估计，不能拿来当判定 —— 必须看 stroke.kind 与路径点。）
        for shape in &output.shapes {
            if let egui::Shape::Path(path) = &shape.shape {
                if !path.stroke.is_empty() {
                    assert_eq!(
                        path.stroke.kind,
                        egui::StrokeKind::Inside,
                        "focused={focused}: 聚焦描边必须压在边界内侧"
                    );
                }
                for p in &path.points {
                    assert!(
                        frame_rect.expand(0.01).contains(*p),
                        "focused={focused}: 路径点 {p:?} 越出了 rect {frame_rect:?}"
                    );
                }
            }
        }
        output.textures_delta.clear();
    }
}

// -----------------------------------------------------------------------
// Switch：idle / hover / press / on 四态 rect 稳定
// -----------------------------------------------------------------------

#[test]
fn switch_rect_stable_across_states() {
    let (ctx, d) = setup_ctx(neo_theme::ThemeMode::Light, 1.0);
    let switch_pos = egui::pos2(200.0, 100.0);

    let render = |input: egui::RawInput, on: bool| -> Rect {
        let mut rect = None;
        let mut output = ctx.run_ui(input, |ui| {
            rect = Some(Switch::new(on).id_salt("stab-switch").show(ui, &d).rect);
        });
        output.textures_delta.clear();
        rect.expect("switch rect not captured")
    };

    let idle = render(idle_input(), false);
    for (name, input, on) in [
        ("hover", hover_input(switch_pos), false),
        ("press", pointer_input(switch_pos, true), false),
        ("on-idle", idle_input(), true),
        ("on-hover", hover_input(switch_pos), true),
    ] {
        let rect = render(input, on);
        assert_rect_eq(&idle, &rect, &format!("switch {name}"));
    }
}

// -----------------------------------------------------------------------
// 相邻控件不动：输入框聚焦时旁边的控件不推移
// -----------------------------------------------------------------------

#[test]
fn adjacent_widgets_do_not_shift_on_field_focus() {
    let (ctx, d) = setup_ctx(neo_theme::ThemeMode::Light, 1.0);
    let mut value = String::new();
    let width = 280.0;

    let render = |focus: bool, value: &mut String| -> (Rect, Rect, Rect) {
        if focus {
            ctx.memory_mut(|m| m.request_focus(edit_id("adj-field")));
        } else {
            ctx.memory_mut(|m| m.surrender_focus(edit_id("adj-field")));
        }
        let mut rects = None;
        let mut output = ctx.run_ui(idle_input(), |ui| {
            ui.vertical(|ui| {
                let r1 = Switch::new(true).id_salt("adj-sw").show(ui, &d).rect;
                let r2 = TextField::new(value)
                    .id_salt("adj-field")
                    .show(ui, &d, width)
                    .rect;
                let r3 = Switch::new(false).id_salt("adj-sw2").show(ui, &d).rect;
                rects = Some((r1, r2, r3));
            });
        });
        output.textures_delta.clear();
        rects.expect("rects not captured")
    };

    let (a1, a2, a3) = render(false, &mut value);
    // 让聚焦环动画跑几帧，覆盖 ring 中间态。
    for _ in 0..5 {
        let (b1, b2, b3) = render(true, &mut value);
        assert_eq!(a1, b1, "上方 Switch 在输入框聚焦后移动了");
        assert_eq!(a2, b2, "输入框自身 rect 在聚焦后变了");
        assert_eq!(a3, b3, "下方 Switch 在输入框聚焦后移动了");
    }
}
