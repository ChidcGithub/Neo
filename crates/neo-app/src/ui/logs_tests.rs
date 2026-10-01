
    use super::*;
    use crate::diagnostics::{Stats, MAX_BYTES, MAX_ENTRIES, MAX_ENTRY_BYTES};

    #[test]
    fn readable_relative_time_and_single_event_format() {
        assert_eq!(relative_time(3_661_007), "+01:01:01.007");
        assert_eq!(relative_time(360_000_000), "+100:00:00.000");
        let view = sample();
        let repeated = format_entry(&view.entries[0]);
        assert!(repeated.contains("×3") && repeated.contains("首次 +00:00:00.001"));
        assert!(!format_entry(&view.entries[1]).contains("首次"));
        let mut event = view.entries[1].clone();
        event.message = "event=delivered id=7 parent=2 tool=click kind=io".into();
        assert!(matches_filter(&event, None, "parent=2"));
        assert!(matches_filter(&event, None, "kind=io"));
        assert!(export([&event]).contains("id=7 parent=2"));
    }

    fn sample() -> Snapshot {
        Snapshot {
            entries: Arc::new(vec![
                Entry {
                    level: Level::Error,
                    component: "Tool".into(),
                    message: "执行失败：timeout".into(),
                    first_ms: 1,
                    last_ms: 2,
                    occurrences: 3,
                },
                Entry {
                    level: Level::Info,
                    component: "app".into(),
                    message: "[已脱敏]".into(),
                    first_ms: 3,
                    last_ms: 3,
                    occurrences: 1,
                },
            ]),
            stats: Stats {
                received: 10,
                merged: 2,
                dropped: 6,
                truncated: 0,
                bytes: 50,
            },
            max_entries: MAX_ENTRIES,
            max_bytes: MAX_BYTES,
            max_entry_bytes: MAX_ENTRY_BYTES,
        }
    }

    #[test]
    fn filter_cache_reuses_indices_and_refreshes_for_conditions_and_snapshot() {
        let mut view = sample();
        view.entries = Arc::new((0..MAX_ENTRIES).map(|index| Entry {
            level: if index % 2 == 0 { Level::Info } else { Level::Error },
            component: "Tool".into(), message: format!("安全事件 {index}").into_boxed_str(),
            first_ms: index as u64, last_ms: index as u64, occurrences: 1,
        }).collect());
        let mut state = LogViewState { keyword: "安全事件".into(), ..Default::default() };
        let first = state.filtered_entries(&view);
        assert_eq!(first.indices.len(), MAX_ENTRIES);
        let mut legacy_matches = 0;
        let mut rebuilt = 0;
        for _ in 0..60 {
            let keyword = state.keyword.trim().to_lowercase();
            let legacy: Vec<_> = view.entries.iter().filter(|entry| {
                legacy_matches += 1;
                matches_filter(entry, state.level, &keyword)
            }).collect();
            assert_eq!(legacy.len(), first.indices.len());
            // egui get_temp 也会克隆状态：索引与日志文本都应继续共享。
            state = state.clone();
            let next = state.filtered_entries(&view.clone());
            rebuilt += usize::from(!Arc::ptr_eq(&first, &next));
        }
        assert_eq!(legacy_matches, 60 * MAX_ENTRIES);
        assert_eq!(rebuilt, 0);
        println!("1000项，预热后60次筛选：旧路径匹配次数={legacy_matches}，缓存索引重建={rebuilt}");
        state.level = Some(Level::Error);
        let errors = state.filtered_entries(&view);
        assert!(!Arc::ptr_eq(&first, &errors));
        assert_eq!(errors.indices.len(), 500);
        state.keyword = " TOOL ".into();
        let by_component = state.filtered_entries(&view);
        assert!(!Arc::ptr_eq(&errors, &by_component));
        assert_eq!(by_component.indices, errors.indices);
        state.keyword = "missing".into();
        assert!(state.filtered_entries(&view).indices.is_empty());
        state.keyword.clear();
        let old = Arc::downgrade(&state.filtered_entries(&view));
        let mut changed = view.clone();
        Arc::make_mut(&mut changed.entries)[1].occurrences = 9;
        let updated = state.filtered_entries(&changed);
        assert!(old.upgrade().is_none(), "仅保留当前筛选而非历史结果");
        assert_eq!(updated.entries[1].occurrences, 9);
        assert!(export(updated.indices.iter().map(|&index| &updated.entries[index])).contains("×9"));
        changed.entries = Arc::new(Vec::new());
        changed.stats = Stats::default();
        let cleared = state.filtered_entries(&changed);
        assert!(cleared.indices.is_empty());
        assert!(!Arc::ptr_eq(&updated, &cleared));
    }

    #[test]
    fn filters_and_copy_use_only_visible_safe_entries() {
        let view = sample();
        assert!(matches_filter(&view.entries[0], Some(Level::Error), "tool"));
        assert!(matches_filter(&view.entries[0], None, "失败"));
        assert!(!matches_filter(&view.entries[0], Some(Level::Warn), ""));
        assert!(!matches_filter(&view.entries[0], None, "missing"));
        let entries: Vec<_> = view
            .entries
            .iter()
            .filter(|entry| matches_filter(entry, Some(Level::Info), ""))
            .collect();
        let copied = export(entries);
        assert!(copied.contains("[已脱敏]"));
        assert!(!copied.contains("timeout"));
        let header = export([]);
        assert!(header.contains(env!("CARGO_PKG_VERSION")));
        assert!(header.contains("diagnostics v1"));
        assert!(!header.contains("timeout") && !header.contains("[已脱敏]"));
    }

    #[test]
    fn virtual_rows_pause_and_resume_without_laying_out_the_entire_buffer() {
        let ctx = egui::Context::default();
        let mut fonts = egui::FontDefinitions::default();
        let family = fonts.families[&egui::FontFamily::Proportional].clone();
        fonts.families.insert(neo_theme::fonts::bold(), family.clone());
        fonts.families.insert(neo_theme::fonts::mono(), family);
        ctx.set_fonts(fonts);
        let theme = neo_theme::Theme::new(neo_theme::ThemeMode::Dark, 1080.0, neo_theme::Distance::Standard);
        theme.apply(&ctx);
        let whale = crate::brand::WhaleMark::cached(&ctx);
        let skin = Skin::new(theme, &whale);
        let mut entries = vec![sample().entries[0].clone(); MAX_ENTRIES];
        for entry in entries.iter_mut().take(120) {
            entry.message = "安全概括".repeat(150).into_boxed_str();
        }
        let entries = Arc::new(entries);
        let mut state = LogViewState { follow: false, ..Default::default() };
        let render = |state: &LogViewState, count: usize, resume: bool| {
            let mut scroll = None;
            let mut output = ctx.run_ui(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(240.0, 700.0))),
                ..Default::default()
            }, |ui| {
                let visible = FilteredEntries {
                    entries: Arc::clone(&entries), level: None, keyword: String::new(),
                    indices: (0..count).collect(),
                };
                scroll = Some(state.draw_entries(ui, &skin, 224.0, &visible, resume));
                assert!(ui.min_rect().width() <= 240.0);
            });
            output.textures_delta.clear();
            assert!(output.platform_output.commands.is_empty());
            let scroll = scroll.unwrap();
            assert!(scroll.inner < 40, "不应排版全部 {count} 条日志");
            scroll
        };
        let initial = render(&state, 500, false);
        assert_eq!(initial.state.offset.y, 0.0);
        let grown = render(&state, MAX_ENTRIES, false);
        assert_eq!(grown.state.offset.y, 0.0);
        state.follow = true;
        let resumed = render(&state, MAX_ENTRIES, true);
        assert!(resumed.state.offset.y > 0.0);
        assert!((resumed.state.offset.y - (resumed.content_size.y - resumed.inner_rect.height())).abs() < 1.0);
        state.follow = false;
        let paused = render(&state, MAX_ENTRIES, false);
        assert!((paused.state.offset.y - resumed.state.offset.y).abs() < 1.0);
    }

    #[test]
    fn pointer_copy_pause_resume_and_wheel_match_the_visible_thousand_entry_view() {
        use crate::ui::composer::ui_regression::{context, frame, pointer, probe};
        let ctx = context();
        let size = egui::vec2(460.0, 900.0);
        let mut view = sample();
        view.entries = Arc::new((0..MAX_ENTRIES).map(|index| Entry {
            level: if index % 2 == 0 { Level::Info } else { Level::Error },
            component: "safe-test".into(), message: format!("安全事件 {index}").into_boxed_str(),
            first_ms: index as u64, last_ms: index as u64, occurrences: 1,
        }).collect());
        let mut state = LogViewState::default();
        let render = |state: &mut LogViewState, events| frame(&ctx, size, events,
            |ui, skin| state.draw_snapshot(ui, skin, 444.0, view.clone()));
        for _ in 0..3 { render(&mut state, vec![]); }
        let cached = state.filtered.clone().unwrap();
        for _ in 0..60 {
            render(&mut state, vec![]);
            assert!(Arc::ptr_eq(&cached, state.filtered.as_ref().unwrap()));
        }
        let (bottom, rect, content, count): (f32, egui::Rect, f32, usize) = probe(&ctx, "neo-logs-scroll-probe");
        assert!(state.follow && bottom > 0.0 && count < 40);
        assert!((bottom + rect.height() - content).abs() < 1.0);
        render(&mut state, vec![egui::Event::PointerMoved(rect.center()),
            egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, 150.0), modifiers: egui::Modifiers::NONE, phase: egui::TouchPhase::Move }]);
        for _ in 0..10 { render(&mut state, vec![]); }
        assert!(!state.follow, "manual scroll must expose the resume action");
        let button: egui::Rect = probe(&ctx, "neo-logs-follow");
        render(&mut state, pointer(button.center(), true));
        render(&mut state, pointer(button.center(), false));
        assert!(state.follow);
        let (offset, rect, content, _): (f32, egui::Rect, f32, usize) = probe(&ctx, "neo-logs-scroll-probe");
        assert!((offset + rect.height() - content).abs() < 1.0);
        render(&mut state, pointer(button.center(), true));
        render(&mut state, pointer(button.center(), false));
        assert!(!state.follow);
        state.level = Some(Level::Error);
        state.keyword = "SAFE-TEST".into();
        render(&mut state, vec![]);
        let copy: egui::Rect = probe(&ctx, "neo-logs-copy");
        render(&mut state, pointer(copy.center(), true));
        let output = render(&mut state, pointer(copy.center(), false));
        let copied = output.platform_output.commands.iter().find_map(|command| {
            if let egui::OutputCommand::CopyText(text) = command { Some(text) } else { None }
        }).expect("copy action must produce only a clipboard command");
        let filtered: Vec<_> = view.entries.iter().filter(|entry| entry.level == Level::Error).collect();
        assert_eq!(copied, &export(filtered));
        assert_eq!(copied.lines().count(), 502);
        assert!(!state.follow);
    }

    #[test]
    fn page_draws_headlessly_in_both_themes_and_narrow_widths() {
        for mode in [neo_theme::ThemeMode::Light, neo_theme::ThemeMode::Dark] {
            for width in [160.0, 240.0, 460.0] {
              for scale in [0.85, 1.0, 1.75, 2.8] {
                let ctx = egui::Context::default();
                let mut fonts = egui::FontDefinitions::default();
                let family = fonts.families[&egui::FontFamily::Proportional].clone();
                fonts
                    .families
                    .insert(neo_theme::fonts::bold(), family.clone());
                fonts.families.insert(neo_theme::fonts::mono(), family);
                ctx.set_fonts(fonts);
                let theme = neo_theme::Theme::from_metrics(mode, neo_theme::Metrics::from_scale(scale));
                theme.apply(&ctx);
                let whale = crate::brand::WhaleMark::cached(&ctx);
                let skin = Skin::new(theme, &whale);
                let mut state = LogViewState {
                    follow: false,
                    ..Default::default()
                };
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width, 700.0),
                        )),
                        ..Default::default()
                    },
                    |ui| {
                        state.draw_snapshot(ui, &skin, width - 16.0, sample());
                        assert!(ui.min_rect().width() <= width, "窄屏布局不应横向溢出");
                    },
                );
                // 纯 egui 测试不提交 GPU 纹理，显式消费待上传列表。
                output.textures_delta.clear();
                assert!(!output.shapes.is_empty());
                assert!(!state.follow);
                assert!(output.platform_output.commands.is_empty());
              }
            }
        }
    }
