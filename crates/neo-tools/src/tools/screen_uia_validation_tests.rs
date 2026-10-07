//! Pure regression tests: no COM, desktop enumeration, window activation or input.
use super::*;
use crate::result::ErrorKind;

fn target() -> ScreenElement {
    ScreenElement {
        role: "Button", name: "最小化".into(), window: "Browser".into(),
        enabled: true, foreground: true, actionable: true,
        rect: [1800, 0, 46, 30],
        identity: ElementIdentity {
            hwnd: 42, process_id: 7, process_started: 100,
            window_process_id: 7, window_process_started: 100,
            window_runtime_id: vec![42, 1], runtime_id: vec![42, 2],
        },
        ..Default::default()
    }
}

#[test]
fn every_identity_and_snapshot_field_still_fails_closed() {
    let expected = target();
    assert_eq!(target_mismatch(&expected, &expected, true, false), None);
    assert_eq!(target_mismatch(&expected, &expected, false, false), Some("element_disabled"));
    assert_eq!(target_mismatch(&expected, &expected, true, true), Some("element_offscreen"));
    type Case = (&'static str, fn(&mut ScreenElement));
    let cases: &[Case] = &[
        ("window_handle_changed", |e| e.identity.hwnd += 1),
        ("element_process_changed", |e| e.identity.process_id += 1),
        ("element_process_changed", |e| e.identity.process_started += 1),
        ("window_process_changed", |e| e.identity.window_process_id += 1),
        ("window_process_changed", |e| e.identity.window_process_started += 1),
        ("window_runtime_id_changed", |e| e.identity.window_runtime_id.push(9)),
        ("element_runtime_id_mismatch", |e| e.identity.runtime_id.push(9)),
        ("element_rect_changed", |e| e.rect[0] += 1),
        ("element_role_changed", |e| e.role = "Custom"),
        ("element_name_changed", |e| e.name = "关闭".into()),
        ("window_title_changed", |e| e.window.push('!')),
        ("unverifiable_identity", |e| e.identity.runtime_id.clear()),
        ("unverifiable_identity", |e| e.identity.window_runtime_id.clear()),
        ("unverifiable_identity", |e| e.identity.process_started = 0),
    ];
    for (code, change) in cases {
        let mut current = expected.clone();
        change(&mut current);
        assert_eq!(target_mismatch(&expected, &current, true, false), Some(*code));
        assert!(!same_target(&expected, &current, true, false));
    }
}

#[test]
fn titlebar_window_and_same_geometry_sibling_are_not_target_proof() {
    let expected = target();
    for role in ["TitleBar", "Window", "Pane", "Button", "Custom"] {
        let mut hit = expected.clone();
        hit.role = role;
        hit.identity.runtime_id.push(99);
        assert_eq!(target_mismatch(&expected, &hit, true, false), Some("element_runtime_id_mismatch"));
        assert!(!may_ascend_hit(&expected, &hit, passive_content(role, false, false, false), 0, true, false));
    }
    // Even an exact element in a stale background snapshot is rejected before any desktop access.
    let mut background = expected;
    background.foreground = false;
    assert!(validate_target(&background).unwrap_err().message.contains("background_window"));
}

#[test]
fn passive_hit_must_reach_exact_target_without_crossing_process_or_window() {
    let expected = target();
    let mut hit = expected.clone();
    hit.role = "Text";
    hit.identity.runtime_id = vec![42, 3];
    assert!(may_ascend_hit(&expected, &hit, true, 0, true, false));
    assert!(!same_target(&expected, &hit, true, false));
    for mutate in [
        (|e: &mut ScreenElement| e.identity.process_id += 1) as fn(&mut ScreenElement),
        |e| e.identity.process_started += 1,
        |e| e.identity.hwnd += 1,
        |e| e.identity.window_process_id += 1,
        |e| e.identity.window_process_started += 1,
        |e| e.identity.window_runtime_id.push(9),
    ] {
        let mut foreign = hit.clone();
        mutate(&mut foreign);
        assert!(!may_ascend_hit(&expected, &foreign, true, 0, true, false));
    }
    assert!(!may_ascend_hit(&expected, &hit, false, 0, true, false));
    assert!(!may_ascend_hit(&expected, &hit, true, HIT_ANCESTORS, true, false));
}

#[test]
fn provider_hresult_is_classified_and_retains_stage() {
    for (hr, code, kind) in [
        (0x80070005, "access_denied", ErrorKind::NotAllowed),
        (0x80040201, "element_unavailable", ErrorKind::NotFound),
        (0x80131505, "provider_timeout", ErrorKind::Timeout),
        (0x800705B4, "provider_timeout", ErrorKind::Timeout),
        (0x80004005, "provider_error", ErrorKind::Io),
    ] {
        let error = validation_error(vec![ValidationFailure::provider("ElementFromPoint", hr)]);
        assert_eq!(error.kind, kind);
        assert!(error.message.contains(code));
        assert!(error.message.contains("stage=ElementFromPoint"));
        assert!(error.message.contains(&format!("HRESULT=0x{hr:08X}")));
    }
}

#[test]
fn passive_hit_provider_errors_propagate_instead_of_identity_mismatch() {
    let expected = target();
    let mut hit = expected.clone();
    hit.role = "Text";
    hit.identity.runtime_id.push(9);
    for (hr, code, kind) in [
        (0x80070005, "access_denied", ErrorKind::NotAllowed),
        (0x80131505, "provider_timeout", ErrorKind::Timeout),
        (0x800705B4, "provider_timeout", ErrorKind::Timeout),
    ] {
        let failures = verified_candidate(vec![(10, 10)], |_| {
            let passive = passive_content_result(hit.role, || Err(ValidationFailure::provider("GetCurrentPattern(Invoke)", hr)), || {
                panic!("pattern errors must propagate before reading more properties")
            })?;
            if may_ascend_hit(&expected, &hit, passive, 0, true, false) { Ok(()) }
            else { Err(ValidationFailure::rejected("element_runtime_id_mismatch")) }
        }).unwrap_err();
        let error = validation_error(failures);
        assert_eq!(error.kind, kind);
        assert!(error.message.contains(code));
        assert!(error.message.contains("stage=GetCurrentPattern(Invoke)"));
        assert!(error.message.contains(&format!("HRESULT=0x{hr:08X}")));
        assert!(!error.message.contains("element_runtime_id_mismatch"));
        // Enumeration may still conservatively skip labels on the same error.
        assert!(!passive_content_result(hit.role, || Err(hr), || Ok((false, false))).unwrap_or(false));
    }
}

#[test]
fn passive_result_keeps_all_content_gates_and_property_errors() {
    for (role, action, focusable, password, allowed) in [
        ("Text", false, false, false, true), ("Image", false, false, false, true),
        ("Button", false, false, false, false), ("Text", true, false, false, false),
        ("Text", false, true, false, false), ("Text", false, false, true, false),
    ] {
        assert_eq!(passive_content_result::<()>(role, || Ok(action), || Ok((focusable, password))), Ok(allowed));
    }
    assert_eq!(passive_content_result("Text", || Ok(false), || Err("property failure")), Err("property failure"));
}

#[test]
fn non_content_hits_retain_identity_mismatch_without_pattern_or_property_queries() {
    let expected = target();
    for role in ["Button", "TitleBar", "Window", "Pane", "Custom", ""] {
        let mut hit = expected.clone();
        hit.role = role;
        hit.identity.runtime_id.push(9);
        let passive = passive_content_result::<()>(role,
            || panic!("non-content hit must not query patterns"),
            || panic!("non-content hit must not query passive properties")).unwrap();
        assert!(!passive);
        assert!(!may_ascend_hit(&expected, &hit, passive, 0, true, false));
        assert_eq!(target_mismatch(&expected, &hit, true, false), Some("element_runtime_id_mismatch"));
    }
}

#[test]
fn zero_hresult_errors_outside_nullable_pattern_abi_are_not_suppressed() {
    let error = validation_error(vec![ValidationFailure::provider("hit.identity", 0)]);
    assert_eq!(error.kind, ErrorKind::Io);
    assert!(error.message.contains("provider_error (stage=hit.identity, HRESULT=0x00000000)"));
}

#[test]
fn different_provider_stages_and_hresults_survive_exact_deduplication() {
    let failures = vec![
        ValidationFailure::provider("ElementFromPoint", 0x80131505),
        ValidationFailure::provider("passive_hit", 0x80131505),
        ValidationFailure::provider("passive_hit", 0x800705B4),
    ];
    let error = validation_error(failures.iter().cloned().cycle().take(12).collect());
    assert_eq!(error.kind, ErrorKind::Timeout);
    for failure in failures {
        assert_eq!(error.message.matches(&format!("{} ({})", failure.code, failure.detail)).count(), 1);
    }
    assert_eq!(error.message.matches("provider_timeout").count(), 3);
    assert!(error.message.contains("=12"));
}

#[test]
fn distinct_raw_hits_survive_but_identical_details_are_deduplicated_and_bounded() {
    let raw_hit = |point| {
        let mut failure = ValidationFailure::rejected("element_runtime_id_mismatch");
        failure.detail = format!("raw_hit_rejected, point=({point}, 10), hit_runtime_id=[42, {point}]");
        failure
    };
    let mut failures = vec![raw_hit(0); 20];
    failures.extend((1..20).map(raw_hit));
    let error = validation_error(failures);
    assert_eq!(error.kind, ErrorKind::Conflict);
    assert_eq!(error.message.matches("element_runtime_id_mismatch").count(), 8);
    for point in 0..8 {
        assert_eq!(error.message.matches(&raw_hit(point).detail).count(), 1);
    }
    for point in 8..20 {
        assert!(!error.message.contains(&raw_hit(point).detail));
    }
    assert!(error.message.contains("=39"));
}

#[test]
fn candidate_errors_survive_and_occlusion_never_returns_a_point() {
    let points = vec![(10, 10), (20, 20), (30, 30)];
    let failures = verified_candidate(points, |point| match point.0 {
        10 => Err(ValidationFailure::provider("ElementFromPoint", 0x80070005)),
        20 => Err(ValidationFailure::rejected("native_window_hit_mismatch")),
        _ => Err(ValidationFailure::rejected("element_runtime_id_mismatch")),
    }).unwrap_err();
    assert_eq!(failures.len(), 3);
    let error = validation_error(failures);
    assert_eq!(error.kind, ErrorKind::NotAllowed);
    for reason in ["access_denied", "native_window_hit_mismatch", "element_runtime_id_mismatch"] {
        assert!(error.message.contains(reason));
    }
    let hint = error.hint.unwrap();
    assert!(hint.contains("不会自动激活窗口"));
    assert!(hint.contains("Invoke"));
}

#[test]
fn only_a_fully_verified_later_candidate_can_succeed() {
    let mut visited = Vec::new();
    let result = verified_candidate(vec![(1, 1), (2, 2), (3, 3)], |p| {
        visited.push(p);
        if p.0 == 1 { Err(ValidationFailure::provider("hit.identity", 0x80040201)) }
        else { Ok(()) }
    });
    assert_eq!(result.unwrap(), (2, 2));
    assert_eq!(visited, vec![(1, 1), (2, 2)]);
}

#[test]
fn no_visible_candidate_and_deadline_are_distinct() {
    let failures = verified_candidate(Vec::new(), |_| -> Result<(), ValidationFailure> {
        panic!("no monitor-visible point may be fabricated")
    }).unwrap_err();
    assert!(validation_error(failures).message.contains("no_visible_candidate"));
    let timeout = validation_error(vec![ValidationFailure::timeout()]);
    assert_eq!(timeout.kind, ErrorKind::Timeout);
    assert!(timeout.message.contains("validation_timeout"));
}

#[test]
fn diagnostic_limit_cannot_hide_late_permission_error() {
    let mut failures: Vec<_> = [
        "background_window", "window_not_visible", "window_minimized", "window_cloaked",
        "element_disabled", "element_offscreen", "element_rect_changed", "element_name_changed",
        "element_role_changed",
    ].into_iter().map(ValidationFailure::rejected).collect();
    failures.push(ValidationFailure::provider("hit.identity", 0x80070005));
    let error = validation_error(failures);
    assert_eq!(error.kind, ErrorKind::NotAllowed);
    assert!(error.message.contains("access_denied (stage=hit.identity, HRESULT=0x80070005)"));
    assert_eq!(error.message.matches("; ").count(), 7);
    assert!(error.message.contains("=10"));
}

#[test]
fn repeated_failures_are_deduplicated_without_losing_total_count() {
    let error = validation_error(vec![ValidationFailure::rejected("native_window_hit_mismatch"); 20]);
    assert_eq!(error.message.matches("native_window_hit_mismatch").count(), 1);
    assert!(error.message.contains("=20"));
    assert_eq!(error.kind, ErrorKind::Conflict);
}
