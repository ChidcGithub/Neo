
    use super::*;

    #[test]
    fn waiting_write_can_be_cancelled_without_replacing_original() {
        use std::sync::{atomic::{AtomicBool, Ordering}, Arc, mpsc};
        use std::time::Duration;
        let root = std::env::temp_dir().join(format!("neo-write-cancel-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("fixture.txt");
        std::fs::write(&path, "keep").unwrap();
        let held = super::super::memory::lock_data_file(&path).unwrap();
        let token = Arc::new(AtomicBool::new(false));
        let scope = Scope::new(&root).with_cancel(token.clone());
        std::thread::scope(|s| {
            let (tx, rx) = mpsc::channel();
            s.spawn(move || { tx.send(crate::dispatch(&scope, "write_file", &json!({
                "path": "fixture.txt", "content": "cancelled", "overwrite": true
            }))).unwrap(); });
            assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
            token.store(true, Ordering::Release);
            assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap().error.unwrap().kind, ErrorKind::NotAllowed);
        });
        drop(held);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn overwrite_budget_and_windows_replace_failure_preserve_original() {
        let root = std::env::temp_dir().join(format!("neo-write-safe-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("原文件.txt");
        std::fs::write(&path, "保留").unwrap();
        let scope = Scope::new(&root);
        let out = crate::dispatch(&scope, "write_file", &json!({"path": "原文件.txt", "content": "拒绝覆盖"}));
        assert_eq!(out.error.unwrap().kind, ErrorKind::Conflict);
        let out = crate::dispatch(&scope, "write_file", &json!({"path": "原文件.txt", "content": "x".repeat(limits::WRITE_BYTES as usize + 1), "overwrite": true}));
        assert_eq!(out.error.unwrap().kind, ErrorKind::TooLarge);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            let held = std::fs::OpenOptions::new().read(true).share_mode(1).open(&path).unwrap();
            for (tool, args) in [
                ("write_file", json!({"path": "原文件.txt", "content": "替换", "overwrite": true})),
                ("edit_file", json!({"path": "原文件.txt", "old_string": "保留", "new_string": "替换"})),
            ] {
                let out = crate::dispatch(&scope, tool, &args);
                assert_eq!(out.error.unwrap().kind, ErrorKind::Io);
                assert_eq!(std::fs::read_to_string(&path).unwrap(), "保留");
            }
            drop(held);
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "保留");
        let out = crate::dispatch(&scope, "write_file", &json!({"path": "原文件.txt", "content": "替换", "overwrite": true}));
        assert!(out.error.is_none());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "替换");
        std::fs::remove_dir_all(root).unwrap();
    }
