use super::*;

#[test]
fn offscreen_budget_preserves_small_sizes_and_caps_large_desktops() {
    for size in [(1, 1), (640, 480), (1280, 720), (1366, 768), (1920, 1080)] {
        assert_eq!(
            offscreen_size(size),
            (
                (size.0 as f32 * RENDER_SCALE).max(1.0) as u32,
                (size.1 as f32 * RENDER_SCALE).max(1.0) as u32,
            )
        );
    }
    assert_eq!(offscreen_size((3840, 2160)), (1152, 648));
    assert_eq!(offscreen_size((7680, 4320)), (1152, 648));
    assert_eq!(offscreen_size((1080, 1920)), (364, 648));
    let wide = offscreen_size((11520, 2160));
    assert!(wide.0 > 1152 && wide.1 < 648, "ultrawide uses the area cap");
}

#[test]
fn offscreen_budget_extremes_and_uniform_scale() {
    let dimensions = [
        0,
        1,
        2,
        3,
        17,
        431,
        1080,
        1920,
        2160,
        11520,
        65535,
        u32::MAX,
    ];
    for width in dimensions {
        for height in dimensions {
            let (w, h) = offscreen_size((width, height));
            assert!(w > 0 && h > 0 && h <= OFFSCREEN_MAX_HEIGHT);
            assert!(u64::from(w) * u64::from(h) <= u64::from(OFFSCREEN_PIXEL_BUDGET));
            assert!(w <= width.max(1) && h <= height.max(1));
            let x = f64::from(width.max(1));
            let y = f64::from(height.max(1));
            // Both floored dimensions must admit the same scale (except the 1px minimum).
            let low = (if w == 1 { 0.0 } else { f64::from(w) / x }).max(if h == 1 {
                0.0
            } else {
                f64::from(h) / y
            });
            let high = ((f64::from(w) + 1.0) / x)
                .min((f64::from(h) + 1.0) / y)
                .min(f64::from(RENDER_SCALE));
            assert!(low <= high, "nonuniform scale: {width}x{height} -> {w}x{h}");
        }
    }
}

#[test]
fn offscreen_scissors_are_bounded_disjoint_and_cover_the_edge_ring() {
    for size in [
        (1, 1),
        (2, 9),
        (64, 648),
        (1152, 648),
        (1597, 467),
        (648, 1152),
        (u32::MAX, 1),
        (1, u32::MAX),
        (u32::MAX, u32::MAX),
    ] {
        let (w, h) = size;
        let rects: Vec<_> = edge_scissors(size).collect();
        assert!(rects.len() == 1 || rects.len() == 4);
        let mut area = 0u64;
        for (i, &(x, y, rw, rh)) in rects.iter().enumerate() {
            assert!(rw > 0 && rh > 0);
            assert!(u64::from(x) + u64::from(rw) <= u64::from(w));
            assert!(u64::from(y) + u64::from(rh) <= u64::from(h));
            area += u64::from(rw) * u64::from(rh);
            for &(xx, yy, ww, hh) in &rects[..i] {
                assert!(x + rw <= xx || xx + ww <= x || y + rh <= yy || yy + hh <= y);
            }
        }
        if rects.len() == 1 {
            assert_eq!(rects, [(0, 0, w, h)]);
        } else {
            let edge = rects[0].3;
            assert_eq!(
                area,
                u64::from(w) * u64::from(h) - u64::from(w - 2 * edge) * u64::from(h - 2 * edge)
            );
            assert_eq!(
                rects,
                [
                    (0, 0, w, edge),
                    (0, h - edge, w, edge),
                    (0, edge, edge, h - 2 * edge),
                    (w - edge, edge, edge, h - 2 * edge)
                ]
            );
        }
    }
    assert_eq!(edge_scissors((0, 100)).count(), 0);
    assert_eq!(edge_scissors((100, 0)).count(), 0);
    assert_eq!(edge_scissors((0, 0)).count(), 0);
}

#[test]
fn offscreen_scissors_skip_only_shader_early_return_pixels() {
    for (w, h) in [(17, 11), (257, 143), (1152, 648), (1597, 467), (364, 648)] {
        let rects: Vec<_> = edge_scissors((w, h)).collect();
        let px = h as f32 / 432.0;
        let radius = 14.0 * px;
        for y in 0..h {
            for x in 0..w {
                let hits = rects
                    .iter()
                    .filter(|&&(xx, yy, ww, hh)| x >= xx && x < xx + ww && y >= yy && y < yy + hh)
                    .count();
                assert!(hits <= 1);
                if hits == 0 {
                    // Same rounded-box SDF as shader, evaluated at fragment centers.
                    let qx = (x as f32 + 0.5 - w as f32 * 0.5).abs() - (w as f32 * 0.5 - 12.0 * px)
                        + radius;
                    let qy = (y as f32 + 0.5 - h as f32 * 0.5).abs() - (h as f32 * 0.5 - 12.0 * px)
                        + radius;
                    let d = qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - radius;
                    assert!(
                        d < -120.0 * px,
                        "clipped visible pixel ({x}, {y}) in {w}x{h}"
                    );
                }
            }
        }
    }
}

#[test]
fn presentation_requires_current_frame_and_reconfigure_invalidates() {
    let mut state = Presentation::default();
    // Timeout / Occluded / Lost / Outdated / empty draw all skip presentation.
    for _ in 0..5 {
        assert!(!state.can_show(RenderOutcome::Skipped));
    }
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
        assert_eq!(
            unsafe { overlay_wnd_proc(std::ptr::null_mut(), WM_NCHITTEST, 0, point) },
            HTTRANSPARENT as isize
        );
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
            assert_eq!(
                unsafe {
                    overlay_wnd_proc(
                        std::ptr::null_mut(),
                        WM_MOUSEACTIVATE,
                        parent,
                        hit_and_message,
                    )
                },
                MA_NOACTIVATE as LRESULT
            );
        }
    }
}

#[test]
fn dimensions_request_adapter_capacity_and_reject_insufficient_limits() {
    let supported = wgpu::Limits {
        max_texture_dimension_2d: 16384,
        ..wgpu::Limits::default()
    };
    let requested = overlay_limits(&supported, (11520, 2160)).unwrap();
    assert_eq!(requested.max_texture_dimension_2d, 16384);
    assert!(requested.check_limits(&supported));
    let limited = wgpu::Limits {
        max_texture_dimension_2d: 4096,
        ..supported.clone()
    };
    assert!(overlay_limits(&limited, (3840, 2160)).is_ok());
    assert!(overlay_limits(&limited, (11520, 2160)).is_err());
    let insufficient = wgpu::Limits {
        max_bind_groups: 0,
        ..supported
    };
    assert!(overlay_limits(&insufficient, (1920, 1080)).is_err());
}

#[test]
fn dimensions_hotplug_checks_device_limit_before_reconfiguration() {
    let supported = wgpu::Limits {
        max_texture_dimension_2d: 16384,
        ..wgpu::Limits::default()
    };
    let device = overlay_limits(&supported, (1920, 1080)).unwrap();
    for size in [(11520, 2160), (16384, 2160), (2160, 16384)] {
        assert!(validate_dimensions(size, device.max_texture_dimension_2d).is_ok());
    }
    for size in [(16385, 2160), (2160, 16385), (0, 2160), (1920, 0)] {
        assert!(validate_dimensions(size, device.max_texture_dimension_2d).is_err());
    }
    for (width, height) in [(0, 1080), (1920, 0), (-1, 1080), (1920, -1)] {
        assert!(DesktopBounds::from_rect(screen::Rect {
            x: -1920,
            y: 0,
            width,
            height
        })
        .is_err());
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
        (None, false),
        (Some((6, 3, 9600)), false),
        (Some((10, 0, 19040)), false),
        (Some((10, 0, 19041)), true),
        (Some((10, 0, 19045)), true),
        (Some((10, 0, 22000)), true),
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
    for modes in [
        vec![],
        vec![Opaque],
        vec![Auto, Inherit],
        vec![PostMultiplied],
    ] {
        assert!(transparent_alpha(&modes).is_err());
    }
    assert_eq!(
        transparent_alpha(&[Opaque, PreMultiplied]).unwrap(),
        PreMultiplied
    );
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

#[test]
fn capture_idle_wait_requires_both_permissions_and_wakes_on_show() {
    for exclusion_first in [false, true] {
        let slot = Arc::new(Mutex::new(CaptureState::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel();
        let worker = {
            let slot = slot.clone();
            let stop = stop.clone();
            std::thread::spawn(move || tx.send(wait_for_capture(&slot, &stop)).unwrap())
        };
        // No GUI, capture API, or polling worker: only the production wait path.
        assert!(matches!(
            rx.recv_timeout(Duration::from_millis(20)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        {
            let mut state = slot.lock().unwrap();
            if exclusion_first {
                state.set_exclude_ok(true);
            } else {
                state.set_enabled(true);
            }
            state.changed.notify_all(); // Spurious wake cannot grant permission.
        }
        assert!(matches!(
            rx.recv_timeout(Duration::from_millis(20)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        let expected = {
            let mut state = slot.lock().unwrap();
            if exclusion_first {
                state.set_enabled(true);
            } else {
                state.set_exclude_ok(true);
            }
            state.begin()
        };
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), expected);
        worker.join().unwrap();
    }
}

#[test]
fn capture_idle_wait_handles_early_notification_and_shutdown() {
    let slot = Arc::new(Mutex::new(safe_capture()));
    let stop = Arc::new(AtomicBool::new(false));
    // Notification before the worker starts waiting must not be lost.
    slot.lock().unwrap().set_enabled(true);
    let expected = slot.lock().unwrap().begin();
    assert_eq!(wait_for_capture(&slot, &stop), expected);
    slot.lock().unwrap().set_enabled(false);
    let (tx, rx) = channel();
    let worker = {
        let slot = slot.clone();
        let stop = stop.clone();
        std::thread::spawn(move || tx.send(wait_for_capture(&slot, &stop)).unwrap())
    };
    assert!(matches!(
        rx.recv_timeout(Duration::from_millis(20)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    ));
    stop.store(true, Ordering::Release);
    slot.lock().unwrap().set_enabled(false);
    assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), None);
    worker.join().unwrap();
    // A queued Show never overrides shutdown, even if the gate still says enabled.
    slot.lock().unwrap().set_enabled(true);
    assert_eq!(wait_for_capture(&slot, &stop), None);
}

#[test]
fn desktop_upload_dedup_is_exact_and_dimension_sensitive() {
    let previous = shot(7);
    assert!(!same_desktop_frame(None, &previous));
    assert!(same_desktop_frame(Some(&previous), &shot(7)));
    for index in 0..previous.rgba.len() {
        let mut changed = shot(7);
        changed.rgba[index] ^= 1;
        assert!(
            !same_desktop_frame(Some(&previous), &changed),
            "byte {index}"
        );
    }
    let reshaped = Shot {
        width: 1,
        height: 4,
        rgba: vec![7; 16],
    };
    assert!(!same_desktop_frame(Some(&previous), &reshaped));
    let malformed = Shot {
        width: 2,
        height: 2,
        rgba: vec![7; 15],
    };
    assert!(!same_desktop_frame(Some(&previous), &malformed));
}

#[test]
fn desktop_upload_dedup_workload_and_reset_require_fresh_frame() {
    let mut state = DesktopUploadState::default();
    let mut uploads = 0;
    for _ in 0..20 {
        let current = desktop_shot(1920, 1080);
        if !same_desktop_frame(state.last.as_ref(), &current) {
            state.uploaded(current);
            uploads += 1;
        }
    }
    assert_eq!(uploads, 1, "20 identical 1080p frames need just one upload");
    assert!(state.reset());
    assert!(state.last.is_none());
    assert!(!same_desktop_frame(
        state.last.as_ref(),
        &desktop_shot(1920, 1080)
    ));
    for value in 0..20 {
        let current = shot(value);
        assert!(!same_desktop_frame(state.last.as_ref(), &current));
        state.uploaded(current);
    }
    let full_frame_bytes = 1920u64 * 1080 * 4;
    assert_eq!(19 * full_frame_bytes, 157_593_600);
    eprintln!("Synthetic static 20-frame 1080p workload: 20 -> 1 uploads, 157593600 fewer bytes; 4K bypasses dedup (not a timing benchmark)");
}

fn desktop_shot(width: u32, height: u32) -> Shot {
    Shot {
        width,
        height,
        rgba: vec![7; width as usize * height as usize * 4],
    }
}

#[test]
fn desktop_cache_budget_boundary_and_overallocated_buffer() {
    assert_eq!(DESKTOP_CACHE_BUDGET, 8_388_608);
    for (width, height, expected) in [
        (1920, 1080, true),
        (2047, 1024, true),
        (2048, 1024, true),
        (2049, 1024, false),
        (3840, 2160, false),
    ] {
        let current = desktop_shot(width, height);
        assert!(valid_upload((width, height), current.rgba.len(), 16384));
        assert_eq!(cacheable_desktop(&current), expected);
        // Self-comparison would always succeed if the budget guard were absent.
        assert_eq!(same_desktop_frame(Some(&current), &current), expected);
        let mut state = DesktopUploadState::default();
        state.uploaded(current);
        assert_eq!(state.last.is_some(), expected);
        assert!(state.has_uploaded_desktop);
    }
    let mut overallocated = shot(7);
    overallocated.rgba.reserve_exact(DESKTOP_CACHE_BUDGET + 1);
    assert!(!cacheable_desktop(&overallocated));
    assert!(!same_desktop_frame(Some(&overallocated), &shot(7)));
    let mut state = DesktopUploadState::default();
    state.uploaded(overallocated);
    assert!(state.last.is_none());
}

#[test]
fn desktop_cache_over_budget_frames_always_upload_and_reset_placeholder() {
    let mut state = DesktopUploadState::default();
    state.uploaded(shot(7));
    assert!(state.last.is_some());
    let mut uploads = 0;
    for _ in 0..20 {
        let current = desktop_shot(2049, 1024);
        assert!(!same_desktop_frame(state.last.as_ref(), &current));
        state.uploaded(current);
        uploads += 1;
        assert!(
            state.last.is_none(),
            "large frame must release the old CPU cache"
        );
        assert!(state.has_uploaded_desktop);
    }
    assert_eq!(uploads, 20);
    assert!(
        state.reset(),
        "uncached large GPU frame still needs a placeholder"
    );
    assert!(!state.has_uploaded_desktop);
    assert!(
        !state.reset(),
        "an existing placeholder needs no allocation"
    );
    state.uploaded(desktop_shot(3840, 2160));
    assert!(state.last.is_none());
    assert!(
        state.reset(),
        "4K reset must not depend on CPU cache presence"
    );
    state.uploaded(desktop_shot(1, 1));
    assert!(state.reset(), "a real 1x1 upload is not the placeholder");
    assert!(state.last.is_none());
}

fn fake_handle() -> (OverlayHandle, Receiver<Cmd>, Sender<()>, Receiver<()>) {
    let (tx, rx) = channel();
    let (release, wait) = channel();
    let (done, finished) = channel();
    let thread = std::thread::spawn(move || {
        let _ = wait.recv_timeout(Duration::from_secs(2));
        let _ = done.send(());
    });
    (
        OverlayHandle {
            tx,
            tid: 0,
            level: Arc::new(AtomicU32::new(0)),
            visible: Arc::new(AtomicBool::new(true)),
            stop: Arc::new(AtomicBool::new(false)),
            capture: Arc::new(Mutex::new(safe_capture())),
            cards: Arc::new(Mutex::new(BTreeMap::new())),
            suspended: Arc::new(AtomicU32::new(0)),
            thread: Some(thread),
            wake_count: Some(AtomicU32::new(0)),
        },
        rx,
        release,
        finished,
    )
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
        let Cmd::Suspend(ack, deadline, live) =
            commands.recv_timeout(Duration::from_secs(1)).unwrap()
        else {
            panic!("必须先申请隐藏")
        };
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
        let Cmd::Suspend(ack, _, _) = commands.recv_timeout(Duration::from_secs(1)).unwrap() else {
            panic!("必须先申请隐藏")
        };
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
    let Cmd::Suspend(_, deadline, live) = commands.recv().unwrap() else {
        panic!("缺少过期请求")
    };
    assert!(
        !desktop_request_live(deadline, &live),
        "迟到请求不能隐藏窗口"
    );
    assert!(!desktop_request_live(
        Instant::now(),
        &AtomicBool::new(true)
    ));
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
    handle.set_card(
        card_id::TOAST,
        Some(Card::passive([10.0, 20.0, 300.0, 100.0], |_| {})),
    );
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
        for id in [
            card_id::CONFIRM,
            card_id::MINI,
            card_id::CLASS,
            card_id::FLASH,
            card_id::DOT,
            card_id::TOAST,
        ] {
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
    }))
    .is_err());
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
            0 => {}
            1 => ready
                .send(Err("fake initialization failure".into()))
                .unwrap(),
            2 => {}
            _ => ready.send(Ok(0)).unwrap(),
        }
        let ready = if outcome == 2 {
            drop(ready);
            None
        } else {
            Some(ready)
        };
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
    DesktopBounds {
        origin: (0, 0),
        size: (2, 2),
    }
}

fn shot(value: u8) -> Shot {
    Shot {
        width: 2,
        height: 2,
        rgba: vec![value; 16],
    }
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
    let shifted = DesktopBounds {
        origin: (-2, 0),
        ..bounds()
    };
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
    let old = DesktopBounds::from_rect(screen::Rect {
        x: 0,
        y: 0,
        width: 1920,
        height: 1080,
    })
    .unwrap();
    let new = DesktopBounds::from_rect(screen::Rect {
        x: -1280,
        y: -200,
        width: 3200,
        height: 1440,
    })
    .unwrap();
    assert_ne!(old, new);
    assert_eq!(new.size, (3200, 1440));
    assert_eq!(new.client_point((50, 60)), (1330, 260));
    let shifted = DesktopBounds {
        origin: (0, 0),
        ..new
    };
    assert_ne!(shifted, new);
    assert_eq!(shifted.size, new.size);
    assert!(DesktopBounds::from_rect(screen::Rect {
        x: 0,
        y: 0,
        width: 0,
        height: 0
    })
    .is_err());
}
