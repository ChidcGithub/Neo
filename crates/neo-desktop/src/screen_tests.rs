
    use super::*;

    #[test]
    fn drag_cancel_after_sleep_only_releases_without_late_moves() {
        let cancel = std::cell::Cell::new(false);
        let mut inputs = Vec::new();
        let result = drag_with((0, 0), (1000, 1000), 10_000,
            |input| { inputs.push(input); Ok(()) }, |_| cancel.set(true), || cancel.get());
        assert!(result.is_err(), "取消不得报告拖拽成功；实际输入数 {}", inputs.len());
        assert_eq!(inputs.len(), 3);
        assert!(matches!(inputs[0], DragInput::Start(_)));
        assert!(matches!(inputs[1], DragInput::Move(_)));
        assert_eq!(inputs[2], DragInput::Release);
    }

    #[test]
    fn drag_cancel_on_start_and_release_failure_preserves_diagnostics() {
        for release_fails in [false, true] {
            let cancel = std::cell::Cell::new(false);
            let mut inputs = Vec::new();
            let error = drag_with((0, 0), (10, 10), 300, |input| {
                inputs.push(input);
                if matches!(input, DragInput::Start(_)) { cancel.set(true); }
                if input == DragInput::Release && release_fails { Err(ToolError::io("假释放失败")) } else { Ok(()) }
            }, |_| panic!("取消后不得等待"), || cancel.get()).unwrap_err();
            assert_eq!(inputs, vec![DragInput::Start((0, 0)), DragInput::Release]);
            assert_eq!(error.kind, if release_fails { ErrorKind::Io } else { crate::cancelled_error().kind });
            if release_fails { assert!(error.message.contains("假释放失败")); }
        }
    }

    #[test]
    fn drag_failures_only_cleanup_and_never_report_success() {
        for fail_at in [0, 1, 2] {
            let mut inputs = Vec::new();
            let error = drag_with((0, 0), (10, 10), 0, |input| {
                inputs.push(input);
                if inputs.len() == fail_at + 1 { Err(ToolError::io("假输入失败")) } else { Ok(()) }
            }, |_| panic!("单步无需等待"), || false).unwrap_err();
            assert_eq!(inputs.last(), Some(&DragInput::Release));
            assert_eq!(inputs.len(), fail_at + 2);
            assert!(error.message.contains("假输入失败"));
        }
        let mut inputs = Vec::new();
        let mut sleeps = 0;
        drag_with((-10, 20), (100, -30), 300,
            |input| { inputs.push(input); Ok(()) }, |_| sleeps += 1, || false).unwrap();
        assert_eq!(inputs.len(), 22);
        assert_eq!(sleeps, 19);
        assert_eq!(inputs[20], DragInput::Move((100, -30)));
        assert_eq!(inputs[21], DragInput::Release);
    }

    #[test]
    fn drag_precancel_has_no_input_or_sleep() {
        let result = drag_with((0, 0), (10, 10), 300,
            |_| panic!("取消后不得输入"), |_| panic!("取消后不得等待"), || true);
        assert_eq!(result.unwrap_err().kind, crate::cancelled_error().kind);
    }

    #[test]
    fn input_normalization_exhaustive_pixel_buckets() {
        for span in [1, 1920, 3840, 7680, 65536] {
            for origin in [0, -1920, -7680] {
                for pixel in 0..span {
                    let n = normalize_axis(origin + pixel, origin, span).unwrap();
                    assert!((0..65536).contains(&n));
                    assert_eq!(i64::from(n) * i64::from(span) / 65536, i64::from(pixel));
                }
            }
        }
        for (pixel, origin, span) in [(0, 0, 65537), (0, 0, 0), (-1, 0, 1920), (1920, 0, 1920)] {
            assert!(normalize_axis(pixel, origin, span).is_err());
        }
    }

    #[test]
    fn display_coverage_rejects_holes_and_unions_overlap() {
        let displays = [Rect { x: -100, y: -100, width: 100, height: 100 }, Rect { x: 0, y: 0, width: 200, height: 100 }];
        let bounds = monitor_bounds(&displays).unwrap();
        assert_eq!(bounds, Rect { x: -100, y: -100, width: 300, height: 200 });
        assert_eq!(monitor_coverage(bounds, &displays), 30000);
        assert!(require_monitor_point(&displays, "点击位置", -1, -1).is_ok());
        assert!(require_monitor_point(&displays, "拖动终点", 0, -1).is_err());
        assert_eq!(monitor_coverage(Rect { x: 0, y: -100, width: 200, height: 100 }, &displays), 0);
        assert_eq!(monitor_coverage(displays[0], &[displays[0], displays[0]]), 10000);
    }

    #[test]
    fn click_debug_evidence_is_bounded_and_does_not_claim_target_response() {
        let mut events = Vec::new();
        ClickEvidence::default().append_events(&mut events, "pure-test");
        assert!(events.is_empty());
        let evidence = ClickEvidence {
            before: Some(ClickPoint {
                cursor: Some((-1, 2)), target: (3, 4), target_own_pid: Some(true),
                target_class: classify_click_class(&"neo-overlay".encode_utf16().collect::<Vec<_>>()),
            }),
            primary: Some(SendEvidence { requested: 3, sent: 3, last_error_immediate: 5, elapsed_us: 1 }),
            ..Default::default()
        };
        evidence.append_events(&mut events, "pure-test");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["data"]["before"]["target_class"], "neo_overlay");
        assert_eq!(events[0]["data"]["before"]["target_own_pid"], true);
        assert_eq!(events[1]["data"]["primary"]["sent"], 3);
        assert_eq!(events[1]["data"]["target_response_verified"], false);
        assert_eq!(events[1]["data"]["accepted_does_not_prove_target_response"], true);
        assert_eq!(events[1]["data"]["last_error_may_be_stale_on_success"], true);
        assert_eq!(classify_click_class(&"private-window-class".encode_utf16().collect::<Vec<_>>()), TargetClass::Other);
        assert_eq!(serde_json::to_string(&TargetClass::Other).unwrap(), "\"other\"");
    }

    #[test]
    fn geometry_and_capture_size_reject_overflow_without_desktop_access() {
        let extreme = Rect { x: i32::MAX - 1, y: i32::MIN, width: 100, height: i32::MAX };
        assert!(extreme.contains(i32::MAX, -2));
        assert!(!extreme.contains(i32::MAX, 0));
        assert!(extreme.outside_error("位置", 0, 0).message.contains("2147483745"));
        assert!(capture_bytes(Rect { x: 0, y: 0, width: i32::MAX, height: i32::MAX }).is_err());
        assert!(capture_bytes(Rect { x: 0, y: 0, width: 0, height: 10 }).is_err());
        assert_eq!(capture_bytes(Rect { x: -1920, y: 0, width: 32, height: 32 }).unwrap(), 4096);
    }

    #[test]
    fn button_parsing_is_forgiving_about_case_and_aliases() {
        for s in ["", "left", "LEFT", " l ", "primary"] {
            assert_eq!(Button::parse(s).unwrap(), Button::Left, "{s}");
        }
        for s in ["right", "Right", "r", "secondary"] {
            assert_eq!(Button::parse(s).unwrap(), Button::Right, "{s}");
        }
        let err = Button::parse("middle").unwrap_err();
        assert_eq!(err.kind, ErrorKind::BadArguments);
        assert!(err.hint.is_some(), "要给可执行的出路");
    }

    /// 边界是**半开**的：副屏排在主屏左边时坐标是负数，也属于桌面。
    #[test]
    fn rect_containment_is_half_open_and_allows_negative() {
        let r = Rect {
            x: -1920,
            y: 0,
            width: 1920,
            height: 1080,
        };
        assert!(r.contains(-1920, 0));
        assert!(r.contains(-1, 1079));
        assert!(!r.contains(0, 0), "右边界不含");
        assert!(!r.contains(-1920, 1080));
        assert!(!r.contains(-1921, 0));
    }

    /// 越界时必须把**合法范围**说出来 —— 模型据此自己改，不用再问一轮。
    #[test]
    fn outside_error_states_the_legal_range() {
        let r = Rect {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let e = r.outside_error("点击位置", 5000, 5000);
        assert_eq!(e.kind, ErrorKind::BadArguments);
        assert!(
            e.message.contains("1919") && e.message.contains("1079"),
            "要含合法范围：{}",
            e.message
        );
        assert!(e.hint.is_some());
    }

    /// 把屏幕层的读数打出来（默认忽略，`--ignored --nocapture` 才跑）。
    #[test]
    #[ignore = "诊断用：打印虚拟桌面与缩放比"]
    fn dump_screen_info() {
        ensure_dpi_aware();
        let vs = virtual_screen();
        println!(
            "虚拟桌面: x={} y={} width={} height={}",
            vs.x, vs.y, vs.width, vs.height
        );
        println!(
            "中心点缩放比: {:?}",
            dpi_scale_at(vs.x + vs.width / 2, vs.y + vs.height / 2)
        );
        assert!(vs.width > 0, "没有桌面会话");
    }

    /// 只读的一条：抓 32×32、编码 PNG，并确认 DPI 查询不炸。
    /// 无头会话里允许失败，但必须是**可读的错误**。
    #[test]
    fn screen_layer_never_panics() {
        ensure_dpi_aware();
        let vs = virtual_screen();
        if vs.width <= 0 {
            eprintln!("跳过：没有可用的虚拟桌面（无头会话）");
            return;
        }
        let probe = Rect {
            x: vs.x,
            y: vs.y,
            width: 32,
            height: 32,
        };
        match capture(probe) {
            Ok(shot) => {
                assert_eq!(shot.width, 32);
                assert_eq!(shot.rgba.len(), 32 * 32 * 4);
                let png = shot.to_png().expect("PNG 编码");
                assert!(png.starts_with(&[0x89, b'P', b'N', b'G']), "PNG 魔数");
                assert!(png.len() > 50, "不该是空图");
            }
            Err(e) => {
                assert!(!e.message.is_empty());
                assert!(e.hint.is_some(), "失败也要给出路");
            }
        }
        // 缩放比：要么给个数，要么给 None —— 但不能 panic
        if let Some(scale) = dpi_scale_at(vs.x + 1, vs.y + 1) {
            assert!(scale > 0.0 && scale < 10.0, "缩放比看起来不对：{scale}");
        }
    }

    /// 越界点击必须**在发出任何事件之前**就被拒 —— 不能让鼠标先跑过去。
    #[test]
    fn out_of_range_click_is_rejected_without_touching_the_mouse() {
        let vs = virtual_screen();
        if vs.width <= 0 {
            return;
        }
        let err = click(vs.x + vs.width + 5000, vs.y + 10, Button::Left, false).unwrap_err();
        assert_eq!(err.kind, ErrorKind::BadArguments);
        assert!(err.message.contains("不在屏幕内"), "{}", err.message);
        // 拖动两端都要查
        let err = drag(
            (vs.x + 1, vs.y + 1),
            (vs.x - 5000, vs.y + 1),
            Button::Left,
            100,
        )
        .unwrap_err();
        assert_eq!(err.kind, ErrorKind::BadArguments);
        assert!(err.message.contains("终点"), "{}", err.message);
    }
