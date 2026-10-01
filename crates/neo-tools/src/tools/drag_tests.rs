
    use super::*;
    use serde_json::json;

    #[test]
    fn preview_shows_both_ends() {
        let tool = crate::find("drag").unwrap();
        let v = json!({ "x": 1, "y": 2, "to_x": 300, "to_y": 400 });
        let a = Args::new(tool, &v);
        assert!(preview(&a).contains("(1, 2)"), "{}", preview(&a));
        assert!(preview(&a).contains("(300, 400)"), "{}", preview(&a));
        assert!(preview(&a).contains("300 ms"), "{}", preview(&a));
    }

    #[test]
    fn incomplete_or_invalid_drag_is_rejected_before_desktop_access() {
        let tool = crate::find("drag").unwrap();
        let scope = Scope::new(std::env::temp_dir());
        for value in [json!({"x": 1, "y": 2, "to_x": 3}),
            json!({"x": 1, "y": 2, "to_x": 3, "to_y": 4, "button": "invalid"}),
            json!({"x": 1, "y": 2, "to_x": 3, "to_y": 4, "duration_ms": 10001}),
            json!(null), json!([]), json!({"x": null, "y": 2, "to_x": 3, "to_y": 4}),
            json!({"x": 1, "y": 2, "to_x": 3, "to_y": 4, "screenshot_id": "old"})] {
            assert!(act(&scope, &Args::new(tool, &value)).is_err());
        }
    }

    #[test]
    fn duration_defaults_to_a_human_like_300ms() {
        let p = PARAMS
            .iter()
            .find(|p| p.name == "duration_ms")
            .expect("duration_ms 参数存在");
        assert_eq!(p.default, Some(crate::spec::Default::Int(300)));
        let (lo, hi) = p.range.expect("有区间");
        assert!(lo <= 300 && hi >= 300, "区间要容得下默认值");
    }
