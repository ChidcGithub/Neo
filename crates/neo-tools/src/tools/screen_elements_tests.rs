
    use super::*;

    fn img(w: u32, h: u32) -> image::RgbaImage {
        image::RgbaImage::from_pixel(w, h, image::Rgba([20, 20, 22, 255]))
    }

    /// 点阵字形要可辨 —— 至少笔画量在一个合理区间（空字形/涂满都是 bug）。
    #[test]
    fn digit_bitmaps_are_plausible() {
        for (d, rows) in DIGITS.iter().enumerate() {
            let ink: usize = rows.iter().map(|r| r.count_ones() as usize).sum();
            assert!(
                (8..=25).contains(&ink),
                "数字 {d} 的笔画量 {ink} 不像 5×7 数字"
            );
        }
        // 手工核对两个签名特征：1 的顶旗、8 的双环
        assert_eq!(DIGITS[1][0], 0b00100);
        assert_eq!(DIGITS[8][3], 0b01110);
    }

    #[test]
    fn intent_hint_is_short_and_does_not_invent_unlabeled_purpose() {
        assert_eq!(intent_hint("Button"), "执行");
        assert_eq!(element_json(&elements(1, "")[0])["intent_hint"], "用途未知");
    }

    fn elements(count: usize, name: &str) -> Vec<screen_uia::ScreenElement> {
        (1..=count).map(|id| screen_uia::ScreenElement {
            id, role: "Button", name: name.into(), label: name.into(), label_source: "name", window: "窗口".into(), rect: [0, 0, 10, 10],
            identity: screen_uia::ElementIdentity::default(), ..Default::default()
        }).collect()
    }

    #[test]
    fn execution_metadata_never_promises_an_unverified_click() {
        let mut el = elements(1, "按钮").remove(0);
        el.enabled = true; el.foreground = true; el.actionable = true;
        el.identity = screen_uia::ElementIdentity { hwnd: 42, process_id: 7, process_started: 100,
            window_process_id: 7, window_process_started: 100, window_runtime_id: vec![4], runtime_id: vec![1] };
        let item = element_json(&el);
        assert_eq!(item["click_candidate"], true);
        assert!(item["clickable"].is_null());
        assert_eq!(item["non_executable_reason"], "requires_live_validation");
        for reason in ["disabled_or_unknown", "background_window", "offscreen_or_unknown", "no_click_action", "unverifiable_identity"] {
            let mut blocked = el.clone();
            match reason {
                "disabled_or_unknown" => blocked.enabled = false,
                "background_window" => blocked.foreground = false,
                "offscreen_or_unknown" => blocked.offscreen = true,
                "no_click_action" => blocked.actionable = false,
                _ => blocked.identity.runtime_id.clear(),
            }
            let item = element_json(&blocked);
            assert_eq!(item["click_candidate"], false);
            assert_eq!(item["clickable"], false);
            assert_eq!(item["non_executable_reason"], reason);
        }
    }

    #[test]
    fn overview_budget_preserves_identifiers_without_outer_truncation() {
        let windows: Vec<_> = (1..=256).map(|hwnd| screen_uia::WindowSummary {
            hwnd, title: "\u{0000}\\\"中文".repeat(1000), process_id: 42,
            rect: [-1920, 0, 800, 600], foreground: hwnd == 1, minimized: false,
        }).collect();
        let data = overview_data(&windows, false);
        assert!(outcome_bytes("screen_elements", "全桌面顶层窗口概览", &data) <= 8000);
        assert_eq!(data["truncated"], true);
        assert_eq!(data["windows"][0]["window_id"], "1");
        let out = Outcome::ok("screen_elements", "合成概览", data);
        let model: Value = serde_json::from_str(&crate::to_model_message(&out)).unwrap();
        assert!(model.get("truncated").is_none());
    }

    #[test]
    fn malformed_local_query_fails_without_desktop_access() {
        let tool = crate::find("screen_elements").unwrap();
        for value in [json!({"x": 0}), json!({"x": 0, "y": 0, "width": 0, "height": 10})] {
            assert!(query_region(&Args::new(tool, &value)).is_err());
        }
        assert!(parse_window_id("0").is_err());
        assert!(parse_window_id("abc").is_err());
        assert_eq!(parse_window_id("42").unwrap(), Some(42));
    }

    #[test]
    fn cancelled_page_never_annotates_publishes_or_advances_cursor() {
        use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
        let _interaction = screen_uia::INTERACTION.lock().unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let scope = Scope::new(std::env::temp_dir()).with_cancel(cancel.clone());
        let els = elements(80, "按钮");
        let mut cursor = PageCursor::default();
        let (id, first, ()) = publish_page(&scope, screen_uia::cache_generation(), "scope", &els, &mut cursor, |_| Ok(())).unwrap();
        let before = cursor.next_start;
        assert_eq!(before, first.end);
        cancel.store(true, Ordering::Release);
        let result = publish_page(&scope, screen_uia::cache_generation(), "scope", &els, &mut cursor,
            |_| -> Result<(), ToolError> { panic!("取消后不得标注/截图"); });
        assert!(result.is_err());
        assert_eq!(cursor.next_start, before);
        assert_eq!(screen_uia::cache_snapshot().unwrap().0, id);
        cancel.store(false, Ordering::Release);
        let result = publish_page(&scope, screen_uia::cache_generation(), "scope", &els, &mut cursor, |_| {
            cancel.store(true, Ordering::Release);
            Ok(())
        });
        assert!(result.is_err());
        assert_eq!(cursor.next_start, before);
        assert_eq!(screen_uia::cache_snapshot().unwrap().0, id);
        cancel.store(false, Ordering::Release);
        let result = publish_page(&scope, screen_uia::cache_generation(), "scope", &els, &mut cursor, |_| {
            screen_uia::cache_invalidate();
            screen_uia::cache_store(&elements(1, "较新观察"));
            Ok(())
        });
        assert!(result.is_err());
        assert_eq!(cursor.next_start, before);
        assert_eq!(screen_uia::cache_snapshot().unwrap().1[0].label, "较新观察");
        screen_uia::cache_invalidate();
    }

    #[test]
    fn page_build_failure_keeps_cursor_and_success_reuses_snapshot() {
        let _interaction = screen_uia::INTERACTION.lock().unwrap();
        let scope = Scope::new(std::env::temp_dir());
        let els = elements(80, "按钮");
        let mut cursor = PageCursor::default();
        let (id, first, ()) = publish_page(&scope, screen_uia::cache_generation(), "scope", &els, &mut cursor, |_| Ok(())).unwrap();
        let generation = screen_uia::cache_generation();
        let result = publish_page(&scope, generation, "scope", &els, &mut cursor,
            |_| Err::<(), _>(ToolError::io("假标注失败")));
        assert!(result.is_err());
        assert_eq!(cursor.next_start, first.end);
        let (second_id, second, ()) = publish_page(&scope, generation, "scope", &els, &mut cursor, |_| Ok(())).unwrap();
        assert_eq!(second_id, id);
        assert_eq!(second.start, first.end);
        assert_eq!(screen_uia::cache_generation(), generation);
        screen_uia::cache_invalidate();
    }

    #[test]
    fn cancelled_elements_and_overview_stop_before_desktop_access() {
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let scope = Scope::new(std::env::temp_dir()).with_cancel(cancel);
        let tool = crate::find("screen_elements").unwrap();
        for value in [json!({"annotate": true}), json!({"mode": "overview"})] {
            assert_eq!(act(&scope, &Args::new(tool, &value)).unwrap_err().kind, crate::cancelled_error().kind);
        }
    }

    #[test]
    fn waiting_uia_tools_cancel_without_desktop_access() {
        use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
        for (name, value) in [("screen_elements", json!({"annotate": true})),
            ("screen_elements", json!({"mode": "overview"})),
            ("screen_element_search", json!({"refresh": true})),
            ("screen_element_search", json!({"refresh": false}))] {
            let interaction = screen_uia::INTERACTION.lock().unwrap();
            let token = Arc::new(AtomicBool::new(false));
            let scope = Scope::new(std::env::temp_dir()).with_cancel(token.clone());
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let entered = barrier.clone();
            let worker = std::thread::spawn(move || {
                let tool = crate::find(name).unwrap();
                entered.wait();
                (tool.run)(&scope, &Args::new(tool, &value))
            });
            barrier.wait();
            token.store(true, Ordering::Release);
            drop(interaction);
            let outcome = worker.join().unwrap();
            assert_eq!(outcome.error.unwrap().kind, crate::cancelled_error().kind);
            assert!(outcome.images.is_empty());
        }
    }

    #[test]
    fn pagination_preserves_live_snapshot_and_resets_after_consumption_or_refresh() {
        let _interaction = screen_uia::INTERACTION.lock().unwrap();
        let els = elements(121, "按钮");
        let mut cursor = PageCursor::default();
        let first = page_snapshot("scope", &els, &mut cursor);
        let first_page = take_page("scope", &els, &mut cursor);
        assert_eq!(first_page.start, 0);
        assert_eq!(page_snapshot("scope", &els, &mut cursor), first);
        assert_eq!(take_page("scope", &els, &mut cursor).start, first_page.end);
        screen_uia::cache_invalidate();
        assert_ne!(page_snapshot("scope", &els, &mut cursor), first);
        assert_eq!(take_page("scope", &els, &mut cursor).start, 0);
        screen_uia::cache_store(&els);
        page_snapshot("scope", &els, &mut cursor);
        assert_eq!(take_page("scope", &els, &mut cursor).start, 0);
        screen_uia::cache_invalidate();
    }

    #[test]
    fn display_or_execution_state_change_resets_snapshot() {
        let _interaction = screen_uia::INTERACTION.lock().unwrap();
        let mut els = elements(80, "按钮");
        let mut cursor = PageCursor::default();
        let first = page_snapshot("scope", &els, &mut cursor);
        take_page("scope", &els, &mut cursor);
        els[0].label = "子文字已变化".into();
        let second = page_snapshot("scope", &els, &mut cursor);
        assert_ne!(first, second);
        assert_eq!(take_page("scope", &els, &mut cursor).start, 0);
        els[0].foreground = !els[0].foreground;
        assert_ne!(page_snapshot("scope", &els, &mut cursor), second);
        assert_eq!(take_page("scope", &els, &mut cursor).start, 0);
        screen_uia::cache_invalidate();
    }

    #[test]
    fn pages_rotate_with_same_scope() {
        let els = elements(121, "按钮");
        let mut cursor = PageCursor::default();
        let mut next = 0;
        let mut number = 0;
        loop {
            let page = take_page("scope", &els, &mut cursor);
            number += 1;
            assert_eq!((page.page, page.start), (number, next));
            assert!(page.end - page.start <= PAGE_SIZE);
            next = page.end;
            if next == els.len() { assert_eq!(page.pages, number); break; }
        }
        assert_eq!(take_page("scope", &els, &mut cursor).start, 0);
    }

    #[test]
    fn budget_pages_never_skip_elements_even_below_fifty() {
        for count in [30, 121] {
            let els = elements(count, &"中文\\".repeat(200));
            let mut cursor = PageCursor::default();
            let mut ids = Vec::new();
            let mut page_count = 0;
            loop {
                let page = take_page("scope", &els, &mut cursor);
                page_count += 1;
                assert_eq!(page.page, page_count);
                assert!(serde_json::to_string(&page.items).unwrap().len() <= JSON_BYTES_BUDGET);
                assert_eq!(page.items.len(), page.end - page.start);
                ids.extend(page.items.iter().map(|v| v["id"].as_u64().unwrap() as usize));
                if page.end == els.len() {
                    assert_eq!(page_count, page.pages);
                    break;
                }
                assert!(page.truncated);
                assert_eq!(cursor.next_start, page.end);
            }
            assert!(page_count > 1);
            assert_eq!(ids, (1..=count).collect::<Vec<_>>());
            assert_eq!(take_page("scope", &els, &mut cursor).start, 0);
        }
    }

    #[test]
    fn full_envelope_budget_preserves_every_page_reference() {
        for name in ["\u{0000}".repeat(400), "\\\"中文".repeat(400), "\u{0000}".repeat(30_000)] {
            let mut els = elements(121, &name);
            for el in &mut els { el.window = name.clone(); }
            let mut cursor = PageCursor { snapshot_id: "4294967295-18446744073709551615-18446744073709551615".into(), ..Default::default() };
            let mut ids = Vec::new();
            loop {
                let page = take_page("scope", &els, &mut cursor);
                let data = page_data(&cursor.snapshot_id, els.len(), &page.items, page.page, page.pages,
                    page.end < els.len(), page.truncated, false,
                    screen::Rect { x: -1920, y: 0, width: 3840, height: 1080 }, 60);
                let out = Outcome::ok("screen_elements", page_summary(els.len()), data);
                assert!(out.to_model_json(usize::MAX).len() <= JSON_BYTES_BUDGET);
                let text = crate::to_model_message(&out);
                assert!(text.len() <= JSON_BYTES_BUDGET);
                let model: Value = serde_json::from_str(&text).unwrap();
                assert!(model.get("truncated").is_none());
                let received = model["data"]["elements"].as_array().unwrap();
                assert_eq!(received.len(), page.end - page.start);
                for (index, item) in received.iter().enumerate() {
                    assert_eq!(item["id"], page.start + index + 1);
                    assert_eq!(item["element_id"], item["id"]);
                    ids.push(item["id"].as_u64().unwrap() as usize);
                    assert!(item["window_id"].is_string());
                    for key in ["x", "y", "w", "h"] { assert!(item[key].is_number()); }
                }
                if page.end == els.len() { break; }
            }
            assert_eq!(ids, (1..=els.len()).collect::<Vec<_>>());
        }
    }

    #[test]
    fn oversized_element_reaches_model_without_losing_id() {
        let mut els = elements(3, &"\u{0000}\\\"中文".repeat(10_000));
        for e in &mut els {
            e.window = e.name.clone();
        }
        let mut cursor = PageCursor::default();
        let page = take_page("scope", &els, &mut cursor);
        let out = Outcome::ok("screen_elements", "fixture", json!({"elements": page.items}));
        let model: Value = serde_json::from_str(&crate::to_model_message(&out)).unwrap();
        assert!(model.get("truncated").is_none());
        assert_eq!(model["data"]["elements"][0]["id"], 1);
        assert_eq!(model["data"]["elements"][0]["text_truncated"], true);
        assert!(out.to_model_json(usize::MAX).len() <= JSON_BYTES_BUDGET);
    }

    #[test]
    fn changed_focused_window_resets_cursor_even_with_same_count() {
        let mut cursor = PageCursor::default();
        let first = elements(80, "旧窗口按钮");
        let second = elements(80, "新窗口按钮");
        let page = take_page("focus|", &first, &mut cursor);
        assert_eq!(cursor.next_start, page.end);
        assert_eq!(take_page("focus|", &second, &mut cursor).start, 0);
    }

    #[test]
    fn page_scope_change_resets_to_first_page() {
        let els = elements(80, "按钮");
        let mut cursor = PageCursor::default();
        take_page("a", &els, &mut cursor);
        assert_eq!(take_page("b", &els, &mut cursor).start, 0);
        assert_eq!(take_page("b", &els[..10], &mut cursor).start, 0);
        assert!(take_page("empty", &[], &mut cursor).items.is_empty());
    }
    /// 标签画上去之后：黑底存在、白点存在，且都落在画布内（不 panic）。
    #[test]
    fn label_is_drawn_inside_canvas() {
        let mut im = img(1920, 1080);
        draw_label(&mut im, 7, 900, 500, 1920, 1080);
        let mut black = 0;
        let mut white = 0;
        for p in im.pixels() {
            if p.0 == [0, 0, 0, 230] {
                black += 1;
            }
            if p.0 == [255, 255, 255, 255] {
                white += 1;
            }
        }
        assert!(black > 100, "标签底色没画上：{black}");
        assert!(white > 10, "数字笔画没画上：{white}");
    }

    /// 编号标签贴屏幕顶时放进框内（y<0 的标签会整个丢掉）。
    #[test]
    fn label_clamps_below_top_edge() {
        let mut im = img(400, 300);
        draw_label(&mut im, 3, 10, 0, 400, 300); // y=0，标签上方放不下
        let white: usize = im.pixels().filter(|p| p.0 == [255, 255, 255, 255]).count();
        assert!(white > 0, "顶边元素的编号标签丢了");
    }

    /// 框画在部分出界的元素上不应 panic，且画布内的部分有墨。
    #[test]
    fn rect_clips_at_canvas_edges() {
        let mut im = img(200, 200);
        draw_rect(&mut im, [-10, -10, 60, 60], [255, 0, 0], 2);
        let red: usize = im.pixels().filter(|p| p.0[0] == 255 && p.0[1] == 0).count();
        assert!(red > 0, "出界框在画布内的部分应该有墨");
        draw_rect(&mut im, [500, 500, 60, 60], [255, 0, 0], 2); // 完全出界：no-op
    }

    /// UIA 使用虚拟桌面绝对坐标，截图位图使用以左上角为零的局部坐标。
    /// 左侧副屏会让虚拟桌面原点为负；标注前必须减掉该原点。
    #[test]
    fn annotation_translates_negative_virtual_desktop_origin() {
        let shot = screen::Shot {
            width: 200,
            height: 100,
            rgba: img(200, 100).into_raw(),
        };
        let els = vec![screen_uia::ScreenElement {
            id: 1,
            role: "Button",
            name: "副屏按钮".into(),
            window: "测试".into(),
            rect: [-1910, 30, 40, 20],
            identity: screen_uia::ElementIdentity::default(), ..Default::default()
        }];
        let marked = annotate_shot(&shot, &els, (-1920, 0)).expect("标注");
        let pixel = image::RgbaImage::from_raw(marked.width, marked.height, marked.rgba)
            .unwrap()
            .get_pixel(10, 30)
            .0;
        assert_eq!(pixel, [255, 72, 72, 255], "框应落在截图局部坐标 (10,30)");
    }
