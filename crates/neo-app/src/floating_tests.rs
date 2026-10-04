use super::*;

#[test]
fn unchanged_drag_pixels_only_move_but_animation_and_invalidation_render() {
    let origin = Point { x: 10.0, y: 20.0 };
    let moved = Point { x: 15.0, y: 25.0 };
    assert_eq!(presentation(false, origin, None), Presentation::Render);
    assert_eq!(
        presentation(false, origin, Some(origin)),
        Presentation::Idle
    );
    assert_eq!(presentation(false, moved, Some(origin)), Presentation::Move);
    // Glass removal, language and DPI changes use the same dirty override.
    assert_eq!(
        presentation(true, moved, Some(origin)),
        Presentation::Render
    );
    let mut animation = MenuAnimation::default();
    animation.advance(true, 0.18);
    let dirty = animation.advance(false, 0.016);
    assert!(dirty);
    assert_eq!(
        presentation(dirty, moved, Some(origin)),
        Presentation::Render
    );
    animation.advance(false, 0.18);
    assert!(!animation.advance(false, 0.016));
    assert_eq!(presentation(false, moved, Some(origin)), Presentation::Move);
}

#[test]
fn menu_labels_and_idle_invalidation_follow_language_changes() {
    use crate::i18n::{tr, with_language, Language};
    with_language(Language::ZhCn, || {
        let mut previous = crate::i18n::language();
        assert!(!refresh_language(&mut previous));
        assert_eq!(Target::Ink.label(), "屏幕书写\n未实现");
        assert_eq!(Target::Board.label(), "画板\n未实现");
        with_language(Language::EnUs, || {
            assert!(refresh_language(&mut previous));
            assert!(!refresh_language(&mut previous));
            assert_eq!(Target::Ink.label(), "Ink\nNot yet");
            assert_eq!(Target::Board.label(), "Board\nNot yet");
            assert_eq!(tr("Neo 悬浮按钮"), "Neo floating control");
            assert_eq!(Target::Close.label(), "");
        });
        assert!(refresh_language(&mut previous));
        assert!(!refresh_language(&mut previous));
    });
}

fn test_handle(capture_allowed: bool) -> (FloatingHandle, std::sync::mpsc::SyncSender<Action>) {
    let (send, actions) = std::sync::mpsc::sync_channel(8);
    (
        FloatingHandle {
            shared: Arc::new(Shared::new(capture_allowed)),
            actions,
            thread: None,
            unavailable_reported: AtomicBool::new(false),
        },
        send,
    )
}

#[test]
fn capture_gate_starts_disabled_and_clears_cached_glass() {
    let (handle, _) = test_handle(false);
    let mut revision = 0;
    let mut glass = Some(vec![0xffabcdef; 4]);
    assert!(!handle.shared.may_capture(revision));
    assert!(refresh_capture(&handle.shared, &mut revision, &mut glass));
    assert!(glass.is_none());
    assert!(!refresh_capture(&handle.shared, &mut revision, &mut glass));
    handle.set_capture_allowed(true);
    assert!(refresh_capture(&handle.shared, &mut revision, &mut glass));
    assert!(handle.shared.may_capture(revision));
    handle.set_capture_allowed(true);
    assert!(!refresh_capture(&handle.shared, &mut revision, &mut glass));
}

#[test]
fn capture_toggle_invalidates_inflight_result_even_when_reenabled() {
    let (handle, _) = test_handle(true);
    let mut revision = 0;
    handle.set_capture_allowed(false);
    assert!(!handle.shared.may_capture(revision));
    handle.set_capture_allowed(true);
    assert!(!handle.shared.may_capture(revision));
    let mut glass = Some(vec![0xffabcdef]);
    assert!(refresh_capture(&handle.shared, &mut revision, &mut glass));
    assert!(glass.is_none());
    assert!(handle.shared.may_capture(revision));
    handle.set_visible(false);
    assert!(!handle.shared.may_capture(revision));
}

#[test]
fn hide_request_disables_interaction_but_requires_worker_acknowledgment() {
    let (handle, send) = test_handle(false);
    assert!(!handle.is_hidden());
    assert!(handle.shared.interactive());
    send.try_send(Action::Wake).unwrap();
    handle.set_visible(false);
    assert!(!handle.shared.interactive());
    assert!(!handle.is_hidden());
    assert_eq!(handle.try_recv(), None);
    // Simulates the acknowledgment published only after native SW_HIDE.
    handle.shared.hidden.store(true, Ordering::Release);
    assert!(handle.is_hidden());
    handle.set_visible(false);
    assert!(handle.is_hidden());
    handle.set_visible(true);
    assert!(!handle.is_hidden());
    assert!(!handle.shared.hidden.load(Ordering::Acquire));
    assert!(handle.shared.interactive());
}

#[test]
fn unavailable_is_once_only_and_not_lost_to_full_wake_queue() {
    let (handle, send) = test_handle(false);
    for _ in 0..8 {
        send.try_send(Action::Wake).unwrap();
    }
    handle.shared.dead.store(true, Ordering::Release);
    assert_eq!(handle.try_recv(), Some(Action::Unavailable));
    assert_eq!(handle.try_recv(), None);
    assert!(!handle.shared.interactive());
    handle.set_visible(false);
    assert!(!handle.is_hidden());
}

#[test]
fn drop_does_not_wait_for_blocked_worker() {
    let (mut handle, _) = test_handle(false);
    let shared = handle.shared.clone();
    let (release, wait) = std::sync::mpsc::channel::<()>();
    let (done, finished) = std::sync::mpsc::channel();
    handle.thread = Some(std::thread::spawn(move || {
        let _ = wait.recv_timeout(std::time::Duration::from_secs(3));
        let _ = done.send(());
    }));
    let before = std::time::Instant::now();
    drop(handle);
    assert!(before.elapsed() < std::time::Duration::from_secs(1));
    assert!(shared.stop.load(Ordering::Acquire));
    assert!(!shared.interactive());
    release.send(()).unwrap();
    finished
        .recv_timeout(std::time::Duration::from_secs(1))
        .unwrap();
}

fn center() -> Point {
    Point { x: 90.0, y: 90.0 }
}
fn pressed() -> Gesture {
    let mut gesture = Gesture::default();
    gesture.down(Target::Main, center(), 0);
    gesture
}

#[test]
fn short_press_wakes_once() {
    let mut g = pressed();
    assert_eq!(
        g.update(center(), true, Some(Target::Main), 100),
        Effect::None
    );
    assert_eq!(
        g.update(center(), false, Some(Target::Main), 120),
        Effect::Wake
    );
    assert_eq!(
        g.update(center(), false, Some(Target::Main), 130),
        Effect::None
    );
    assert!(!g.active());
}

#[test]
fn immediate_move_drags_and_release_never_wakes() {
    let mut g = pressed();
    let moved = Point { x: 105.0, y: 97.0 };
    assert_eq!(
        g.update(moved, true, Some(Target::Main), 20),
        Effect::Drag(Point { x: 15.0, y: 7.0 })
    );
    assert!(!g.menu);
    let next = Point { x: 109.0, y: 100.0 };
    assert_eq!(
        g.update(next, true, None, 600),
        Effect::Drag(Point { x: 4.0, y: 3.0 })
    );
    assert_eq!(g.update(next, false, Some(Target::Main), 650), Effect::None);
    assert_eq!(g.update(next, false, Some(Target::Main), 660), Effect::None);
}

#[test]
fn stationary_hold_opens_menu_once_and_release_does_not_select() {
    let mut g = pressed();
    assert_eq!(
        g.update(center(), true, Some(Target::Main), HOLD_MS - 1),
        Effect::None
    );
    assert_eq!(
        g.update(center(), true, Some(Target::Main), HOLD_MS),
        Effect::MenuChanged
    );
    assert!(g.menu);
    assert_eq!(
        g.update(center(), true, Some(Target::Main), HOLD_MS + 1),
        Effect::None
    );
    assert_eq!(
        g.update(center(), false, Some(Target::Main), 700),
        Effect::None
    );
    assert!(g.menu);
}

#[test]
fn moving_after_menu_opens_transitions_to_drag() {
    let mut g = pressed();
    g.update(center(), true, Some(Target::Main), HOLD_MS);
    assert!(g.menu);
    assert!(matches!(
        g.update(Point { x: 110.0, y: 90.0 }, true, None, 550),
        Effect::Drag(_)
    ));
    assert!(!g.menu);
    assert_eq!(
        g.update(center(), false, Some(Target::Main), 800),
        Effect::None
    );
}

#[test]
fn early_motion_drags_even_if_pointer_returns() {
    let mut g = pressed();
    assert_eq!(
        g.update(Point { x: 97.0, y: 90.0 }, true, Some(Target::Main), 20),
        Effect::Drag(Point { x: 7.0, y: 0.0 })
    );
    assert_eq!(
        g.update(center(), true, Some(Target::Main), 600),
        Effect::Drag(Point { x: -7.0, y: 0.0 })
    );
    assert_eq!(
        g.update(center(), false, Some(Target::Main), 700),
        Effect::None
    );
    assert!(!g.menu);
}

#[test]
fn small_jitter_is_a_click_but_release_outside_is_not() {
    let mut g = pressed();
    assert_eq!(
        g.update(Point { x: 92.0, y: 92.0 }, false, Some(Target::Main), 100),
        Effect::Wake
    );
    let mut g = pressed();
    assert_eq!(g.update(center(), false, None, 100), Effect::None);
}

#[test]
fn delayed_hold_release_does_not_become_click() {
    let mut g = pressed();
    assert_eq!(
        g.update(center(), false, Some(Target::Main), HOLD_MS),
        Effect::None
    );
}

#[test]
fn cancel_hides_menu_and_swallows_late_release() {
    let mut g = pressed();
    g.update(center(), true, Some(Target::Main), HOLD_MS);
    g.cancel();
    assert!(!g.menu);
    assert!(!g.active());
    assert_eq!(
        g.update(center(), false, Some(Target::Main), 700),
        Effect::None
    );
}

#[test]
fn menu_main_click_dismisses_without_wake() {
    let mut g = Gesture {
        menu: true,
        ..Gesture::default()
    };
    g.down(Target::Main, center(), 800);
    assert_eq!(
        g.update(center(), false, Some(Target::Main), 900),
        Effect::MenuChanged
    );
    assert!(!g.menu);
}

#[test]
fn close_dismisses_only_menu_and_placeholders_do_nothing() {
    for target in [Target::Close, Target::Ink, Target::Board] {
        let mut g = Gesture {
            menu: true,
            ..Gesture::default()
        };
        g.down(target, center(), 1000);
        assert_eq!(
            g.update(center(), false, Some(target), 1100),
            if target == Target::Close {
                Effect::MenuChanged
            } else {
                Effect::None
            }
        );
        assert_eq!(g.menu, target != Target::Close);
        if target == Target::Close {
            let remaining: Vec<_> = circles(g.menu).collect();
            assert_eq!(remaining.len(), 1);
            assert_eq!(remaining[0].target, Target::Main);
            assert_eq!(hit(center(), g.menu), Some(Target::Main));
            g.down(Target::Main, center(), 1200);
            assert_eq!(
                g.update(center(), false, Some(Target::Main), 1300),
                Effect::Wake
            );
        }
        let mut g = Gesture {
            menu: true,
            ..Gesture::default()
        };
        g.down(target, center(), 1000);
        assert_eq!(
            g.update(center(), false, Some(Target::Main), 1100),
            Effect::None
        );
        let mut g = Gesture {
            menu: true,
            ..Gesture::default()
        };
        g.down(target, center(), 1000);
        assert_eq!(g.update(center(), false, Some(target), 1600), Effect::None);
    }
}

#[test]
fn circle_geometry_is_disjoint_inside_canvas_and_radial() {
    let all: Vec<_> = circles(true).collect();
    assert_eq!(all.len(), 4);
    assert_eq!(circles(false).count(), 1);
    for (i, a) in all.iter().enumerate() {
        assert!(a.center.x - a.radius >= 0.0 && a.center.x + a.radius <= EXTENT);
        assert!(a.center.y - a.radius >= 0.0 && a.center.y + a.radius <= EXTENT);
        assert_eq!(hit(a.center, true), Some(a.target));
        if i > 0 {
            assert!((a.center.distance(center()) - 62.0).abs() < 0.01);
        }
        for b in &all[i + 1..] {
            assert!(a.center.distance(b.center) > a.radius + b.radius);
        }
    }
    for point in [
        Point::default(),
        Point { x: 179.0, y: 179.0 },
        Point { x: 90.0, y: 59.0 },
    ] {
        assert_eq!(hit(point, true), None);
    }
    assert_eq!(hit(all[1].center, false), None);
    assert_eq!(hit(Point { x: 113.0, y: 113.0 }, false), None);
}

#[test]
fn menu_animation_is_smooth_reversible_and_stops_when_settled() {
    let mut animation = MenuAnimation::default();
    assert!(!animation.active(false));
    assert!(!animation.advance(false, 1.0));
    assert!(animation.advance(true, 0.09));
    assert!((animation.amount() - 0.5).abs() < 0.001);
    assert!(animation.active(true));
    assert!(animation.advance(false, 0.045));
    assert!(animation.amount() < 0.5 && animation.amount() > 0.0);
    animation.advance(true, 1.0);
    assert_eq!(animation.amount(), 1.0);
    assert!(!animation.active(true));
    assert!(!animation.advance(true, 0.016));
    animation.advance(false, 0.18);
    assert_eq!(animation.amount(), 0.0);
    assert_eq!(animated_circles(animation.amount()).count(), 1);
}

#[test]
fn animated_geometry_keeps_main_size_and_disjoint_menu_circles() {
    for step in 0..=20 {
        let amount = step as f32 / 20.0;
        let all: Vec<_> = animated_circles(amount).collect();
        assert_eq!(all[0].radius, 28.0);
        assert_eq!(all[0].center, center());
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert!(a.center.distance(b.center) > a.radius + b.radius);
            }
        }
    }
    for (a, b) in animated_circles(1.0).zip(circles(true)) {
        assert_eq!(a.center, b.center);
        assert_eq!(a.radius, b.radius);
    }
}

#[test]
fn circle_edge_has_subpixel_coverage_at_multiple_dpi_scales() {
    let circle = circles(false).next().unwrap();
    for scale in [1.0, 1.25, 1.5, 2.0, 3.0] {
        assert_eq!(circle_coverage(center(), circle, scale), 1.0);
        assert_eq!(circle_coverage(Point::default(), circle, scale), 0.0);
        let edge = Point {
            x: circle.center.x + circle.radius,
            y: circle.center.y,
        };
        let coverage = circle_coverage(edge, circle, scale);
        assert!(coverage > 0.0 && coverage < 1.0);
        let outside = Point {
            x: edge.x + 0.2 / scale,
            y: edge.y,
        };
        assert!(
            circle_coverage(outside, circle, scale) > 0.0,
            "不能先按圆形硬裁剪再绘制半透明边缘"
        );
    }
}

#[test]
fn dpi_and_negative_monitor_work_area_keep_full_menu_inside() {
    for dpi in [48, 96, 120, 144, 192, 384, 768] {
        let scale = fit_scale(dpi, 1920, 1040);
        let size = (EXTENT * scale).round() as i32;
        let origin = clamp_origin(
            Point {
                x: -50.0,
                y: 3000.0,
            },
            -1920,
            -200,
            0,
            840,
            size,
        );
        assert!(origin.x >= -1920.0 && origin.y >= -200.0);
        assert!(origin.x + size as f32 <= 0.0 && origin.y + size as f32 <= 840.0);
    }
    assert_eq!(fit_scale(144, 1920, 1080), 1.5);
    let scale = fit_scale(192, 80, 60);
    assert!(EXTENT * scale <= 60.001);
}
