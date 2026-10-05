
    use super::*;
    #[test]
    fn stalled_thread_does_not_block_shutdown() {
        let (release, wait) = mpsc::channel();
        let (done, completed) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _ = wait.recv();
            let _ = done.send(());
        });
        assert!(!join_bounded(worker, Duration::from_millis(20)));
        release.send(()).unwrap();
        completed.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(join_bounded(std::thread::spawn(|| {}), Duration::from_secs(1)));
    }
    #[test]
    fn modal_requires_confirmed_splash_shutdown() {
        let runtime = Runtime {
            active: AtomicBool::new(true), starting: AtomicBool::new(false),
            splash_running: AtomicBool::new(true), policy: AtomicU8::new(1),
            log: Mutex::new(None),
            #[cfg(windows)]
            events: Mutex::new(None),
        };
        assert!(!wait_splash_closed(&runtime, Duration::ZERO));
        runtime.splash_running.store(false, Ordering::Release);
        assert!(wait_splash_closed(&runtime, Duration::ZERO));
    }
    #[test]
    fn early_preferences_are_strict_and_errors_are_categorized() {
        let dir = std::env::temp_dir().join(format!("neo-startup-policy-{}-{}", std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.db");
        let store = neo_store::Store::open(&path).unwrap();
        store.set_setting("classroom_safe", "true").unwrap();
        store.set_setting("silent_startup_errors", "true").unwrap();
        assert_eq!(early_policy(path.clone(), Duration::from_secs(2)), Ok(1));
        store.set_setting("silent_startup_errors", "1").unwrap();
        assert_eq!(early_policy(path.clone(), Duration::from_secs(2)), Ok(3));
        drop(store);
        fs::write(dir.join("invalid.db"), "invalid sqlite").unwrap();
        assert_eq!(early_policy(dir.join("invalid.db"), Duration::from_secs(2)), Err("preference_open_failed"));
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn strict_early_policy_and_dialog_matrix() {
        for classroom in [None, Some("1"), Some("0"), Some("true"), Some("")] {
            for silent in [None, Some("1"), Some("0"), Some("true"), Some("")] {
                assert_eq!(suppress_dialog(policy(classroom, silent)), classroom != Some("0") && silent == Some("1"));
            }
        }
    }
    #[test]
    fn uninitialized_api_is_inert() {
        assert!(RUNTIME.get().is_none());
        set_silent_policy(true, true);
        finish_splash();
        record_error("settings", "secret");
        fatal("window", "secret");
        assert!(!poll_duplicate());
        assert!(log_dir().is_none());
    }
    #[test]
    fn log_writer_subprocess() {
        let Some(dir) = std::env::var_os("NEO_STARTUP_LOG_TEST_DIR") else {
            return;
        };
        let dir = PathBuf::from(dir);
        let deadline = Instant::now() + Duration::from_secs(10);
        for index in 0..800 {
            loop {
                match append_log(&dir, &log_line("database", "database_open_failed", true)) {
                    Ok(()) => break,
                    // Production logging is bounded/best-effort; this stress test must
                    // complete every write without assuming fair OS lock scheduling.
                    Err(error)
                        if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("log write {index}/800 failed: {error:?}"),
                }
            }
            assert!(
                Instant::now() < deadline,
                "log writer exceeded total time budget"
            );
        }
    }
    #[test]
    fn concurrent_process_log_rotation() {
        let dir = std::env::temp_dir().join(format!(
            "neo-startup-process-log-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let children: Vec<_> = (0..3)
            .map(|_| {
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "startup::tests::log_writer_subprocess",
                        "--test-threads=1",
                    ])
                    .env("NEO_STARTUP_LOG_TEST_DIR", &dir)
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .unwrap()
            })
            .collect();
        // Reap every writer before asserting, even when one writer failed.
        let outputs: Vec<_> = children
            .into_iter()
            .map(|child| child.wait_with_output().unwrap())
            .collect();
        for (index, output) in outputs.iter().enumerate() {
            assert!(
                output.status.success(),
                "log writer {index} exited {}\nstdout:\n{}\nstderr:\n{}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        for entry in fs::read_dir(&dir).unwrap().map(Result::unwrap) {
            if entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "log")
            {
                assert!(entry.metadata().unwrap().len() <= LOG_LIMIT);
                for line in fs::read_to_string(entry.path()).unwrap().lines() {
                    assert!(line.starts_with("timestamp="));
                    assert!(line.ends_with("component=database category=database_open_failed"));
                }
            }
        }
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn held_log_lock_returns_busy_without_writing() {
        let dir = std::env::temp_dir().join(format!(
            "neo-startup-held-lock-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join("startup.lock"))
            .unwrap();
        lock.try_lock().unwrap();
        let result = append_log(&dir, "must not be written\n");
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::WouldBlock);
        assert!(!dir.join("startup.log").exists());
        drop(lock);
        append_log(&dir, "after release\n").unwrap();
        assert_eq!(
            fs::read_to_string(dir.join("startup.log")).unwrap(),
            "after release\n"
        );
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn bounded_rotation_and_directory_fallback() {
        let dir = std::env::temp_dir().join(format!("neo-startup-log-test-{}-{}", std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir_all(&dir).unwrap();
        let blocked = dir.join("not-a-directory");
        fs::write(&blocked, "blocked").unwrap();
        let logs = dir.join("logs");
        assert_eq!(prepare_log([blocked.join("log"), logs.clone()]), Some(logs.clone()));
        let line = log_line("sk-sensitive\nforged=true", "sk-sensitive\nforged=true", true);
        assert!(!line.contains("sensitive"));
        assert_eq!(line.lines().count(), 1);
        for _ in 0..(LOG_LIMIT as usize * 6 / line.len()) { append_log(&logs, &line).unwrap(); }
        let files: Vec<_> = fs::read_dir(&logs).unwrap().map(Result::unwrap)
            .filter(|entry| entry.path().extension().is_some_and(|extension| extension == "log")).collect();
        assert_eq!(files.len(), LOG_BACKUPS + 1);
        for file in files { assert!(file.metadata().unwrap().len() <= LOG_LIMIT); }
        fs::remove_dir_all(dir).unwrap();
    }
