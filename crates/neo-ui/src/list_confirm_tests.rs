use super::*;
use neo_theme::{Distance, Theme, ThemeMode};

fn design(mode: ThemeMode, viewport_h: f32, distance: Distance) -> Design {
    Design::new(Theme::new(mode, viewport_h, distance))
}

#[test]
fn flow_confirmation_wraps_targets_and_keyboard_is_safe() {
    for mode in [ThemeMode::Light, ThemeMode::Dark] {
        for scale in [0.85, 1.0, 1.75, 2.8] {
            for width in [240.0, 320.0, 560.0] {
                let ctx = egui::Context::default();
                neo_theme::fonts::install(&ctx);
                let theme = Theme::from_metrics(mode, neo_theme::Metrics::from_scale(scale));
                theme.apply(&ctx);
                let d = Design::new(theme);
                let question = "删除这条记忆？此操作无法撤销。Long question ".repeat(5);
                let draw = |opening, events| {
                    let mut result = None;
                    let mut output = ctx.run_ui(egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(width, 4000.0))),
                        events, ..Default::default()
                    }, |ui| {
                        let parent = Rect::from_min_size(egui::pos2(8.0, 8.0), Vec2::new(width - 16.0, 3984.0));
                        crate::at(ui, parent, |ui| {
                            ui.shrink_clip_rect(parent);
                            let response = ConfirmBar::new(&question).show(ui, &d, "safe-confirm", opening);
                            for rect in [response.cancel.rect, response.confirm.rect] {
                                assert!(ui.clip_rect().contains_rect(rect), "确认热区不得越过父级裁剪");
                            }
                            result = Some(response);
                        });
                    });
                    output.textures_delta.clear();
                    result.unwrap()
                };
                let key = |key| vec![egui::Event::Key { key, physical_key: None, pressed: true,
                    repeat: false, modifiers: egui::Modifiers::NONE }];
                let first = draw(true, vec![]);
                assert_eq!(first.outcome, ConfirmOutcome::None);
                let settled = draw(false, vec![]);
                assert!(settled.cancel.has_focus());
                assert_eq!(first.cancel.id, settled.cancel.id);
                assert!(settled.question.rect.bottom() <= settled.cancel.rect.top());
                assert!(!settled.cancel.rect.shrink(0.1).intersects(settled.confirm.rect.shrink(0.1)));
                for rect in [settled.cancel.rect, settled.confirm.rect] {
                    assert!(rect.right() <= width);
                    assert!(rect.height() >= d.m().hit_target(0.0));
                }
                assert_eq!(draw(false, key(egui::Key::Enter)).outcome, ConfirmOutcome::Cancel);
                draw(true, vec![]);
                draw(false, key(egui::Key::Tab));
                let focused = draw(false, vec![]);
                assert!(focused.confirm.has_focus(), "Tab {width}/{scale}");
                assert_eq!(draw(false, key(egui::Key::Enter)).outcome, ConfirmOutcome::Confirm);
                assert_eq!(draw(false, key(egui::Key::Escape)).outcome, ConfirmOutcome::Cancel);
                assert_eq!(draw(true, key(egui::Key::Enter)).outcome, ConfirmOutcome::None);
            }
        }
    }
}

/// **回归**：文案区必须严格在按钮左边（曾经重叠 22pt，把「删除」压在问句上）。
#[test]
fn label_never_overlaps_the_button() {
    let bar = Rect::from_min_size(egui::pos2(100.0, 50.0), Vec2::new(220.0, 32.0));
    for mode in [ThemeMode::Dark, ThemeMode::Light] {
        for h in [768.0f32, 1080.0, 2160.0] {
            for dist in [
                Distance::Standard,
                Distance::Classroom,
                Distance::Auditorium,
            ] {
                let d = design(mode, h, dist);
                let (label, btn) = confirm_geometry(&d, bar);
                assert!(
                    label.right() <= btn.left(),
                    "文案压到按钮上了：label.right={:.1} btn.left={:.1}（{mode:?} {h} {dist:?}）",
                    label.right(),
                    btn.left()
                );
                // 极端放大下文案区会退化成零宽（而不是负宽）—— 这也是对的，
                // 至少不会画到按钮上去。
                assert!(label.right() >= label.left(), "文案区出现了负宽度");
            }
        }
    }
}

/// 按钮贴着右边距，与原来的取消键同位。
#[test]
fn button_sits_at_the_right_edge() {
    let d = design(ThemeMode::Dark, 1080.0, Distance::Classroom);
    let bar = Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(220.0, 32.0));
    let (_, btn) = confirm_geometry(&d, bar);
    assert!((bar.right() - btn.right() - d.m().s(12.0)).abs() < 0.01);
    // 竖直居中
    assert!((btn.center().y - bar.center().y).abs() < 0.01);
}

/// 窄到放不下时，文案区退化成零宽（而不是"负宽"或跑到按钮右边）。
#[test]
fn degenerate_width_does_not_overflow() {
    let d = design(ThemeMode::Dark, 1080.0, Distance::Classroom);
    let bar = Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(40.0, 32.0));
    let (label, btn) = confirm_geometry(&d, bar);
    assert!(label.right() <= btn.left(), "窄行也不许压上去");
    assert!(label.width() >= 0.0);
}
