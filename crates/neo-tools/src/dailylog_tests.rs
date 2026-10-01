
    use super::*;

    /// 隔离：指向临时 NEO_HOME，跑完恢复（跨模块靠 NEO_HOME_TEST_LOCK 串行）。
    struct EnvGuard(Option<String>);
    impl EnvGuard {
        fn set(dir: &std::path::Path) -> Self {
            let old = std::env::var("NEO_HOME").ok();
            std::env::set_var("NEO_HOME", dir);
            Self(old)
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.0 {
                Some(v) => std::env::set_var("NEO_HOME", v),
                None => std::env::remove_var("NEO_HOME"),
            }
        }
    }

    fn temp_home(tag: &str) -> (EnvGuard, PathBuf) {
        let dir = std::env::temp_dir().join(format!("neo-dailylog-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let guard = EnvGuard::set(&dir);
        (guard, dir)
    }

    #[test]
    fn append_and_load_roundtrip_updates_index() {
        let _lock = crate::NEO_HOME_TEST_LOCK.lock().unwrap();
        let (_g, _dir) = temp_home("roundtrip");
        let n1 = append(" 10:32 看到学生打游戏 ").unwrap();
        assert_eq!(n1.content, "10:32 看到学生打游戏");
        let n2 = append("14:05 语文课《岳阳楼记》").unwrap();
        assert_eq!(n2.id, n1.id + 1, "id 应递增");

        let day = load_day(&today_key());
        assert_eq!(day.len(), 2);
        assert_eq!(day[0].content, "10:32 看到学生打游戏");

        let index = load_index();
        assert_eq!(index.len(), 1);
        assert_eq!(index[0].date, today_key());
        assert!(index[0].gist.contains("打游戏"), "索引应含 gist：{}", index[0].gist);
        assert!(index[0].gist.contains("岳阳楼记"), "索引应含两条：{}", index[0].gist);
    }

    #[test]
    fn failed_index_write_is_recovered_without_duplicate_note() {
        let _lock = crate::NEO_HOME_TEST_LOCK.lock().unwrap();
        let (_g, dir) = temp_home("index-recovery");
        std::fs::create_dir_all(index_path()).unwrap();
        let saved = append("正文成功，索引失败").unwrap();
        assert!(pending_index_path().exists());
        assert_eq!(load_day(&today_key()).len(), 1);
        assert!(append("恢复前不提交下一条").is_err());
        std::fs::remove_dir(index_path()).unwrap();
        let index = load_index();
        assert_eq!(index.len(), 1);
        assert!(index[0].gist.contains("正文成功"));
        assert!(!pending_index_path().exists());
        assert_eq!(load_day(&today_key())[0].id, saved.id);
        append("下一条").unwrap();
        assert_eq!(load_day(&today_key()).len(), 2);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn unreadable_index_preserves_history_and_pending_recovery() {
        use std::os::windows::fs::OpenOptionsExt;
        let _lock = crate::NEO_HOME_TEST_LOCK.lock().unwrap();
        let (_g, dir) = temp_home("index-sharing");
        let history = vec![DailyNote { id: 1, ts_ms: 0, content: "历史记录".into() }];
        save_json(&day_path("2024-02-29"), &history).unwrap();
        update_index_unlocked("2024-02-29", &history).unwrap();
        let before = std::fs::read(index_path()).unwrap();
        let held = std::fs::OpenOptions::new().read(true).share_mode(0).open(index_path()).unwrap();
        let note = append("正文已提交").unwrap();
        let body = std::fs::read(day_path(&today_key())).unwrap();
        assert!(pending_index_path().exists());
        assert!(append("不可重复写入").is_err());
        assert!(load_index().is_empty());
        assert_eq!(std::fs::read(day_path(&today_key())).unwrap(), body);
        drop(held);
        assert_eq!(std::fs::read(index_path()).unwrap(), before);
        for _ in 0..2 {
            let index = load_index();
            assert_eq!(index.len(), 2);
            assert_eq!(index[0].date, "2024-02-29");
            assert_eq!(index[0].gist, "历史记录");
            assert_eq!(index[1].gist, "正文已提交");
        }
        assert!(!pending_index_path().exists());
        let notes = load_day(&today_key());
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].id, note.id);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn failed_body_and_pending_cleanup_recover_idempotently() {
        use std::os::windows::fs::OpenOptionsExt;
        let _lock = crate::NEO_HOME_TEST_LOCK.lock().unwrap();
        let (_g, dir) = temp_home("commit-stages");
        let first = append("原记录").unwrap();
        let path = day_path(&today_key());
        let before = std::fs::read(&path).unwrap();
        let held = std::fs::OpenOptions::new().read(true).share_mode(1).open(&path).unwrap();
        assert!(append("稍后重试").is_err());
        assert!(pending_index_path().exists());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        drop(held);
        assert_eq!(load_index()[0].gist, "原记录");
        assert!(!pending_index_path().exists());
        assert_eq!(append("稍后重试").unwrap().id, first.id + 1);
        let before = std::fs::read(&path).unwrap();
        save_json(&pending_index_path(), &Some(today_key())).unwrap();
        let held = std::fs::OpenOptions::new().read(true).share_mode(1).open(pending_index_path()).unwrap();
        for _ in 0..2 {
            let index = load_index();
            assert_eq!(index.len(), 1);
            assert_eq!(index[0].gist, "原记录；稍后重试");
            assert!(pending_index_path().exists());
        }
        assert!(append("清理失败时不追加").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        drop(held);
        assert_eq!(load_index().len(), 1);
        assert!(!pending_index_path().exists());
        assert_eq!(load_day(&today_key()).len(), 2);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn append_rejects_empty_and_oversize() {
        let _lock = crate::NEO_HOME_TEST_LOCK.lock().unwrap();
        let (_g, _dir) = temp_home("reject");
        assert!(append("   ").is_err());
        let long = "长".repeat(501);
        let err = append(&long).unwrap_err();
        assert_eq!(err.kind, crate::ErrorKind::TooLarge);
    }

    #[test]
    fn corrupt_day_file_is_quarantined() {
        let _lock = crate::NEO_HOME_TEST_LOCK.lock().unwrap();
        let (_g, _dir) = temp_home("corrupt");
        let path = day_path(&today_key());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{ not json").unwrap();
        assert!(load_day(&today_key()).is_empty());
        assert!(path.with_extension("json.bad").exists());
    }

    #[test]
    fn pending_index_recovery_uses_recorded_day_and_gist_stays_bounded() {
        let _lock = crate::NEO_HOME_TEST_LOCK.lock().unwrap();
        let (_g, dir) = temp_home("past-day");
        let notes: Vec<_> = (1..=100).map(|id| DailyNote {
            id, ts_ms: 0, content: "字".repeat(20),
        }).collect();
        assert!(gist_of(&notes).chars().count() <= 80);
        save_json(&day_path("2024-02-29"), &notes).unwrap();
        save_json(&pending_index_path(), &Some("2024-02-29")).unwrap();
        let index = load_index();
        assert_eq!(index.len(), 1);
        assert_eq!(index[0].date, "2024-02-29");
        assert_eq!(load_day("2024-02-29").len(), 100);
        assert!(load_day("../memories").is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn index_char_cap_drops_oldest_but_keeps_newest() {
        let _lock = crate::NEO_HOME_TEST_LOCK.lock().unwrap();
        let (_g, _dir) = temp_home("cap");
        // 60 天、每天 4 条顶格 gist（~80 字符）：总重远超 4000 上限。
        for day in 1..=60 {
            let date = format!("2026-01-{day:02}");
            let notes: Vec<DailyNote> = (1..=4)
                .map(|i| DailyNote {
                    id: i,
                    ts_ms: 0,
                    content: format!("第 {i} 条很长的记录内容，撑满单条 20 字符的上限"),
                })
                .collect();
            update_index_unlocked(&date, &notes).unwrap();
        }
        let index = load_index();
        let total: usize = index
            .iter()
            .map(|e| e.date.len() + e.gist.chars().count())
            .sum();
        assert!(total <= INDEX_MAX_CHARS, "索引超重: {total}");
        assert_eq!(index.last().unwrap().date, "2026-01-60", "最新一条必须保住");
        assert!(index.len() < 60, "最早的应被丢弃，剩 {} 条", index.len());
        // 窗口必须连续（从某天起到最后一天）。
        let first: usize = index.first().unwrap().date[8..].parse().unwrap();
        assert_eq!(first + index.len() - 1, 60, "丢弃后日期应连续");
    }
