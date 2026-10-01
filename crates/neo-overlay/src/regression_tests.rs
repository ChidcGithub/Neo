use super::*;

#[test]
fn presentation_requires_current_frame_and_reconfigure_invalidates() {
    let mut state = Presentation::default();
    // Timeout / Occluded / Lost / Outdated / empty draw all skip presentation.
    for _ in 0..5 { assert!(!state.can_show(RenderOutcome::Skipped)); }
    assert!(!state.can_show(RenderOutcome::Presented));
    state.ready = true; // only queue.present grants readiness
    assert!(state.can_show(RenderOutcome::Presented));
    assert!(!state.can_show(RenderOutcome::Skipped));
    state.shown = true;
    assert!(!state.can_show(RenderOutcome::Presented));
    state.invalidate(); // hide, resize, loss, DPI/origin change
    assert!(!state.ready && !state.shown);
    assert!(!state.can_show(RenderOutcome::Skipped));
    assert!(!state.can_show(RenderOutcome::Presented));
    state.ready = true;
    assert!(state.can_show(RenderOutcome::Presented));
}

#[test]
fn drawing_style_and_hit_are_always_passive() {
    assert_ne!(DRAW_EX_STYLE & WS_EX_LAYERED, 0);
    assert_ne!(DRAW_EX_STYLE & WS_EX_TRANSPARENT, 0);
    assert_ne!(DRAW_EX_STYLE & WS_EX_NOACTIVATE, 0);
    assert_ne!(DRAW_EX_STYLE & WS_EX_TOOLWINDOW, 0);
    // 入口在任何 HWND/userdata 访问之前返回，无 GUI 或真实输入。
    for point in [0, -1, (200 << 16) | 450] {
        assert_eq!(unsafe { overlay_wnd_proc(std::ptr::null_mut(), WM_NCHITTEST, 0, point) }, HTTRANSPARENT as isize);
    }
}

#[test]
fn passive_mouse_activation_never_depends_on_taskbar_focus() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MA_NOACTIVATE, WM_MOUSEACTIVATE};
    // Pure wndproc branch: no HWND access, activation API or input dispatch.
    // Foreground/top-level identity and hit-test/mouse-message payload cannot
    // make the drawing layer request activation. This is not a Shell test.
    for parent in [0, 1, usize::MAX] {
        for hit_and_message in [0, -1, (0x0201 << 16) | 1] {
            assert_eq!(unsafe {
                overlay_wnd_proc(std::ptr::null_mut(), WM_MOUSEACTIVATE, parent, hit_and_message)
            }, MA_NOACTIVATE as LRESULT);
        }
    }
}

#[test]
fn dimensions_request_adapter_capacity_and_reject_insufficient_limits() {
    let supported = wgpu::Limits { max_texture_dimension_2d: 16384, ..wgpu::Limits::default() };
    let requested = overlay_limits(&supported, (11520, 2160)).unwrap();
    assert_eq!(requested.max_texture_dimension_2d, 16384);
    assert!(requested.check_limits(&supported));
    let limited = wgpu::Limits { max_texture_dimension_2d: 4096, ..supported.clone() };
    assert!(overlay_limits(&limited, (3840, 2160)).is_ok());
    assert!(overlay_limits(&limited, (11520, 2160)).is_err());
    let insufficient = wgpu::Limits { max_bind_groups: 0, ..supported };
    assert!(overlay_limits(&insufficient, (1920, 1080)).is_err());
}

#[test]
fn dimensions_hotplug_checks_device_limit_before_reconfiguration() {
    let supported = wgpu::Limits { max_texture_dimension_2d: 16384, ..wgpu::Limits::default() };
    let device = overlay_limits(&supported, (1920, 1080)).unwrap();
    for size in [(11520, 2160), (16384, 2160), (2160, 16384)] {
        assert!(validate_dimensions(size, device.max_texture_dimension_2d).is_ok());
    }
    for size in [(16385, 2160), (2160, 16385), (0, 2160), (1920, 0)] {
        assert!(validate_dimensions(size, device.max_texture_dimension_2d).is_err());
    }
    for (width, height) in [(0, 1080), (1920, 0), (-1, 1080), (1920, -1)] {
        assert!(DesktopBounds::from_rect(screen::Rect { x: -1920, y: 0, width, height }).is_err());
    }
}

#[test]
fn dimensions_upload_rejects_zero_oversize_and_malformed_frames() {
    assert!(valid_upload((11520, 2160), 11520 * 2160 * 4, 16384));
    assert!(!valid_upload((11520, 2160), 11520 * 2160 * 4, 8192));
    assert!(!valid_upload((0, 0), 0, 16384));
    assert!(!valid_upload((2, 2), 15, 16384));
    assert!(!valid_upload((u32::MAX, u32::MAX), 0, u32::MAX));
}

#[test]
fn capture_exclusion_requires_19041_and_successful_affinity() {
    for (version, supported) in [
        (None, false), (Some((6, 3, 9600)), false),
        (Some((10, 0, 19040)), false), (Some((10, 0, 19041)), true),
        (Some((10, 0, 19045)), true), (Some((10, 0, 22000)), true),
    ] {
        assert_eq!(supports_capture_exclusion(version), supported);
        for affinity_ok in [false, true] {
            let mut state = CaptureState::default();
            state.set_enabled(true);
            state.set_exclude_ok(supports_capture_exclusion(version) && affinity_ok);
            assert_eq!(state.begin().is_some(), supported && affinity_ok);
        }
    }
}

#[test]
fn transparency_never_falls_back_to_opaque_or_wrong_alpha_convention() {
    use wgpu::CompositeAlphaMode::*;
    for modes in [vec![], vec![Opaque], vec![Auto, Inherit], vec![PostMultiplied]] {
        assert!(transparent_alpha(&modes).is_err());
    }
    assert_eq!(transparent_alpha(&[Opaque, PreMultiplied]).unwrap(), PreMultiplied);
}

fn safe_capture() -> CaptureState {
    let mut state = CaptureState::default();
    state.set_exclude_ok(true);
    state
}

#[test]
fn capture_safety_blocks_work_even_after_show_and_topology_changes() {
    let mut state = CaptureState::default();
    for _ in 0..100 {
        state.set_enabled(true);
        state.invalidate();
        assert!(state.begin().is_none());
    }
    state.set_exclude_ok(true);
    let old = state.begin().unwrap();
    state.publish(old, bounds(), shot(1));
    state.set_exclude_ok(false);
    state.publish(old, bounds(), shot(2));
    assert!(state.take(bounds()).is_none());
    state.set_enabled(false);
    state.set_exclude_ok(true);
    assert!(state.begin().is_none(), "安全许可不能覆盖 Hide");
    state.set_enabled(true);
    state.publish(old, bounds(), shot(3));
    assert!(state.take(bounds()).is_none());
}

#[test]
fn capture_cadence_is_at_most_twenty_hz_without_catch_up() {
    for millis in [0, 5, 17, 33, 50, 100, 1000] {
        let elapsed = Duration::from_millis(millis);
        let cycle = elapsed + capture_rest(elapsed);
        assert!(cycle >= CAPTURE_INTERVAL);
        assert!(cycle >= elapsed + Duration::from_millis(33));
    }
    let cycles = |rest: Duration| (0..1000).step_by(rest.as_millis() as usize).count();
    assert_eq!(cycles(Duration::from_millis(33)), 31);
    assert_eq!(cycles(capture_rest(Duration::ZERO)), 20);
    eprintln!("合成 1s 零耗时采集：旧 31 次，新 20 次；慢采集不加速");
}

fn fake_handle() -> (OverlayHandle, Receiver<Cmd>, Sender<()>, Receiver<()>) {
    let (tx, rx) = channel();
    let (release, wait) = channel();
    let (done, finished) = channel();
    let thread = std::thread::spawn(move || {
        let _ = wait.recv_timeout(Duration::from_secs(2));
        let _ = done.send(());
    });
    (OverlayHandle {
        tx, tid: 0,
        level: Arc::new(AtomicU32::new(0)),
        visible: Arc::new(AtomicBool::new(true)),
        stop: Arc::new(AtomicBool::new(false)),
        capture: Arc::new(Mutex::new(safe_capture())),
        cards: Arc::new(Mutex::new(BTreeMap::new())),
        suspended: Arc::new(AtomicU32::new(0)),
        thread: Some(thread),
        wake_count: Some(AtomicU32::new(0)),
    }, rx, release, finished)
}

fn wake_count(handle: &OverlayHandle) -> u32 {
    handle.wake_count.as_ref().unwrap().load(Ordering::Relaxed)
}

#[test]
fn desktop_barrier_ack_then_thread_exit_and_cancel_fail_closed() {
    let (handle, commands, release, finished) = fake_handle();
    let gate = handle.desktop_gate();
    let cancel = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let waiting = scope.spawn(|| gate.acquire(&cancel, Duration::from_secs(1)));
        let Cmd::Suspend(ack, deadline, live) = commands.recv_timeout(Duration::from_secs(1)).unwrap() else { panic!("必须先申请隐藏") };
        assert!(desktop_request_live(deadline, &live));
        assert!(!desktop_can_show(&gate.count));
        ack.send(Ok(())).unwrap();
        let guard = waiting.join().unwrap().unwrap();
        assert!(!desktop_can_show(&gate.count));
        drop(guard);
        assert!(desktop_can_show(&gate.count));
    });
    while commands.try_recv().is_ok() {}
    std::thread::scope(|scope| {
        let waiting = scope.spawn(|| gate.acquire(&cancel, Duration::from_secs(1)));
        let Cmd::Suspend(ack, _, _) = commands.recv_timeout(Duration::from_secs(1)).unwrap() else { panic!("必须先申请隐藏") };
        gate.stop.store(true, Ordering::Release);
        let _ = ack.send(Ok(()));
        assert!(waiting.join().unwrap().is_err());
    });
    assert!(desktop_can_show(&gate.count));
    release.send(()).unwrap();
    finished.recv_timeout(Duration::from_secs(1)).unwrap();
}

#[test]
fn desktop_barrier_timeout_and_last_guard_restore() {
    let (handle, commands, release, finished) = fake_handle();
    let gate = handle.desktop_gate();
    let cancel = AtomicBool::new(false);
    let started = Instant::now();
    assert!(gate.acquire(&cancel, Duration::from_millis(15)).is_err());
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(gate.count.load(Ordering::Acquire), 0);
    let Cmd::Suspend(_, deadline, live) = commands.recv().unwrap() else { panic!("缺少过期请求") };
    assert!(!desktop_request_live(deadline, &live), "迟到请求不能隐藏窗口");
    assert!(!desktop_request_live(Instant::now(), &AtomicBool::new(true)));
    while commands.try_recv().is_ok() {}
    let first = gate.reserve();
    let second = gate.reserve();
    handle.show();
    handle.set_card(card_id::MINI, Some(Card::passive([0.0; 4], |_| {})));
    assert!(!desktop_can_show(&gate.count));
    drop(first);
    assert!(!desktop_can_show(&gate.count));
    drop(second);
    assert!(desktop_can_show(&gate.count));
    cancel.store(true, Ordering::Release);
    assert!(gate.acquire(&cancel, Duration::from_secs(1)).is_err());
    assert_eq!(gate.count.load(Ordering::Acquire), 0);
    release.send(()).unwrap();
    finished.recv_timeout(Duration::from_secs(1)).unwrap();
}

#[test]
fn toast_passive_card_never_opens_capture_or_posts_show_hide() {
    let (handle, commands, release, finished) = fake_handle();
    handle.set_card(card_id::TOAST, Some(Card::passive([10.0, 20.0, 300.0, 100.0], |_| {})));
    assert!(handle.capture.lock().unwrap().begin().is_none());
    assert!(commands.try_recv().is_err());
    assert_eq!(wake_count(&handle), 1);
    handle.set_card(card_id::TOAST, None);
    handle.set_card(card_id::TOAST, None);
    assert!(handle.cards.lock().unwrap().is_empty());
    assert_eq!(wake_count(&handle), 2);
    assert!(commands.try_recv().is_err());
    release.send(()).unwrap();
    finished.recv_timeout(Duration::from_secs(1)).unwrap();
}

#[test]
fn card_repeated_empty_removals_do_not_notify() {
    let (handle, _commands, release, finished) = fake_handle();
    for _ in 0..5 {
        for id in [card_id::CONFIRM, card_id::MINI, card_id::CLASS, card_id::FLASH, card_id::DOT, card_id::TOAST] {
            handle.set_card(id, None);
        }
    }
    assert!(handle.cards.lock().unwrap().is_empty());
    assert_eq!(wake_count(&handle), 0, "5Hz × 6 张空卡不应产生 30 次通知");
    release.send(()).unwrap();
    finished.recv_timeout(Duration::from_secs(1)).unwrap();
}

#[test]
fn card_updates_and_last_removal_notify_without_changing_ripple() {
    for showing in [false, true] {
        let (handle, _commands, release, finished) = fake_handle();
        let mut ripple = RippleState::default();
        if showing {
            ripple.show();
        }
        handle.set_card(card_id::MINI, Some(Card::passive([0.0; 4], |_| {})));
        assert_eq!(wake_count(&handle), 1);
        let replacement = Card::passive([0.0; 4], |_| {});
        let rect = replacement.rect.clone();
        handle.set_card(card_id::MINI, Some(replacement));
        assert_eq!(wake_count(&handle), 2);
        {
            let cards = handle.cards.lock().unwrap();
            assert_eq!(cards.len(), 1);
            assert!(Arc::ptr_eq(&cards[&card_id::MINI].rect, &rect));
        }
        handle.set_card(card_id::DOT, Some(Card::passive([0.0; 4], |_| {})));
        assert_eq!(wake_count(&handle), 3);
        handle.set_card(card_id::FLASH, None);
        assert_eq!(wake_count(&handle), 3);
        handle.set_card(card_id::MINI, None);
        assert_eq!(wake_count(&handle), 4);
        assert!(ripple.active(!handle.cards.lock().unwrap().is_empty()));
        handle.set_card(card_id::DOT, None);
        assert_eq!(wake_count(&handle), 5);
        assert!(handle.cards.lock().unwrap().is_empty());
        assert_eq!(ripple.active(false), showing);
        handle.set_card(card_id::DOT, None);
        assert_eq!(wake_count(&handle), 5);
        release.send(()).unwrap();
        finished.recv_timeout(Duration::from_secs(1)).unwrap();
    }
}

#[test]
fn card_poisoned_lock_does_not_notify_or_mutate() {
    let (handle, _commands, release, finished) = fake_handle();
    handle.set_card(card_id::MINI, Some(Card::passive([0.0; 4], |_| {})));
    let cards = handle.cards.clone();
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = cards.lock().unwrap();
        panic!("fake poisoned card slot");
    })).is_err());
    handle.set_card(card_id::MINI, None);
    handle.set_card(card_id::DOT, Some(Card::passive([0.0; 4], |_| {})));
    assert_eq!(wake_count(&handle), 1);
    let cards = handle.cards.lock().unwrap_or_else(|p| p.into_inner());
    assert_eq!(cards.len(), 1);
    assert!(cards.contains_key(&card_id::MINI));
    drop(cards);
    release.send(()).unwrap();
    finished.recv_timeout(Duration::from_secs(1)).unwrap();
}

#[test]
fn card_shutdown_handle_does_not_notify_or_mutate() {
    let (mut handle, _commands, release, finished) = fake_handle();
    handle.set_card(card_id::MINI, Some(Card::passive([0.0; 4], |_| {})));
    handle.shutdown();
    let before = wake_count(&handle);
    handle.set_card(card_id::MINI, None);
    handle.set_card(card_id::DOT, Some(Card::passive([0.0; 4], |_| {})));
    assert_eq!(wake_count(&handle), before);
    let cards = handle.cards.lock().unwrap();
    assert_eq!(cards.len(), 1);
    assert!(cards.contains_key(&card_id::MINI));
    drop(cards);
    release.send(()).unwrap();
    finished.recv_timeout(Duration::from_secs(1)).unwrap();
}

#[test]
fn lifecycle_shutdown_and_drop_do_not_wait_for_stalled_thread() {
    let (mut handle, commands, release, finished) = fake_handle();
    assert!(handle.is_alive());
    handle.show();
    assert!(matches!(commands.try_recv(), Ok(Cmd::Show)));
    let capture = handle.capture.clone();
    let generation = capture.lock().unwrap().begin().unwrap();
    let start = Instant::now();
    handle.shutdown();
    assert!(start.elapsed() < Duration::from_millis(500));
    assert!(!handle.is_alive());
    assert!(!handle.is_visible());
    assert!(matches!(commands.try_recv(), Ok(Cmd::Shutdown)));
    handle.show();
    let mut state = capture.lock().unwrap();
    state.publish(generation, bounds(), shot(1));
    assert!(state.begin().is_none());
    assert!(state.take(bounds()).is_none());
    drop(state);
    drop(handle);
    assert!(finished.try_recv().is_err());
    release.send(()).unwrap();
    finished.recv_timeout(Duration::from_secs(1)).unwrap();
}

#[test]
fn lifecycle_drop_closes_gate_without_waiting_for_worker() {
    let (handle, _commands, release, finished) = fake_handle();
    handle.show();
    let capture = handle.capture.clone();
    let stop = handle.stop.clone();
    let start = Instant::now();
    drop(handle);
    assert!(start.elapsed() < Duration::from_millis(500));
    assert!(stop.load(Ordering::Acquire));
    assert!(capture.lock().unwrap().begin().is_none());
    assert!(finished.try_recv().is_err());
    release.send(()).unwrap();
    finished.recv_timeout(Duration::from_secs(1)).unwrap();
}

#[test]
fn lifecycle_ready_timeout_error_disconnect_and_success() {
    for outcome in 0..4 {
        let (handle, _commands, release, finished) = fake_handle();
        let stop = handle.stop.clone();
        let capture = handle.capture.clone();
        capture.lock().unwrap().set_enabled(true);
        let (ready, rx) = channel();
        match outcome {
            0 => {},
            1 => ready.send(Err("fake initialization failure".into())).unwrap(),
            2 => {},
            _ => ready.send(Ok(0)).unwrap(),
        }
        let ready = if outcome == 2 { drop(ready); None } else { Some(ready) };
        let start = Instant::now();
        let result = handle.await_ready(rx, Duration::from_millis(10));
        assert!(start.elapsed() < Duration::from_millis(500));
        assert_eq!(result.is_ok(), outcome == 3);
        if outcome != 3 {
            assert!(stop.load(Ordering::Acquire));
            assert!(capture.lock().unwrap().begin().is_none());
            if let Some(ready) = ready {
                assert!(ready.send(Ok(0)).is_err());
            }
        }
        drop(result);
        release.send(()).unwrap();
        finished.recv_timeout(Duration::from_secs(1)).unwrap();
    }
}

#[test]
fn lifecycle_stalled_initialization_keeps_single_engine_lease() {
    static RUNNING: AtomicBool = AtomicBool::new(false);
    let lease = EngineLease::acquire(&RUNNING).unwrap();
    let (release, wait) = channel();
    let worker = std::thread::spawn(move || {
        let _lease = lease;
        let _ = wait.recv_timeout(Duration::from_secs(2));
    });
    for _ in 0..100 {
        assert!(EngineLease::acquire(&RUNNING).is_err());
    }
    release.send(()).unwrap();
    worker.join().unwrap();
    assert!(EngineLease::acquire(&RUNNING).is_ok());
}

#[test]
fn lifecycle_finished_thread_is_not_alive_even_without_stop_signal() {
    let (mut handle, _commands, release, finished) = fake_handle();
    handle.set_card(card_id::MINI, Some(Card::passive([0.0; 4], |_| {})));
    release.send(()).unwrap();
    finished.recv_timeout(Duration::from_secs(1)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    while !handle.thread.as_ref().unwrap().is_finished() && Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert!(!handle.stop.load(Ordering::Acquire));
    assert!(!handle.is_alive());
    assert!(!handle.is_visible());
    let before = wake_count(&handle);
    handle.set_card(card_id::MINI, None);
    handle.set_card(card_id::DOT, Some(Card::passive([0.0; 4], |_| {})));
    assert_eq!(wake_count(&handle), before);
    let cards = handle.cards.lock().unwrap();
    assert_eq!(cards.len(), 1);
    assert!(cards.contains_key(&card_id::MINI));
    drop(cards);
    handle.show();
    assert!(handle.capture.lock().unwrap().begin().is_none());
    handle.shutdown();
}

fn bounds() -> DesktopBounds {
    DesktopBounds { origin: (0, 0), size: (2, 2) }
}

fn shot(value: u8) -> Shot {
    Shot { width: 2, height: 2, rgba: vec![value; 16] }
}

#[test]
fn hide_stops_capture_before_fade_with_or_without_cards() {
    for have_cards in [false, true] {
        let mut capture = safe_capture();
        let mut ripple = RippleState::default();
        assert!(capture.begin().is_none());
        assert_eq!(ripple.active(have_cards), have_cards);
        capture.set_enabled(true);
        ripple.show();
        let in_flight = capture.begin().unwrap();
        capture.publish(in_flight, bounds(), shot(1));
        let frozen = capture.take(bounds()).unwrap();
        capture.publish(in_flight, bounds(), shot(2));

        capture.set_enabled(false);
        ripple.hide();
        assert!(ripple.active(have_cards));
        assert!(!ripple.finish_fade(0.5));
        assert!(capture.begin().is_none());
        assert!(capture.take(bounds()).is_none());
        // 在途系统调用不能撤销，但返回后不能再更新淡出画面。
        capture.publish(in_flight, bounds(), shot(3));
        assert!(capture.take(bounds()).is_none());
        assert_eq!(frozen.rgba, vec![1; 16]);
        for _ in 0..40 {
            assert!(capture.begin().is_none());
        }
        assert!(ripple.finish_fade(0.01));
        assert_eq!(ripple.active(have_cards), have_cards);
        assert!(!ripple.finish_fade(0.0));
        assert!(capture.begin().is_none());
    }
}

#[test]
fn reshow_rejects_old_in_flight_and_queued_frames() {
    let mut capture = safe_capture();
    let mut ripple = RippleState::default();
    capture.set_enabled(true);
    ripple.show();
    let old = capture.begin().unwrap();
    capture.publish(old, bounds(), shot(1));
    capture.set_enabled(false);
    ripple.hide();
    capture.set_enabled(true);
    ripple.show();
    let new = capture.begin().unwrap();
    assert_ne!(old, new);
    assert!(capture.take(bounds()).is_none());
    capture.publish(old, bounds(), shot(2));
    assert!(capture.take(bounds()).is_none());
    capture.publish(new, bounds(), shot(3));
    // 迟到旧帧也不能覆盖本轮已发布的新帧。
    capture.publish(old, bounds(), shot(4));
    assert_eq!(capture.take(bounds()).unwrap().rgba, vec![3; 16]);
    assert!(!ripple.finish_fade(0.0));
    assert!(ripple.active(false));
}

#[test]
fn queued_show_cannot_reopen_gate_after_hide() {
    let mut capture = safe_capture();
    let mut ripple = RippleState::default();
    // 模拟窗口线程尚未排空命令时调用线程已连续 Show/Hide。
    capture.set_enabled(true);
    capture.set_enabled(false);
    ripple.show();
    assert!(capture.begin().is_none());
    ripple.hide();
    assert!(capture.begin().is_none());
    // 淡出收尾也不得关掉已经投递的下一轮 Show 许可。
    capture.set_enabled(true);
    let new = capture.begin().unwrap();
    assert!(ripple.finish_fade(0.0));
    assert_eq!(capture.begin(), Some(new));
    ripple.show();
    assert!(ripple.active(false));
}

#[test]
fn repeated_hide_and_show_invalidate_previous_work() {
    let mut capture = safe_capture();
    let mut ripple = RippleState::default();
    for _ in 0..2 {
        capture.set_enabled(false);
        ripple.hide();
        assert!(!ripple.active(false));
        assert!(!ripple.finish_fade(0.0));
        assert!(capture.begin().is_none());
    }
    capture.set_enabled(true);
    let old = capture.begin().unwrap();
    capture.set_enabled(true);
    capture.publish(old, bounds(), shot(1));
    assert!(capture.take(bounds()).is_none());
    let current = capture.begin().unwrap();
    capture.publish(current, bounds(), shot(2));
    assert!(capture.take(bounds()).is_some());
}

#[test]
fn topology_invalidation_rejects_in_flight_even_if_bounds_return() {
    let mut capture = safe_capture();
    capture.set_enabled(true);
    let old = capture.begin().unwrap();
    capture.invalidate();
    capture.publish(old, bounds(), shot(1));
    assert!(capture.take(bounds()).is_none());
    let current = capture.begin().unwrap();
    let shifted = DesktopBounds { origin: (-2, 0), ..bounds() };
    capture.publish(current, shifted, shot(2));
    assert!(capture.take(bounds()).is_none());
    let mut wrong_size = shot(3);
    wrong_size.width = 1;
    capture.publish(current, bounds(), wrong_size);
    assert!(capture.take(bounds()).is_none());
    capture.publish(current, bounds(), shot(4));
    assert!(capture.take(bounds()).is_some());
    capture.set_enabled(false);
    capture.invalidate();
    assert!(capture.begin().is_none());
    capture.publish(current, bounds(), shot(5));
    assert!(capture.take(bounds()).is_none());
}

#[test]
fn topology_change_updates_negative_origin_and_size() {
    let old = DesktopBounds::from_rect(screen::Rect { x: 0, y: 0, width: 1920, height: 1080 }).unwrap();
    let new = DesktopBounds::from_rect(screen::Rect { x: -1280, y: -200, width: 3200, height: 1440 }).unwrap();
    assert_ne!(old, new);
    assert_eq!(new.size, (3200, 1440));
    assert_eq!(new.client_point((50, 60)), (1330, 260));
    let shifted = DesktopBounds { origin: (0, 0), ..new };
    assert_ne!(shifted, new);
    assert_eq!(shifted.size, new.size);
    assert!(DesktopBounds::from_rect(screen::Rect { x: 0, y: 0, width: 0, height: 0 }).is_err());
}
