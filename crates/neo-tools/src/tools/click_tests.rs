
    use super::*;
    use serde_json::json;

    fn act(args: &Args) -> Result<Outcome, ToolError> {
        super::act(&Scope::new(std::env::temp_dir()), args, None)
    }

    #[test]
    fn debug_envelope_has_fixed_session_and_no_arguments() {
        let event = debug_envelope("A", "click_entered_and_completed", json!({"error_kind": "bad_arguments"}), "pure-test");
        assert_eq!(event["sessionId"], "agent-click-routing");
        assert_eq!(event["data"], json!({"error_kind": "bad_arguments"}));
        assert!(event.get("args").is_none());
    }

    #[test]
    fn preview_reads_like_a_sentence() {
        let tool = crate::find("click").unwrap();

        let v = json!({ "x": 100, "y": 200 });
        assert_eq!(preview(&Args::new(tool, &v)), "在 (100, 200) 点击左键");

        let v = json!({ "x": 1, "y": 2, "button": "right", "double": true });
        assert_eq!(preview(&Args::new(tool, &v)), "在 (1, 2) 双击右键");

        // 编号定位的确认框要说的是"点哪个元素"，不是坐标
        let v = json!({ "element_id": 7 });
        assert_eq!(preview(&Args::new(tool, &v)), "点击左键：元素 #7");
    }

    /// 两种定位方式必须给一种 —— 漏了要报错，不能被悄悄当成 (0,0) 点下去。
    #[test]
    fn missing_locator_is_rejected_without_touching_the_mouse() {
        let tool = crate::find("click").unwrap();
        let value = json!({});
        let a = Args::new(tool, &value);
        let err = act(&a).unwrap_err();
        assert!(err.message.contains("缺少定位"), "实际：{}", err.message);
    }

    #[test]
    fn partial_xy_is_rejected_without_touching_the_mouse() {
        let tool = crate::find("click").unwrap();
        for value in [json!({ "x": 10 }), json!({ "y": 20 })] {
            let err = act(&Args::new(tool, &value)).unwrap_err();
            assert!(
                err.message.contains("同时给 x 和 y"),
                "实际：{}",
                err.message
            );
        }
    }

    #[test]
    fn mixed_or_unpaired_references_fail_without_input() {
        let tool = crate::find("click").unwrap();
        for value in [json!({"element_id": 1, "x": 1, "y": 1}),
            json!({"snapshot_id": "old", "x": 1, "y": 1}),
            json!({"snapshot_id": "old", "element_id": 1}),
            json!({"screenshot_id": "old", "element_id": 1}),
            json!({"screenshot_id": "old", "snapshot_id": "other", "x": 1, "y": 1}),
            json!({"screenshot_id": "unknown", "x": 1, "y": 1}),
            json!({"screenshot_id": null, "x": 1, "y": 1}),
            json!({"screenshot_id": "", "x": 1, "y": 1}),
            json!({"x": null, "y": 1}), json!({"x": 1, "y": 1, "unexpected": true})] {
            assert!(act(&Args::new(tool, &value)).is_err());
        }
    }

    /// 查不到的编号要在**发事件之前**拒绝，并告诉模型下一步怎么做。
    #[test]
    fn unknown_element_id_fails_before_sending_input() {
        let tool = crate::find("click").unwrap();
        let value = json!({ "element_id": 9999 });
        let a = Args::new(tool, &value);
        let err = act(&a).unwrap_err();
        assert!(err.message.contains("9999"));
        assert!(err.hint.unwrap().contains("screen_elements"));
    }
