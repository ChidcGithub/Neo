
    use super::*;
    use std::sync::Arc;

    #[test]
    fn structured_events_allow_only_known_metadata_and_keep_distinct_ids() {
        let mut buffer = Buffer::new(10, MAX_BYTES, MAX_ENTRY_BYTES);
        for _ in 0..2 {
            let span = Span::new("tool", Some(42));
            let outcome = neo_tools::Outcome::fail("UNTRUSTED_TOOL", neo_tools::ToolError::io("PRIVATE_BODY"))
                .with_image("PRIVATE_IMAGE");
            let message = span.message(Phase::Failed, Details::outcome(&outcome));
            assert!(message.contains("parent=42") && message.contains("tool=unknown") && message.contains("kind=io"));
            assert!(message.contains("elapsed_ms="));
            buffer.push(Level::Warn, "tool", &message, 1);
        }
        assert_eq!(buffer.entries.len(), 2, "不同调用不能聚合掉关联 ID");
        assert!(!format!("{:?}", buffer.snapshot()).contains("PRIVATE"));
        assert!(!format!("{:?}", buffer.snapshot()).contains("UNTRUSTED"));
        let span = Span::new("PRIVATE_COMPONENT", None);
        assert_eq!(span.component, "diagnostic");
        let message = span.message(Phase::Started, Details { tool: Some("PRIVATE_NAME"), ..Details::default() });
        assert!(!message.contains("PRIVATE"));
    }

    #[test]
    fn http_errors_extract_only_anchored_status_class() {
        for (error, kind) in [
            ("接口返回 401 Unauthorized：PRIVATE_BODY sk-private https://private", "http_4xx"),
            ("接口返回 503 Service Unavailable：PRIVATE_BODY", "http_5xx"),
            ("模型列表接口返回 HTTP 429，请检查接口地址、密钥或服务状态", "http_4xx"),
            ("接口返回 302 Found：PRIVATE_BODY", "http_other"),
            ("请求失败：https://user:PRIVATE_KEY@example.org", "transport"),
            ("PRIVATE_BODY HTTP 401", "model_other"),
            ("接口返回 4010 PRIVATE_BODY", "model_other"),
            ("接口返回 401PRIVATE_BODY", "model_other"),
            ("接口返回 ９９９：PRIVATE_BODY", "model_other"),
            ("接口返回 999：PRIVATE_BODY", "model_other"),
            ("接口返回 🦀：PRIVATE_BODY", "model_other"),
        ] {
            let message = Span::new("model", None).message(Phase::Failed, Details {
                failure: Some(model_failure(error)), ..Details::default()
            });
            assert!(message.contains(&format!("kind={kind}")), "{message}");
            assert!(!message.contains("PRIVATE") && !message.contains("://") && !message.contains("sk-"));
        }
    }

    #[test]
    fn uia_extracts_only_known_stage_and_exact_eight_hex_digits() {
        for (stage, shown) in [("ElementFromPoint", "ElementFromPoint"), ("CoInitializeEx", "CoInitializeEx"),
            ("CurrentIsPassword", "sensitive_field_check")] {
            let error = neo_tools::ToolError::io(format!(
                "UIA 即时核验拒绝输入：provider_error (stage={stage}, HRESULT=0x80070005); PRIVATE_WINDOW；失败候选/检查数=1"
            )).with_hint("PRIVATE_COMMAND");
            let outcome = neo_tools::Outcome::fail("click", error);
            let message = Span::new("tool", None).message(Phase::WorkerFinished, Details::outcome(&outcome));
            assert!(message.contains(&format!("uia_stage={shown} hresult=0x80070005")));
            assert!(!message.contains("PRIVATE"));
            assert_eq!(&*sanitize(&message, MAX_ENTRY_BYTES).0, message);
        }
        for detail in [
            "stage=PRIVATE_STAGE, HRESULT=0x80070005)",
            "stage=ElementFromPoint_PRIVATE, HRESULT=0x80070005)",
            "stage=ElementFromPoint, HRESULT=0x800700050)",
            "stage=ElementFromPoint, HRESULT=0x80070005PRIVATE)",
            "stage=ElementFromPoint, HRESULT=0x8007)",
            "stage=ElementFromPoint, HRESULT=0x8007000Z)",
            "stage=ElementFromPoint, HRESULT=0x🦀🦀)",
        ] {
            let error = neo_tools::ToolError::io(format!("UIA 即时核验拒绝输入：provider_error ({detail}"));
            assert!(uia_failure("click", &error).is_none(), "{detail}");
        }
        let valid = neo_tools::ToolError::io("UIA 即时核验拒绝输入：provider_error (stage=ElementFromPoint, HRESULT=0x80070005)");
        assert!(uia_failure("read_file", &valid).is_none());
        assert!(uia_failure("PRIVATE_TOOL", &valid).is_none());
        assert!(uia_failure("click", &neo_tools::ToolError::io("stage=ElementFromPoint, HRESULT=0x80070005)")).is_none());
        assert!(uia_failure("click", &neo_tools::ToolError::io(format!("{}{}", valid.message, "X".repeat(MAX_INPUT_BYTES)))).is_none());
    }

    #[test]
    fn uia_new_stages_survive_actual_outcome_logging_without_private_text() {
        for (stage, shown) in [
            ("GetCurrentPattern(Invoke)", "GetCurrentPattern(Invoke)"),
            ("GetCurrentPattern(Toggle)", "GetCurrentPattern(Toggle)"),
            ("GetCurrentPattern(SelectionItem)", "GetCurrentPattern(SelectionItem)"),
            ("GetCurrentPattern(ExpandCollapse)", "GetCurrentPattern(ExpandCollapse)"),
            ("passive_hit.CurrentControlType", "passive_hit.CurrentControlType"),
            ("passive_hit.CurrentIsKeyboardFocusable", "passive_hit.CurrentIsKeyboardFocusable"),
            ("passive_hit.CurrentIsPassword", "passive_hit.sensitive_field_check"),
            ("CurrentIsPassword", "sensitive_field_check"),
        ] {
            let outcome = neo_tools::Outcome::fail("click", neo_tools::ToolError::io(format!(
                "UIA 即时核验拒绝输入：provider_error (stage={stage}, HRESULT=0x80004005); PRIVATE_WINDOW；失败候选/检查数=1"
            )).with_hint("PRIVATE_COMMAND sk-PRIVATE_KEY https://private.example/PRIVATE_BODY"));
            // 走 Outcome -> Details -> event -> record -> Buffer 脱敏，而非只检查解析器。
            let span = Span::new("tool", Some(42));
            let view = capture_for_test(|| span.event(Phase::WorkerFinished, Details::outcome(&outcome)));
            assert_eq!(view.entries.len(), 1, "{stage}");
            let entry = &view.entries[0];
            assert_eq!(entry.level, Level::Warn);
            assert_eq!(&*entry.component, "tool");
            for field in ["event=worker_finished", "parent=42", "elapsed_ms=", "tool=click", "kind=io", "hresult=0x80004005"] {
                assert!(entry.message.contains(field), "{stage}: missing {field}");
            }
            assert!(entry.message.contains(&format!("uia_stage={shown} ")), "{stage}");
            assert!(entry.message.contains(&format!(" id={} ", span.id)));
            assert!(!entry.message.contains(REDACTED), "{stage}: whole event redacted");
            assert!(!entry.message.to_ascii_lowercase().contains("password"));
            assert!(!format!("{view:?}").contains("PRIVATE"));
            assert_eq!(view.stats.truncated, 0);
        }
    }

    #[test]
    fn uia_new_stage_allowlist_rejects_near_matches_in_actual_logs() {
        for stage in [
            "GetCurrentPattern(PRIVATE_PATTERN)", "GetCurrentPattern(Invoke)PRIVATE_SUFFIX",
            "GetCurrentPattern(invoke)", "passive_hit.CurrentControlType.PRIVATE_SUFFIX",
            "passive_hit.CurrentIsKeyboardFocusablePRIVATE_SUFFIX", "passive_hit.CurrentIsPasswordPRIVATE_SUFFIX",
            "passive_hit.PRIVATE_PROPERTY",
        ] {
            let outcome = neo_tools::Outcome::fail("click", neo_tools::ToolError::io(format!(
                "UIA 即时核验拒绝输入：provider_error (stage={stage}, HRESULT=0x80004005)"
            )));
            let view = capture_for_test(|| Span::new("tool", None).event(Phase::Delivered, Details::outcome(&outcome)));
            let message = &view.entries[0].message;
            assert!(message.contains("event=delivered") && message.contains("kind=io"));
            assert!(!message.contains("uia_stage=") && !message.contains("hresult="), "{stage}");
            assert!(!message.contains(REDACTED) && !message.contains("PRIVATE"));
        }
    }

    #[test]
    fn background_metadata_never_serializes_command_output_or_pid() {
        let outcome = neo_tools::Outcome::ok("powershell", "PRIVATE_SUMMARY", serde_json::json!({
            "background": true, "command": "PRIVATE_COMMAND", "cwd": "PRIVATE_PATH",
            "stdout": "PRIVATE_BODY", "pid": 987654321, "executable": "PRIVATE_EXECUTABLE"
        }));
        let span = Span::new("tool", None);
        let message = span.message(Phase::WorkerFinished, Details::outcome(&outcome));
        assert!(message.contains("background=true"));
        assert!(!message.contains("PRIVATE") && !message.contains("987654321"));
        let mut poisoned = outcome.clone();
        poisoned.data["background"] = serde_json::json!("PRIVATE_COMMAND");
        assert!(Details::outcome(&poisoned).background.is_none());
        poisoned.tool = "read_file";
        poisoned.data["background"] = serde_json::json!(true);
        assert!(Details::outcome(&poisoned).background.is_none());
    }

    #[test]
    fn unchanged_thousand_entry_snapshots_avoid_repeated_string_copies() {
        let mut buffer = Buffer::new(MAX_ENTRIES, MAX_BYTES, MAX_ENTRY_BYTES);
        for index in 0..MAX_ENTRIES {
            buffer.push(Level::Info, "test", &format!("安全事件 {index}"), index as u64);
        }
        let view = buffer.snapshot();
        assert_eq!(view.entries.len(), MAX_ENTRIES);
        assert!(view.entries.iter().zip(&buffer.entries).all(|(copy, original)| {
            copy.component.as_ptr() != original.component.as_ptr()
                && copy.message.as_ptr() != original.message.as_ptr()
        }));
        for repeats in [4, 60] {
            let mut legacy_copies = 0;
            let mut cached_copies = 0;
            let mut rebuilt = 0;
            for _ in 0..repeats {
                // 与优化前相同的 collect/cloned 路径，活跃分配地址不可重用。
                let legacy: Vec<_> = buffer.entries.iter().cloned().collect();
                legacy_copies += legacy.iter().zip(&buffer.entries).map(|(a, b)| {
                    usize::from(a.component.as_ptr() != b.component.as_ptr())
                        + usize::from(a.message.as_ptr() != b.message.as_ptr())
                }).sum::<usize>();
                let next = buffer.snapshot();
                rebuilt += usize::from(!Arc::ptr_eq(&view.entries, &next.entries));
                cached_copies += next.entries.iter().zip(view.entries.iter()).map(|(a, b)| {
                    usize::from(a.component.as_ptr() != b.component.as_ptr())
                        + usize::from(a.message.as_ptr() != b.message.as_ptr())
                }).sum::<usize>();
            }
            assert_eq!(legacy_copies, repeats * MAX_ENTRIES * 2);
            assert_eq!((cached_copies, rebuilt), (0, 0));
            println!("1000项，预热后{repeats}次读取：旧路径Box字符串复制={legacy_copies}，缓存复制={cached_copies}，缓存重建={rebuilt}");
        }
    }

    #[test]
    fn record_merge_eviction_and_clear_refresh_only_the_latest_snapshot() {
        let logger = Logger {
            started: Instant::now(),
            buffer: Mutex::new(Buffer::new(1, 100, 40)),
        };
        let empty = logger.lock().snapshot();
        logger.record(Level::Info, "test", "first");
        let first = logger.lock().snapshot();
        assert!(!Arc::ptr_eq(&empty.entries, &first.entries));
        let old = Arc::downgrade(&first.entries);
        logger.record(Level::Info, "test", "first");
        let merged = logger.lock().snapshot();
        assert!(!Arc::ptr_eq(&first.entries, &merged.entries));
        assert_eq!(first.entries[0].occurrences, 1);
        assert_eq!(merged.entries[0].occurrences, 2);
        assert_eq!(merged.stats.merged, 1);
        assert!(merged.entries[0].last_ms >= first.entries[0].last_ms);
        drop(first);
        assert!(old.upgrade().is_none(), "缓存不能累积历史快照");
        logger.record(Level::Error, "test", "second");
        let evicted = logger.lock().snapshot();
        assert!(!Arc::ptr_eq(&merged.entries, &evicted.entries));
        assert_eq!(evicted.stats.dropped, 2);
        assert_eq!(&*evicted.entries[0].message, "second");
        logger.lock().clear();
        let cleared = logger.lock().snapshot();
        assert!(!Arc::ptr_eq(&evicted.entries, &cleared.entries));
        assert!(cleared.entries.is_empty());
        assert_eq!(cleared.stats, Stats::default());
        assert!(Arc::ptr_eq(&cleared.entries, &logger.lock().snapshot().entries));
        logger.record(Level::Info, "test", "new");
        assert_eq!(logger.lock().snapshot().stats.received, 1);
    }

    #[test]
    fn dropped_and_truncated_records_invalidate_even_without_new_entries() {
        for (count, bytes) in [(0, 100), (3, 1)] {
            let mut buffer = Buffer::new(count, bytes, 20);
            let before = buffer.snapshot();
            buffer.push(Level::Warn, "test", &"长".repeat(100), 1);
            let after = buffer.snapshot();
            assert!(!Arc::ptr_eq(&before.entries, &after.entries));
            assert!(after.entries.is_empty());
            assert_eq!(after.stats.received, 1);
            assert_eq!(after.stats.dropped, 1);
            assert_eq!(after.stats.truncated, 1);
            assert_eq!(before.stats, Stats::default());
        }
    }

    #[test]
    fn count_and_byte_limits_evict_oldest() {
        let mut buffer = Buffer::new(2, 5, 20);
        buffer.push(Level::Info, "c", "aa", 0);
        buffer.push(Level::Info, "c", "bb", 1);
        assert_eq!(buffer.entries.len(), 1);
        assert_eq!(buffer.stats.bytes, 3);
        assert_eq!(buffer.stats.dropped, 1);
        let mut buffer = Buffer::new(2, 100, 20);
        for message in ["a", "b", "c"] {
            buffer.push(Level::Info, "c", message, 0);
        }
        assert_eq!(buffer.entries.len(), 2);
        assert_eq!(&*buffer.entries[0].message, "b");
        assert_eq!(buffer.stats.dropped, 1);
    }

    #[test]
    fn oversized_entry_is_dropped_and_zero_capacity_is_safe() {
        for (count, bytes) in [(0, 100), (3, 1)] {
            let mut buffer = Buffer::new(count, bytes, 20);
            buffer.push(Level::Info, "c", "aa", 0);
            assert!(buffer.entries.is_empty());
            assert_eq!(buffer.stats.dropped, 1);
            assert_eq!(buffer.stats.bytes, 0);
        }
    }

    #[test]
    fn unicode_truncation_and_controls_stay_bounded() {
        for limit in 0..40 {
            let mut buffer = Buffer::new(10, 100, limit);
            buffer.push(Level::Info, "组件", &"你好🦀".repeat(10), 0);
            assert!(buffer.entries[0].bytes() <= limit);
            assert_eq!(buffer.stats.truncated, 1);
        }
        assert_eq!(&*sanitize("你好\n警告\u{202e}", 100).0, "你好 警告 ");
        assert_eq!(
            &*sanitize("一\u{2028}二\u{2029}三\u{061c}\u{200e}\u{200f}", 100).0,
            "一 二 三   "
        );
        assert!(sanitize(&"中".repeat(10_000), 100).0.contains("省略"));
    }

    #[test]
    fn secrets_and_urls_are_not_retained() {
        for text in [
            "Authorization: Bearer TOPSECRET",
            "{\"api_key\":\"TOPSECRET\"}",
            "API-KEY = TOPSECRET",
            "api key: TOPSECRET",
            "access_token = TOPSECRET",
            "password=\"TOPSECRET other words\"",
            "Bearer TOPSECRET",
            "Basic TOPSECRET",
            "Basic\tTOPSECRET",
            "Basic\nTOPSECRET",
            "Basic\u{00a0}TOPSECRET",
            "sk-TOPSECRET",
            "ghp_TOPSECRET",
            "github_pat_TOPSECRET",
            "AIzaTOPSECRET",
            "eyJhbGciOiJIUzI1NiJ9.TOPSECRET.signature",
            "cookie: TOPSECRET",
            "https://user:TOPSECRET@example.org/path?value=TOPSECRET#fragment",
            "/models?value=TOPSECRET",
            "TOKEN:\nTOPSECRET",
            "组件 apiKey TOPSECRET",
        ] {
            assert_eq!(&*sanitize(text, 100).0, REDACTED, "{text}");
            let mut buffer = Buffer::new(10, 100, 100);
            buffer.push(Level::Error, text, text, 0);
            let view = buffer.snapshot();
            assert!(!format!("{view:?}").contains("TOPSECRET"));
        }
        assert_eq!(
            &*sanitize("工具执行失败：timeout", 100).0,
            "工具执行失败：timeout"
        );
    }

    #[test]
    fn interleaved_duplicates_are_aggregated_and_counted_on_eviction() {
        let mut buffer = Buffer::new(2, 100, 40);
        buffer.push(Level::Error, "tool", "失败", 1);
        buffer.push(Level::Info, "app", "启动", 2);
        buffer.push(Level::Error, "tool", "失败", 3);
        assert_eq!(buffer.entries.len(), 2);
        let entry = &buffer.entries[1];
        assert_eq!(
            (entry.first_ms, entry.last_ms, entry.occurrences),
            (1, 3, 2)
        );
        assert_eq!(buffer.stats.merged, 1);
        buffer.push(Level::Warn, "app", "警告", 4);
        buffer.push(Level::Debug, "app", "调试", 5);
        assert_eq!(buffer.stats.dropped, 3);
        assert_eq!(buffer.stats.received, 5);
    }

    #[test]
    fn clear_resets_all_counts_and_does_not_mutate_snapshots() {
        let mut buffer = Buffer::new(1, 100, 12);
        for message in ["a", "a", "很长很长很长很长很长"] {
            buffer.push(Level::Error, "c", message, 0);
        }
        let before = buffer.snapshot();
        assert!(before.stats.merged > 0 && before.stats.dropped > 0 && before.stats.truncated > 0);
        buffer.clear();
        assert!(buffer.entries.is_empty());
        assert_eq!(buffer.stats, Stats::default());
        assert_eq!(before.entries.len(), 1);
        buffer.push(Level::Info, "c", "新", 1);
        assert_eq!(buffer.stats.received, 1);
    }

    #[test]
    fn parallel_eviction_clear_and_snapshot_preserve_invariants() {
        let logger = Arc::new(Logger::new());
        std::thread::scope(|scope| {
            for worker in 0..4 {
                let logger = &logger;
                scope.spawn(move || {
                    for index in 0..600 {
                        logger.record(Level::Info, "test", &format!("安全事件 {worker}-{index}"));
                        if worker == 0 && index % 100 == 0 {
                            logger.lock().clear();
                        }
                        let view = logger.lock().snapshot();
                        assert!(view.entries.len() <= MAX_ENTRIES);
                        assert!(view.stats.bytes <= MAX_BYTES);
                        assert_eq!(view.stats.bytes, view.entries.iter().map(Entry::bytes).sum::<usize>());
                        assert_eq!(view.stats.received, view.stats.dropped + view.entries.iter().map(|entry| entry.occurrences).sum::<u64>());
                        assert!(view.entries.windows(2).all(|pair| pair[0].last_ms <= pair[1].last_ms));
                        assert!(view.entries.iter().all(|entry| entry.first_ms <= entry.last_ms));
                    }
                });
            }
        });
    }

    #[test]
    fn poisoned_lock_does_not_disable_safe_logging() {
        let logger = Arc::new(Logger::new());
        let worker = Arc::clone(&logger);
        assert!(std::thread::spawn(move || {
            let _guard = worker.lock();
            panic!("合成锁中毒");
        }).join().is_err());
        logger.record(Level::Warn, "test", "安全事件");
        assert_eq!(logger.lock().snapshot().stats.received, 1);
        logger.lock().clear();
        assert_eq!(logger.lock().snapshot().stats, Stats::default());
    }

    #[test]
    fn parallel_record_and_snapshot_are_consistent() {
        let logger = Arc::new(Logger::new());
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let logger = Arc::clone(&logger);
                std::thread::spawn(move || {
                    for _ in 0..500 {
                        logger.record(Level::Error, "tool", "执行失败：timeout");
                        let view = logger.lock().snapshot();
                        assert_eq!(
                            view.stats.bytes,
                            view.entries.iter().map(Entry::bytes).sum::<usize>()
                        );
                        assert!(view.entries.len() <= MAX_ENTRIES);
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        let view = logger.lock().snapshot();
        assert_eq!(view.stats.received, 4_000);
        assert_eq!(view.stats.merged, 3_999);
        assert_eq!(view.entries[0].occurrences, 4_000);
    }
