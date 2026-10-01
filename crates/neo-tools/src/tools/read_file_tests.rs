
    use super::*;

    struct Fixture(std::path::PathBuf);

    impl Fixture {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "neo-read-regression-{name}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
            ));
            std::fs::create_dir(&root).unwrap();
            Self(root)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn read_edit_roundtrip_preserves_crlf_lf_and_mixed_bytes() {
        let fixture = Fixture::new("roundtrip");
        let scope = Scope::new(&fixture.0);
        let path = fixture.0.join("fixture.txt");
        for endings in [["\r\n", "\r\n", "\r\n"], ["\n", "\n", "\n"], ["\r\n", "\n", "\r\n"]] {
            let selected = format!("first{}second{}third", endings[0], endings[1]);
            let prefix = "\u{feff}outside-before\r\n";
            let suffix = format!("{}outside-after\n", endings[2]);
            let original = format!("{prefix}{selected}{suffix}");
            std::fs::write(&path, &original).unwrap();
            let read = crate::dispatch(&scope, "read_file", &json!({
                "path": "fixture.txt", "offset": 1, "limit": 3,
            }));
            assert!(read.is_ok(), "{read:?}");
            let model: serde_json::Value = serde_json::from_str(&crate::to_model_message(&read)).unwrap();
            assert_eq!(model["data"]["content"], selected);
            assert_eq!(model["data"]["next_offset"], 4);
            let edited = crate::dispatch(&scope, "edit_file", &json!({
                "path": "fixture.txt", "old_string": model["data"]["content"], "new_string": "replacement",
            }));
            assert!(edited.is_ok(), "{edited:?}");
            assert_eq!(std::fs::read(&path).unwrap(), format!("{prefix}replacement{suffix}").as_bytes());
        }
    }

    #[test]
    fn model_pages_deliver_every_line_once_with_escaped_and_unicode_text() {
        let fixture = Fixture::new("model-pages");
        let scope = Scope::new(&fixture.0);
        for unit in ["x", "\"", "\\", "中", "\"\\中文"] {
            let expected: Vec<String> = (0..401).map(|i| format!("{i:03}{}", unit.repeat(97))).collect();
            std::fs::write(fixture.0.join("fixture.txt"), expected.join("\n")).unwrap();
            let mut offset = 0;
            let mut actual = Vec::new();
            loop {
                let out = crate::dispatch(&scope, "read_file", &json!({"path": "fixture.txt", "offset": offset}));
                assert!(out.is_ok(), "{out:?}");
                let message = crate::to_model_message(&out);
                assert_eq!(message, out.to_model_json(usize::MAX));
                assert!(message.len() <= 32 * 1024);
                assert!(message.chars().count() <= limits::MODEL_JSON_CHARS);
                let page: serde_json::Value = serde_json::from_str(&message).unwrap();
                assert!(page.get("head").is_none());
                let data = &page["data"];
                let count = data["lines_returned"].as_u64().unwrap() as usize;
                assert!(count > 0);
                assert_eq!(data["offset"], offset);
                assert_eq!(data["lines_total"], expected.len());
                let lines: Vec<_> = data["content"].as_str().unwrap().lines().map(str::to_owned).collect();
                assert_eq!(lines.len(), count);
                actual.extend(lines);
                if data["truncated"] == false {
                    assert!(data.get("next_offset").is_none());
                    break;
                }
                assert_eq!(data["next_offset"], offset + count);
                offset += count;
                assert!(offset < expected.len());
            }
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn oversized_line_is_a_structured_error_after_any_delivered_prefix() {
        let fixture = Fixture::new("long-line");
        let scope = Scope::new(&fixture.0);
        for prefix in ["", "short\n"] {
            std::fs::write(fixture.0.join("fixture.txt"), format!("{prefix}{}", "x".repeat(30_000))).unwrap();
            let mut out = crate::dispatch(&scope, "read_file", &json!({"path": "fixture.txt"}));
            if !prefix.is_empty() {
                assert!(out.is_ok());
                assert_eq!(out.data["content"], "short");
                assert_eq!(out.data["next_offset"], 1);
                out = crate::dispatch(&scope, "read_file", &json!({"path": "fixture.txt", "offset": 1}));
            }
            assert_eq!(out.error.as_ref().unwrap().kind, ErrorKind::TooLarge);
            let model: serde_json::Value = serde_json::from_str(&crate::to_model_message(&out)).unwrap();
            assert_eq!(model["ok"], false);
            assert_eq!(model["error"]["kind"], "too_large");
            assert!(model["error"]["hint"].as_str().unwrap().contains("read_document"));
            assert!(model.get("head").is_none());
        }
    }

    #[test]
    fn line_ranges_keep_empty_lines_and_only_omit_final_terminator() {
        let fixture = Fixture::new("line-ranges");
        let scope = Scope::new(&fixture.0);
        for text in ["", "\n", "\r\n", "a\r\nb\nc\r\n", "a\r\n\r\nb\r", "a\n\n"] {
            std::fs::write(fixture.0.join("fixture.txt"), text).unwrap();
            let total = text.lines().count();
            for offset in 0..=total + 1 {
                for limit in 1..=3 {
                    let out = crate::dispatch(&scope, "read_file", &json!({"path": "fixture.txt", "offset": offset, "limit": limit}));
                    assert!(out.is_ok());
                    let selected = text.split_inclusive('\n').skip(offset).take(limit).collect::<String>();
                    let expected = selected.strip_suffix("\r\n").or_else(|| selected.strip_suffix('\n')).unwrap_or(&selected);
                    assert_eq!(out.data["content"], expected);
                    assert_eq!(out.data["lines_total"], total);
                    assert_eq!(out.data["lines_returned"], total.saturating_sub(offset).min(limit));
                }
            }
        }
    }

    #[test]
    fn byte_limit_and_many_short_lines_remain_bounded() {
        let root = std::env::temp_dir().join(format!("neo-read-bound-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("边界.txt");
        std::fs::write(&path, b"1234").unwrap();
        assert_eq!(read_bounded(&path, 4).unwrap(), b"1234");
        assert_eq!(read_bounded(&path, 3).unwrap_err().kind, ErrorKind::TooLarge);
        std::fs::write(&path, "\n".repeat(limits::READ_BYTES as usize)).unwrap();
        let out = crate::dispatch(&Scope::new(&root), "read_file", &json!({"path": "边界.txt", "limit": 1}));
        assert!(out.error.is_none());
        assert_eq!(out.data["lines_returned"], 1);
        assert_eq!(out.data["next_offset"], 1);
        std::fs::remove_dir_all(root).unwrap();
    }
