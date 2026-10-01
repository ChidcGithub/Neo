
    use super::*;

    #[test]
    fn edits_wait_for_other_writers_and_cancellation_prevents_commit() {
        use std::sync::{atomic::{AtomicBool, Ordering}, Arc, mpsc};
        use std::time::Duration;
        let root = std::env::temp_dir().join(format!("neo-edit-lock-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("fixture.txt");
        std::fs::write(&path, "first second").unwrap();
        let token = Arc::new(AtomicBool::new(false));
        let scope = Scope::new(&root).with_cancel(token.clone());
        for cancel in [false, true] {
            let held = super::super::memory::lock_data_file(&path).unwrap();
            std::thread::scope(|s| {
                let (tx, rx) = mpsc::channel();
                let scope = &scope;
                s.spawn(move || {
                    tx.send(crate::dispatch(scope, "edit_file", &json!({
                        "path": "fixture.txt", "old_string": "first", "new_string": "edited"
                    }))).unwrap();
                });
                assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
                if cancel {
                    token.store(true, Ordering::Release);
                    let out = rx.recv_timeout(Duration::from_secs(5)).unwrap();
                    assert_eq!(out.error.unwrap().kind, ErrorKind::NotAllowed);
                    drop(held);
                } else {
                    std::fs::write(&path, "first other-update").unwrap();
                    drop(held);
                    assert!(rx.recv_timeout(Duration::from_secs(5)).unwrap().error.is_none());
                }
            });
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "edited other-update");
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_target_stays_not_found_without_creating_lock() {
        let root = std::env::temp_dir().join(format!("neo-edit-missing-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let out = crate::dispatch(&Scope::new(&root), "edit_file", &json!({
            "path": "missing.txt", "old_string": "old", "new_string": "new"
        }));
        assert_eq!(out.error.unwrap().kind, ErrorKind::NotFound);
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn expanded_edit_rejects_without_changing_file() {
        let root = std::env::temp_dir().join(format!("neo-edit-budget-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("fixture.txt");
        std::fs::write(&path, "aa").unwrap();
        let result = crate::dispatch(&Scope::new(&root), "edit_file", &json!({
            "path": "fixture.txt", "old_string": "a",
            "new_string": "x".repeat(limits::WRITE_BYTES as usize), "replace_all": true,
        }));
        let actual = std::fs::read(&path).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(result.error.unwrap().kind, ErrorKind::TooLarge);
        assert_eq!(actual, b"aa");
    }

    #[test]
    fn replacement_size_is_checked_before_allocation() {
        assert_eq!(checked_replacement_len(4, 1, 3, 4, 12).unwrap(), 12);
        assert_eq!(checked_replacement_len(4, 1, 3, 4, 11).unwrap_err().kind, ErrorKind::TooLarge);
        assert_eq!(checked_replacement_len(12, 3, 0, 4, 1).unwrap(), 0);
        assert_eq!(checked_replacement_len(10, 1, 2, 1, 10).unwrap_err().kind, ErrorKind::TooLarge);
        assert_eq!(checked_replacement_len(2, 1, usize::MAX, 2, u64::MAX).unwrap_err().kind, ErrorKind::TooLarge);
        assert_eq!(checked_replacement_len(usize::MAX, 1, 2, 1, u64::MAX).unwrap_err().kind, ErrorKind::TooLarge);
        assert_eq!(checked_replacement_len(6, "汉".len(), "文文".len(), 2, 12).unwrap(), 12);
    }
