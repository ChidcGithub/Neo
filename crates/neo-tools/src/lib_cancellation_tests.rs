
    use super::*;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    #[test]
    fn cancelled_dispatch_rejects_writes_before_side_effects() {
        let token = Arc::new(AtomicBool::new(false));
        let root = std::env::temp_dir().join(format!("neo-cancel-dispatch-{}", std::process::id()));
        let scope = Scope::new(&root).with_cancel(token.clone());
        let cloned = scope.clone();
        token.store(true, Ordering::Release);
        let result = dispatch(
            &cloned,
            "write_file",
            &serde_json::json!({
                "path": "must-not-exist.txt", "content": "cancelled"
            }),
        );
        let error = result.error.unwrap();
        assert_eq!(error.kind, ErrorKind::NotAllowed);
        assert!(error.message.contains("取消"));
        assert!(!root.join("must-not-exist.txt").exists());
        // 即使参数无效或工具不存在，也先返回取消，而不是继续分发。
        assert!(dispatch(&cloned, "unknown", &serde_json::Value::Null)
            .error
            .unwrap()
            .message
            .contains("取消"));
    }
