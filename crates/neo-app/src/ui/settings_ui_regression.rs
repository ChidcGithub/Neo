
    use super::*;
    use crate::ui::composer::ui_regression::{context, frame, pointer, probe};

    #[test]
    fn ui_safety_narrow_settings_log_navigation_is_reachable() {
        for width in [320.0, 460.0] {
            let ctx = context();
            let loaded = neo_theme::fonts::install(&ctx);
            let mut state = AppState::default();
            let size = egui::vec2(width, 640.0);
            let mut render = |ui: &mut Ui, skin: &Skin<'_>| {
                panel(ui, skin, Rect::from_min_size(egui::pos2(8.0, 8.0), size - egui::vec2(16.0, 16.0)), &mut state, size.y, &loaded);
            };
            for _ in 0..2 { frame(&ctx, size, vec![], &mut render); }
            let nav: Rect = probe(&ctx, "settings-nav-probe");
            frame(&ctx, size, pointer(nav.center(), true), &mut render);
            frame(&ctx, size, pointer(nav.center(), false), &mut render);
            frame(&ctx, size, vec![], &mut render);
            let (item, clip): (Rect, Rect) = probe(&ctx, "settings-log-item-probe");
            assert!(clip.contains_rect(item));
            assert!(Rect::from_min_size(egui::Pos2::ZERO, size).contains_rect(item));
            frame(&ctx, size, pointer(item.center(), true), &mut render);
            frame(&ctx, size, pointer(item.center(), false), &mut render);
            let output = frame(&ctx, size, vec![], &mut render);
            assert_eq!(state.settings_tab, SettingsTab::Logs);
            assert!(!output.shapes.is_empty());
        }
    }

    #[test]
    fn narrow_scaled_settings_keep_warnings_and_scrolled_safety_text_readable() {
        for width in [240.0, 320.0, 460.0] {
            for scale in [0.85, 1.0, 1.75, 2.8] {
                for dpi in [0.85, 1.0, 2.8] {
                    let ctx = context();
                    let theme = neo_theme::Theme::from_metrics(neo_theme::ThemeMode::Dark,
                        neo_theme::Metrics::from_scale(scale));
                    theme.apply(&ctx);
                    let whale = crate::brand::WhaleMark::cached(&ctx);
                    let skin = Skin::new(theme, &whale);
                    let size = egui::vec2(width, 720.0);
                    let mut state = AppState::default();
                    state.preferences_unsaved = true;
                    let mut seen = std::collections::BTreeSet::new();
                    let mut expected = 0;
                    let mut warning_seen = std::collections::BTreeSet::new();
                    let mut warning_rows = 0;
                    for step in 0..65 {
                        let mut input = egui::RawInput {
                            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, size)),
                            time: Some(step as f64 / 10.0),
                            ..Default::default()
                        };
                        input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().native_pixels_per_point = Some(dpi);
                        if step > 1 {
                            input.events = vec![egui::Event::PointerMoved(egui::pos2(width * 0.5, 670.0)),
                                egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point,
                                    delta: egui::vec2(0.0, -45.0), modifiers: egui::Modifiers::NONE, phase: egui::TouchPhase::Move }];
                        }
                        let mut output = ctx.run_ui(input, |ui| {
                            panel(ui, &skin, Rect::from_min_size(egui::pos2(8.0, 8.0), size - egui::vec2(16.0, 16.0)),
                                &mut state, size.y, &LoadedFonts::default());
                        });
                        output.textures_delta.clear();
                        for clipped in &output.shapes {
                            if let egui::Shape::Text(text) = &clipped.shape {
                                if text.galley.job.text.starts_with("设置尚未保存") {
                                    warning_rows = text.galley.rows.len();
                                    assert!(!text.galley.elided);
                                    for (index, row) in text.galley.rows.iter().enumerate() {
                                        let rect = row.rect().translate(text.pos.to_vec2());
                                        assert!(rect.left() >= 0.0 && rect.right() <= width);
                                        if clipped.clip_rect.expand(1.0).contains_rect(rect) { warning_seen.insert(index); }
                                    }
                                }
                                if text.galley.job.text.starts_with("这不是离线模式") {
                                    expected = text.galley.rows.len();
                                    for (index, row) in text.galley.rows.iter().enumerate() {
                                        let rect = row.rect().translate(text.pos.to_vec2());
                                        assert!(rect.left() >= 0.0 && rect.right() <= width, "safety overflow {width}/{scale}/{dpi}");
                                        if clipped.clip_rect.expand(1.0).contains_rect(rect) { seen.insert(index); }
                                    }
                                }
                            }
                        }
                    }
                    assert!(warning_rows > 0);
                    assert_eq!(warning_seen.len(), warning_rows, "all warning lines reachable {width}/{scale}/{dpi}");
                    assert!(expected > 0);
                    assert_eq!(seen.len(), expected, "all safety lines reachable {width}/{scale}/{dpi}");
                }
            }
        }
    }

    #[test]
    fn model_credentials_hint_wraps_without_painting_the_key() {
        for width in [240.0, 320.0, 460.0] {
            for scale in [0.85, 1.0, 1.75, 2.8] {
                let ctx = context();
                let theme = neo_theme::Theme::from_metrics(neo_theme::ThemeMode::Light,
                    neo_theme::Metrics::from_scale(scale));
                theme.apply(&ctx);
                let whale = crate::brand::WhaleMark::cached(&ctx);
                let skin = Skin::new(theme, &whale);
                let mut state = AppState::default();
                state.api_key = "PRIVATE-KEY-NOT-FOR-DISPLAY".into();
                let mut output = ctx.run_ui(egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 6000.0))),
                    ..Default::default()
                }, |ui| {
                    ui.set_max_width(width - 16.0);
                    model_tab(ui, &skin, width - 16.0, &mut state);
                    assert!(ui.min_rect().width() <= width, "model overflow {width}/{scale}");
                });
                output.textures_delta.clear();
                let mut found = false;
                for clipped in &output.shapes {
                    if let egui::Shape::Text(text) = &clipped.shape {
                        assert!(!text.galley.job.text.contains(&state.api_key));
                        if text.galley.job.text.starts_with("密钥仅在本机持久化") {
                            found = true;
                            assert!(!text.galley.elided);
                            assert!(clipped.clip_rect.expand(1.0).contains_rect(Rect::from_min_size(text.pos, text.galley.size())));
                        }
                    }
                }
                assert!(found);
                assert!(output.platform_output.commands.is_empty());
            }
        }
    }

    #[test]
    fn five_thinking_choices_wrap_without_overlap_and_all_click() {
        for width in [200.0, 320.0, 720.0, 1200.0] {
            for scale in [0.85, 1.0, 1.75, 2.8] {
                let ctx = context();
                neo_theme::fonts::install(&ctx);
                let theme = neo_theme::Theme::from_metrics(neo_theme::ThemeMode::Light,
                    neo_theme::Metrics::from_scale(scale));
                theme.apply(&ctx);
                let whale = crate::brand::WhaleMark::cached(&ctx);
                let skin = Skin::new(theme, &whale);
                let labels: Vec<&str> = neo_llm::Thinking::ALL.iter().map(|t| t.label()).collect();
                let mut selected = 0;
                let mut draw = |events| {
                    let mut output = ctx.run_ui(egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 1600.0))),
                        events, ..Default::default()
                    }, |ui| {
                        ui.set_max_width(width - 16.0);
                        if let Some(i) = choice_row(ui, &skin, width - 16.0, &labels, selected, "test-thinking") {
                            selected = i;
                        }
                    });
                    output.textures_delta.clear();
                    output
                };
                draw(vec![]);
                let output = draw(vec![]);
                let rects: Vec<Rect> = (0..labels.len()).map(|i| probe(&ctx, ("test-thinking", i))).collect();
                for (i, rect) in rects.iter().enumerate() {
                    assert!(rect.left() >= 0.0 && rect.right() <= width, "choice overflow {width}/{scale}");
                    for other in &rects[i + 1..] { assert!(!rect.shrink(0.1).intersects(other.shrink(0.1)), "targets overlap {width}/{scale}: {rect:?}, {other:?}"); }
                }
                for clipped in &output.shapes {
                    if let egui::Shape::Text(text) = &clipped.shape {
                        if labels.contains(&text.galley.text()) {
                            let i = labels.iter().position(|label| *label == text.galley.text()).unwrap();
                            assert!(!text.galley.elided);
                            assert!(rects[i].expand(1.0).contains_rect(Rect::from_min_size(text.pos, text.galley.size())),
                                "label crosses its target {width}/{scale}");
                        }
                    }
                }
                drop(draw);
                for (i, rect) in rects.iter().enumerate() {
                    for pressed in [true, false] {
                        let mut output = ctx.run_ui(egui::RawInput {
                            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 1600.0))),
                            events: pointer(rect.center(), pressed), ..Default::default()
                        }, |ui| {
                            ui.set_max_width(width - 16.0);
                            if let Some(index) = choice_row(ui, &skin, width - 16.0, &labels, selected, "test-thinking") { selected = index; }
                        });
                        output.textures_delta.clear();
                    }
                    assert_eq!(selected, i, "choice not operable {width}/{scale}/{i}");
                }
            }
        }
    }

    #[test]
    fn long_values_and_section_titles_use_real_wrapped_height() {
        for width in [200.0, 320.0, 720.0] {
            for scale in [1.0, 1.75, 2.8] {
                let ctx = context();
                let theme = neo_theme::Theme::from_metrics(neo_theme::ThemeMode::Dark,
                    neo_theme::Metrics::from_scale(scale));
                theme.apply(&ctx);
                let whale = crate::brand::WhaleMark::cached(&ctx);
                let skin = Skin::new(theme, &whale);
                let values = ["provider/model-long-name-".repeat(8),
                    "D:\\School\\Neo\\very-long-database-path\\".repeat(6),
                    "VeryLongFontFamilyNameWithoutBreaks".repeat(8)];
                let mut output = ctx.run_ui(egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 12000.0))),
                    ..Default::default()
                }, |ui| {
                    ui.set_max_width(width - 16.0);
                    ui.spacing_mut().item_spacing = Vec2::ZERO;
                    section_label_row(ui, &skin, width - 16.0, "上下文预算（估算 token）");
                    for (key, value) in ["当前模型", "数据库", "字体"].iter().zip(&values) {
                        kv_row(ui, &skin, width - 16.0, key, value);
                    }
                    assert!(ui.min_rect().width() <= width);
                });
                output.textures_delta.clear();
                let mut bottom = 0.0;
                let mut found = 0;
                for clipped in &output.shapes {
                    if let egui::Shape::Text(text) = &clipped.shape {
                        let rect = Rect::from_min_size(text.pos, text.galley.size());
                        assert!(!text.galley.elided);
                        assert!(clipped.clip_rect.expand(1.0).contains_rect(rect));
                        assert!(rect.top() >= bottom - 1.0, "rows overlap {width}/{scale}");
                        bottom = rect.bottom();
                        if values.contains(&text.galley.job.text) { found += 1; }
                    }
                }
                assert_eq!(found, 3);
            }
        }
    }

    #[test]
    fn long_page_title_reserves_clickable_close_target() {
        for width in [240.0, 320.0, 720.0] {
            for scale in [1.0, 1.75, 2.8] {
                let ctx = context();
                let loaded = neo_theme::fonts::install(&ctx);
                let theme = neo_theme::Theme::from_metrics(neo_theme::ThemeMode::Light,
                    neo_theme::Metrics::from_scale(scale));
                theme.apply(&ctx);
                let whale = crate::brand::WhaleMark::cached(&ctx);
                let skin = Skin::new(theme, &whale);
                let size = egui::vec2(width, 420.0);
                let mut state = AppState::default();
                state.settings_tab = SettingsTab::WakeTest;
                state.preferences_unsaved = true;
                let mut closed = false;
                let mut render = |events| {
                    let mut output = ctx.run_ui(egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, size)),
                        events, ..Default::default()
                    }, |ui| {
                        closed = panel(ui, &skin, Rect::from_min_size(egui::pos2(8.0, 8.0), size - egui::vec2(16.0, 16.0)),
                            &mut state, size.y, &loaded);
                    });
                    output.textures_delta.clear();
                    output
                };
                render(vec![]);
                let output = render(vec![]);
                let (title, close): (Rect, Rect) = probe(&ctx, "settings-title-probe");
                assert!(title.contains_rect(close), "标题热区 {width}/{scale}: {title:?}, {close:?}");
                let mut found = false;
                for clipped in &output.shapes {
                    if let egui::Shape::Text(text) = &clipped.shape {
                        let rect = Rect::from_min_size(text.pos, text.galley.size());
                        if text.galley.text() == "麦克风测试" && rect.intersects(title) {
                            assert!(rect.right() < close.left());
                            assert!(title.expand(1.0).contains_rect(rect));
                            found = true;
                        }
                    }
                }
                assert!(found);
                render(pointer(close.center(), true));
                render(pointer(close.center(), false));
                drop(render);
                assert!(closed, "close not operable {width}/{scale}");
            }
        }
    }

    #[test]
    fn low_height_large_scale_keeps_body_scrollable_and_close_clear() {
        for (width, height, scale) in [(240.0, 360.0, 2.8), (320.0, 360.0, 2.8), (720.0, 300.0, 1.75), (1000.0, 320.0, 1.0)] {
            let ctx = context();
            let theme = neo_theme::Theme::from_metrics(neo_theme::ThemeMode::Dark,
                neo_theme::Metrics::from_scale(scale));
            theme.apply(&ctx);
            let whale = crate::brand::WhaleMark::cached(&ctx);
            let skin = Skin::new(theme, &whale);
            let size = egui::vec2(width, height);
            let mut state = AppState::default();
            state.preferences_unsaved = true;
            state.settings_tab = SettingsTab::About;
            state.db_path = Some("D:\\School\\database-long-path\\".repeat(8));
            let mut seen = std::collections::BTreeSet::new();
            let mut warning_rows = 0;
            let mut database_seen = false;
            for step in 0..140 {
                let events = if step < 2 { vec![] } else {
                    vec![egui::Event::PointerMoved(egui::pos2(width * 0.6, height - 50.0)),
                        egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point,
                            delta: egui::vec2(0.0, -20.0), modifiers: egui::Modifiers::NONE, phase: egui::TouchPhase::Move }]
                };
                let mut output = ctx.run_ui(egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, size)),
                    time: Some(step as f64 * 0.1), events, ..Default::default()
                }, |ui| {
                    panel(ui, &skin, Rect::from_min_size(egui::pos2(8.0, 8.0), size - egui::vec2(16.0, 16.0)),
                        &mut state, height, &LoadedFonts::default());
                });
                output.textures_delta.clear();
                let body: Rect = probe(&ctx, "settings-body-probe");
                assert!(body.height() >= skin.t().caption * 1.5, "body starved {width}/{height}/{scale}: {body:?}");
                let (title, close): (Rect, Rect) = probe(&ctx, "settings-title-probe");
                assert!(Rect::from_min_size(egui::Pos2::ZERO, size).contains_rect(close));
                for clipped in &output.shapes {
                    if let egui::Shape::Text(text) = &clipped.shape {
                        let rect = Rect::from_min_size(text.pos, text.galley.size());
                        if text.galley.text() == page_name(SettingsTab::About) && rect.intersects(title) {
                            assert!(rect.right() < close.left());
                        }
                        if text.galley.text().starts_with("设置尚未保存") {
                            warning_rows = text.galley.rows.len();
                            for (i, row) in text.galley.rows.iter().enumerate() {
                                if clipped.clip_rect.contains_rect(row.rect().translate(text.pos.to_vec2())) { seen.insert(i); }
                            }
                        }
                        if text.galley.text() == "数据库" && clipped.clip_rect.contains_rect(rect) { database_seen = true; }
                    }
                }
            }
            assert!(warning_rows > 0);
            assert_eq!(seen.len(), warning_rows);
            assert!(database_seen, "body unreachable {width}/{height}/{scale}");
        }
    }

    #[test]
    fn memory_controls_expose_full_accessible_labels_and_disabled_state() {
        let ctx = context();
        ctx.enable_accesskit();
        let output = frame(&ctx, egui::vec2(320.0, 600.0), vec![], |ui, skin| {
            let button = neo_ui::Button::new("完整的危险操作说明不能只剩省略号")
                .id_salt("disabled-semantic-button").enabled(false).touch_layout().show(ui, &skin.d());
            assert!(!button.enabled());
            let icon = IconButton::new(Icon::Trash).label("删除当前记忆，需再次确认")
                .id_salt("disabled-semantic-icon").enabled(false).show_touch(ui, &skin.d());
            assert!(!icon.enabled());
        });
        let tree = output.platform_output.accesskit_update.unwrap();
        for label in ["完整的危险操作说明不能只剩省略号", "删除当前记忆，需再次确认"] {
            let node = tree.nodes.iter().find(|(_, node)| node.label() == Some(label)).expect("full accessible button label");
            assert!(node.1.is_disabled());
        }
    }

    #[test]
    fn memory_delete_requires_confirmation_and_targets_do_not_overlap() {
        for mode in [neo_theme::ThemeMode::Light, neo_theme::ThemeMode::Dark] {
            for scale in [0.85, 1.0, 1.75, 2.8] {
                for width in [240.0, 320.0, 560.0] {
                    for cancel in [false, true] {
                        let ctx = context();
                        neo_theme::fonts::install(&ctx);
                        let theme = neo_theme::Theme::from_metrics(mode, neo_theme::Metrics::from_scale(scale));
                        theme.apply(&ctx);
                        let whale = crate::brand::WhaleMark::cached(&ctx);
                        let skin = Skin::new(theme, &whale);
                        let mut state = AppState::default();
                        state.memories = (1..=3).map(|id| neo_tools::tools::memory::Memory {
                            id, content: "memory".into(), updated_ms: 0,
                        }).collect();
                        let calls = std::cell::Cell::new(0);
                        let mut render = |events| {
                            let mut output = ctx.run_ui(egui::RawInput {
                                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 6000.0))),
                                events, ..Default::default()
                            }, |ui| {
                                ui.set_max_width(width - 16.0);
                                memory_tab_with_actions(ui, &skin, width - 16.0, &mut state,
                                    |_, _| panic!("unexpected edit"), |id| {
                                        calls.set(calls.get() + 1);
                                        Ok(Some(neo_tools::tools::memory::Memory { id, content: "memory".into(), updated_ms: 0 }))
                                    });
                            });
                            output.textures_delta.clear();
                        };
                        render(vec![]); render(vec![]);
                        let mut rects = Vec::new();
                        for id in 1..=3_u64 {
                            let (pen, trash): (Rect, Rect) = probe(&ctx, ("neo-memory-actions-probe", id));
                            rects.extend([pen, trash]);
                        }
                        for (i, rect) in rects.iter().enumerate() {
                            assert!(rect.right() <= width);
                            for other in &rects[i + 1..] { assert!(!rect.shrink(0.1).intersects(other.shrink(0.1))); }
                        }
                        render(pointer(rects[1].center(), true));
                        render(pointer(rects[1].center(), false));
                        assert_eq!(calls.get(), 0);
                        render(vec![]); render(vec![]);
                        assert_eq!(calls.get(), 0);
                        let (cancel_rect, confirm_rect): (Rect, Rect) = probe(&ctx, "neo-memory-confirm-probe");
                        let target = if cancel { cancel_rect } else { confirm_rect };
                        render(pointer(target.center(), true));
                        render(pointer(target.center(), false));
                        assert_eq!(calls.get(), if cancel { 0 } else { 1 });
                        for _ in 0..4 { render(vec![]); }
                        render(pointer(target.center(), false));
                        assert_eq!(calls.get(), if cancel { 0 } else { 1 }, "后续帧不得重复删除");
                        drop(render);
                        assert_eq!(state.memories.len(), if cancel { 3 } else { 2 });
                        assert_eq!(state.memories[0].id, if cancel { 1 } else { 2 });
                    }
                }
            }
        }
    }

    #[test]
    fn long_memory_list_scrolls_back_to_error_owner_and_preserves_draft() {
        let ctx = context();
        let theme = neo_theme::Theme::from_metrics(neo_theme::ThemeMode::Dark, neo_theme::Metrics::from_scale(1.0));
        theme.apply(&ctx);
        let whale = crate::brand::WhaleMark::cached(&ctx);
        let skin = Skin::new(theme, &whale);
        let mut state = AppState::default();
        state.memories = (1..=80).map(|id| neo_tools::tools::memory::Memory {
            id, content: "original".into(), updated_ms: 0,
        }).collect();
        state.memory_editing = Some((1, "unsaved draft".into()));
        let mut step = 0;
        let mut render = |events, bottom| {
            step += 1;
            let mut output = ctx.run_ui(egui::RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(320.0, 480.0))),
                time: Some(step as f64 * 0.1), events, ..Default::default()
            }, |ui| {
                let mut scroll = egui::ScrollArea::vertical().id_salt("error-owner-scroll");
                if bottom { scroll = scroll.vertical_scroll_offset(100000.0); }
                scroll.show(ui, |ui| {
                    ui.set_max_width(280.0);
                    memory_tab_with_actions(ui, &skin, 280.0, &mut state,
                        |_, _| panic!("不得写入真实记忆"), |_| panic!("不得删除真实记忆"));
                });
            });
            output.textures_delta.clear();
        };
        render(vec![], true); render(vec![], true); render(vec![], false);
        let (pen, _): (Rect, Rect) = probe(&ctx, ("neo-memory-actions-probe", 80_u64));
        assert!(pen.is_positive() && pen.top() > 0.0 && pen.bottom() < 480.0);
        render(pointer(pen.center(), true), false);
        render(pointer(pen.center(), false), false);
        for _ in 0..20 { render(vec![], false); }
        let (notice, clip): (Rect, Rect) = probe(&ctx, "neo-memory-error-probe");
        assert!(clip.contains_rect(notice), "应从列表底部回到草稿所在行: {notice:?}/{clip:?}");
        drop(render);
        assert_eq!(state.memory_editing, Some((1, "unsaved draft".into())));
        assert_eq!(state.memories.len(), 80);
    }

    #[test]
    fn long_memory_list_keeps_failed_first_row_visible() {
        let ctx = context();
        let mut state = AppState::default();
        state.memories = (1..=80).map(|id| neo_tools::tools::memory::Memory {
            id, content: "original".into(), updated_ms: 0,
        }).collect();
        state.memory_editing = Some((1, "draft".into()));
        let size = egui::vec2(320.0, 480.0);
        let mut render = |ui: &mut Ui, skin: &Skin<'_>| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.set_max_width(280.0);
                memory_tab_with_actions(ui, skin, 280.0, &mut state,
                    |_, _| Err(neo_tools::ToolError::io("write denied")), |_| panic!("unexpected delete"));
            });
        };
        frame(&ctx, size, vec![], &mut render);
        frame(&ctx, size, vec![], &mut render);
        let save: Rect = probe(&ctx, "neo-memory-save-probe");
        frame(&ctx, size, pointer(save.center(), true), &mut render);
        frame(&ctx, size, pointer(save.center(), false), &mut render);
        for _ in 0..8 { frame(&ctx, size, vec![], &mut render); }
        let (notice, clip): (Rect, Rect) = probe(&ctx, "neo-memory-error-probe");
        assert!(clip.expand(1.0).contains_rect(notice));
        assert_eq!(state.memory_editing, Some((1, "draft".into())));
    }

    #[test]
    fn failed_memory_save_keeps_draft_and_paints_error() {
        for missing in [false, true] {
            let ctx = context();
            let mut state = AppState::default();
            state.memories = vec![neo_tools::tools::memory::Memory {
                id: 7, content: "original".into(), updated_ms: 0,
            }];
            state.memory_editing = Some((7, "  unsaved draft  ".into()));
            let mut calls = 0;
            let size = egui::vec2(640.0, 600.0);
            let mut render = |ui: &mut Ui, skin: &Skin<'_>| {
                memory_tab_with_edit(ui, skin, 580.0, &mut state, |id, draft| {
                    calls += 1;
                    assert_eq!(id, 7);
                    assert_eq!(draft, "unsaved draft");
                    if missing { Ok(None) } else { Err(neo_tools::ToolError::io("test write denied")) }
                });
            };
            for _ in 0..2 { frame(&ctx, size, vec![], &mut render); }
            let save: Rect = probe(&ctx, "neo-memory-save-probe");
            frame(&ctx, size, pointer(save.center(), true), &mut render);
            frame(&ctx, size, pointer(save.center(), false), &mut render);
            let output = frame(&ctx, size, vec![], &mut render);
            assert_eq!(calls, 1);
            assert_eq!(state.memory_editing, Some((7, "  unsaved draft  ".into())));
            assert_eq!(state.memories[0].content, "original");
            let message: String = probe(&ctx, "neo-memory-write-error");
            assert!(message.contains("草稿已保留"));
            assert!(message.contains(if missing { "已不存在" } else { "test write denied" }));
            assert!(output.shapes.iter().any(|clipped| {
                if let egui::Shape::Text(text) = &clipped.shape {
                    text.galley.job.text == message
                        && clipped.clip_rect.contains_rect(Rect::from_min_size(text.pos, text.galley.size()))
                } else { false }
            }), "error label must be painted inside the visible clip");
        }
    }
