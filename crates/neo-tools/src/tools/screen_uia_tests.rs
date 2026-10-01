
    use super::*;
    use crate::tools::screen::Rect;

    fn vs() -> Rect {
        Rect {
            x: 0,
            y: 0,
            width: 3200,
            height: 2000,
        }
    }

    fn raw(name: &str, rect: [i32; 4]) -> RawElement {
        RawElement {
            role: "Button",
            name: name.to_owned(), label: display_label(name), label_source: "name",
            enabled: true, foreground: true, offscreen: false, actionable: false, clickable_point: None,
            window: "窗口".to_owned(),
            rect,
            parent_role: None,
            identity: ElementIdentity::default(),
        }
    }

    #[test]
    fn cancelled_and_late_publishers_cannot_replace_new_snapshot() {
        let _interaction = INTERACTION.lock().unwrap();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let scope = crate::Scope::new(std::env::temp_dir()).with_cancel(cancel.clone());
        let old_generation = cache_generation();
        cache_invalidate();
        let newer = cache_store(&[]);
        let late = cache_store_checked(&[], &scope, old_generation);
        let unchanged = cache_snapshot().unwrap().0 == newer;
        cancel.store(true, std::sync::atomic::Ordering::Release);
        let cancelled = cache_store_checked(&[], &scope, cache_generation());
        let still_unchanged = cache_snapshot().unwrap().0 == newer;
        let stale_invalidation = cache_invalidate_checked(&crate::Scope::new(std::env::temp_dir()), old_generation);
        let not_cleared = cache_snapshot().is_some();
        cache_invalidate();
        drop(_interaction);
        assert!(late.is_err(), "失效前启动的发布者必须拒绝");
        assert!(unchanged && still_unchanged, "旧代或已取消发布者不能覆盖新快照");
        assert!(cancelled.is_err(), "取消的 Scope 必须拒绝发布");
        assert!(stale_invalidation.is_err() && not_cleared, "旧观察失败也不能清空新缓存");
    }

    #[test]
    fn checked_publish_survives_job_drop_but_not_explicit_invalidation() {
        let _interaction = INTERACTION.lock().unwrap();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let scope = crate::Scope::new(std::env::temp_dir()).with_cancel(cancel.clone());
        let id = cache_store_checked(&[], &scope, cache_generation()).unwrap();
        let at = CACHE.lock().unwrap().as_ref().unwrap().at;
        let generation = cache_generation();
        assert_eq!(cache_publish_checked(&[], &scope, generation, Some(&id)).unwrap(), id);
        assert_eq!(CACHE.lock().unwrap().as_ref().unwrap().at, at);
        assert_eq!(cache_generation(), generation);
        cancel.store(true, std::sync::atomic::Ordering::Release);
        drop(scope);
        assert_eq!(cache_snapshot().unwrap().0, id, "正常任务结束不使成功快照失效");
        cache_invalidate();
        assert!(cache_snapshot().is_none());
        assert_ne!(cache_generation(), generation);
        let empty_generation = cache_generation();
        cache_invalidate();
        assert_ne!(cache_generation(), empty_generation);
    }

    #[test]
    fn publication_waiting_on_cache_lock_rechecks_cancellation_inside_lock() {
        let _interaction = INTERACTION.lock().unwrap();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let scope = crate::Scope::new(std::env::temp_dir()).with_cancel(cancel.clone());
        let newer = cache_store(&[]);
        let generation = cache_generation();
        let lock = CACHE.lock().unwrap();
        let (started, ready) = std::sync::mpsc::channel();
        let result = std::thread::scope(|threads| {
            let job = threads.spawn(|| {
                started.send(()).unwrap();
                cache_store_checked(&[], &scope, generation)
            });
            ready.recv().unwrap();
            cancel.store(true, std::sync::atomic::Ordering::Release);
            drop(lock);
            job.join().unwrap()
        });
        assert!(result.is_err());
        assert_eq!(cache_snapshot().unwrap().0, newer);
        cache_invalidate();
    }

    #[test]
    fn window_filter_selects_targets_before_spending_element_budget() {
        let windows = [("其他窗口", 800), ("目标 EDITOR", 2), ("Editor 第二窗口", 3)];
        let mut remaining = 4;
        let mut visited = Vec::new();
        for (title, count) in windows {
            if window_matches(title, Some("editor")) {
                let taken = count.min(remaining);
                visited.push((title, taken));
                remaining -= taken;
            }
        }
        assert_eq!(visited, vec![("目标 EDITOR", 2), ("Editor 第二窗口", 2)]);
        assert!(window_matches("任意窗口", None));
        assert!(window_matches("ÄBC", Some("äb")));
        assert!(!window_matches("其他窗口", Some("目标")));
    }

    /// 里程碑式的 happy path：过滤、排序、编号一条龙。
    #[test]
    fn finalize_sorts_by_reading_order_and_numbers() {
        let els = finalize(
            vec![
                raw("右下", [500, 400, 100, 40]),
                raw("左上", [10, 10, 100, 40]),
                raw("同行右", [200, 10, 100, 40]),
            ],
            vs(),
            800,
        );
        let ids: Vec<(usize, &str)> = els.iter().map(|e| (e.id, e.name.as_str())).collect();
        assert_eq!(ids, vec![(1, "左上"), (2, "同行右"), (3, "右下")]);
    }

    /// 本机实测的两大噪音源：无名元素与滚动条步进按钮。
    #[test]
    fn noise_is_filtered() {
        let els = finalize(
            vec![
                raw("", [10, 10, 100, 40]),             // 无名
                raw("垂直小幅下降", [10, 60, 24, 100]), // 滚动条步进
                raw("真按钮", [10, 200, 100, 40]),
            ],
            vs(),
            800,
        );
        assert_eq!(els.len(), 1);
        assert_eq!(els[0].name, "真按钮");
    }

    /// 父元素是 ScrollBar 的子按钮一律丢（比名字特征更可靠的信号）。
    #[test]
    fn scrollbar_children_are_dropped_by_parent_type() {
        let mut sb = raw("拖我", [10, 60, 24, 100]);
        sb.parent_role = Some(SCROLLBAR);
        let els = finalize(vec![sb, raw("真按钮", [10, 200, 100, 40])], vs(), 800);
        assert_eq!(els.len(), 1);
    }

    /// 叶子级可见性：部分露出窗口的按钮仍然保留（bbox 与桌面相交即可），
    /// 但完全在桌面外的不留 —— 这就是"不能用 IsOffscreen 剪枝"的正确替代。
    #[test]
    fn visibility_is_decided_by_bbox_intersection() {
        let els = finalize(
            vec![
                raw("半露出", [-50, 10, 100, 40]),  // 左半在屏幕外
                raw("全在外", [-500, 10, 100, 40]), // 完全在桌面左侧之外
                raw("在桌面内", [10, 10, 100, 40]),
            ],
            vs(),
            800,
        );
        let names: Vec<&str> = els.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["半露出", "在桌面内"]);
    }

    #[test]
    fn duplicates_are_collapsed() {
        let els = finalize(
            vec![
                raw("同一个", [10, 10, 100, 40]),
                raw("同一个", [10, 10, 100, 40]),
            ],
            vs(),
            800,
        );
        assert_eq!(els.len(), 1);
    }

    #[test]
    fn budget_truncates_after_numbering() {
        let raws: Vec<RawElement> = (0..50)
            .map(|i| raw(&format!("b{i}"), [0, i * 50, 100, 40]))
            .collect();
        let els = finalize(raws, vs(), 10);
        assert_eq!(els.len(), 10);
        assert_eq!(els.last().unwrap().id, 10);
    }

    #[test]
    fn cache_round_trip_and_expiry_semantics() {
        let els = vec![ScreenElement {
            id: 1,
            role: "Button",
            name: "开始".into(),
            window: "任务栏".into(),
            rect: [1061, 1904, 90, 96],
            identity: ElementIdentity::default(),
            ..Default::default()
        }];
        let _interaction = INTERACTION.lock().unwrap();
        let snapshot = cache_store(&els);
        let got = cache_lookup(&snapshot, 1).expect("刚存过就应当查得到");
        assert_eq!(got.name, "开始");
        assert_eq!(got.center(), (1106, 1952));
        assert!(cache_lookup(&snapshot, 99).is_none(), "没存过的编号查不到");
        let newer = cache_store(&els);
        assert!(cache_lookup(&snapshot, 1).is_none());
        assert!(cache_lookup(&newer, 1).is_some());
        CACHE.lock().unwrap().as_mut().unwrap().at = Instant::now() - Duration::from_secs(59);
        assert!(cache_snapshot().is_some());
        assert!(cache_remaining_seconds() <= 1);
        let at = CACHE.lock().unwrap().as_ref().unwrap().at;
        assert!(cache_snapshot().is_some());
        assert_eq!(CACHE.lock().unwrap().as_ref().unwrap().at, at);
        cache_invalidate();
        assert!(cache_snapshot().is_none());
        assert!(cache_lookup(&newer, 1).is_none());
    }

    #[test]
    fn snapshot_is_consumed_once_across_threads_without_desktop_access() {
        let _interaction = INTERACTION.lock().unwrap();
        let els = finalize(vec![raw("保存", [0, 0, 100, 40])], vs(), 1);
        let snapshot = cache_store(&els);
        let barrier = std::sync::Barrier::new(8);
        let successes = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8).map(|_| scope.spawn(|| {
                barrier.wait();
                usize::from(cache_consume(&snapshot, 1).is_some())
            })).collect();
            handles.into_iter().map(|h| h.join().unwrap()).sum::<usize>()
        });
        assert_eq!(successes, 1);
        assert!(cache_snapshot().is_none());
        assert!(validate_target(&els[0]).is_err());
    }

    #[test]
    fn stale_ambiguous_and_changed_targets_fail_closed() {
        let mut el = finalize(vec![raw("保存", [10, 10, 100, 40])], vs(), 10).remove(0);
        el.identity = ElementIdentity { hwnd: 42, process_id: 7, process_started: 100,
            window_process_id: 7, window_process_started: 100, window_runtime_id: vec![4, 5], runtime_id: vec![1, 2, 3] };
        let mut cached = Cached { at: Instant::now(), id: "snapshot".into(), els: vec![el.clone()] };
        assert!(lookup(&cached, "snapshot", 1).is_some());
        assert!(lookup(&cached, "", 1).is_none());
        assert!(lookup(&cached, "old", 1).is_none());
        cached.at = Instant::now() - CACHE_TTL - Duration::from_secs(1);
        assert!(lookup(&cached, "snapshot", 1).is_none());
        cached.at = Instant::now();
        cached.els.push(el.clone());
        assert!(lookup(&cached, "snapshot", 1).is_none());
        assert!(same_target(&el, &el, true, false));
        assert!(!same_target(&el, &el, false, false));
        assert!(!same_target(&el, &el, true, true));
        let mut changed = el.clone();
        changed.rect[0] += 1;
        assert!(!same_target(&el, &changed, true, false));
        changed = el.clone(); changed.identity.hwnd += 1;
        assert!(!same_target(&el, &changed, true, false));
        changed = el.clone(); changed.identity.window_runtime_id.push(9);
        assert!(!same_target(&el, &changed, true, false));
        changed = el.clone(); changed.identity.process_started += 1;
        assert!(!same_target(&el, &changed, true, false));
        changed = el.clone(); changed.identity.window_process_started += 1;
        assert!(!same_target(&el, &changed, true, false));
        changed = el.clone(); changed.identity.runtime_id.push(9);
        assert!(!same_target(&el, &changed, true, false));
        changed = el.clone(); changed.name = "删除".into();
        assert!(!same_target(&el, &changed, true, false));
        changed = el.clone(); changed.identity.runtime_id.clear();
        assert!(!same_target(&changed, &changed, true, false));
    }

    #[test]
    fn local_query_filters_before_result_budget_and_handles_negative_coordinates() {
        let query = Query { keyword: "ä保", window_id: Some(42), region: Some([-1920, 0, 200, 200]), ..Default::default() };
        assert!(query.matches("Ä保存", [-1900, 10, 100, 40]));
        assert!(!query.matches("Ä保存", [100, 10, 100, 40]));
        assert!(!query.matches("删除", [-1900, 10, 100, 40]));
        assert!(intersects([i32::MAX - 1, 0, 100, 1], [i32::MAX, 0, 1, 1]));
        assert!(!intersects([0, 0, 10, 10], [10, 0, 10, 10]));
    }

    #[test]
    fn labels_do_not_change_raw_identity_and_unlabeled_actions_survive() {
        let full = format!("  {}尾部", "中".repeat(120));
        let mut source = raw(&full, [0, 0, 100, 40]);
        source.identity = ElementIdentity { hwnd: 42, process_id: 7, process_started: 100,
            window_process_id: 7, window_process_started: 100, window_runtime_id: vec![4], runtime_id: vec![1] };
        let expected = finalize(vec![source], vs(), 1).remove(0);
        assert_eq!(expected.name, full);
        assert_eq!(expected.label.chars().count(), LABEL_CHARS);
        let mut current = expected.clone();
        current.label = "不同显示文字".into(); current.label_source = "child_text";
        assert!(same_target(&expected, &current, true, false));
        current.name.push('变');
        assert!(!same_target(&expected, &current, true, false));
        let mut unnamed = raw("", [0, 0, 10, 10]);
        unnamed.actionable = true; unnamed.label_source = "unlabeled";
        let result = finalize(vec![unnamed], vs(), 1);
        assert_eq!(result.len(), 1);
        assert!(result[0].label.is_empty());
        let mut executable = expected.clone(); executable.actionable = true;
        assert!(executable.non_executable_reason().is_none());
        executable.foreground = false;
        assert_eq!(executable.non_executable_reason(), Some("background_window"));
        assert!(validate_target(&executable).is_err());
    }

    #[test]
    fn label_limits_exclude_password_input_and_nested_actions() {
        assert!(label_node_allowed("Text", false, false, false, 2, 16));
        for (role, action, focus, password, depth, visits) in [
            ("Text", false, false, true, 1, 1), ("Edit", false, false, false, 1, 1),
            ("Button", false, false, false, 1, 1), ("Text", true, false, false, 1, 1),
            ("Text", false, true, false, 1, 1), ("Text", false, false, false, 3, 1),
            ("Text", false, false, false, 1, 17),
        ] { assert!(!label_node_allowed(role, action, focus, password, depth, visits)); }
        let mut pieces = Vec::new();
        for text in ["甲", "乙", "丙", "丁"] { add_label_piece(&mut pieces, text); }
        assert_eq!(pieces.len(), 3);
        assert_eq!(display_label(&"中".repeat(200)).chars().count(), 96);
        assert!(!passive_content("Text", false, false, true));
        assert!(passive_content("Image", false, false, false));
        assert!(!passive_content("Unknown", false, false, false));
        assert!(!passive_content("Text", true, false, false));
        assert!(!passive_content("Image", false, true, false));
        assert!(!label_node_allowed("Unknown", false, false, false, 1, 1));
        assert!(!label_node_allowed("Pane", true, false, false, 1, 1));
    }

    #[test]
    fn partial_offscreen_candidates_use_visible_pixels_not_original_center_or_gaps() {
        let rect = [-90, 10, 100, 40];
        let monitors = [[0, 0, 100, 100], [200, 0, 100, 100]];
        let points = candidate_points(rect, Some((-40, 30)), &monitors);
        assert!(!points.is_empty());
        assert!(points.iter().all(|p| point_in(rect, *p) && point_in(monitors[0], *p)));
        assert!(candidate_points([110, 10, 50, 40], Some((120, 20)), &monitors).is_empty());
        let points = candidate_points([0, 0, 300, 100], Some((150, 50)), &monitors);
        assert!(points.iter().all(|p| monitors.iter().any(|m| point_in(*m, *p))));
        assert_eq!(rect, [-90, 10, 100, 40]);
    }

    #[test]
    fn text_ownership_requires_exact_ancestor_and_rejects_nested_button() {
        let mut expected = finalize(vec![raw("保存", [0, 0, 100, 40])], vs(), 1).remove(0);
        expected.identity = ElementIdentity { hwnd: 42, process_id: 7, process_started: 100,
            window_process_id: 7, window_process_started: 100, window_runtime_id: vec![4], runtime_id: vec![1] };
        let mut text = expected.clone(); text.role = "Text"; text.identity.runtime_id = vec![2];
        assert!(!same_target(&expected, &text, true, false));
        assert!(may_ascend_hit(&expected, &text, passive_content("Text", false, false, false), 0, true, false));
        assert!(same_target(&expected, &expected, true, false));
        assert!(!may_ascend_hit(&expected, &text, passive_content("Button", true, false, false), 0, true, false));
        assert!(!may_ascend_hit(&expected, &text, true, HIT_ANCESTORS, true, false));
        assert!(!may_ascend_hit(&expected, &text, true, 0, false, false));
        assert!(!may_ascend_hit(&expected, &text, true, 0, true, true));
        let mut sibling = expected.clone(); sibling.identity.runtime_id = vec![9];
        assert!(!same_target(&expected, &sibling, true, false));
        text.identity.hwnd += 1;
        assert!(!may_ascend_hit(&expected, &text, true, 0, true, false));
    }

    #[test]
    fn passwords_never_read_their_own_name() {
        let name = protected_name::<()>(true, || panic!("password Name must not be read"));
        assert_eq!(name.unwrap(), "");
        assert_eq!(protected_name::<()>(false, || Ok("  原始名称  ".into())).unwrap(), "  原始名称  ");
        assert!(protected_name(false, || Err::<String, _>("provider failure")).is_err());
    }

    #[test]
    fn candidate_failures_do_not_bypass_occlusion_or_abort_other_candidates() {
        let monitors = [[0, 0, 100, 100]];
        let points = candidate_points([0, 0, 100, 100], Some((10, 10)), &monitors);
        let mut visited = Vec::new();
        let selected = verified_candidate(points.clone(), |point| {
            visited.push(point);
            match visited.len() {
                1 => Err("provider failure"),
                2 => Err("occluded"), // 遮挡不能退回几何坐标。
                _ => Ok(()),
            }
        });
        assert_eq!(visited, points[..3]);
        assert_eq!(selected, Ok(points[2]));
        assert_eq!(verified_candidate(points.clone(), |_| Err("occluded")), Err(vec!["occluded"; points.len()]));
        let fallback = candidate_points([0, 0, 100, 100], None, &monitors);
        assert!(!fallback.is_empty());
        assert_eq!(verified_candidate(fallback.clone(), |_| Err("unknown")), Err(vec!["unknown"; fallback.len()]));
    }

    #[test]
    fn monitor_gaps_do_not_consume_visible_element_budget() {
        let monitors = [[-100, 0, 100, 100], [100, 0, 100, 100]];
        let found: Vec<_> = [[10, 10, 50, 50], [-10, 10, 30, 30], [110, 10, 30, 30]].into_iter()
            .filter(|rect| visible_on_monitors(*rect, &monitors)).take(1).collect();
        assert_eq!(found, vec![[-10, 10, 30, 30]]);
        assert!(!visible_on_monitors([0, 0, 100, 100], &monitors));
        assert!(!visible_on_monitors([0, 0, 1, 1], &[]));
    }

    #[test]
    fn role_prefilter_spends_budget_only_on_normalized_roles() {
        assert_eq!(parse_role(" button ").unwrap(), Some("Button"));
        assert_eq!(parse_role(" custom ").unwrap(), Some("Custom"));
        assert!(parse_role("Buttons").is_err());
        let query = Query { role: parse_role("button").unwrap(), ..Default::default() };
        let found: Vec<_> = ["Edit", "Edit", "Button", "Button"].into_iter()
            .filter(|r| query.matches_role(r)).take(1).collect();
        assert_eq!(found, vec!["Button"]);
    }

    /// 只读的 Windows UIA 烟雾探针；默认忽略，避免无头 CI 被桌面会话影响。
    #[test]
    #[ignore = "诊断用：枚举当前桌面的 UIA 元素"]
    fn enumerate_live_desktop_without_panicking() {
        let els = enumerate_mode(None, true, 600, 800).expect("UIA 全桌面枚举应成功");
        assert!(!els.is_empty(), "真实桌面应该至少有一个可交互元素");
        assert!(els.len() <= 800);
        assert!(els.iter().enumerate().all(|(i, e)| e.id == i + 1));
        println!(
            "UIA 全桌面枚举到 {} 个元素，首项：{:?}",
            els.len(),
            els.first()
        );

        let focused = enumerate_mode(None, false, 600, 800).expect("UIA 焦点窗口枚举应成功");
        assert!(!focused.is_empty(), "焦点窗口应该至少有一个可交互元素");
        let title = &focused[0].window;
        assert!(
            focused.iter().all(|e| &e.window == title),
            "焦点模式不应混入其他顶层窗口"
        );
        println!("焦点窗口「{title}」枚举到 {} 个元素", focused.len());
    }

    /// 编号必须从 1 连续递增 —— 模型把它当一个列表引用，断号会让人怀疑丢数据。
    #[test]
    fn ids_are_contiguous_from_one() {
        let raws: Vec<RawElement> = (0..7)
            .map(|i| raw(&format!("x{i}"), [0, i * 50, 100, 40]))
            .collect();
        let els = finalize(raws, vs(), 800);
        for (i, e) in els.iter().enumerate() {
            assert_eq!(e.id, i + 1);
        }
    }
