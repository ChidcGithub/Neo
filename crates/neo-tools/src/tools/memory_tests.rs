
    use super::*;

    /// 隔离：指向临时 NEO_HOME，跑完恢复。
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
        let dir = std::env::temp_dir().join(format!("neo-mem-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        (EnvGuard::set(&dir), dir)
    }

    #[test]
    fn waiting_memory_tools_cancel_without_writing() {
        use std::sync::{atomic::AtomicBool, Arc, mpsc};
        use std::time::Duration;
        let _lock = crate::NEO_HOME_TEST_LOCK.lock().unwrap();
        let (_g, dir) = temp_home("cancel");
        add_memory("keep").unwrap();
        let before = std::fs::read(memories_path()).unwrap();
        for (tool, args) in [
            ("remember", serde_json::json!({"content": "cancelled"})),
            ("forget", serde_json::json!({"query": "keep"})),
        ] {
            let held = lock_data_file(&memories_path()).unwrap();
            let token = Arc::new(AtomicBool::new(false));
            let scope = Scope::new(&dir).with_cancel(token.clone());
            std::thread::scope(|s| {
                let (tx, rx) = mpsc::channel();
                s.spawn(move || { tx.send(crate::dispatch(&scope, tool, &args)).unwrap(); });
                assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
                token.store(true, Ordering::Release);
                let out = rx.recv_timeout(Duration::from_secs(5)).unwrap();
                assert_eq!(out.error.unwrap().kind, crate::ErrorKind::NotAllowed);
            });
            drop(held);
            assert_eq!(std::fs::read(memories_path()).unwrap(), before);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn cross_process_storage_writers() {
        const CHILD: &str = "NEO_STORAGE_TEST_CHILD";
        if std::env::var_os(CHILD).is_some() {
            let root = PathBuf::from(std::env::var_os("NEO_HOME").unwrap());
            assert!(root.starts_with(std::env::temp_dir()));
            let path = root.join("counter.json");
            for i in 0..8 {
                let _disk = lock_data_file(&path).unwrap();
                let value: u64 = load_json(&path).unwrap();
                std::thread::sleep(std::time::Duration::from_millis(2));
                save_json(&path, &(value + 1)).unwrap();
                drop(_disk);
                add_memory(&format!("process {} note {i}", std::process::id())).unwrap();
                crate::dailylog::append(&format!("process {} note {i}", std::process::id())).unwrap();
                let out = crate::dispatch(&Scope::new(&root), "edit_file", &serde_json::json!({
                    "path": "edits.txt", "old_string": "count=", "new_string": "count=x"
                }));
                assert!(out.error.is_none(), "{:?}", out.error);
            }
            return;
        }
        let _lock = crate::NEO_HOME_TEST_LOCK.lock().unwrap();
        let (_g, dir) = temp_home("processes");
        std::fs::write(dir.join("edits.txt"), "count=").unwrap();
        let mut children: Vec<_> = (0..3).map(|_| {
            std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "tools::memory::tests::cross_process_storage_writers", "--nocapture"])
                .env(CHILD, "1").env("NEO_HOME", &dir)
                .stdout(std::process::Stdio::null()).spawn().unwrap()
        }).collect();
        for child in &mut children {
            assert!(child.wait().unwrap().success());
        }
        assert_eq!(load_json::<u64>(&dir.join("counter.json")).unwrap(), 24);
        assert_eq!(load_memories().len(), 24);
        assert_eq!(crate::dailylog::load_day(&crate::classlog::today_key()).len(), 24);
        assert_eq!(crate::dailylog::load_index().len(), 1);
        assert_eq!(std::fs::read_to_string(dir.join("edits.txt")).unwrap(), format!("count={}", "x".repeat(24)));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn persistence_failures_preserve_data_and_retries_recover() {
        let _lock = crate::NEO_HOME_TEST_LOCK.lock().unwrap();
        let (_g, dir) = temp_home("failures");
        add_memory("保留数据").unwrap();
        let path = memories_path();
        let before = std::fs::read(&path).unwrap();
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // 不共享 DELETE：模拟其它程序持有目标文件，原子替换必须失败。
            let held = std::fs::OpenOptions::new().read(true).share_mode(1).open(&path).unwrap();
            assert!(add_memory("失败重试").is_err());
            assert_eq!(std::fs::read(&path).unwrap(), before);
            drop(held);
            add_memory("失败重试").unwrap();
            assert_eq!(load_memories().len(), 2);
            // 不共享 READ：不能误判为空集合并覆盖。
            let held = std::fs::OpenOptions::new().read(true).share_mode(0).open(&path).unwrap();
            assert!(add_memory("不可读").is_err());
            drop(held);
            assert_eq!(load_memories().len(), 2);
        }
        #[cfg(not(windows))]
        assert_eq!(std::fs::read(&path).unwrap(), before);
        std::fs::write(&path, b"bad one").unwrap();
        assert!(load_memories().is_empty());
        std::fs::write(&path, [0xff, 0xfe]).unwrap();
        assert!(load_memories().is_empty());
        assert_eq!(std::fs::read(path.with_extension("json.bad")).unwrap(), b"bad one");
        assert_eq!(std::fs::read_dir(&dir).unwrap().filter(|e| e.as_ref().unwrap().path().extension().is_some_and(|e| e == "bad")).count(), 2);
        save_json(&path, &vec![Memory { id: u64::MAX, content: "编号耗尽".into(), updated_ms: 0 }]).unwrap();
        let before = std::fs::read(&path).unwrap();
        assert!(add_memory("不能溢出").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn concurrent_memory_writers_do_not_lose_updates() {
        let _lock = crate::NEO_HOME_TEST_LOCK.lock().unwrap();
        let (_g, dir) = temp_home("concurrent");
        std::thread::scope(|s| {
            for i in 0..12 {
                s.spawn(move || { add_memory(&format!("合成记录 {i}")).unwrap(); });
            }
        });
        let mut ids: Vec<_> = load_memories().iter().map(|m| m.id).collect();
        ids.sort_unstable();
        assert_eq!(ids, (1..=12).collect::<Vec<_>>());
        // 不经过 FILE_LOCK，验证独立文件句柄锁确实串行化读改写。
        let path = dir.join("counter.json");
        std::thread::scope(|s| {
            for _ in 0..8 {
                let path = &path;
                s.spawn(move || {
                    let _guard = lock_data_file(path).unwrap();
                    let value: u64 = load_json(path).unwrap();
                    save_json(path, &(value + 1)).unwrap();
                });
            }
        });
        assert_eq!(load_json::<u64>(&path).unwrap(), 8);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn forget_rejects_empty_normalized_query_without_changing_memories() {
        let _lock = crate::NEO_HOME_TEST_LOCK.lock().unwrap();
        let (_g, dir) = temp_home("empty-forget");
        let (first, _) = add_memory("保留第一条").unwrap();
        add_memory("保留最近一条").unwrap();
        let before = std::fs::read(memories_path()).unwrap();
        for query in ["#", "###", "  #  ", "", "   "] {
            assert_eq!(forget_memory(query).unwrap_err().kind, crate::ErrorKind::BadArguments);
            assert_eq!(std::fs::read(memories_path()).unwrap(), before);
        }
        assert_eq!(forget_memory(&format!("#{}", first.id)).unwrap().unwrap().id, first.id);
        assert_eq!(load_memories().len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// 环境变量是进程级的：两个测试并行会互相串台，合成一个跑。
    /// 跨模块（classlog 也改 NEO_HOME）靠 `NEO_HOME_TEST_LOCK` 串行。
    #[test]
    fn add_dedup_forget_and_import_roundtrip() {
        let _lock = crate::NEO_HOME_TEST_LOCK.lock().unwrap();
        let (_g, dir) = temp_home("roundtrip");
        let (m1, fresh) = add_memory(" 用户教高二物理 ").unwrap();
        assert!(fresh);
        assert_eq!(m1.content, "用户教高二物理");
        // 完全相同内容不重复记
        let (m2, fresh2) = add_memory("用户教高二物理").unwrap();
        assert!(!fresh2);
        assert_eq!(m1.id, m2.id);
        assert_eq!(load_memories().len(), 1);
        // 关键词删
        let removed = forget_memory("高二物理").unwrap().unwrap();
        assert_eq!(removed.id, m1.id);
        assert!(load_memories().is_empty());
        assert!(forget_memory("不存在的东西").unwrap().is_none());

        // 导入合并：同内容跳过、新内容重排 id
        add_memory("甲").unwrap();
        let src = dir.join("in.json");
        std::fs::write(
            &src,
            r#"[{"id":1,"content":"甲","updated_ms":0},{"id":2,"content":"乙","updated_ms":0}]"#,
        )
        .unwrap();
        let (added, skipped) = import_memories(&src).unwrap();
        assert_eq!((added, skipped), (1, 1));
        let list = load_memories();
        assert_eq!(list.len(), 2);
        assert_eq!(list.iter().map(|m| m.id).max(), Some(2));
    }
