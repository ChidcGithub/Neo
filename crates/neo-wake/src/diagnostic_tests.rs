use super::*;

fn fake_engine() -> WakeEngine {
    WakeEngine {
        stop: Arc::new(AtomicBool::new(false)),
        mode: Arc::new(AtomicU8::new(MODE_DETECT)),
        monitor: Arc::new(WakeMonitor::new(0.25)),
        thread: None,
    }
}

#[test]
fn diagnostics_are_opt_in_and_rate_limited_without_events() {
    let engine = fake_engine();
    let start = Instant::now();
    let mut meter = DiagnosticMeter::new(start);
    meter.audio(&[i16::MIN, i16::MAX]);
    meter.score(start, 0.9);
    assert_eq!(meter.samples, 0);
    assert_eq!(meter.power, 0.0);
    assert!(meter.scores.is_empty());
    engine.set_diagnostics(true);
    meter.enable(true, start);
    meter.audio(&[16384, -16384]);
    meter.score(start, 0.7);
    meter.publish(&engine.monitor, start + Duration::from_millis(199), 7, false);
    assert!(engine.diagnostics().rms.is_none());
    meter.publish(&engine.monitor, start + DIAGNOSTIC_INTERVAL, 7, false);
    let s = engine.diagnostics();
    assert_eq!(s.rms, Some(0.5));
    assert!((s.dbfs.unwrap() + 6.0206).abs() < 0.001);
    assert_eq!(s.peak, Some(0.5));
    assert_eq!(s.score, Some(0.7));
    assert_eq!(s.score_peak_2s, Some(0.7));
    assert_eq!(s.warmup_frames, 7);
    assert_eq!(s.warmup_total, 25);
    assert!(s.last_audio_ms > 0);
    engine.set_diagnostics(false);
    meter.enable(false, start + DIAGNOSTIC_INTERVAL);
    meter.audio(&[i16::MIN; 100]);
    assert_eq!(meter.samples, 0);
    assert!(engine.diagnostics().rms.is_none());
    assert!(engine.diagnostics().score.is_none());
    assert_eq!(engine.mode.load(Ordering::Relaxed), MODE_DETECT);
    assert!(!engine.stop.load(Ordering::Relaxed));
}

#[test]
fn silence_and_full_scale_are_finite_and_old_scores_expire() {
    let engine = fake_engine();
    engine.set_diagnostics(true);
    let start = Instant::now();
    let mut meter = DiagnosticMeter::new(start);
    meter.enable(true, start);
    meter.audio(&[0; 10]);
    meter.score(start, 0.9);
    meter.publish(&engine.monitor, start + DIAGNOSTIC_INTERVAL, 25, false);
    assert_eq!(engine.diagnostics().dbfs, Some(-120.0));
    meter.audio(&[i16::MIN; 10]);
    meter.score(start + Duration::from_secs(1), 0.2);
    meter.publish(&engine.monitor, start + Duration::from_secs(2), 25, false);
    let s = engine.diagnostics();
    assert_eq!(s.dbfs, Some(0.0));
    assert_eq!(s.peak, Some(1.0));
    assert_eq!(s.score_peak_2s, Some(0.2));
    meter.publish(&engine.monitor, start + Duration::from_secs(3), 25, false);
    assert!(engine.diagnostics().score_peak_2s.is_none());
    assert!(engine.diagnostics().rms.is_none());
    for _ in 0..1000 { meter.score(start + Duration::from_secs(3), 0.4); }
    assert_eq!(meter.scores.len(), SCORE_HISTORY);
}

#[test]
fn lifecycle_error_is_bounded_and_survives_diagnostic_toggles() {
    let engine = fake_engine();
    assert_eq!(engine.diagnostics().phase, WakePhase::Loading);
    engine.monitor.update(|s| {
        s.phase = WakePhase::Ready;
        s.device_name = Some("fake microphone".into());
        s.sample_rate = Some(48_000);
    });
    engine.monitor.fail(&"错".repeat(1000));
    engine.set_diagnostics(true);
    engine.set_diagnostics(false);
    let s = engine.diagnostics();
    assert_eq!(s.phase, WakePhase::Error);
    assert_eq!(s.error.unwrap().chars().count(), 512);
    assert_eq!(s.device_name.as_deref(), Some("fake microphone"));
    assert_eq!(s.sample_rate, Some(48_000));
    assert_eq!(s.threshold, 0.25);
}

#[test]
fn test_mode_counts_without_dispatch_and_rejects_in_flight_results() {
    let engine = fake_engine();
    let (tx, rx) = mpsc::channel();
    assert!(engine.monitor.detected(0, 0.6, &tx));
    assert!(matches!(rx.try_recv(), Ok(WakeEvent::Detected { epoch: 0, score }) if score == 0.6));
    engine.set_test_mode(true);
    engine.set_test_mode(true); // 幂等，不重复重置正在测试的窗口。
    assert_eq!(engine.monitor.test_epoch.load(Ordering::Acquire), 1);
    assert!(engine.monitor.detected(0, 0.8, &tx));
    assert!(engine.monitor.detected(1, 0.8, &tx));
    assert!(rx.try_recv().is_err());
    let s = engine.diagnostics();
    assert!(s.test_mode);
    assert_eq!(s.hit_count, 2);
    assert!(s.last_hit_ms > 0);
    engine.set_test_mode(false);
    assert!(engine.monitor.detected(1, 0.8, &tx));
    assert!(rx.try_recv().is_err());
    assert_eq!(engine.diagnostics().hit_count, 2);
    assert_eq!(engine.diagnostics().threshold, 0.25);
    assert!(engine.monitor.detected(2, 0.5, &tx));
    assert!(matches!(rx.try_recv(), Ok(WakeEvent::Detected { .. })));
    engine.set_test_mode(true);
    engine.set_test_mode(false);
    assert_eq!(engine.monitor.test_epoch.load(Ordering::Acquire), 4);
    assert_eq!(engine.diagnostics().warmup_frames, 0);
}

#[test]
fn event_epoch_survives_forwarding_and_normal_dictation_switches() {
    let engine = fake_engine();
    let (tx, rx) = mpsc::channel();
    assert!(engine.monitor.detected(engine.event_epoch(), 0.8, &tx));
    let detected = rx.recv().unwrap();
    let audio = WakeEvent::Audio { epoch: engine.event_epoch(), frame: vec![0.1; 16] };
    assert!(detected.is_current(engine.event_epoch()));
    engine.set_dictation(true);
    assert!(audio.is_current(engine.event_epoch()));
    engine.set_dictation(false);
    assert!(detected.is_current(engine.event_epoch()));
    assert!(audio.is_current(engine.event_epoch()));

    engine.set_test_mode(true);
    assert!(!detected.is_current(engine.event_epoch()));
    assert!(!audio.is_current(engine.event_epoch()));
    assert!(!WakeEvent::Audio { epoch: engine.event_epoch(), frame: vec![] }.is_current(engine.event_epoch()));
    assert!(WakeEvent::Error("synthetic failure".into()).is_current(engine.event_epoch()));
    engine.set_test_mode(false);
    assert_eq!(engine.event_epoch(), 2);
    assert!(!detected.is_current(engine.event_epoch()));
    assert!(!audio.is_current(engine.event_epoch()));
    assert!(engine.monitor.detected(engine.event_epoch(), 0.7, &tx));
    assert!(rx.recv().unwrap().is_current(engine.event_epoch()));
    engine.set_dictation(true);
    assert!(WakeEvent::Audio { epoch: 2, frame: vec![0.2] }.is_current(engine.event_epoch()));
    assert!(WakeEvent::Error("synthetic failure".into()).is_current(engine.event_epoch()));
}

#[test]
fn test_transition_discards_queued_audio_and_filter_tail() {
    let (tx, rx) = mpsc::sync_channel(SAMPLE_CHUNKS);
    for _ in 0..SAMPLE_CHUNKS { tx.try_send(vec![i16::MAX; 10]).unwrap(); }
    let mut resampler = Resampler::new(48_000, SAMPLE_RATE);
    let mut pending = Vec::new();
    resampler.process(&[i16::MAX; 128], &mut pending);
    assert!(resampler.started);
    let mut chunk = vec![1.0; FRAME_SAMPLES * CHUNK_FRAMES];
    reset_test_audio(&rx, &mut resampler, 48_000, &mut chunk);
    assert!(rx.try_recv().is_err());
    assert!(!resampler.started);
    assert!(chunk.iter().all(|&s| s == 0.0));
    pending.clear();
    resampler.process(&[0; 128], &mut pending);
    assert!(pending.iter().all(|&s| s == 0));
}
