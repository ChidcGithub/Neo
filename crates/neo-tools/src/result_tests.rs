
    use super::*;

    use crate::diagnostic::{with_enabled_for_test, ErrorTrace, SourceLocation};

    #[test]
    fn diagnostic_constructors_report_call_sites() {
        with_enabled_for_test(true, || {
            macro_rules! check_site {
                ($create:expr) => {{
                    let expected_line = line!(); let error = $create;
                    let trace = error.diagnostic.unwrap();
                    assert_eq!(trace.location.file, file!());
                    assert_eq!(trace.location.line, expected_line);
                    assert!(trace.location.column > 0);
                    if !trace.truncated { assert_eq!(trace.causes, ["public"]); }
                }};
            }
            check_site!(ToolError::new(ErrorKind::Internal, "public"));
            check_site!(ToolError::bad_args("public"));
            check_site!(ToolError::not_found("public"));
            check_site!(ToolError::not_allowed("public"));
            check_site!(ToolError::io("public"));
        });
    }

    #[test]
    fn source_attachment_preserves_creation_trace_and_clones_share_arc() {
        with_enabled_for_test(true, || {
            let original = ToolError::io("public");
            let cloned = original.clone();
            assert!(Arc::ptr_eq(original.diagnostic.as_ref().unwrap(), cloned.diagnostic.as_ref().unwrap()));
            let source = std::io::Error::other("first source");
            let attached = cloned.with_source(&source);
            let before = original.diagnostic.as_ref().unwrap();
            let after = attached.diagnostic.as_ref().unwrap();
            assert_eq!(after.location, before.location);
            assert_eq!(after.backtrace, before.backtrace);
            if !before.truncated { assert_eq!(before.causes, ["public"]); }
            if !after.truncated {
                assert_eq!(after.causes, ["first source"]);
            }
            let replaced = attached.with_source(&std::io::Error::other("replacement"));
            let after = replaced.diagnostic.as_ref().unwrap();
            assert_eq!(after.location, before.location);
            assert_eq!(after.backtrace, before.backtrace);
            if !after.truncated {
                assert_eq!(after.causes, ["replacement"]);
            }
            assert_eq!(replaced.to_string(), "public");
            assert!(Error::source(&replaced).is_none());
        });
    }

    #[test]
    fn source_attachment_does_not_capture_late_or_when_disabled() {
        let absent = with_enabled_for_test(false, || ToolError::io("public"));
        with_enabled_for_test(true, || {
            assert!(absent.with_source(&std::io::Error::other("late")).diagnostic.is_none());
        });
        let captured = with_enabled_for_test(true, || ToolError::io("public"));
        let original_trace = captured.diagnostic.clone().unwrap();
        with_enabled_for_test(false, || {
            let unchanged = captured.with_source(&std::io::Error::other("ignored"));
            assert!(Arc::ptr_eq(&original_trace, unchanged.diagnostic.as_ref().unwrap()));
        });
    }

    #[test]
    fn diagnostic_sentinels_never_enter_model_json_or_tool_debug() {
        let mut error = with_enabled_for_test(false, || ToolError::io("public").with_hint("action"));
        error.diagnostic = Some(Arc::new(ErrorTrace {
            location: SourceLocation { file: "PRIVATE_FILE_SENTINEL".into(), line: 1, column: 2 },
            backtrace: "PRIVATE_STACK_SENTINEL\n  frame".into(),
            causes: vec!["PRIVATE_CAUSE_SENTINEL".into()],
            truncated: true,
        }));
        assert_eq!(error.to_json(), json!({"kind": "io", "message": "public", "hint": "action"}));
        assert!(!format!("{error:?}").contains("PRIVATE_"));
        assert!(!format!("{error:#?}").contains("PRIVATE_"));
        let outcome = Outcome::fail("read_file", error);
        assert!(!format!("{outcome:?}").contains("PRIVATE_"));
        for limit in [0, 1, 2, 18, 64, 200, 4096, usize::MAX] {
            let json = outcome.to_model_json(limit);
            assert!(!json.contains("PRIVATE_"));
            assert!(!json.contains("diagnostic"));
            serde_json::from_str::<Value>(&json).unwrap();
        }
        assert!(!crate::to_model_message(&outcome).contains("PRIVATE_"));
    }

    fn outcome_ok() -> Outcome {
        Outcome::ok("read_file", "读取 a.rs", json!({ "lines": 3 }))
    }

    #[test]
    fn model_json_shape_on_success() {
        let v: Value = serde_json::from_str(&outcome_ok().to_model_json(4096)).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["tool"], "read_file");
        assert_eq!(v["data"]["lines"], 3);
    }

    #[test]
    fn model_json_shape_on_failure() {
        let out = Outcome::fail("bash", ToolError::not_allowed("越界").with_hint("换个路径"));
        let v: Value = serde_json::from_str(&out.to_model_json(4096)).unwrap();
        assert_eq!(v["ok"], false);
        assert_eq!(v["error"]["kind"], "not_allowed");
        assert_eq!(v["error"]["hint"], "换个路径");
    }

    #[test]
    fn truncation_stays_valid_json() {
        let out = Outcome::ok("bash", "输出很长", json!({ "stdout": "汉".repeat(500) }));
        let v: Value = serde_json::from_str(&out.to_model_json(200)).expect("截断后仍是合法 JSON");
        assert_eq!(v["truncated"], true);
        assert!(v["head"].as_str().unwrap().chars().count() <= 200);
    }

    #[test]
    fn model_json_budget_counts_final_escaped_output_and_summary() {
        for content in ["中文".repeat(500), "\\\"\n\u{00}".repeat(500)] {
            let out = Outcome::ok("bash", content.clone(), json!({"stdout": content}));
            let original = out.data.clone();
            for limit in 0..=400 {
                let text = out.to_model_json(limit);
                assert!(text.chars().count() <= limit.max(1), "limit={limit}");
                let value: Value = serde_json::from_str(&text).unwrap();
                if limit >= 200 {
                    assert_eq!(value["ok"], true);
                    assert_eq!(value["tool"], "bash");
                    assert_eq!(value["truncated"], true);
                    assert!(value["summary"].is_string());
                    assert!(value["head"].is_string());
                }
            }
            assert_eq!(out.data, original);
        }
        assert_eq!(outcome_ok().to_model_json(0), "0");
        assert_eq!(outcome_ok().to_model_json(1), "0");
        assert_eq!(outcome_ok().to_model_json(2), "{}");
        let out = Outcome::ok("bash", "中文", json!({"text": "汉字"}));
        let full = out.to_model_json(4096);
        assert!(full.len() > full.chars().count());
        assert_eq!(out.to_model_json(full.chars().count()), full);
        let fail = Outcome::fail("bash", ToolError::io("\\".repeat(500)));
        let text = fail.to_model_json(200);
        assert!(text.chars().count() <= 200);
        assert_eq!(serde_json::from_str::<Value>(&text).unwrap()["ok"], false);
    }

    #[test]
    fn args_report_range_and_type() {
        let tool = crate::spec::find("read_file").unwrap();

        let v = json!({ "path": "a.txt", "limit": 99999 });
        let err = Args::new(tool, &v).opt_int("limit").unwrap_err();
        assert_eq!(err.kind, ErrorKind::BadArguments);
        assert!(err.hint.unwrap().contains("2000"));

        let v = json!({ "path": "a.txt", "limit": "x" });
        assert_eq!(
            Args::new(tool, &v).opt_int("limit").unwrap_err().kind,
            ErrorKind::BadArguments
        );

        // 5.0 型浮点整数接纳（模型常这么写）；5.5 仍报错
        let v = json!({ "path": "a.txt", "limit": 5.0 });
        assert_eq!(Args::new(tool, &v).opt_int("limit").unwrap(), 5);
        let v = json!({ "path": "a.txt", "limit": 5.5 });
        assert_eq!(
            Args::new(tool, &v).opt_int("limit").unwrap_err().kind,
            ErrorKind::BadArguments
        );

        let v = json!({});
        assert_eq!(
            Args::new(tool, &v).require_str("path").unwrap_err().kind,
            ErrorKind::BadArguments
        );

        let v = json!({ "path": "a.txt", "nope": 1 });
        assert_eq!(
            Args::new(tool, &v).reject_unknown().unwrap_err().kind,
            ErrorKind::BadArguments
        );
    }

    #[test]
    fn defaults_come_from_declaration() {
        let tool = crate::spec::find("read_file").unwrap();
        let v = json!({ "path": "a.txt" });
        let args = Args::new(tool, &v);
        assert_eq!(args.opt_int("limit").unwrap(), 400);
        assert_eq!(args.opt_int("offset").unwrap(), 0);
        let null = json!({ "path": "a.txt", "limit": null });
        let args = Args::new(tool, &null);
        assert!(args.raw()["limit"].is_null());
        assert!(!args.has("limit"));
        assert_eq!(args.opt_int("limit").unwrap(), 400);
    }

    #[test]
    fn empty_string_is_allowed_only_where_declared() {
        let tool = crate::spec::find("edit_file").unwrap();
        let v = json!({ "path": "a.txt", "old_string": "x", "new_string": "" });
        let args = Args::new(tool, &v);
        // 删除一段文本 → new_string 合法为空
        assert_eq!(args.require_present_str("new_string").unwrap(), "");
        // old_string 为空则无意义 → 必须报错
        let v2 = json!({ "path": "a.txt", "old_string": "", "new_string": "y" });
        assert_eq!(
            Args::new(tool, &v2)
                .require_str("old_string")
                .unwrap_err()
                .kind,
            ErrorKind::BadArguments
        );
    }
