use super::*;
use std::sync::mpsc;

fn shared() -> Shared {
    Shared::new(Arc::new(AtomicBool::new(true)), egui::Context::default())
}
fn desktop() -> Rect {
    Rect {
        x: -10000,
        y: -4000,
        width: 20000,
        height: 8000,
    }
}
fn wait_finished(handle: &CaptureHandle) {
    let until = Instant::now() + Duration::from_secs(5);
    while !handle.is_finished() {
        assert!(Instant::now() < until, "fake worker did not exit");
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn real_start_is_disabled_even_with_live_connection() {
    assert!(
        CaptureHandle::start(Arc::new(AtomicBool::new(true)), egui::Context::default()).is_err()
    );
}

#[test]
fn selection_is_physical_half_open_and_supports_negative_origin_reverse_drag() {
    let rect = selection((200, 50), (-300, -150), desktop()).unwrap();
    assert_eq!(
        rect,
        Rect {
            x: -300,
            y: -150,
            width: 500,
            height: 200
        }
    );
    assert_eq!(selection((0, 0), (1, 1), desktop()).unwrap().width, 1);
    assert!(selection((10000, 4000), (9999, 3999), desktop()).is_ok());
}

#[test]
fn limits_apply_to_actual_selection_without_downscaling() {
    assert_eq!(rgba_bytes(8192, 1024).unwrap(), MAX_RGBA);
    assert_eq!(rgba_bytes(4096, 2048).unwrap(), MAX_RGBA);
    for (w, h) in [
        (8193, 1),
        (1, 8193),
        (8192, 1025),
        (0, 1),
        (-1, 5),
        (i32::MAX, i32::MAX),
    ] {
        assert!(rgba_bytes(w, h).is_err(), "{w}x{h}");
    }
    assert!(selection((-9000, 0), (0, 1), desktop()).is_err());
    assert!(selection((0, 0), (0, 1), desktop()).is_err());
    assert!(selection((-10001, 0), (0, 1), desktop()).is_err());
    assert!(selection(
        (0, 0),
        (1, 1),
        Rect {
            x: i32::MAX,
            y: 0,
            width: 1,
            height: 1
        }
    )
    .is_err());
}

#[test]
fn deadline_and_connection_loss_latch_cancellation() {
    let mut state = shared();
    state.deadline = Instant::now() - Duration::from_secs(1);
    assert!(state.check().unwrap_err().contains("60"));
    assert!(state.cancel.load(Ordering::Acquire));
    let state = shared();
    state.live.store(false, Ordering::Release);
    assert!(state.check().is_err());
    state.live.store(true, Ordering::Release);
    assert!(state.check().is_err());
}

#[test]
fn fake_cancel_does_not_acknowledge_native_exit() {
    let (mut handle, control) =
        CaptureHandle::fake(Arc::new(AtomicBool::new(true)), egui::Context::default());
    handle.cancel();
    assert!(control.is_cancelled());
    assert!(!handle.is_finished());
    assert!(handle.poll().is_none());
    control.finish(Ok(vec![1, 2, 3]));
    assert!(handle.is_finished());
    assert!(handle.poll().unwrap().is_err());
    assert!(handle.poll().is_none());
}

#[test]
fn live_loss_discards_completed_png_and_drop_only_cancels() {
    let live = Arc::new(AtomicBool::new(true));
    let (mut handle, control) = CaptureHandle::fake(live.clone(), egui::Context::default());
    control.finish(Ok(vec![42]));
    live.store(false, Ordering::Release);
    assert!(handle.poll().unwrap().is_err());
    let (handle, control) =
        CaptureHandle::fake(Arc::new(AtomicBool::new(true)), egui::Context::default());
    drop(handle);
    assert!(control.is_cancelled());
    assert!(!control.shared.finished());
    control.finish(Err("cleanup complete".into()));
}

#[test]
fn fake_success_is_once_only_and_cancel_overrides_queued_success() {
    let (mut handle, control) =
        CaptureHandle::fake(Arc::new(AtomicBool::new(true)), egui::Context::default());
    assert!(handle.poll().is_none());
    control.finish(Ok(vec![42]));
    assert_eq!(handle.poll().unwrap().unwrap(), vec![42]);
    assert!(handle.poll().is_none());
    let (mut handle, control) =
        CaptureHandle::fake(Arc::new(AtomicBool::new(true)), egui::Context::default());
    control.finish(Ok(vec![42]));
    handle.cancel();
    assert!(handle.poll().unwrap().is_err());
}

#[test]
fn result_and_single_flight_wait_for_actual_thread_exit_even_after_drop() {
    let flight = Mutex::new(None);
    let (published, ready) = mpsc::sync_channel(1);
    let (release, blocked) = mpsc::sync_channel(1);
    let mut handle = CaptureHandle::spawn(
        Arc::new(AtomicBool::new(true)),
        egui::Context::default(),
        &flight,
        move |state| {
            state.publish(Ok(vec![7]));
            published.send(()).unwrap();
            blocked.recv_timeout(Duration::from_secs(5)).unwrap();
            Ok(vec![8])
        },
    )
    .unwrap();
    ready.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(handle.poll().is_none());
    assert!(!handle.is_finished());
    handle.cancel();
    assert!(handle.poll().is_none());
    let observer = handle.shared.clone();
    drop(handle);
    assert!(CaptureHandle::spawn(
        Arc::new(AtomicBool::new(true)),
        egui::Context::default(),
        &flight,
        |_| Ok(vec![])
    )
    .is_err());
    release.send(()).unwrap();
    let mut old = CaptureHandle { shared: observer };
    wait_finished(&old);
    assert!(old.poll().unwrap().is_err());
    let next = CaptureHandle::spawn(
        Arc::new(AtomicBool::new(true)),
        egui::Context::default(),
        &flight,
        |_| Ok(vec![]),
    )
    .unwrap();
    wait_finished(&next);
}

#[test]
fn worker_panic_is_reported_only_after_real_exit_and_releases_flight() {
    let flight = Mutex::new(None);
    let mut handle = CaptureHandle::spawn(
        Arc::new(AtomicBool::new(true)),
        egui::Context::default(),
        &flight,
        |_| panic!("fake worker panic"),
    )
    .unwrap();
    wait_finished(&handle);
    assert!(handle.poll().unwrap().unwrap_err().contains("异常退出"));
    let next = CaptureHandle::spawn(
        Arc::new(AtomicBool::new(true)),
        egui::Context::default(),
        &flight,
        |_| Ok(vec![]),
    )
    .unwrap();
    wait_finished(&next);
}

#[test]
fn dead_connection_never_starts_worker() {
    let flight = Mutex::new(None);
    assert!(CaptureHandle::spawn(
        Arc::new(AtomicBool::new(false)),
        egui::Context::default(),
        &flight,
        |_| panic!("must not run")
    )
    .is_err());
    assert!(flight.lock().unwrap().is_none());
}

#[test]
fn png_sink_is_bounded_and_cancellation_aware() {
    let state = shared();
    let mut sink = PngSink {
        bytes: Vec::new(),
        shared: &state,
    };
    sink.write_all(&vec![0; MAX_PNG]).unwrap();
    assert!(sink.write_all(&[1]).is_err());
    assert_eq!(sink.bytes.len(), MAX_PNG);
    state.cancel.store(true, Ordering::Release);
    assert!(sink.flush().is_err());
    assert!(sink.write(&[]).is_err());
}

#[test]
fn png_encoder_validates_shot_and_encodes_only_synthetic_pixels() {
    let state = shared();
    let rect = Rect {
        x: -2,
        y: -2,
        width: 2,
        height: 2,
    };
    let png = encode(
        Shot {
            width: 2,
            height: 2,
            rgba: vec![255; 16],
        },
        rect,
        &state,
    )
    .unwrap();
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    let decoded = image::load_from_memory(&png).unwrap().to_rgba8();
    assert_eq!(decoded.dimensions(), (2, 2));
    assert_eq!(decoded.as_raw(), &vec![255; 16]);
    assert!(encode(
        Shot {
            width: 2,
            height: 2,
            rgba: vec![0; 15]
        },
        rect,
        &state
    )
    .is_err());
    assert!(encode(
        Shot {
            width: 1,
            height: 4,
            rgba: vec![0; 16]
        },
        rect,
        &state
    )
    .is_err());
    state.live.store(false, Ordering::Release);
    assert!(encode(
        Shot {
            width: 2,
            height: 2,
            rgba: vec![0; 16]
        },
        rect,
        &state
    )
    .is_err());
}
