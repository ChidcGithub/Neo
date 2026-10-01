
    use super::*;
    use crate::ui::composer::ui_regression::{context, pointer, probe};

    #[test]
    fn long_confirmation_scrolls_without_moving_actions_and_blocks_batch_in_safe_mode() {
        for mode in [neo_theme::ThemeMode::Light, neo_theme::ThemeMode::Dark] {
            for width in [320.0, 560.0] {
                for (enabled, allow_batch) in [(true, false), (true, true), (false, true)] {
                    let ctx = context();
                    let mut meta = ToolMeta::restored("write_file");
                    meta.call_id = "long-confirm".into();
                    meta.preview = "完整动作预览 ".repeat(150);
                    meta.args = serde_json::json!({"content": "long-content-without-spaces".repeat(100)});
                    let size = Vec2::new(width, 420.0);
                    let answer = std::cell::Cell::new(None);
                    let mut render = |ui: &mut Ui, skin: &Skin<'_>| {
                        ui.add_enabled_ui(enabled, |ui| {
                            answer.set(confirm(ui, skin, &meta, 3, allow_batch));
                        });
                    };
                    let frame = |events, render: &mut dyn FnMut(&mut Ui, &Skin<'_>)| {
                        crate::ui::composer::ui_regression::frame_themed(&ctx, size, events, mode, render)
                    };
                    for _ in 0..3 { frame(vec![], &mut render); }
                    let (body, _, content_h): (Rect, f32, f32) = probe(&ctx, "neo-confirm-scroll-probe");
                    assert!(content_h > body.height());
                    let (batch, clip): (Rect, Rect) = probe(&ctx, ("neo-confirm-button-probe", Answer::Always as u8));
                    assert!(clip.contains_rect(batch));
                    assert!(batch.top() >= body.bottom());
                    if enabled {
                        for _ in 0..8 {
                            frame(vec![egui::Event::PointerMoved(body.center()), egui::Event::MouseWheel {
                                unit: egui::MouseWheelUnit::Point, delta: Vec2::new(0.0, -10000.0),
                                phase: egui::TouchPhase::Move, modifiers: egui::Modifiers::NONE,
                            }], &mut render);
                        }
                        let (body, offset, content_h): (Rect, f32, f32) = probe(&ctx, "neo-confirm-scroll-probe");
                        assert!(offset > 0.0);
                        assert!(offset + body.height() >= content_h - 1.0);
                    }
                    let (after, _): (Rect, Rect) = probe(&ctx, ("neo-confirm-button-probe", Answer::Always as u8));
                    assert_eq!(batch, after);
                    frame(pointer(batch.center(), true), &mut render);
                    frame(pointer(batch.center(), false), &mut render);
                    assert_eq!(answer.get(), (enabled && allow_batch).then_some(Answer::Always));
                    for action in [Answer::Once, Answer::Deny] {
                        let (button, _): (Rect, Rect) = probe(&ctx, ("neo-confirm-button-probe", action as u8));
                        frame(pointer(button.center(), true), &mut render);
                        frame(pointer(button.center(), false), &mut render);
                        assert_eq!(answer.get(), enabled.then_some(action));
                    }
                }
            }
        }
    }

    #[test]
    fn long_ask_options_wrap_scroll_to_tail_and_respect_disabled() {
        for mode in [neo_theme::ThemeMode::Light, neo_theme::ThemeMode::Dark] {
        let frame = |ctx: &egui::Context, size, events, draw: &mut dyn FnMut(&mut Ui, &Skin<'_>)| {
            crate::ui::composer::ui_regression::frame_themed(ctx, size, events, mode, draw)
        };
        for enabled in [false, true] {
            let ctx = context();
            let meta = ToolMeta {
                call_id: "pure-ask".into(), name: "ask_user".into(), title: "提问", risk: "read",
                preview: "question".into(), state: crate::state::ToolState::AwaitingConfirm,
                outcome: None,
                args: serde_json::json!({"question": "Choose", "options": format!("{}|{}", "long option with wrapping ".repeat(35), "last option tail ".repeat(35))}),
            };
            let size = Vec2::new(360.0, 420.0);
            let answer = std::cell::Cell::new(None);
            let mut render = |ui: &mut Ui, skin: &Skin<'_>| {
                ui.add_enabled_ui(enabled, |ui| { answer.set(ask(ui, skin, &meta)); });
            };
            for _ in 0..3 { frame(&ctx, size, vec![], &mut render); }
            let (first, clip, text, rows, response_enabled): (Rect, Rect, Vec2, usize, bool) =
                probe(&ctx, ("neo-ask-option-probe", 0usize));
            assert!(rows > 1);
            assert!(first.height() >= text.y + 19.0);
            assert!(text.x <= first.width() - 31.0);
            assert_eq!(response_enabled, enabled);
            let (skip, skip_clip): (Rect, Rect) = probe(&ctx, "neo-ask-skip-probe");
            assert!(skip_clip.contains_rect(skip));
            let (tail, _, _, _, _): (Rect, Rect, Vec2, usize, bool) =
                probe(&ctx, ("neo-ask-option-probe", 1usize));
            assert!(tail.bottom() > clip.bottom());
            // 实际滚轮事件只滚动正文，底部操作保持固定。
            if enabled {
                for _ in 0..8 {
                    frame(&ctx, size, vec![egui::Event::PointerMoved(clip.center()), egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point, delta: Vec2::new(0.0, -2000.0), phase: egui::TouchPhase::Move,
                        modifiers: egui::Modifiers::NONE,
                    }], &mut render);
                }
                let (tail, tail_clip, _, _, _): (Rect, Rect, Vec2, usize, bool) =
                    probe(&ctx, ("neo-ask-option-probe", 1usize));
                assert!(tail.bottom() <= tail_clip.bottom() + 1.0, "{tail:?}, {tail_clip:?}");
                let pos = egui::pos2(tail.center().x, tail.bottom() - 12.0);
                frame(&ctx, size, pointer(pos, true), &mut render);
                frame(&ctx, size, pointer(pos, false), &mut render);
                assert_eq!(answer.get(), Some(AskAnswer::Pick(1)));
                let (after, _): (Rect, Rect) = probe(&ctx, "neo-ask-skip-probe");
                assert_eq!(skip, after);
            } else {
                let pos = first.intersect(clip).center();
                frame(&ctx, size, pointer(pos, true), &mut render);
                frame(&ctx, size, pointer(pos, false), &mut render);
                assert_eq!(answer.get(), None);
                frame(&ctx, size, pointer(skip.center(), true), &mut render);
                frame(&ctx, size, pointer(skip.center(), false), &mut render);
                assert_eq!(answer.get(), None);
            }
        }
        }
    }
