
    use super::*;

    fn active() -> ClassMonitor {
        let mut m = ClassMonitor::default();
        m.enabled = true;
        m.tx.scope = ClassScope { generation: 2, session: 3 };
        m.phase = ClassPhase::Active(Session::new(), None);
        m.status = "new class".into();
        m.analysis_in_flight = true;
        m
    }

    #[test]
    fn budgets_keep_utf8_recent_material_and_visible_notice() {
        let mut session = Session::new();
        for i in 0..300 {
            let text = format!("主题{i}{}最近作业{i}", "中文😀".repeat(800));
            push_material(&mut session.screen_notes, &text, NOTES_BYTES);
            push_material(&mut session.transcript, &text, TRANSCRIPT_BYTES);
        }
        for (items, limit) in [(&session.screen_notes, NOTES_BYTES), (&session.transcript, TRANSCRIPT_BYTES)] {
            assert!(items.iter().map(|s| s.len() + 1).sum::<usize>() <= limit);
            assert!(items.iter().all(|s| s.len() <= SEGMENT_BYTES));
            assert!(items.last().unwrap().contains("最近作业299"));
            assert!(items.join("\n").contains(LIMIT_NOTICE));
        }
        let request = material_excerpt(&session, REQUEST_NOTES_BYTES, REQUEST_TRANSCRIPT_BYTES);
        assert!(request.len() + 1_024 <= REQUEST_BYTES);
        assert!(request.contains("最近作业299"));
        assert!(fallback_summary(&session, "原始敏感错误").len() <= SUMMARY_BYTES);
        assert!(!fallback_summary(&session, "原始敏感错误").contains("原始敏感错误"));
        let ready = ReadySummary::new(session, "总结".repeat(10_000), true);
        assert!(ready.summary.len() <= SUMMARY_BYTES);
        assert!(ready.summary.contains(LIMIT_NOTICE));
    }

    #[test]
    fn streams_stop_at_content_reasoning_and_event_budgets() {
        for limit in [VISION_OUTPUT_BYTES, SUMMARY_BYTES] {
            let (tx, rx) = mpsc::channel();
            tx.send(neo_llm::Event::Delta { content: "中文😀".repeat(limit), reasoning: String::new() }).unwrap();
            let stream = neo_llm::Stream::new_for_test(rx);
            let text = drain_limited(&stream, Instant::now() + VISION_TIMEOUT, &AtomicBool::new(false), limit).unwrap();
            assert!(text.len() <= limit);
            assert!(text.contains(OUTPUT_NOTICE));
            assert!(stream.cancel.load(Ordering::Relaxed));
        }
        for reasoning in [true, false] {
            let (tx, rx) = mpsc::channel();
            for _ in 0..if reasoning { 1 } else { STREAM_EVENTS } {
                tx.send(neo_llm::Event::Delta { content: String::new(),
                    reasoning: if reasoning { "x".repeat(STREAM_BYTES) } else { String::new() } }).unwrap();
            }
            let stream = neo_llm::Stream::new_for_test(rx);
            assert!(drain_stream(&stream, Instant::now() + VISION_TIMEOUT, &AtomicBool::new(false)).unwrap_err().contains("预算"));
            assert!(stream.cancel.load(Ordering::Relaxed));
        }
    }

    #[test]
    fn failures_are_retained_and_capacity_pauses_new_sessions() {
        let mut m = active();
        m.phase = ClassPhase::Presenting;
        for i in 0..SUMMARY_SLOTS {
            m.publish_summary(ReadySummary::new(Session::new(), format!("result{i}"), true), true, false);
        }
        m.dismiss();
        assert_eq!(m.ready.as_ref().unwrap().summary, "result0");
        assert!(m.ready.as_ref().unwrap().close_blocked);
        for _ in 0..100 {
            m.on_event(ScopedEvent { scope: m.tx.scope, event: ClassEvent::Maximized });
        }
        assert_eq!(m.summary_slots(), SUMMARY_SLOTS);
        assert!(matches!(m.phase, ClassPhase::Presenting));
        assert!(m.status.contains("已暂停"));
        m.shutdown();
        assert_eq!(m.summary_slots(), SUMMARY_SLOTS);
        assert_eq!(m.ready.as_ref().unwrap().summary, "result0");
        m.ready.as_mut().unwrap().save_state = SaveState::Saved;
        m.dismiss();
        assert_eq!(m.ready.as_ref().unwrap().summary, "result1");
    }

    #[test]
    fn save_recovery_index_failure_and_duplicate_events_are_idempotent() {
        // 本测试唯一的磁盘路径位于临时 NEO_HOME；只注入文件失败，不启动任何设备或网络。
        struct HomeGuard { old: Option<std::ffi::OsString>, path: std::path::PathBuf }
        impl Drop for HomeGuard {
            fn drop(&mut self) {
                match &self.old {
                    Some(old) => std::env::set_var("NEO_HOME", old),
                    None => std::env::remove_var("NEO_HOME"),
                }
                let _ = std::fs::remove_dir_all(&self.path);
            }
        }
        let path = std::env::temp_dir().join(format!("neo-class-reliability-{}-{}", std::process::id(), now_ms()));
        std::fs::create_dir_all(&path).unwrap();
        let _home = HomeGuard { old: std::env::var_os("NEO_HOME"), path: path.clone() };
        std::env::set_var("NEO_HOME", &path);
        let mut m = active();
        m.phase = ClassPhase::Polishing;
        m.polishing.push(m.tx.scope);
        // 把 class 目录位置变成文件，真实 append 必须失败。
        std::fs::write(path.join("class"), b"blocked").unwrap();
        m.on_event(ScopedEvent { scope: m.tx.scope, event: ClassEvent::Polished {
            summary: "离线测试总结".into(), session: Box::new(Session::new()),
        }});
        assert_eq!(m.ready.as_ref().unwrap().save_state, SaveState::Unsaved);
        m.dismiss();
        for _ in 0..3 { m.retry_save(); }
        assert!(m.ready.as_ref().unwrap().pending_note.is_some());
        assert_eq!(m.summary_slots(), 1);
        std::fs::remove_file(path.join("class")).unwrap();
        // 分记忆可恢复，但记忆文件被目录挡住：不得再次 append 已成功阶段。
        std::fs::create_dir(path.join("memories.json")).unwrap();
        m.retry_save();
        let date = m.ready.as_ref().unwrap().date.clone();
        assert_eq!(m.ready.as_ref().unwrap().save_state, SaveState::IndexFailed);
        assert!(m.ready.as_ref().unwrap().pending_note.is_none());
        for _ in 0..3 { m.retry_save(); }
        assert_eq!(neo_tools::classlog::load_day(&date).len(), 1);
        std::fs::remove_dir(path.join("memories.json")).unwrap();
        m.retry_save();
        assert_eq!(m.ready.as_ref().unwrap().save_state, SaveState::Saved);
        for _ in 0..3 { m.retry_save(); }
        m.ready.as_mut().unwrap().retry_with(|_, _| panic!("重复 append"), |_| panic!("重复 index"));
        assert_eq!(neo_tools::classlog::load_day(&date).len(), 1);
        let memory: serde_json::Value = serde_json::from_slice(&std::fs::read(path.join("memories.json")).unwrap()).unwrap();
        assert_eq!(memory.as_array().unwrap().len(), 1);
        m.dismiss();
        // 已关闭卡片的同一结果重复投递，不能重新 append 或重新发布。
        m.on_event(ScopedEvent { scope: m.tx.scope, event: ClassEvent::Polished {
            summary: "迟到重复结果".into(), session: Box::new(Session::new()),
        }});
        assert!(m.ready.is_none());
        assert_eq!(neo_tools::classlog::load_day(&date).len(), 1);
        assert_eq!(m.summary_pings, 1);

        // 模拟午夜在落盘前到来，提交后丢回执，再在次日重试；不改系统时钟。
        let clock = std::cell::Cell::new("2026-12-31");
        let mut first = ReadySummary::new(Session::new(), "跨日回执测试".into(), true);
        first.date = clock.get().into();
        let mut committed = None;
        first.retry_with(|target, note| {
            clock.set("2027-01-01");
            assert_ne!(target, clock.get());
            committed = Some(neo_tools::classlog::append_class_on(target, note).unwrap());
            Err(neo_tools::ToolError::io("lost receipt"))
        }, |_| panic!("must not index before receipt"));
        assert_eq!(first.save_state, SaveState::Unsaved);
        assert!(first.pending_note.is_some());
        let committed = committed.unwrap();
        let mut next = ReadySummary::new(Session::new(), "次日课堂测试".into(), true);
        next.date = clock.get().into();
        next.retry();
        assert_eq!(next.save_state, SaveState::Saved);

        // 回执恢复后只剩索引失败；反复重试不能把前日课堂写进次日文件。
        first.retry_with(|target, note| {
            assert_eq!(target, "2026-12-31");
            let saved = neo_tools::classlog::append_class_on(target, note)?;
            assert_eq!((saved.id, saved.ended_ms), (committed.id, committed.ended_ms));
            Ok(saved)
        }, |_| Err(neo_tools::ToolError::io("index blocked")));
        assert_eq!(first.save_state, SaveState::IndexFailed);
        for _ in 0..3 {
            first.retry_with(|_, _| panic!("must not append after receipt"), |_| {
                Err(neo_tools::ToolError::io("index blocked"))
            });
        }
        first.retry_with(|_, _| panic!("must not append after receipt"), |text| {
            assert!(text.contains(&format!("class/2026-12-31.json，第 {} 节", committed.id)));
            neo_tools::tools::memory::add_memory(text).map(|_| ())
        });
        assert_eq!(first.save_state, SaveState::Saved);
        first.retry_with(|_, _| panic!("duplicate append"), |_| panic!("duplicate index"));
        assert_eq!(first.date, "2026-12-31");
        for ready in [&first, &next] {
            let day = neo_tools::classlog::load_day(&ready.date);
            let matching: Vec<_> = day.iter().filter(|note| note.summary == ready.summary).collect();
            assert_eq!(matching.len(), 1);
            let other = if ready.date == first.date { &next } else { &first };
            assert!(!day.iter().any(|note| note.summary == other.summary));
        }
        let memory: serde_json::Value = serde_json::from_slice(&std::fs::read(path.join("memories.json")).unwrap()).unwrap();
        assert_eq!(memory.as_array().unwrap().len(), 3);
    }

    #[test]
    fn capacity_resume_is_cancelled_when_teaching_window_leaves() {
        let mut m = active();
        m.phase = ClassPhase::Presenting;
        m.seed_pending_summaries();
        m.on_event(ScopedEvent { scope: m.tx.scope, event: ClassEvent::Maximized });
        assert!(!m.can_resume());
        m.ready.as_mut().unwrap().save_state = SaveState::Saved;
        m.dismiss();
        assert!(m.can_resume());
        m.on_event(ScopedEvent { scope: m.tx.scope, event: ClassEvent::Unmaximized });
        assert!(!m.can_resume());
    }

    #[test]
    fn disabling_retains_active_and_polishing_material_without_io() {
        let mut m = active();
        let old = ClassScope { generation: 2, session: 2 };
        let mut session = Session::new();
        session.transcript.push("old material".into());
        m.polishing.push(old);
        m.polishing_material.push((old, session));
        m.tx.send(ClassEvent::Line("queued tail".into())).unwrap();
        m.shutdown();
        assert_eq!(m.pending_saves(), 2);
        assert!(m.ready.as_ref().unwrap().summary.contains("queued tail"));
        assert!(m.queued.front().unwrap().summary.contains("old material"));
        m.shutdown();
        assert_eq!(m.pending_saves(), 2);
        m.enabled = true;
        m.on_event(ScopedEvent { scope: old, event: ClassEvent::Polished {
            summary: "late result".into(), session: Box::new(Session::new()),
        }});
        assert_eq!(m.pending_saves(), 2);
        assert!(m.queued.front().unwrap().pending_note.is_some());
    }

    #[test]
    fn save_and_index_retries_keep_each_cards_fixed_date() {
        for date in ["2026-12-31", "2027-01-01"] {
            let mut ready = ReadySummary::new(Session::new(), "summary".into(), true);
            ready.date = date.into();
            ready.retry_with(|target, _| {
                assert_eq!(target, date);
                Err(neo_tools::ToolError::io("blocked"))
            }, |_| panic!("must not index unsaved note"));
            assert_eq!(ready.save_state, SaveState::Unsaved);
            ready.retry_with(|target, note| {
                assert_eq!(target, date);
                Ok(neo_tools::classlog::ClassNote {
                    id: 7, started_ms: note.started_ms, ended_ms: now_ms(), subject: note.subject,
                    summary: note.summary, over_limit: false, screen_notes: note.screen_notes,
                    transcript: note.transcript,
                })
            }, |_| Err(neo_tools::ToolError::io("blocked")));
            assert_eq!(ready.date, date);
            assert_eq!(ready.save_state, SaveState::IndexFailed);
            ready.retry_with(|_, _| panic!("must not append again"), |text| {
                assert!(text.contains(&format!("class/{date}.json，第 7 节")));
                Ok(())
            });
            assert_eq!(ready.date, date);
            assert_eq!(ready.save_state, SaveState::Saved);
        }
    }

    #[test]
    fn stale_events_cannot_mutate_new_class() {
        let mut m = active();
        for scope in [ClassScope { generation: 1, session: 3 }, ClassScope { generation: 2, session: 2 }] {
            for event in [ClassEvent::Line("old".into()),
                ClassEvent::Vision { subject: Some("old".into()), note: "old".into() },
                ClassEvent::VisionFailed("old".into()), ClassEvent::SttDied("old".into())] {
                m.on_event(ScopedEvent { scope, event });
            }
        }
        m.on_event(ScopedEvent { scope: ClassScope { generation: 1, session: 3 }, event: ClassEvent::Unmaximized });
        assert!(m.analysis_in_flight);
        assert_eq!(m.status, "new class");
        let ClassPhase::Active(session, _) = &m.phase else { panic!("changed phase") };
        assert!(session.transcript.is_empty());
        assert!(session.screen_notes.is_empty());
        assert!(session.subject.is_none());
    }

    #[test]
    fn missing_audio_warning_is_once_scoped_and_cancel_safe() {
        let mut m = active();
        let mut sent = false;
        report_missing_audio(false, &mut sent, &m.tx, &AtomicBool::new(false));
        report_missing_audio(true, &mut sent, &m.tx, &AtomicBool::new(true));
        assert!(!sent);
        assert!(m.rx.try_recv().is_err());
        report_missing_audio(true, &mut sent, &m.tx, &AtomicBool::new(false));
        report_missing_audio(true, &mut sent, &m.tx, &AtomicBool::new(false));
        let event = m.rx.try_recv().unwrap();
        assert!(matches!(event.event, ClassEvent::AudioMissing));
        assert!(m.rx.try_recv().is_err());
        m.phase = ClassPhase::Settling(Session::new(), Instant::now());
        m.on_event(event);
        assert_eq!(m.status(), Some(AUDIO_MISSING_NOTICE));
        assert!(matches!(m.phase, ClassPhase::Settling(..)));
        m.status = "current".into();
        for scope in [ClassScope { generation: 1, session: 3 }, ClassScope { generation: 2, session: 2 }] {
            m.on_event(ScopedEvent { scope, event: ClassEvent::AudioMissing });
        }
        assert_eq!(m.status(), Some("current"));
    }

    #[test]
    fn settling_accepts_current_tail() {
        let mut m = active();
        m.phase = ClassPhase::Settling(Session::new(), Instant::now());
        m.on_event(ScopedEvent { scope: m.tx.scope, event: ClassEvent::Line("tail".into()) });
        let ClassPhase::Settling(session, _) = &m.phase else { panic!("changed phase") };
        assert_eq!(session.transcript, ["tail"]);
    }

    #[test]
    fn closed_summary_does_not_publish_but_normal_crossing_does() {
        let mut m = active();
        let ready = || ReadySummary::new(Session::new(), "summary".into(), true);
        m.publish_summary(ready(), false, false);
        assert!(m.ready.is_none());
        assert_eq!(m.summary_pings, 0);
        assert!(m.analysis_in_flight);
        m.publish_summary(ready(), true, true);
        assert!(m.ready.is_some());
        assert_eq!(m.summary_pings, 1);
        assert!(matches!(m.phase, ClassPhase::Active(..)));
        assert_eq!(m.status, "new class");
    }

    #[test]
    fn old_polish_cannot_block_or_complete_new_session() {
        let mut m = active();
        m.phase = ClassPhase::Polishing;
        let old = m.tx.scope;
        m.begin_session();
        assert!(!m.analysis_in_flight);
        m.analysis_in_flight = true;
        for phase in [ClassPhase::Active(Session::new(), None), ClassPhase::Polishing] {
            m.phase = phase;
            let (publish, crossing) = m.complete_polish(old);
            assert!(publish && crossing);
            assert!(m.analysis_in_flight);
            let polishing = matches!(m.phase, ClassPhase::Polishing);
            m.publish_summary(ReadySummary::new(Session::new(), "summary".into(), true), publish, crossing);
            assert_eq!(matches!(m.phase, ClassPhase::Polishing), polishing);
            assert_eq!(m.status, "new class");
        }
        assert_eq!(m.complete_polish(m.tx.scope), (true, false));
        assert!(!m.analysis_in_flight);
    }

    #[test]
    fn reopen_rejects_previous_generation_summary_bookkeeping() {
        let mut m = active();
        let old = m.tx.scope;
        m.shutdown();
        m.shutdown();
        assert_eq!(m.tx.scope.generation, old.generation + 1);
        m.enabled = true;
        m.begin_session();
        m.phase = ClassPhase::Polishing;
        m.analysis_in_flight = true;
        assert_eq!(m.complete_polish(old), (false, true));
        assert!(m.analysis_in_flight);
        assert!(m.ready.is_none());
    }

    #[test]
    fn pending_stt_keeps_tail_visible_before_completion() {
        let pending = Arc::new(std::sync::atomic::AtomicUsize::new(1));
        let done = SttDone(pending.clone());
        let m = active();
        m.tx.send(ClassEvent::Line("tail".into())).unwrap();
        assert_eq!(pending.load(Ordering::Acquire), 1);
        drop(done);
        assert_eq!(pending.load(Ordering::Acquire), 0);
        assert!(matches!(m.rx.try_recv().unwrap().event, ClassEvent::Line(text) if text == "tail"));
    }

    #[test]
    fn shutdown_never_waits_for_engine_lease() {
        let mut m = active();
        let slot = m.stt_engine.clone();
        let _lease = slot.lock().unwrap();
        m.shutdown();
        assert!(!m.enabled);
        assert_eq!(m.tx.scope.generation, 3);
    }

    #[test]
    fn stopped_worker_does_not_load_or_open_microphone() {
        let m = active();
        stt_main(m.tx.clone(), Arc::new(AtomicBool::new(true)), m.stt_engine.clone(), m.cache_enabled.clone(), m.cancelled.clone());
        assert!(m.rx.try_recv().is_err());
        assert!(m.stt_engine.lock().unwrap().is_none());
    }

    #[test]
    fn shutdown_cancels_workers_but_normal_stop_keeps_summary_enabled() {
        let mut m = active();
        let cancelled = m.cancelled.clone();
        let stop = Arc::new(AtomicBool::new(false));
        m.stt_stop = Some(stop.clone());
        m.stop_stt();
        assert!(stop.load(Ordering::Relaxed));
        assert!(!cancelled.load(Ordering::Acquire));
        m.begin_session();
        assert!(!cancelled.load(Ordering::Acquire));
        m.shutdown();
        assert!(cancelled.load(Ordering::Acquire));
    }

    #[test]
    fn cancelled_workers_skip_capture_network_and_locked_engine() {
        let m = active();
        let cancelled = Arc::new(AtomicBool::new(true));
        let cfg = neo_llm::Config { base_url: String::new(), api_key: String::new(), model: String::new(), thinking: Default::default(), context_tokens: neo_llm::CONTEXT_TOKENS };
        assert!(matches!(run_vision(&cfg, true, &cancelled), ClassEvent::VisionFailed(_)));
        assert!(run_polish(&cfg, &Session::new(), &cancelled).is_err());
        let _lease = m.stt_engine.lock().unwrap();
        stt_main(m.tx.clone(), Arc::new(AtomicBool::new(false)), m.stt_engine.clone(), m.cache_enabled.clone(), cancelled);
        assert!(m.rx.try_recv().is_err());
    }

    #[test]
    fn waiting_stt_lease_observes_cancellation() {
        let m = active();
        let slot = m.stt_engine.clone();
        let lease = slot.lock().unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancelled.clone();
        let worker_slot = slot.clone();
        let tx = m.tx.clone();
        let (done, finished) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            stt_main(tx, Arc::new(AtomicBool::new(false)), worker_slot, Arc::new(AtomicBool::new(false)), worker_cancel);
            let _ = done.send(());
        });
        cancelled.store(true, Ordering::Release);
        let result = finished.recv_timeout(Duration::from_secs(1));
        drop(lease);
        worker.join().unwrap();
        assert!(result.is_ok());
        assert!(m.rx.try_recv().is_err());
    }

    #[test]
    fn cancelled_stream_rejects_queued_completion() {
        let (tx, rx) = mpsc::channel();
        tx.send(neo_llm::Event::Done { tool_calls: false }).unwrap();
        let stream = neo_llm::Stream::new_for_test(rx);
        assert!(drain_stream(&stream, Instant::now() + VISION_TIMEOUT, &AtomicBool::new(true)).is_err());
        assert!(stream.cancel.load(Ordering::Relaxed));
        assert!(stream.rx.try_recv().is_ok());
    }

    #[test]
    fn stream_cancellation_wakes_without_network_events() {
        let (_tx, rx) = mpsc::channel();
        let stream = neo_llm::Stream::new_for_test(rx);
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancel = cancelled.clone();
        let worker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            cancel.store(true, Ordering::Release);
        });
        assert!(drain_stream(&stream, Instant::now() + Duration::from_secs(2), &cancelled).unwrap_err().contains("取消"));
        worker.join().unwrap();
        assert!(stream.cancel.load(Ordering::Relaxed));
    }

    #[test]
    fn stream_deadline_applies_to_queued_events() {
        let (tx, rx) = mpsc::channel();
        for _ in 0..10_000 {
            tx.send(neo_llm::Event::Delta { content: String::new(), reasoning: String::new() }).unwrap();
        }
        let stream = neo_llm::Stream::new_for_test(rx);
        assert!(drain_stream(&stream, Instant::now(), &AtomicBool::new(false)).is_err());
        assert!(stream.cancel.load(Ordering::Relaxed));
        assert!(stream.rx.try_recv().is_ok());
    }

    #[test]
    fn disconnected_stream_does_not_publish_truncated_summary() {
        let (tx, rx) = mpsc::channel();
        tx.send(neo_llm::Event::Delta { content: "partial".into(), reasoning: String::new() }).unwrap();
        drop(tx);
        let stream = neo_llm::Stream::new_for_test(rx);
        assert!(drain_stream(&stream, Instant::now() + VISION_TIMEOUT, &AtomicBool::new(false)).unwrap_err().contains("中断"));
    }

    #[test]
    fn shutdown_discards_queued_success_and_fallback_before_persistence() {
        let mut m = active();
        let old = m.tx.clone();
        m.shutdown();
        old.send(ClassEvent::Polished { summary: "old".into(), session: Box::new(Session::new()) }).unwrap();
        old.send(ClassEvent::PolishFailed { error: "old".into(), session: Box::new(Session::new()) }).unwrap();
        // 模拟重新启用后才排到旧结果；on_event 必须在调用落盘路径前拒绝。
        m.enabled = true;
        while let Ok(event) = m.rx.try_recv() {
            m.on_event(event);
        }
        assert!(m.ready.is_none());
        assert_eq!(m.summary_pings, 0);
        assert!(matches!(m.phase, ClassPhase::Idle));
    }

    #[test]
    fn stream_normal_completion_keeps_text() {
        let (tx, rx) = mpsc::channel();
        tx.send(neo_llm::Event::Delta { content: "ok".into(), reasoning: String::new() }).unwrap();
        tx.send(neo_llm::Event::Done { tool_calls: false }).unwrap();
        let stream = neo_llm::Stream::new_for_test(rx);
        assert_eq!(drain_stream(&stream, Instant::now() + VISION_TIMEOUT, &AtomicBool::new(false)).unwrap(), "ok");
        assert!(!stream.cancel.load(Ordering::Relaxed));
    }
