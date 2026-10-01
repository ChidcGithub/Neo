use super::*;

#[test]
fn cancelled_tap_never_starts_device_handshake() {
    assert!(AudioTap::start_cancellable(Arc::new(AtomicBool::new(true))).is_err());
    let (tx, rx) = mpsc::sync_channel(TAP_FRAMES);
    let (hello, ready) = mpsc::channel();
    tap_main(tx, hello, Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(true)), Arc::new(AtomicBool::new(false)));
    assert!(rx.try_recv().is_err());
    assert!(ready.try_recv().is_err());
}

#[test]
fn handshake_uses_absolute_deadline_and_cancel_before_queued_success() {
    let (tx, rx) = mpsc::channel();
    tx.send(Ok("synthetic".into())).unwrap();
    let stop = AtomicBool::new(false);
    assert!(wait_tap_hello(&rx, &stop, &AtomicBool::new(false), Instant::now()).is_err());
    assert!(wait_tap_hello(&rx, &stop, &AtomicBool::new(true), Instant::now() + Duration::from_secs(1)).is_err());
    assert!(wait_tap_hello(&rx, &AtomicBool::new(true), &AtomicBool::new(false), Instant::now() + Duration::from_secs(1)).is_err());
    assert_eq!(wait_tap_hello(&rx, &stop, &AtomicBool::new(false), Instant::now() + Duration::from_secs(1)).unwrap(), "synthetic");
}

#[test]
fn cancelled_wake_skips_models_and_microphone() {
    let (tx, rx) = mpsc::channel();
    let _lease = WAKE_LEASE.lock().unwrap_or_else(|p| p.into_inner());
    engine_main(WakeConfig::default(), tx, Arc::new(AtomicBool::new(true)), Arc::new(AtomicU8::new(MODE_DETECT)), Arc::new(WakeMonitor::new(0.25)));
    assert!(rx.try_recv().is_err());
}

#[test]
fn dropping_wake_does_not_join_a_busy_worker() {
    let (release, wait) = mpsc::channel();
    let thread = std::thread::spawn(move || { let _ = wait.recv(); });
    let stop = Arc::new(AtomicBool::new(false));
    let engine = WakeEngine { stop: stop.clone(), mode: Arc::new(AtomicU8::new(MODE_DETECT)), monitor: Arc::new(WakeMonitor::new(0.25)), thread: Some(thread) };
    let (done, finished) = mpsc::channel();
    let dropper = std::thread::spawn(move || { drop(engine); let _ = done.send(()); });
    let result = finished.recv_timeout(Duration::from_secs(1));
    release.send(()).unwrap();
    dropper.join().unwrap();
    assert!(result.is_ok());
    assert!(stop.load(Ordering::Relaxed));
}

#[test]
fn dropping_tap_does_not_join_a_busy_worker() {
    let (release, wait) = mpsc::channel();
    let thread = std::thread::spawn(move || { let _ = wait.recv(); });
    let (_tx, rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let tap = AudioTap { rx, stop: stop.clone(), missing: Arc::new(AtomicBool::new(false)), thread: Some(thread), device: "synthetic".into() };
    let (done, finished) = mpsc::channel();
    let dropper = std::thread::spawn(move || { drop(tap); let _ = done.send(()); });
    let result = finished.recv_timeout(Duration::from_secs(1));
    release.send(()).unwrap();
    dropper.join().unwrap();
    assert!(result.is_ok());
    assert!(stop.load(Ordering::Relaxed));
}

#[test]
fn normal_stop_cancels_waiting_handshake_without_cancelling_generation() {
    let (_tx, rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = stop.clone();
    let cancelled = AtomicBool::new(false);
    let worker = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(10));
        worker_stop.store(true, Ordering::Relaxed);
    });
    let result = wait_tap_hello(&rx, &stop, &cancelled, Instant::now() + Duration::from_secs(1));
    worker.join().unwrap();
    assert!(result.unwrap_err().contains("取消"));
    assert!(!cancelled.load(Ordering::Acquire));
    assert!(AudioTap::start_with_stop(stop, Arc::new(cancelled)).is_err());
}

#[test]
fn stop_joins_producer_without_losing_queued_tail() {
    let (tx, rx) = mpsc::sync_channel(TAP_FRAMES);
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    let missing = Arc::new(AtomicBool::new(false));
    let worker_missing = missing.clone();
    let thread = std::thread::spawn(move || {
        assert!(push_tap_frame(&tx, &worker_missing, vec![1.0; FRAME_SAMPLES]));
        while !stopped.load(Ordering::Relaxed) {
            std::thread::yield_now();
        }
        assert!(push_tap_frame(&tx, &worker_missing, vec![2.0; 17]));
    });
    let mut tap = AudioTap { rx, stop, missing, thread: Some(thread), device: "synthetic".into() };
    assert!(tap.stop_capture());
    assert!(!tap.is_alive());
    assert!(!tap.has_missing_audio());
    assert_eq!(tap.next_frame(Duration::ZERO).unwrap().len(), FRAME_SAMPLES);
    assert_eq!(tap.next_frame(Duration::ZERO).unwrap(), vec![2.0; 17]);
    assert!(tap.next_frame(Duration::ZERO).is_none());
    assert!(tap.stop_capture());
}

#[test]
fn slow_consumer_has_bounded_fifo_and_full_queue_does_not_block_stop() {
    let (tx, rx) = mpsc::sync_channel(TAP_FRAMES);
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    let missing = Arc::new(AtomicBool::new(false));
    let worker_missing = missing.clone();
    let (ready, filled) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        for i in 0..TAP_FRAMES * 4 {
            assert!(push_tap_frame(&tx, &worker_missing, vec![i as f32; FRAME_SAMPLES]));
        }
        ready.send(()).unwrap();
        while !stopped.load(Ordering::Relaxed) { std::thread::yield_now(); }
        assert!(push_tap_frame(&tx, &worker_missing, vec![-1.0; 17]));
    });
    let mut tap = AudioTap { rx, stop, missing, thread: Some(thread), device: "synthetic".into() };
    let full = filled.recv_timeout(Duration::from_secs(2));
    let stopped = tap.stop_capture();
    assert!(full.is_ok());
    assert!(stopped);
    assert!(tap.has_missing_audio());
    for i in 0..TAP_FRAMES {
        assert_eq!(tap.next_frame(Duration::ZERO).unwrap(), vec![i as f32; FRAME_SAMPLES]);
    }
    assert!(tap.next_frame(Duration::ZERO).is_none());
    let (tx, rx) = mpsc::sync_channel(1);
    drop(rx);
    assert!(!push_tap_frame(&tx, &AtomicBool::new(false), vec![0.0]));
}

#[test]
fn explicit_stop_times_out_without_claiming_busy_worker_is_dead() {
    let (release, wait) = mpsc::channel();
    let (_tx, rx) = mpsc::sync_channel(TAP_FRAMES);
    let stop = Arc::new(AtomicBool::new(false));
    let thread = std::thread::spawn(move || { let _ = wait.recv(); });
    let mut tap = AudioTap { rx, stop: stop.clone(), missing: Arc::new(AtomicBool::new(false)), thread: Some(thread), device: "synthetic".into() };
    let (done, finished) = mpsc::channel();
    let stopper = std::thread::spawn(move || {
        let start = Instant::now();
        let stopped = tap.stop_capture();
        let _ = done.send((stopped, tap.is_alive(), tap.has_missing_audio(), start.elapsed()));
        tap
    });
    let result = finished.recv_timeout(Duration::from_secs(2));
    // 即使回归为无限 join，也先放开假驱动再断言，测试不遗留挂起线程。
    release.send(()).unwrap();
    let mut tap = stopper.join().unwrap();
    let (stopped, alive, missing, elapsed) = result.expect("stop exceeded bounded wait");
    assert!(!stopped && alive && missing);
    assert!(elapsed >= TAP_STOP_TIMEOUT && elapsed < Duration::from_secs(2));
    assert!(stop.load(Ordering::Relaxed));
    assert!(tap.stop_capture());
    assert!(!tap.is_alive());
    assert!(tap.has_missing_audio());
}
