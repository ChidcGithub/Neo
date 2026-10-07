use super::*;
use serde_json::{json, Value};

#[test]
fn local_failures_use_current_language_without_network() {
    use crate::i18n::{with_language, Language};
    for language in [Language::ZhCn, Language::EnUs] {
        with_language(language, || {
            let ctx = egui::Context::default();
            let mut checker = UpdateChecker::default();
            checker.start(
                &ctx,
                || unreachable!(),
                |_| Err(io::Error::other("raw OS error")),
            );
            assert_eq!(checker.poll(), Some(Status::Failed(tr(SPAWN_ERROR).into())));
            assert_eq!(checker.poll(), None);
            checker.start(
                &ctx,
                || unreachable!(),
                |job| {
                    drop(job);
                    Ok(())
                },
            );
            assert_eq!(
                checker.poll(),
                Some(Status::Failed(tr(RESPONSE_ERROR).into()))
            );
            assert_eq!(checker.poll(), None);
        });
    }
}

fn release(tag: &str, prerelease: bool) -> Value {
    json!({
        "tag_name": tag,
        "html_url": format!("https://github.com/ChidcGithub/Neo/releases/tag/{tag}"),
        "draft": false,
        "prerelease": prerelease,
    })
}

fn select(releases: Vec<Value>, current: &str) -> Status {
    select_release(&serde_json::to_vec(&releases).unwrap(), current).unwrap()
}

fn available(tag: &str) -> Status {
    Status::Available {
        version: tag.strip_prefix('v').unwrap_or(tag).into(),
        url: format!("https://github.com/ChidcGithub/Neo/releases/tag/{tag}"),
    }
}

#[test]
fn semver_orders_numeric_components_and_prerelease_identifiers() {
    assert_eq!(
        select(
            vec![release("v0.9.0", false), release("0.10.0", false)],
            "0.8.0"
        ),
        available("0.10.0")
    );
    assert_eq!(
        select(
            vec![release("v1.0.0-rc.10", true), release("v1.0.0-rc.2", true)],
            "1.0.0-rc.1",
        ),
        available("v1.0.0-rc.10")
    );
    // SemVer 本身按字典序；Neo 更新策略有意兼容历史 preN 数字序号。
    assert!(Version::parse("0.1.0-pre9")
        .unwrap()
        .cmp_precedence(&Version::parse("0.1.0-pre10").unwrap())
        .is_gt());
    assert_eq!(
        select(vec![release("v0.1.0-pre10", true)], "0.1.0-pre2"),
        available("v0.1.0-pre10")
    );
    assert_eq!(
        select(vec![release("v1.0.0+new", false)], "1.0.0+old"),
        Status::UpToDate
    );
}

#[test]
fn pre_numbers_are_numeric_in_both_comparisons_and_keep_original_links() {
    for tags in [
        ["v0.1.0-pre9", "v0.1.0-pre10"],
        ["v0.1.0-pre10", "v0.1.0-pre9"],
    ] {
        assert_eq!(
            select(
                tags.into_iter().map(|t| release(t, true)).collect(),
                "0.1.0-pre8"
            ),
            available("v0.1.0-pre10")
        );
    }
    assert_eq!(
        select(vec![release("v0.1.0-pre10", true)], "0.1.0-pre9"),
        available("v0.1.0-pre10")
    );
    assert_eq!(
        select(vec![release("v0.1.0-pre9", true)], "0.1.0-pre10"),
        Status::UpToDate
    );
    for (current, tag) in [
        ("0.1.0-pre09", "v0.1.0-pre9"),
        ("0.1.0-pre9", "v0.1.0-pre09"),
        ("0.1.0-pre9", "v0.1.0-pre.9"),
        ("0.1.0-pre9+old", "v0.1.0-pre9+new"),
    ] {
        assert_eq!(select(vec![release(tag, true)], current), Status::UpToDate);
    }
    assert_eq!(
        select(vec![release("v0.1.0-pre.10", true)], "0.1.0-pre9"),
        available("v0.1.0-pre.10")
    );
    assert_eq!(
        select(vec![release("v0.1.0-pre10", true)], "0.1.0-pre.9"),
        available("v0.1.0-pre10")
    );
    assert_eq!(
        select(
            vec![release("v0.1.0-pre18446744073709551616", true)],
            "0.1.0-pre18446744073709551615"
        ),
        available("v0.1.0-pre18446744073709551616")
    );
}

#[test]
fn actual_release_history_never_recommends_old_rc_to_pre5() {
    // Official GET /repos/ChidcGithub/Neo/releases, checked 2026-10-01.
    // published_at is evidence of the naming migration, NOT the update sort key.
    let history = [
        ("v0.1.0-rc269251305", "2026-09-25T07:00:26Z"),
        ("v0.1.0-rc269271504", "2026-09-27T07:56:50Z"),
        ("v0.1.0-rc269271930", "2026-09-27T11:45:47Z"),
        ("v0.1.0-pre2", "2026-09-28T09:33:46Z"),
        ("v0.1.0-pre3", "2026-09-28T23:07:27Z"),
        ("v0.1.0-pre5", "2026-10-01T00:36:29Z"),
    ];
    let mut releases: Vec<Value> = history
        .into_iter()
        .map(|(tag, published)| {
            let mut item = release(tag, true);
            item["published_at"] = json!(published);
            item
        })
        .collect();
    for _ in 0..2 {
        assert_eq!(select(releases.clone(), "0.1.0-pre5"), Status::UpToDate);
        assert_eq!(select(releases.clone(), "0.1.0"), Status::UpToDate);
        for current in ["0.1.0-pre3", "0.1.0-rc269251305", "0.1.0-rc269271930"] {
            assert_eq!(select(releases.clone(), current), available("v0.1.0-pre5"));
        }
        releases.reverse();
    }
}

#[test]
fn legacy_migration_does_not_require_current_release_or_dates() {
    // Includes untagged historical Cargo versions and an unseen legacy-shaped tag:
    // compatibility is a format/epoch rule, not a blacklist of three GitHub tags.
    for rc in ["rc260251648", "rc269271207", "rc269271930", "rc269281234"] {
        let tag = format!("v0.1.0-{rc}");
        assert_eq!(
            select(vec![release(&tag, true)], "0.1.0-pre5"),
            Status::UpToDate
        );
        assert_eq!(
            select(vec![release("v0.1.0-pre1", true)], &format!("0.1.0-{rc}")),
            available("v0.1.0-pre1")
        );
    }
    assert_eq!(
        select(
            vec![release("v0.1.0-rc269271930", true)],
            "0.1.0-rc269271504"
        ),
        available("v0.1.0-rc269271930")
    );
    assert_eq!(
        select(
            vec![release("v0.1.0-rc269271504", true)],
            "0.1.0-rc269271930"
        ),
        Status::UpToDate
    );
    assert_eq!(
        select(vec![release("v0.1.0", false)], "0.1.0-rc269271930"),
        available("v0.1.0")
    );
}

#[test]
fn compatibility_is_scoped_and_preserves_standard_semver_channels() {
    for (current, tag) in [
        ("0.1.0-pre10", "v0.1.0-rc.1"),
        ("0.1.0-pre10", "v0.1.0-rc1"),
        ("0.2.0-pre10", "v0.2.0-rc269271930"),
        ("0.1.0-pre10", "v0.2.0-alpha.1"),
        ("0.1.0-alpha.2", "v0.1.0-beta.1"),
        ("0.1.0-alpha10", "v0.1.0-alpha9"),
    ] {
        assert_eq!(select(vec![release(tag, true)], current), available(tag));
    }
    assert_eq!(
        select(vec![release("v0.1.0-pre99", true)], "0.1.0-rc.1"),
        Status::UpToDate
    );
    assert_eq!(
        select(vec![release("v0.1.0-pre99", true)], "0.2.0-alpha.1"),
        Status::UpToDate
    );
    // Similar-looking compound identifiers are not rewritten as a preN sequence.
    assert_eq!(
        select(
            vec![release("v0.1.0-pre10.extra", true)],
            "0.1.0-pre9.extra"
        ),
        Status::UpToDate
    );
}

#[test]
fn release_dates_and_array_order_cannot_override_version_precedence() {
    let mut older = release("v0.1.0-pre9", true);
    older["published_at"] = json!("2030-01-01T00:00:00Z");
    let mut legacy = release("v0.1.0-rc269271930", true);
    legacy["published_at"] = json!("2031-01-01T00:00:00Z");
    assert_eq!(select(vec![older, legacy], "0.1.0-pre10"), Status::UpToDate);
    let mut stable = release("v0.1.0", false);
    stable["published_at"] = json!("2026-01-01T00:00:00Z");
    assert_eq!(
        select(vec![stable, release("v0.1.0-pre10", true)], "0.1.0-pre5"),
        available("v0.1.0")
    );
}

#[test]
fn update_order_is_transitive_across_the_legacy_boundary() {
    let ordered: Vec<Version> = [
        "0.0.9",
        "0.1.0-rc269251305",
        "0.1.0-rc269271930",
        "0.1.0-alpha.1",
        "0.1.0-beta.1",
        "0.1.0-pre9",
        "0.1.0-pre10",
        "0.1.0-rc.1",
        "0.1.0-rc.10",
        "0.1.0",
        "0.2.0-alpha.1",
    ]
    .into_iter()
    .map(|v| Version::parse(v).unwrap())
    .collect();
    for (i, left) in ordered.iter().enumerate() {
        for (j, right) in ordered.iter().enumerate() {
            assert_eq!(
                cmp_update_versions(left, right),
                i.cmp(&j),
                "{left} vs {right}"
            );
        }
    }
}

#[test]
fn prerelease_channel_accepts_stable_and_prerelease() {
    assert_eq!(
        select(vec![release("v0.1.0-pre3", true)], "0.1.0-pre2"),
        available("v0.1.0-pre3")
    );
    assert_eq!(
        select(
            vec![release("v0.1.0", false), release("v0.1.0-pre3", true)],
            "0.1.0-pre2"
        ),
        available("v0.1.0")
    );
}

#[test]
fn stable_channel_rejects_both_kinds_of_prerelease_marker() {
    assert_eq!(
        select(
            vec![
                release("v2.0.0-rc.1", false),
                release("v2.0.0-pre10", false),
                release("v2.0.0-pre11", true),
                release("v3.0.0", true),
                release("v1.1.0", false)
            ],
            "1.0.0",
        ),
        available("v1.1.0")
    );
}

#[test]
fn drafts_removed_releases_invalid_tags_and_older_versions_are_ignored() {
    let mut draft = release("v9.0.0", false);
    draft["draft"] = json!(true);
    assert_eq!(
        select(
            vec![
                draft,
                release("release-8.0.0", false),
                release("v01.0.0", false),
                release("v0.9.0", false)
            ],
            "1.0.0"
        ),
        Status::UpToDate
    );
    assert_eq!(select(Vec::new(), "0.1.0-pre2"), Status::UpToDate);
}

#[test]
fn only_matching_repository_release_links_are_allowed() {
    for url in [
        "javascript:alert(1)",
        "http://github.com/ChidcGithub/Neo/releases/tag/v2.0.0",
        "https://evil.example/ChidcGithub/Neo/releases/tag/v2.0.0",
        "https://github.com/Other/Neo/releases/tag/v2.0.0",
        "https://github.com/ChidcGithub/Other/releases/tag/v2.0.0",
        "https://github.com@evil.example/ChidcGithub/Neo/releases/tag/v2.0.0",
        "https://github.com/ChidcGithub/Neo/releases/tag/v1.0.0",
        "https://github.com/ChidcGithub/Neo/releases/tag/v2.0.0?next=https://evil.example",
        "https://github.com/ChidcGithub/Neo/releases/tag/v2.0.0#fragment",
    ] {
        let mut item = release("v2.0.0", false);
        item["html_url"] = json!(url);
        assert_eq!(select(vec![item], "1.0.0"), Status::UpToDate, "{url}");
    }
}

#[test]
fn giant_bodies_are_rejected_with_or_without_content_length() {
    let body = vec![b' '; MAX_BODY_BYTES + 10];
    assert_eq!(
        read_body(body.as_slice(), Some(body.len() as u64)),
        Err(RESPONSE_ERROR)
    );
    assert_eq!(read_body(body.as_slice(), None), Err(RESPONSE_ERROR));
    assert_eq!(read_body(body.as_slice(), Some(1)), Err(RESPONSE_ERROR));
    assert_eq!(select_release(&body, "1.0.0"), Err(RESPONSE_ERROR));
    let exact = vec![b' '; MAX_BODY_BYTES];
    assert_eq!(
        read_body(exact.as_slice(), None).unwrap().len(),
        MAX_BODY_BYTES
    );
}

#[test]
fn malformed_and_unbounded_metadata_only_produce_safe_errors() {
    for body in [
        b"not json secret".as_slice(),
        b"{}",
        b"[{}]",
        b"null",
        b"[] trailing",
    ] {
        assert_eq!(select_release(body, "1.0.0"), Err(RESPONSE_ERROR));
    }
    let many = serde_json::to_vec(&vec![release("v2.0.0", false); MAX_RELEASES + 1]).unwrap();
    assert_eq!(select_release(&many, "1.0.0"), Err(RESPONSE_ERROR));
    for (field, limit) in [("tag_name", MAX_TAG_BYTES), ("html_url", MAX_URL_BYTES)] {
        let mut item = release("v2.0.0", false);
        item[field] = json!("x".repeat(limit + 1));
        assert_eq!(
            select_release(&serde_json::to_vec(&vec![item]).unwrap(), "1.0.0"),
            Err(RESPONSE_ERROR)
        );
    }
    assert_eq!(select_release(b"[]", "invalid"), Err(RESPONSE_ERROR));
}

#[test]
fn duplicate_requests_are_single_flight_and_completion_is_delivered_once() {
    let ctx = egui::Context::default();
    let mut checker = UpdateChecker::default();
    assert_eq!(checker.status(), &Status::Idle);
    assert_eq!(checker.poll(), None);
    let mut job = None;
    checker.start(
        &ctx,
        || Status::UpToDate,
        |worker| {
            job = Some(worker);
            Ok(())
        },
    );
    assert_eq!(checker.status(), &Status::Checking);
    checker.start(&ctx, || unreachable!(), |_| panic!("duplicate spawn"));
    assert_eq!(checker.poll(), None);
    job.unwrap()();
    assert_eq!(checker.poll(), Some(Status::UpToDate));
    assert_eq!(checker.poll(), None);
}

#[test]
fn cancel_discards_old_results_but_holds_single_flight_until_worker_finishes() {
    let ctx = egui::Context::default();
    let mut checker = UpdateChecker::default();
    let mut old_job = None;
    checker.start(
        &ctx,
        || available("v2.0.0"),
        |job| {
            old_job = Some(job);
            Ok(())
        },
    );
    checker.cancel();
    assert_eq!(checker.status(), &Status::Idle);
    assert!(checker.is_running());
    for _ in 0..10 {
        checker.start(
            &ctx,
            || unreachable!(),
            |_| panic!("cancel bypassed single flight"),
        );
        checker.cancel();
    }
    old_job.unwrap()();
    assert!(!checker.is_running());
    assert_eq!(checker.poll(), None);
    assert_eq!(checker.status(), &Status::Idle);
    checker.start(
        &ctx,
        || Status::UpToDate,
        |job| {
            job();
            Ok(())
        },
    );
    assert_eq!(checker.poll(), Some(Status::UpToDate));
}

#[test]
fn cancel_also_discards_already_queued_completion() {
    let ctx = egui::Context::default();
    let mut checker = UpdateChecker::default();
    checker.start(
        &ctx,
        || available("v2.0.0"),
        |job| {
            job();
            Ok(())
        },
    );
    checker.cancel();
    assert_eq!(checker.poll(), None);
    assert_eq!(checker.status(), &Status::Idle);
}

#[test]
fn spawn_failure_is_visible_once_and_can_be_retried() {
    let ctx = egui::Context::default();
    let mut checker = UpdateChecker::default();
    checker.start(
        &ctx,
        || unreachable!(),
        |_| Err(io::Error::other("private OS details")),
    );
    assert_eq!(checker.status(), &Status::Failed(SPAWN_ERROR.into()));
    assert_eq!(checker.poll(), Some(Status::Failed(SPAWN_ERROR.into())));
    assert_eq!(checker.poll(), None);
    checker.start(
        &ctx,
        || Status::UpToDate,
        |job| {
            job();
            Ok(())
        },
    );
    assert_eq!(checker.poll(), Some(Status::UpToDate));
}

#[test]
fn abandoned_worker_releases_single_flight_and_allows_retry() {
    let ctx = egui::Context::default();
    let mut checker = UpdateChecker::default();
    let job = checker.request_controlled(&ctx, Status::UpToDate);
    assert!(checker.is_running());
    drop(job);
    assert!(!checker.is_running());
    assert_eq!(
        checker.poll(),
        Some(Status::Failed(tr(RESPONSE_ERROR).into()))
    );
    assert_eq!(checker.poll(), None);
    checker.request_controlled(&ctx, Status::UpToDate)();
    assert_eq!(checker.poll(), Some(Status::UpToDate));
}

#[test]
fn unwinding_worker_reports_failure_and_allows_retry() {
    let ctx = egui::Context::default();
    let mut checker = UpdateChecker::default();
    let mut job = None;
    checker.start(
        &ctx,
        || panic!("synthetic worker failure"),
        |worker| {
            job = Some(worker);
            Ok(())
        },
    );
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(job.unwrap())).is_err());
    assert!(!checker.is_running());
    assert_eq!(
        checker.poll(),
        Some(Status::Failed(tr(RESPONSE_ERROR).into()))
    );
    assert_eq!(checker.poll(), None);
    checker.request_controlled(&ctx, Status::UpToDate)();
    assert_eq!(checker.poll(), Some(Status::UpToDate));
}

#[test]
fn completion_disconnects_and_releases_before_notifying_ui() {
    let ctx = egui::Context::default();
    let running = Arc::new(AtomicBool::new(true));
    let (sender, receiver) = mpsc::channel::<Status>();
    let observed = Arc::new(AtomicBool::new(false));
    let observed_callback = Arc::clone(&observed);
    let running_callback = Arc::clone(&running);
    let receiver = std::sync::Mutex::new(receiver);
    ctx.set_request_repaint_callback(move |_| {
        assert!(!running_callback.load(Ordering::Acquire));
        assert!(matches!(
            receiver.lock().unwrap().try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
        observed_callback.store(true, Ordering::Release);
    });
    drop(Completion {
        running,
        repaint: ctx,
        sender: Some(sender),
    });
    assert!(observed.load(Ordering::Acquire));
}

#[test]
fn drop_does_not_wait_for_pending_worker() {
    let ctx = egui::Context::default();
    let mut checker = UpdateChecker::default();
    let mut pending = None;
    checker.start(
        &ctx,
        || Status::UpToDate,
        |job| {
            pending = Some(job);
            Ok(())
        },
    );
    drop(checker);
    pending.unwrap()();
}
