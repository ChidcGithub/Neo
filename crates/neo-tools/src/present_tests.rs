
    use super::*;
    use serde_json::json;

    #[test]
    fn classifies_upstream_and_neo_tool_names() {
        // 上游原表的抽样
        assert_eq!(Variant::of("bash"), Variant::Bash);
        // 屏幕交互自成一类：它不是"读文件"，也不是"跑命令"。
        for name in ["screenshot", "click", "drag"] {
            assert_eq!(Variant::of(name), Variant::Screen, "{name}");
            assert_eq!(Variant::of(name).title(), "屏幕", "{name} 的中文标题");
        }
        assert_eq!(Variant::of("某个体不认识工具"), Variant::Others);
        assert_eq!(Variant::of("pwsh"), Variant::Bash);
        assert_eq!(Variant::of("web_fetch"), Variant::Read);
        assert_eq!(Variant::of("grep"), Variant::Search);
        assert_eq!(Variant::of("glob"), Variant::Search);
        assert_eq!(Variant::of("cordis_inspect"), Variant::Read);
        // Neo 自己的六个工具
        assert_eq!(Variant::of("read_file"), Variant::Read);
        assert_eq!(Variant::of("read_document"), Variant::Read);
        assert_eq!(Variant::of("view_image"), Variant::Read);
        assert_eq!(Variant::of("write_file"), Variant::Write);
        assert_eq!(Variant::of("edit_file"), Variant::Edit);
        assert_eq!(Variant::of("powershell"), Variant::Bash);
        assert_eq!(Variant::of("open_file"), Variant::Others);
        // 不认识的不能猜
        assert_eq!(Variant::of("天外飞仙"), Variant::Others);
    }

    #[test]
    fn bash_prefers_description_over_command() {
        let args = json!({ "command": "cargo test", "description": "跑单元测试" });
        assert_eq!(summary(Variant::Bash, &args), "跑单元测试");
        let only_cmd = json!({ "command": "cargo test" });
        assert_eq!(summary(Variant::Bash, &only_cmd), "cargo test");
    }

    #[test]
    fn read_uses_path_and_takes_first_line_only() {
        let args = json!({ "path": "src/a.rs", "content": "x" });
        assert_eq!(summary(Variant::Read, &args), "src/a.rs");
        // 多行值只取首行（摘要是一行）
        let multi = json!({ "command": "line1\nline2" });
        assert_eq!(summary(Variant::Bash, &multi), "line1");
        // 前后空白要剪掉
        let padded = json!({ "path": "  src/b.rs  " });
        assert_eq!(summary(Variant::Read, &padded), "src/b.rs");
    }

    #[test]
    fn unknown_variant_falls_back_to_first_string_value() {
        // Others 没有键表 → 取参数里第一个非空字符串
        let args = json!({ "path": "a.txt", "flag": true });
        assert_eq!(summary(Variant::Others, &args), "a.txt");
        // 完全没有可用字符串 → 空
        assert_eq!(summary(Variant::Others, &json!({ "n": 1 })), "");
        assert_eq!(summary(Variant::Others, &json!({})), "");
        assert_eq!(summary(Variant::Others, &Value::Null), "");
    }

    #[test]
    fn every_neo_tool_yields_a_readable_summary() {
        // 用真实参数形状过一遍，确保六个工具都有像样的摘要可显示。
        let cases = [
            (
                "read_file",
                json!({ "path": "src/main.rs", "offset": 0 }),
                "src/main.rs",
            ),
            (
                "read_document",
                json!({ "path": "lesson.pptx", "offset": 0 }),
                "lesson.pptx",
            ),
            (
                "view_image",
                json!({ "path": "docs/screens/01.png" }),
                "docs/screens/01.png",
            ),
            (
                "write_file",
                json!({ "path": "notes/a.md", "content": "hi" }),
                "notes/a.md",
            ),
            (
                "edit_file",
                json!({ "path": "src/lib.rs", "old_string": "a", "new_string": "b" }),
                "src/lib.rs",
            ),
            (
                "powershell",
                json!({ "command": "cargo test" }),
                "cargo test",
            ),
            (
                "open_file",
                json!({ "path": "docs/spec.md" }),
                "docs/spec.md",
            ),
        ];
        for (name, args, want) in cases {
            let v = Variant::of(name);
            assert_eq!(summary(v, &args), want, "{name} 的摘要不对");
            assert!(!v.title().is_empty(), "{name} 没有标题");
        }
    }

    #[test]
    fn file_path_only_for_file_variants() {
        let args = json!({ "path": "src/a.rs", "url": "https://x" });
        assert_eq!(file_path(Variant::Read, &args).as_deref(), Some("src/a.rs"));
        assert_eq!(file_path(Variant::Edit, &args).as_deref(), Some("src/a.rs"));
        // bash 不该给出可打开路径
        assert!(file_path(Variant::Bash, &args).is_none());
        // 只有 url 时 read 也不给路径（上游：FILE_PATH_KEYS 只有 path/file_path）
        assert!(file_path(Variant::Read, &json!({ "url": "https://x" })).is_none());
    }

    #[test]
    fn snake_case_file_path_key_is_also_accepted() {
        let args = json!({ "file_path": "/tmp/x" });
        assert_eq!(file_path(Variant::Read, &args).as_deref(), Some("/tmp/x"));
    }
