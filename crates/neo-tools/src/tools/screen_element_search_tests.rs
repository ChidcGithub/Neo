
    use super::*;
    fn vs() -> Rect { Rect { x: -1920, y: 0, width: 3840, height: 1080 } }
    fn element() -> ScreenElement {
        ScreenElement { id: 1, role: "Button", name: "保存设置".into(), label: "保存设置".into(), label_source: "name", window: "Neo".into(),
            rect: [-1900, 0, 80, 40], identity: screen_uia::ElementIdentity::default(), ..Default::default() }
    }
    #[test]
    fn cancelled_search_never_refreshes_or_reads_desktop() {
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let scope = Scope::new(std::env::temp_dir()).with_cancel(cancel);
        let tool = crate::find("screen_element_search").unwrap();
        for refresh in [false, true] {
            let value = json!({"refresh": refresh});
            assert_eq!(act(&scope, &Args::new(tool, &value)).unwrap_err().kind, crate::cancelled_error().kind);
        }
    }

    #[test]
    fn region_names_are_spatial() {
        assert_eq!(region_name([-1920, 0, 1, 1], vs()), "top_left");
        assert!(region_matches([-1920, 0, 1, 1], "left", vs()));
    }
    #[test]
    fn middle_matches_the_whole_middle_row_on_negative_origin_desktop() {
        let screen = Rect { x: -300, y: -300, width: 900, height: 900 };
        for (y, vertical) in [(-150, "top"), (150, "middle"), (450, "bottom")] {
            for (x, horizontal) in [(-150, "left"), (150, "center"), (450, "right")] {
                let rect = [x, y, 2, 2];
                for wanted in ["top", "middle", "bottom"] {
                    assert_eq!(region_matches(rect, wanted, screen), wanted == vertical);
                }
                assert_eq!(region_matches(rect, "left", screen), horizontal == "left");
                assert_eq!(region_matches(rect, "right", screen), horizontal == "right");
                assert_eq!(region_matches(rect, "center", screen), vertical == "middle" && horizontal == "center");
            }
        }
    }

    #[test]
    fn matching_uses_name_role_and_position() {
        assert!(matches_element(&element(), "保存", "button", "top_left", vs()));
        assert!(!matches_element(&element(), "删除", "", "", vs()));
    }
    #[test]
    fn query_uses_display_label_not_raw_identity() {
        let mut el = element();
        el.name.clear(); el.label = "关联标签".into(); el.label_source = "child_text";
        assert!(matches_element(&el, "关联", "Button", "", vs()));
        el.name = "秘密原始名称".into();
        assert!(!matches_element(&el, "原始", "", "", vs()));
    }

    #[test]
    fn omitted_filters_match_every_candidate_and_refresh_is_explicit() {
        let tool = crate::find("screen_element_search").unwrap();
        let value = json!({});
        let args = Args::new(tool, &value);
        assert!(!args.flag("refresh").unwrap());
        assert!(matches_element(&element(), &args.opt_str("query").unwrap(),
            &args.opt_str("role").unwrap(), &args.opt_str("position").unwrap(), vs()));
        assert_eq!(args.opt_int("limit").unwrap(), 10);
    }

    #[test]
    fn escaped_names_fit_hard_budget_and_keep_references() {
        let mut el = element();
        el.name = "\u{0000}\\\"中文".repeat(10_000);
        el.label = el.name.clone();
        el.window = el.name.clone();
        let candidates = vec![&el; 50];
        let data = search_data("synthetic-snapshot", &candidates, 50, 50, false, vs());
        assert!(super::super::screen_elements::outcome_bytes("screen_element_search", "找到 50 个屏幕元素候选", &data) <= SEARCH_BYTES);
        assert_eq!(data["truncated"], true);
        assert_eq!(data["snapshot_id"], "synthetic-snapshot");
        assert_eq!(data["elements"][0]["element_id"], 1);
        let out = Outcome::ok("screen_element_search", "合成测试", data);
        let model: Value = serde_json::from_str(&crate::to_model_message(&out)).unwrap();
        assert!(model.get("truncated").is_none());
    }
