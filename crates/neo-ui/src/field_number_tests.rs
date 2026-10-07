//! NumberField 的矩形稳定性与数值语义测试。
//!
//! 矩形部分沿用 `field_stability_tests.rs` 的四态约定（idle / hover / focus /
//! disabled 的 `Response.rect` 逐字节相等）；数值部分锁定范围夹取、↑/↓ 步进、
//! 失焦提交与非法输入回退。

use super::*;
use egui::{Key, Modifiers};

fn idle_input() -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(800.0, 600.0),
        )),
        ..Default::default()
    }
}

fn hover_input(pos: egui::Pos2) -> egui::RawInput {
    let mut input = idle_input();
    input.events = vec![egui::Event::PointerMoved(pos)];
    input
}

fn key_input(key: Key) -> egui::RawInput {
    let mut input = idle_input();
    input.events = vec![egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    }];
    input
}

fn setup_ctx(mode: neo_theme::ThemeMode, scale: f32) -> (egui::Context, Design) {
    let ctx = egui::Context::default();
    neo_theme::fonts::install(&ctx);
    let theme = neo_theme::Theme::from_metrics(mode, neo_theme::Metrics::from_scale(scale));
    theme.apply(&ctx);
    let d = Design::new(theme);
    (ctx, d)
}

/// `id_salt("salt")` 的 NumberField 内部 `TextEdit` 的 Id。
fn edit_id(salt: &str) -> egui::Id {
    crate::hash_id(salt).with("edit")
}

// -----------------------------------------------------------------------
// 四态 rect 稳定（与 TextField 同一约定）
// -----------------------------------------------------------------------

#[test]
fn number_field_rect_stable_across_states() {
    for mode in [neo_theme::ThemeMode::Light, neo_theme::ThemeMode::Dark] {
        for scale in [0.85, 1.0, 1.75] {
            let (ctx, d) = setup_ctx(mode, scale);
            let field_pos = egui::pos2(200.0, 100.0);
            let mut value = 1_000_000usize;

            let render = |input: egui::RawInput, focus: bool, value: &mut usize| -> Rect {
                if focus {
                    ctx.memory_mut(|m| m.request_focus(edit_id("stab-num")));
                } else {
                    ctx.memory_mut(|m| m.surrender_focus(edit_id("stab-num")));
                }
                let mut rect = None;
                let mut output = ctx.run_ui(input, |ui| {
                    rect = Some(
                        NumberField::new(value)
                            .id_salt("stab-num")
                            .range(8192..=4_000_000)
                            .step(1024)
                            .show(ui, &d, 280.0)
                            .rect,
                    );
                });
                output.textures_delta.clear();
                rect.expect("number field rect not captured")
            };

            let idle = render(idle_input(), false, &mut value);
            let hover = render(hover_input(field_pos), false, &mut value);
            assert_eq!(idle, hover, "number-field hover 时 rect 变了");

            let focus = render(idle_input(), true, &mut value);
            assert_eq!(idle, focus, "number-field focus 时 rect 变了");

            let focus_hover = render(hover_input(field_pos), true, &mut value);
            assert_eq!(idle, focus_hover, "number-field focus+hover 时 rect 变了");
        }
    }
}

#[test]
fn number_field_disabled_rect_matches_enabled() {
    let (ctx, d) = setup_ctx(neo_theme::ThemeMode::Light, 1.0);
    let mut value = 8192usize;

    let render = |enabled: bool, value: &mut usize| -> Rect {
        let mut rect = None;
        let mut output = ctx.run_ui(idle_input(), |ui| {
            rect = Some(
                NumberField::new(value)
                    .id_salt("disabled-num")
                    .range(8192..=4_000_000)
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
    assert_eq!(enabled, disabled, "number-field 禁用态 rect 与可用态不一致");
}

/// 高度与 TextField 一致（设计系统对齐，同一表单里混排不跳）。
#[test]
fn number_field_matches_text_field_height() {
    for scale in [0.85, 1.0, 1.75, 2.8] {
        let (_, d) = setup_ctx(neo_theme::ThemeMode::Light, scale);
        assert_eq!(NumberField::height(&d), TextField::height(&d));
    }
}

// -----------------------------------------------------------------------
// 数值语义：步进 / 范围夹取 / 失焦提交 / 非法输入回退
// -----------------------------------------------------------------------

#[test]
fn number_field_arrow_keys_step_within_range() {
    let (ctx, d) = setup_ctx(neo_theme::ThemeMode::Light, 1.0);
    let mut value = 8192usize;

    let frame = |input: egui::RawInput, value: &mut usize| {
        let mut output = ctx.run_ui(input, |ui| {
            NumberField::new(value)
                .id_salt("num-keys")
                .range(8192..=4_000_000)
                .step(1024)
                .show(ui, &d, 280.0);
        });
        output.textures_delta.clear();
    };

    // 聚焦。
    ctx.memory_mut(|m| m.request_focus(edit_id("num-keys")));
    frame(idle_input(), &mut value);
    assert_eq!(value, 8192);

    // ↑ 三次：8192 → 9216 → 10240 → 11264。
    for expected in [9216, 10240, 11264] {
        frame(key_input(Key::ArrowUp), &mut value);
        assert_eq!(value, expected, "ArrowUp 步进不对");
    }

    // ↓ 一次回到 10240。
    frame(key_input(Key::ArrowDown), &mut value);
    assert_eq!(value, 10240, "ArrowDown 步进不对");

    // 压到下界：把值换成 8192（等价外部写入），再按 ↓ 不会越过 8192。
    let mut low = 8192usize;
    frame(idle_input(), &mut low);
    frame(key_input(Key::ArrowDown), &mut low);
    assert_eq!(low, 8192, "下界被越过");

    // 压到上界：↑ 不越过 4_000_000。
    let mut high = 4_000_000usize;
    frame(idle_input(), &mut high);
    frame(key_input(Key::ArrowUp), &mut high);
    assert_eq!(high, 4_000_000, "上界被越过");
}

#[test]
fn number_field_arrow_ignored_when_disabled() {
    let (ctx, d) = setup_ctx(neo_theme::ThemeMode::Light, 1.0);
    let mut value = 8192usize;
    ctx.memory_mut(|m| m.request_focus(edit_id("num-disabled")));

    let mut output = ctx.run_ui(key_input(Key::ArrowUp), |ui| {
        NumberField::new(&mut value)
            .id_salt("num-disabled")
            .range(8192..=4_000_000)
            .enabled(false)
            .show(ui, &d, 280.0);
    });
    output.textures_delta.clear();
    assert_eq!(value, 8192, "禁用态仍响应了 ↑");
}

#[test]
fn number_field_commits_typed_value_on_focus_lost_and_clamps() {
    let (ctx, d) = setup_ctx(neo_theme::ThemeMode::Light, 1.0);
    let mut value = 8192usize;

    let frame = |events: Vec<egui::Event>, value: &mut usize| {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                NumberField::new(value)
                    .id_salt("num-type")
                    .range(8192..=4_000_000)
                    .show(ui, &d, 280.0);
            },
        );
        output.textures_delta.clear();
    };

    let select_all_and_clear = |value: &mut usize| {
        frame(
            vec![egui::Event::Key {
                key: Key::A,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::COMMAND,
            }],
            value,
        );
        frame(
            vec![egui::Event::Key {
                key: Key::Backspace,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            }],
            value,
        );
    };
    let type_text = |text: &str, value: &mut usize| {
        for ch in text.chars() {
            frame(vec![egui::Event::Text(ch.to_string())], value);
        }
    };

    // 聚焦并输入 "65536"：实时提交，敲完时已是 65536。
    ctx.memory_mut(|m| m.request_focus(edit_id("num-type")));
    frame(vec![], &mut value);
    select_all_and_clear(&mut value);
    // 清空本身不改值（空文本不是合法数，值保持 8192）。
    assert_eq!(value, 8192, "删空不该改值");
    type_text("65536", &mut value);
    assert_eq!(value, 65536, "合法输入没有实时写回");
    // 失焦：值不变、文本归位。
    ctx.memory_mut(|m| m.surrender_focus(edit_id("num-type")));
    frame(vec![], &mut value);
    assert_eq!(value, 65536, "失焦不该改变已提交的值");

    // 输入越界值：每个前缀都合法（"9"、"99"…实时夹到上界），文本保持用户所敲，
    // 失焦时把越界文本夹到上界提交。
    ctx.memory_mut(|m| m.request_focus(edit_id("num-type")));
    frame(vec![], &mut value);
    select_all_and_clear(&mut value);
    type_text("99999999", &mut value);
    assert_eq!(value, 4_000_000, "越界输入没有被夹到上界");
    ctx.memory_mut(|m| m.surrender_focus(edit_id("num-type")));
    frame(vec![], &mut value);
    assert_eq!(value, 4_000_000, "失焦后越界值没有保持上界");

    // 输入非法文本：失焦回退到当前值。
    ctx.memory_mut(|m| m.request_focus(edit_id("num-type")));
    frame(vec![], &mut value);
    select_all_and_clear(&mut value);
    type_text("abc", &mut value);
    assert_eq!(value, 4_000_000, "非法输入在聚焦期间不该写回");
    ctx.memory_mut(|m| m.surrender_focus(edit_id("num-type")));
    frame(vec![], &mut value);
    assert_eq!(value, 4_000_000, "非法输入没有回退到原值");
}

/// 未聚焦时若持久层把值改了，缓冲区必须跟随（而不是停在旧文本）。

#[test]
fn number_field_buffer_follows_external_value_change() {
    let (ctx, d) = setup_ctx(neo_theme::ThemeMode::Light, 1.0);
    let mut value = 8192usize;

    let frame = |value: &mut usize| {
        let mut output = ctx.run_ui(idle_input(), |ui| {
            NumberField::new(value)
                .id_salt("num-follow")
                .range(8192..=4_000_000)
                .show(ui, &d, 280.0);
        });
        output.textures_delta.clear();
    };

    frame(&mut value);
    value = 1_000_000;
    frame(&mut value);
    // 失焦后再聚焦：缓冲区应从权威值重建，不残留 8192。
    ctx.memory_mut(|m| m.request_focus(edit_id("num-follow")));
    frame(&mut value);
    let text: String = ctx.data(|data| {
        data.get_temp(crate::hash_id("num-follow").with("buf"))
            .unwrap_or_default()
    });
    assert_eq!(text, "1000000", "缓冲区没有跟随外部改值");
}
