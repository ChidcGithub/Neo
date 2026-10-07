//! Badge 测试：语义色对比度、四态矩形稳定、实色底 + 配对前景。

use super::*;

fn luminance(c: egui::Color32) -> f32 {
    let lin = |v: u8| {
        let v = v as f32 / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
}

fn contrast(a: egui::Color32, b: egui::Color32) -> f32 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

fn setup(mode: neo_theme::ThemeMode, scale: f32) -> (egui::Context, Design) {
    let ctx = egui::Context::default();
    neo_theme::fonts::install(&ctx);
    let theme = neo_theme::Theme::from_metrics(mode, neo_theme::Metrics::from_scale(scale));
    theme.apply(&ctx);
    (ctx, Design::new(theme))
}

/// 状态徽标（Info/Success/Warn/Danger）必须使用实色底 + 配对前景，
/// 且配对对比度 ≥ 4.5（WCAG AA）。浮点精度容差 0.01。
#[test]
fn badge_state_tones_meet_contrast_floor() {
    for mode in [neo_theme::ThemeMode::Light, neo_theme::ThemeMode::Dark] {
        let (_, d) = setup(mode, 1.0);
        for tone in [
            BadgeTone::Info,
            BadgeTone::Success,
            BadgeTone::Warn,
            BadgeTone::Danger,
        ] {
            let badge = Badge::new("T").tone(tone);
            let (fill, fg) = badge.colors(&d);
            let ratio = contrast(fg, fill);
            assert!(
                ratio >= 4.49,
                "{mode:?}/{tone:?}: fg {fg:?} on bg {fill:?} = {ratio:.2} < 4.5"
            );
        }
    }
}

/// 状态徽标的底色必须是实色（不透明），不能是 alpha 叠加。
#[test]
fn badge_state_fills_are_solid() {
    for mode in [neo_theme::ThemeMode::Light, neo_theme::ThemeMode::Dark] {
        let (_, d) = setup(mode, 1.0);
        for tone in [
            BadgeTone::Info,
            BadgeTone::Success,
            BadgeTone::Warn,
            BadgeTone::Danger,
        ] {
            let badge = Badge::new("T").tone(tone);
            let (fill, _) = badge.colors(&d);
            assert_eq!(
                fill.a(),
                255,
                "{mode:?}/{tone:?}: 底色不是实色 alpha={}",
                fill.a()
            );
        }
    }
}

/// 中性徽标的对比度也要达标。
#[test]
fn badge_neutral_tone_meets_contrast_floor() {
    for mode in [neo_theme::ThemeMode::Light, neo_theme::ThemeMode::Dark] {
        let (_, d) = setup(mode, 1.0);
        let badge = Badge::new("T");
        let (fill, fg) = badge.colors(&d);
        let ratio = contrast(fg, fill);
        assert!(
            ratio >= 4.5,
            "{mode:?}/Neutral: fg {fg:?} on bg {fill:?} = {ratio:.2} < 4.5"
        );
    }
}

/// 徽标矩形在所有色调下尺寸一致（同文字、同字号、同内边距）。
#[test]
fn badge_rect_stable_across_tones() {
    for mode in [neo_theme::ThemeMode::Light, neo_theme::ThemeMode::Dark] {
        for scale in [0.85, 1.0, 1.75] {
            let (ctx, d) = setup(mode, scale);
            let mut rects = Vec::new();
            for tone in [
                BadgeTone::Neutral,
                BadgeTone::Accent,
                BadgeTone::Info,
                BadgeTone::Success,
                BadgeTone::Warn,
                BadgeTone::Danger,
            ] {
                let mut rect = None;
                let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                    rect = Some(Badge::new("v1.0").tone(tone).show(ui, &d));
                });
                output.textures_delta.clear();
                rects.push((tone, rect.expect("badge rect")));
            }
            let (_, first) = &rects[0];
            for (tone, rect) in &rects[1..] {
                assert_eq!(
                    first, rect,
                    "{mode:?} scale={scale}: {tone:?} 与 Neutral 矩形不同"
                );
            }
        }
    }
}

/// 徽标矩形不因 hover 而改变（纯展示控件，hover 不该改几何）。
#[test]
fn badge_rect_stable_on_hover() {
    for mode in [neo_theme::ThemeMode::Light, neo_theme::ThemeMode::Dark] {
        let (ctx, d) = setup(mode, 1.0);
        let idle_rect = {
            let mut rect = None;
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                rect = Some(Badge::new("状态").tone(BadgeTone::Success).show(ui, &d));
            });
            output.textures_delta.clear();
            rect.expect("idle rect")
        };
        // hover 到徽标位置
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
                rect = Some(Badge::new("状态").tone(BadgeTone::Success).show(ui, &d));
            });
            output.textures_delta.clear();
            rect.expect("hover rect")
        };
        assert_eq!(idle_rect, hover_rect, "{mode:?}: hover 改变了徽标矩形");
    }
}
