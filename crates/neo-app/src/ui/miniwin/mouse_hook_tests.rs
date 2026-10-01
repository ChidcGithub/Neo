
    use super::*;

    #[test]
    fn interrupt_hook_filters_injection_but_keeps_physical_touch() {
        let slot = ClickSlot::default();
        for flags in [1, 2, 3] { slot.mock_down(flags, 0, 10, 20); }
        slot.record(false, 0, 0, 10, 20);
        assert_eq!(slot.latest(), None);
        slot.mock_down(0, 0, -1920, -240);
        let first = slot.latest().unwrap();
        assert_eq!(first.position, egui::pos2(-1920.0, -240.0));
        assert_eq!(first.sequence, 1);
        slot.mock_down(1, 0, 99, 99);
        assert_eq!(slot.latest(), Some(first), "模型点击不能覆盖真实事件");
        slot.mock_down(1, 0xff51_578a, 30, 40);
        assert_eq!(slot.latest().unwrap().sequence, 2);
        assert_eq!(slot.latest().unwrap().position, egui::pos2(30.0, 40.0));
        for (flags, extra) in [(3, 0xff51_578a), (1, 0xff51_570a), (1, 0x1234_0080)] {
            slot.mock_down(flags, extra, 99, 99);
            assert_eq!(slot.latest().unwrap().sequence, 2);
        }
    }

    #[test]
    fn interrupt_hook_mailbox_is_coherent_and_does_not_wait_for_writer() {
        let slot = ClickSlot::default();
        slot.mock_down(0, 0, 10, 20);
        slot.version.store(3, Ordering::SeqCst);
        assert!(slot.latest().is_none(), "写入中立即返回，下帧重试");
        slot.position.store((30_u64 << 32) | 40, Ordering::SeqCst);
        slot.version.store(4, Ordering::SeqCst);
        assert_eq!(slot.latest(), Some(Click { sequence: 2, position: egui::pos2(30.0, 40.0) }));
    }

    #[test]
    fn interrupt_hook_shutdown_never_joins_a_stalled_worker() {
        // 无 Windows hook/真实输入：故意让 worker 等到 Drop 返回后才退出。
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let (release, wait) = std::sync::mpsc::channel();
        let (done, finished) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            wait.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
            assert!(worker_stop.load(Ordering::Acquire));
            done.send(()).unwrap();
        });
        let hook = MouseHook { slot: Arc::new(ClickSlot::default()), stop: stop.clone(), tid: Arc::new(AtomicU32::new(0)), thread: Some(thread) };
        assert!(!hook.finished());
        assert!(hook.latest().is_none());
        drop(hook);
        assert!(stop.load(Ordering::Acquire));
        release.send(()).unwrap();
        finished.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
    }
