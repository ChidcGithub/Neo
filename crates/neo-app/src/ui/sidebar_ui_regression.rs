
    use super::*;
    use crate::ui::composer::ui_regression::{context, frame};

    #[test]
    fn session_actions_and_delete_confirmation_have_separate_touch_targets() {
        use crate::ui::composer::ui_regression::{frame_themed, pointer};
        for mode in [neo_theme::ThemeMode::Light, neo_theme::ThemeMode::Dark] {
            for action in [RowAction::Rename, RowAction::Delete] {
                let ctx = context();
                let chosen = std::cell::Cell::new(None);
                let size = Vec2::new(268.0, 300.0);
                let mut render = |ui: &mut Ui, skin: &Skin<'_>| {
                    chosen.set(ListRow::new(1, &"long title ".repeat(30), "12:30")
                        .show_normal(ui, &skin.d(), 244.0));
                };
                for _ in 0..3 { frame_themed(&ctx, size, vec![], mode, &mut render); }
                let pen = ctx.read_response(Id::new(("neo-row-pen", 1i64))).unwrap().rect;
                let trash = ctx.read_response(Id::new(("neo-row-trash", 1i64))).unwrap().rect;
                assert!(pen.width() >= 48.0 && pen.height() >= 48.0);
                assert!(trash.width() >= 48.0 && trash.height() >= 48.0);
                assert!(pen.right() <= trash.left());
                let target = if action == RowAction::Rename { pen } else { trash };
                let pos = target.left_top() + Vec2::splat(2.0);
                frame_themed(&ctx, size, pointer(pos, true), mode, &mut render);
                frame_themed(&ctx, size, pointer(pos, false), mode, &mut render);
                assert_eq!(chosen.get(), Some(action));
            }
            let ctx = context();
            let size = Vec2::new(268.0, 300.0);
            let confirmed = std::cell::Cell::new(false);
            let mut render = |ui: &mut Ui, skin: &Skin<'_>| {
                let row = Rect::from_min_size(egui::pos2(8.0, 8.0), Vec2::new(244.0, ListRow::height(&skin.d())));
                confirmed.set(matches!(neo_ui::list::confirm_row(ui, &skin.d(), row, 1,
                    &ConfirmBar::new("删除这条会话？")), ConfirmOutcome::Confirm));
            };
            for _ in 0..3 { frame_themed(&ctx, size, vec![], mode, &mut render); }
            let hit = ctx.read_response(Id::new(("neo-confirm-del", 1i64))).unwrap().rect;
            assert!(hit.height() >= 48.0);
            let pos = hit.center_top() + egui::vec2(0.0, 2.0);
            frame_themed(&ctx, size, pointer(pos, true), mode, &mut render);
            frame_themed(&ctx, size, pointer(pos, false), mode, &mut render);
            assert!(confirmed.get(), "视觉按钮外、触控区内的点击也应生效");
        }
    }

    #[test]
    fn escape_consumption_reaches_sidebar_caller_only_for_pending_action() {
        for action in 0..3 {
            let ctx = context();
            let mut state = AppState::default();
            state.sessions = vec![neo_store::SessionRow { id: 1, title: "session".into(), updated_ms: 0 }];
            state.renaming = (action == 1).then_some(1);
            state.confirming_delete = (action == 2).then_some(1);
            state.generating = true;
            let events = vec![egui::Event::Key {
                key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false,
                modifiers: egui::Modifiers::NONE,
            }];
            let mut consumed = false;
            frame(&ctx, Vec2::new(300.0, 600.0), events, |ui, skin| {
                let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
                let out = draw(ui, skin, ui.max_rect(), &mut state, escape);
                consumed |= out.escape_consumed;
                assert!(out.renamed.is_none());
                assert!(out.delete_confirmed.is_none());
            });
            assert_eq!(consumed, action != 0);
            assert_eq!(state.renaming, None);
            assert_eq!(state.confirming_delete, None);
            assert!(state.generating);
        }
    }
