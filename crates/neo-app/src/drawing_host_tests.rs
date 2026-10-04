use super::*;
use crate::drawing_manager::tests::{attach, state, Fake};
use crate::drawing_manager::DrawingManager;
use crate::drawing_runtime::Event;
use std::sync::{atomic::AtomicBool, Arc};

fn configured(capture: bool) -> (DrawingManager, Fake, State) {
    let mut manager = DrawingManager::default();
    manager.set_services(
        neo_llm::Config::deepseek("test-only"),
        &egui::Context::default(),
    );
    manager.safe = !capture;
    let mut s = state(capture, false, !capture);
    s.permissions = manager.permissions();
    let fake = attach(&mut manager, BoardKind::Drawing, 99);
    fake.emit(Event::Ready(s.clone()));
    manager.poll(!capture);
    fake.respond(s.clone());
    manager.poll(!capture);
    (manager, fake, s)
}
fn params(s: &State, capture: bool) -> Value {
    let mut p = json!({"document_id":s.document_id,"page_id":s.page_id,"revision":s.revision,
        "job_id":"job-1","user_authorized":true});
    if capture {
        p["windows_hidden_confirmed"] = json!(true);
    } else {
        p["prompt"] = json!("explain");
        p["asset_refs"] = json!(["asset:one"]);
        p["write_back"] = json!(true);
    }
    p
}
fn ask(fake: &Fake, id: &str, method: &str, params: Value) {
    fake.emit(Event::HostRequest {
        id: id.into(),
        method: method.into(),
        params,
    });
}

#[test]
fn resources_are_session_local_bounded_crc_and_chunked() {
    assert_eq!(crc32(b"123456789"), 0xcbf43926);
    let mut resources = Resources::default();
    let bytes = [b"\x89PNG\r\n\x1a\n".as_slice(), &vec![2; 9000]].concat();
    let descriptor = resources.insert(bytes.clone()).unwrap();
    let id = descriptor["asset_ref"].as_str().unwrap();
    assert!(token(id));
    let p = json!({"asset_ref":id,"offset":0,"length":8192});
    let chunk = resources.request("resources.read", &p).unwrap();
    assert_eq!(chunk["bytes"].as_array().unwrap().len(), 8192);
    assert_eq!(chunk["next_offset"], 8192);
    assert_eq!(chunk["eof"], false);
    assert!(Resources::default().request("resources.read", &p).is_err());
    assert!(resources
        .request(
            "resources.read",
            &json!({"asset_ref":id,"offset":0,"length":8193})
        )
        .is_err());
    assert_eq!(
        resources.request("resources.release", &p).unwrap()["released"],
        true
    );
    assert_eq!(
        resources.request("resources.release", &p).unwrap()["released"],
        false
    );
    assert!(resources.request("resources.read", &p).is_err());
    for _ in 0..4 {
        resources.insert(bytes.clone()).unwrap();
    }
    assert!(resources.insert(bytes.clone()).is_err());
    for a in resources.assets.values_mut() {
        a.expires = Instant::now();
    }
    resources.expire();
    assert!(resources.assets.is_empty());
    assert!(resources.insert(vec![0; PNG_MAX + 1]).is_err());
    assert!(!token("asset:../secret"));
    assert!(!token("asset:图"));
}

#[test]
fn ask_does_not_read_before_consent_and_vision_is_required() {
    let (mut m, f, s) = configured(false);
    ask(&f, "runtime:ask", "host.ask_agent", params(&s, false));
    m.poll(true);
    assert!(m.consent_pending());
    assert!(!f
        .sent()
        .iter()
        .any(|(_, method, _)| method == "resources.read"));
    let host = &mut m.boards[0].host;
    let c = host.consent().unwrap();
    assert!(!c.write_back && !c.structured_edit && !c.vision_confirmed);
    assert!(c.write_allowed);
    host.authorize(true);
    assert!(host.consent().is_some());
    host.consent().unwrap().vision_confirmed = true;
    host.authorize(true);
    m.poll(true);
    assert_eq!(f.sent().last().unwrap().1, "resources.read");
    assert_eq!(f.sent().last().unwrap().2["length"], 8192);
}

#[test]
fn malformed_context_authorization_types_and_limits_fail_without_reads() {
    for key in [
        "user_authorized",
        "revision",
        "page_id",
        "document_id",
        "write_back",
        "asset_refs",
        "job_id",
        "prompt",
    ] {
        let (mut m, f, s) = configured(false);
        let mut p = params(&s, false);
        p[key] = Value::Null;
        ask(&f, "runtime:bad", "host.ask_agent", p);
        m.poll(true);
        assert!(!m.boards[0].host.busy(), "{key}");
        assert!(f.replies().last().unwrap().1.is_err());
        assert!(!f.sent().iter().any(|r| r.1 == "resources.read"));
    }
}

#[test]
fn host_and_window_response_maps_are_independent_and_bad_chunks_fail() {
    let (mut m, f, s) = configured(false);
    ask(&f, "runtime:ask", "host.ask_agent", params(&s, false));
    m.poll(true);
    m.boards[0].host.consent().unwrap().vision_confirmed = true;
    m.boards[0].host.authorize(true);
    m.boards[0].next_state = Instant::now();
    m.poll(true);
    let read = f
        .sent()
        .into_iter()
        .find(|r| r.1 == "resources.read")
        .unwrap();
    let window_id = m.boards[0].pending.as_ref().unwrap().id.clone();
    f.emit(Event::Response { id: read.0, method: read.1,
        result: Ok(json!({"asset_ref":"asset:one","offset":1,"total_bytes":8,"bytes":[1],"next_offset":2,"eof":false})) });
    m.poll(true);
    assert_eq!(m.boards[0].pending.as_ref().unwrap().id, window_id);
    assert!(f
        .replies()
        .iter()
        .any(|(id, r)| id == "runtime:ask" && r.is_err()));
}

#[test]
fn global_busy_and_revision_change_cancel_authorization() {
    let (mut m, f, mut s) = configured(false);
    ask(&f, "runtime:a", "host.ask_agent", params(&s, false));
    ask(&f, "runtime:b", "host.ask_agent", params(&s, false));
    m.poll(true);
    assert_eq!(f.replies()[0].1.as_ref().unwrap_err().code, "busy");
    s.revision += 1;
    f.emit(Event::StateChanged(s));
    m.poll(true);
    assert!(!m.consent_pending());
    assert!(f
        .replies()
        .iter()
        .any(|(id, r)| id == "runtime:a" && r.is_err()));
}

#[test]
fn capture_cancellation_ack_and_close_wait_for_actual_cleanup() {
    let (mut m, f, s) = configured(true);
    ask(
        &f,
        "runtime:capture",
        "host.capture_region",
        params(&s, true),
    );
    m.poll(false);
    assert!(m.capture_waiting());
    let (worker, control) = CaptureHandle::fake(f.live(), egui::Context::default());
    m.boards[0].host.task.as_mut().unwrap().phase = Phase::Capture(worker);
    ask(
        &f,
        "runtime:cancel",
        "jobs.cancel",
        json!({"request_id":"runtime:capture","job_id":"job-1"}),
    );
    m.poll(false);
    assert!(control.is_cancelled());
    assert!(m.capture_active());
    assert!(f.replies().is_empty());
    m.begin_close();
    m.poll(false);
    assert!(!f.sent().iter().any(|r| r.1 == "close"));
    control.finish(Ok(b"\x89PNG\r\n\x1a\n".to_vec()));
    m.poll(false);
    assert!(!m.capture_active());
    assert!(f
        .replies()
        .iter()
        .any(|(id, r)| id == "runtime:capture" && r.is_err()));
    assert!(f
        .replies()
        .iter()
        .any(|(id, r)| id == "runtime:cancel" && r.as_ref().unwrap()["cancelled"] == true));
    assert!(f.sent().iter().any(|r| r.1 == "close"));
    assert!(m.boards[0].host.resources.assets.is_empty());
}

#[test]
fn capture_publish_rechecks_live_and_hidden_state_and_retains_session_on_eof() {
    for disconnect in [false, true] {
        let (mut m, f, mut s) = configured(true);
        ask(&f, "runtime:c", "host.capture_region", params(&s, true));
        m.poll(false);
        let (w, control) = CaptureHandle::fake(f.live(), egui::Context::default());
        m.boards[0].host.task.as_mut().unwrap().phase = Phase::Capture(w);
        if disconnect {
            f.emit(Event::Exited);
        } else {
            s.hidden_confirmed = false;
            s.visible = true;
            f.emit(Event::StateChanged(s));
        }
        m.poll(false);
        assert!(m.capture_active());
        assert!(control.is_cancelled());
        control.finish(Ok(b"\x89PNG\r\n\x1a\n".to_vec()));
        m.poll(false);
        assert!(!m.capture_active());
        if disconnect {
            assert!(m.closed());
        } else {
            assert!(f.replies().last().unwrap().1.is_err());
        }
    }
}

#[test]
fn single_board_capture_serves_resource_until_release_and_rejects_multiple_boards() {
    let (mut m, f, s) = configured(true);
    ask(&f, "runtime:c", "host.capture_region", params(&s, true));
    m.poll(false);
    let (w, control) =
        CaptureHandle::fake(Arc::new(AtomicBool::new(true)), egui::Context::default());
    m.boards[0].host.task.as_mut().unwrap().phase = Phase::Capture(w);
    control.finish(Ok(b"\x89PNG\r\n\x1a\n".to_vec()));
    m.poll(false);
    let descriptor = f.replies().last().unwrap().1.clone().unwrap();
    ask(
        &f,
        "runtime:read",
        "resources.read",
        json!({"asset_ref":descriptor["asset_ref"],"offset":0,"length":8192}),
    );
    m.poll(false);
    assert_eq!(
        f.replies().last().unwrap().1.as_ref().unwrap()["total_bytes"],
        8
    );
    attach(&mut m, BoardKind::Blackboard, 100);
    ask(&f, "runtime:multi", "host.capture_region", params(&s, true));
    m.poll(false);
    assert_eq!(
        f.replies().last().unwrap().1.as_ref().unwrap_err().code,
        "busy"
    );
}

fn large_capture_png() -> Vec<u8> {
    use image::ImageEncoder;
    let mut seed = 123456789u32;
    let pixels: Vec<u8> = (0..128 * 128 * 4)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as u8
        })
        .collect();
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(&pixels, 128, 128, image::ExtendedColorType::Rgba8)
        .unwrap();
    assert!(png.len() > 8192);
    png
}

fn complete_capture(m: &mut DrawingManager, f: &Fake, s: &State, id: &str, png: Vec<u8>) {
    ask(f, id, "host.capture_region", params(s, true));
    m.poll(false);
    let (worker, control) = CaptureHandle::fake(f.live(), egui::Context::default());
    m.boards[0].host.task.as_mut().unwrap().phase = Phase::Capture(worker);
    control.finish(Ok(png));
    m.poll(false);
}

#[test]
fn completed_capture_survives_window_restore_and_downloads_all_chunks_then_releases() {
    for restore_before_write in [false, true] {
        let (mut m, f, mut s) = configured(true);
        let png = large_capture_png();
        if restore_before_write {
            f.defer_guarded();
        }
        complete_capture(&mut m, &f, &s, "runtime:large", png.clone());
        s.hidden_confirmed = false;
        s.visible = true;
        s.effective_visible = true;
        if restore_before_write {
            f.emit(Event::StateChanged(s.clone()));
            m.poll(false);
            assert!(
                f.queued_guard().load(Ordering::Acquire),
                "completed PNG needs no hide lease"
            );
            f.flush_guarded();
        }
        let descriptor = f.replies().last().unwrap().1.clone().unwrap();
        let mut downloaded = Vec::new();
        while downloaded.len() < png.len() {
            let offset = downloaded.len();
            ask(
                &f,
                &format!("runtime:chunk-{offset}"),
                "resources.read",
                json!({"asset_ref":descriptor["asset_ref"],"offset":offset,"length":8192}),
            );
            // Actual runtime order: descriptor, window restoration, read, state_changed.
            f.emit(Event::StateChanged(s.clone()));
            m.poll(false);
            let chunk = f.replies().last().unwrap().1.clone().unwrap();
            assert_eq!(chunk["offset"], offset);
            assert_eq!(chunk["total_bytes"], png.len());
            downloaded.extend(
                chunk["bytes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_u64().unwrap() as u8),
            );
            assert_eq!(chunk["next_offset"], downloaded.len());
            assert_eq!(chunk["eof"], downloaded.len() == png.len());
        }
        assert_eq!(downloaded, png);
        assert_eq!(descriptor["crc32"], crc32(&downloaded));
        ask(
            &f,
            "runtime:release-large",
            "resources.release",
            json!({"asset_ref":descriptor["asset_ref"]}),
        );
        m.poll(false);
        assert_eq!(
            f.replies().last().unwrap().1.as_ref().unwrap()["released"],
            true
        );
        assert!(m.boards[0].host.resources.assets.is_empty());
    }
}

#[test]
fn old_invalid_capture_context_and_old_cancel_cannot_delete_new_capture_asset() {
    let (mut m, f, mut s) = configured(true);
    let png = large_capture_png();
    complete_capture(&mut m, &f, &s, "runtime:old-capture", png.clone());
    let old = f.replies().last().unwrap().1.clone().unwrap();
    s.revision += 1;
    f.emit(Event::StateChanged(s.clone()));
    m.poll(false);
    assert!(!m.boards[0]
        .host
        .resources
        .assets
        .contains_key(old["asset_ref"].as_str().unwrap()));
    assert!(!m.boards[0].host.reply_contexts["runtime:old-capture"]
        .guard
        .load(Ordering::Acquire));
    complete_capture(&mut m, &f, &s, "runtime:new-capture", png.clone());
    let new = f.replies().last().unwrap().1.clone().unwrap();
    for _ in 0..3 {
        m.poll(false);
    }
    ask(
        &f,
        "runtime:cancel-old-capture",
        "jobs.cancel",
        json!({"request_id":"runtime:old-capture","job_id":"job-1"}),
    );
    m.poll(false);
    assert_eq!(m.boards[0].host.resources.assets.len(), 1);
    ask(
        &f,
        "runtime:read-new-capture",
        "resources.read",
        json!({"asset_ref":new["asset_ref"],"offset":8192,"length":8192}),
    );
    m.poll(false);
    let chunk = f.replies().last().unwrap().1.clone().unwrap();
    assert_eq!(chunk["bytes"], json!(&png[8192..16384]));
    assert!(m.boards[0].host.reply_contexts["runtime:new-capture"]
        .guard
        .load(Ordering::Acquire));
    ask(
        &f,
        "runtime:cancel-new-capture",
        "jobs.cancel",
        json!({"request_id":"runtime:new-capture","job_id":"job-1"}),
    );
    m.poll(false);
    assert!(m.boards[0].host.resources.assets.is_empty());
}

#[test]
fn fake_agent_direct_text_writeback_and_cancelled_late_result() {
    for cancel in [false, true] {
        let (mut m, f, s) = configured(false);
        let mut p = params(&s, false);
        p["asset_refs"] = json!([]);
        ask(&f, "runtime:agent", "host.ask_agent", p);
        m.poll(true);
        let host = &mut m.boards[0].host;
        host.consent().unwrap().write_back = true;
        host.authorize(true);
        let done = Arc::new(AtomicBool::new(false));
        let answer = json!({"answer":"actual answer","operations":[{"op":"add","object":{
            "id":"neo-test-text","kind":{"type":"text","position":{"x":80,"y":80},
            "text":"actual answer","size":26,"color":{"r":32,"g":32,"b":32,"a":255}}}}]});
        host.task.as_mut().unwrap().phase = Phase::FakeAgent {
            finished: done.clone(),
            result: Ok(answer.clone()),
        };
        if cancel {
            ask(
                &f,
                "runtime:stop",
                "jobs.cancel",
                json!({"request_id":"runtime:agent","job_id":"job-1"}),
            );
        }
        m.poll(true);
        assert!(f.replies().is_empty());
        done.store(true, Ordering::Release);
        m.poll(true);
        let replies = f.replies();
        let final_reply = &replies
            .iter()
            .find(|(id, _)| id == "runtime:agent")
            .unwrap()
            .1;
        if cancel {
            assert!(final_reply.is_err());
            assert_eq!(replies.len(), 2);
        } else {
            assert_eq!(final_reply.as_ref().unwrap(), &answer);
        }
        assert!(!m.boards[0].host.busy());
    }
}

#[test]
fn sixty_second_timeout_waits_for_capture_cleanup_and_configuration_change_revokes_consent() {
    let (mut m, f, s) = configured(true);
    ask(
        &f,
        "runtime:timeout",
        "host.capture_region",
        params(&s, true),
    );
    m.poll(false);
    let (w, control) = CaptureHandle::fake(f.live(), egui::Context::default());
    let t = m.boards[0].host.task.as_mut().unwrap();
    t.phase = Phase::Capture(w);
    t.deadline = Instant::now();
    m.poll(false);
    assert!(m.capture_active());
    assert!(control.is_cancelled());
    assert!(f.replies().is_empty());
    control.finish(Err("timeout".into()));
    m.poll(false);
    assert_eq!(
        f.replies().last().unwrap().1.as_ref().unwrap_err().code,
        "timeout"
    );

    let (mut m, f, s) = configured(false);
    ask(&f, "runtime:ask", "host.ask_agent", params(&s, false));
    m.poll(true);
    let mut cfg = neo_llm::Config::deepseek("test-only");
    cfg.model = "different-model".into();
    m.set_services(cfg, &egui::Context::default());
    m.poll(true);
    assert!(!m.consent_pending());
    assert!(f.replies().last().unwrap().1.is_err());
    assert!(!f.sent().iter().any(|r| r.1 == "resources.read"));
}

#[test]
fn queued_success_is_revoked_under_writer_backpressure() {
    let (mut m, f, mut s) = configured(false);
    let mut p = params(&s, false);
    p["asset_refs"] = json!([]);
    ask(&f, "runtime:queued", "host.ask_agent", p);
    m.poll(true);
    m.boards[0].host.task.as_mut().unwrap().phase = Phase::FakeAgent {
        finished: Arc::new(AtomicBool::new(true)),
        result: Ok(json!({"answer":"must not arrive"})),
    };
    f.block_replies(true);
    m.poll(true);
    assert!(f.replies().is_empty());
    assert!(m.boards[0].host.busy());
    s.revision += 1;
    f.emit(Event::StateChanged(s));
    m.poll(true);
    f.block_replies(false);
    m.poll(true);
    assert!(f.replies().last().unwrap().1.is_err());
    assert!(!m.boards[0].host.busy());
}

#[test]
fn old_cancel_is_idempotent_without_cancelling_a_new_request() {
    let (mut m, f, s) = configured(false);
    let mut p = params(&s, false);
    p["asset_refs"] = json!([]);
    ask(&f, "runtime:old", "host.ask_agent", p.clone());
    m.poll(true);
    m.boards[0].host.authorize(false);
    m.poll(true);
    p["job_id"] = json!("job-2");
    ask(&f, "runtime:new", "host.ask_agent", p);
    m.poll(true);
    ask(
        &f,
        "runtime:cancel-old",
        "jobs.cancel",
        json!({"request_id":"runtime:old","job_id":"job-1"}),
    );
    m.poll(true);
    assert!(m.consent_pending());
    assert_eq!(
        f.replies().last().unwrap().1.as_ref().unwrap()["cancelled"],
        true
    );
}

#[test]
fn capture_host_request_precedes_hidden_state_and_early_ui_permit_cannot_start() {
    for same_poll in [false, true] {
        let (mut m, f, hidden) = configured(true);
        let mut visible = hidden.clone();
        visible.hidden_confirmed = false;
        visible.visible = true;
        visible.effective_visible = true;
        f.emit(Event::StateChanged(visible));
        m.poll(false);
        ask(
            &f,
            "runtime:ordered",
            "host.capture_region",
            params(&hidden, true),
        );
        if same_poll {
            f.emit(Event::StateChanged(hidden.clone()));
        }
        m.poll(false);
        assert!(m.capture_active());
        assert!(f.replies().is_empty());
        let (worker, control) = CaptureHandle::fake(f.live(), egui::Context::default());
        m.inject_capture(worker);
        if !same_poll {
            assert!(
                !m.capture_waiting(),
                "UI barrier must wait for runtime hidden state"
            );
            m.capture_permitted(Ok(()), &egui::Context::default());
            assert!(matches!(
                m.boards[0].host.task.as_ref().unwrap().phase,
                Phase::CaptureWait
            ));
            assert!(
                m.boards[0].host.fake_capture.is_some(),
                "early permit cannot consume worker"
            );
            m.boards[0].next_state = Instant::now();
            m.poll(false);
            assert_eq!(f.sent().last().unwrap().1, "get_state");
            f.respond(hidden);
            m.poll(false);
        }
        assert!(m.capture_waiting());
        m.capture_permitted(Ok(()), &egui::Context::default());
        assert!(matches!(
            m.boards[0].host.task.as_ref().unwrap().phase,
            Phase::Capture(_)
        ));
        control.finish(Ok(b"\x89PNG\r\n\x1a\n".to_vec()));
        m.poll(false);
        assert!(f.replies().last().unwrap().1.is_ok());
    }
}

#[test]
fn capture_pending_hidden_state_times_out_and_context_still_invalidates() {
    for expire in [false, true] {
        let (mut m, f, mut s) = configured(true);
        s.hidden_confirmed = false;
        s.visible = true;
        s.effective_visible = true;
        f.emit(Event::StateChanged(s.clone()));
        ask(
            &f,
            "runtime:pending",
            "host.capture_region",
            params(&s, true),
        );
        m.poll(false);
        assert!(m.capture_active());
        assert!(!m.capture_waiting());
        if expire {
            m.boards[0].host.task.as_mut().unwrap().deadline = Instant::now();
        } else {
            s.revision += 1;
            f.emit(Event::StateChanged(s));
        }
        m.poll(false);
        assert!(!m.capture_active());
        assert_eq!(
            f.replies().last().unwrap().1.as_ref().unwrap_err().code,
            if expire { "timeout" } else { "cancelled" }
        );
    }
}

#[test]
fn capture_permit_rechecks_hidden_state_after_ui_barrier() {
    let (mut m, f, s) = configured(true);
    ask(
        &f,
        "runtime:permit",
        "host.capture_region",
        params(&s, true),
    );
    m.poll(false);
    let (worker, _) = CaptureHandle::fake(f.live(), egui::Context::default());
    m.inject_capture(worker);
    assert!(m.capture_waiting());
    // Simulate state already consumed by a different UI hook before this permit.
    m.boards[0].state.as_mut().unwrap().hidden_confirmed = false;
    m.capture_permitted(Ok(()), &egui::Context::default());
    assert!(m.boards[0].host.fake_capture.is_some());
    assert!(matches!(
        m.boards[0].host.task.as_ref().unwrap().phase,
        Phase::CaptureWait
    ));
}

#[test]
fn enqueued_reply_guard_survives_successful_enqueue_and_revokes_before_write() {
    for change in [
        "credentials",
        "model",
        "revision",
        "page",
        "permissions",
        "cancel",
        "expiry",
    ] {
        let (mut m, f, mut s) = configured(false);
        let mut p = params(&s, false);
        p["asset_refs"] = json!([]);
        ask(&f, "runtime:enqueued", "host.ask_agent", p);
        m.poll(true);
        m.boards[0].host.task.as_mut().unwrap().phase = Phase::FakeAgent {
            finished: Arc::new(AtomicBool::new(true)),
            result: Ok(json!({"answer":"not yet written"})),
        };
        f.defer_guarded();
        m.poll(true);
        assert!(!m.boards[0].host.busy(), "enqueue succeeded");
        let guard = f.queued_guard();
        assert!(guard.load(Ordering::Acquire));
        assert!(f.replies().is_empty());
        match change {
            "credentials" | "model" => {
                let mut cfg = neo_llm::Config::deepseek("test-only");
                if change == "credentials" {
                    cfg.api_key = "changed-key".into();
                } else {
                    cfg.model = "changed-model".into();
                }
                m.set_services(cfg, &egui::Context::default());
            }
            "revision" => {
                s.revision += 1;
                f.emit(Event::StateChanged(s));
            }
            "page" => {
                s.page_id = "other-page".into();
                f.emit(Event::StateChanged(s));
            }
            "permissions" => {
                s.permissions = Permissions::restricted(true);
                f.emit(Event::StateChanged(s));
            }
            "cancel" => ask(
                &f,
                "runtime:cancel-enqueued",
                "jobs.cancel",
                json!({"request_id":"runtime:enqueued","job_id":"job-1"}),
            ),
            "expiry" => {
                m.boards[0]
                    .host
                    .reply_contexts
                    .get_mut("runtime:enqueued")
                    .unwrap()
                    .expires = Instant::now()
            }
            _ => unreachable!(),
        }
        m.poll(true);
        assert!(!guard.load(Ordering::Acquire), "{change}");
        f.flush_guarded();
        let replies = f.replies();
        let reply = &replies
            .iter()
            .find(|(id, _)| id == "runtime:enqueued")
            .unwrap()
            .1;
        assert_eq!(reply.as_ref().unwrap_err().code, "cancelled", "{change}");
    }
}

#[test]
fn unknown_method_and_unknown_cancel_are_errors_not_hangs() {
    let (mut m, f, _) = configured(false);
    ask(&f, "runtime:x", "host.unknown", json!({}));
    ask(
        &f,
        "runtime:y",
        "jobs.cancel",
        json!({"request_id":"runtime:none","job_id":"j"}),
    );
    m.poll(true);
    assert_eq!(f.replies().len(), 2);
    assert!(f.replies().iter().all(|(_, r)| r.is_err()));
}

#[test]
fn denied_or_cancelled_consent_cannot_be_reauthorized_to_read_objects() {
    for cancel in [false, true] {
        let (mut m, f, s) = configured(false);
        let mut p = params(&s, false);
        p["asset_refs"] = json!([]);
        ask(&f, "runtime:edit", "host.ask_agent", p);
        m.poll(true);
        let host = &mut m.boards[0].host;
        host.agent_requests = Some(Vec::new());
        let c = host.consent().unwrap();
        c.write_back = true;
        c.structured_edit = true;
        if cancel {
            host.cancel();
        } else {
            host.authorize(false);
        }
        host.authorize(true);
        assert_edit_failed(&mut m, &f);
        assert!(!f
            .sent()
            .iter()
            .any(|r| matches!(r.1.as_str(), "resources.read" | "objects.list")));
    }
}

#[test]
fn snapshot_byte_budget_counts_array_overhead_and_allows_exact_limit() {
    for extra in [0, 1] {
        let mut snapshot = Snapshot {
            objects: vec![],
            offset: 0,
            total: None,
            bytes: 2,
            reading: None,
        };
        let mut image = json!({"id":"image","kind":{"type":"image","asset_ref":""}});
        let overhead = serde_json::to_vec(&vec![image.clone()]).unwrap().len();
        image["kind"]["asset_ref"] = json!("x".repeat(MAX_SNAPSHOT_BYTES - overhead + extra));
        // Split into normal wire-sized pages to check aggregate accounting.
        let content = image["kind"]["asset_ref"].as_str().unwrap().len();
        let per_object =
            serde_json::to_vec(&json!({"id":"image","kind":{"type":"image","asset_ref":""}}))
                .unwrap()
                .len();
        let mut remaining = content - 3 * (per_object + 1);
        for offset in 0..4 {
            let length = remaining.min(35 * 1024);
            remaining -= length;
            image["kind"]["asset_ref"] = json!("x".repeat(length));
            let result = snapshot.accept(
                object_page(&state(false, false, true), offset, 4, vec![image.clone()]),
                &state(false, false, true),
            );
            if offset == 3 && extra == 1 {
                assert_eq!(result.unwrap_err().code, "resource_limit");
            } else {
                result.unwrap();
            }
        }
        assert_eq!(snapshot.bytes, MAX_SNAPSHOT_BYTES + extra);
        assert!(snapshot.objects.is_empty());
        assert_eq!(snapshot.complete(), extra == 0);
    }
}

fn oversized_object(s: &State, offset: usize, id: &str, total: usize) -> RpcError {
    RpcError {
        code: "object_too_large".into(),
        message: "Object cannot fit the requested page".into(),
        data: Some(json!({"document_id":s.document_id,"page_id":s.page_id,
            "revision":s.revision,"object_id":id,"offset":offset,
            "total_bytes":total,"next_offset":offset + 1})),
    }
}
fn object_chunk(s: &State, id: &str, bytes: &[u8], offset: usize) -> Value {
    let next = (offset + OBJECT_CHUNK_BYTES).min(bytes.len());
    json!({"document_id":s.document_id,"page_id":s.page_id,"revision":s.revision,
        "object_id":id,"offset":offset,"total_bytes":bytes.len(),
        "encoding":"utf8_json_u8_array","bytes":&bytes[offset..next],
        "next_offset":next,"eof":next == bytes.len()})
}
fn respond_chunk(f: &Fake, result: Reply) {
    let (id, method, _) = f
        .sent()
        .into_iter()
        .rev()
        .find(|r| r.1 == "objects.read")
        .unwrap();
    f.emit(Event::Response { id, method, result });
}
fn download_object(m: &mut DrawingManager, f: &Fake, s: &State, object: &Value) {
    let bytes = serde_json::to_vec(object).unwrap();
    let object_id = object["id"].as_str().unwrap();
    for offset in (0..bytes.len()).step_by(OBJECT_CHUNK_BYTES) {
        let (_, method, p) = f.sent().last().unwrap().clone();
        assert_eq!(method, "objects.read");
        assert_eq!(
            p,
            json!({"document_id":s.document_id,"page_id":s.page_id,
            "expected_revision":s.revision,"object_id":object_id,
            "offset":offset,"length":8192})
        );
        respond_chunk(f, Ok(object_chunk(s, object_id, &bytes, offset)));
        m.poll(true);
    }
}

#[test]
fn oversized_text_falls_back_to_chunks_then_resumes_unknown_total_list() {
    for content in ["x".repeat(60 * 1024), "中文边界".repeat(7000)] {
        let (mut m, f, s) = begin_edit();
        let mut object = edit_text("大对象");
        object["kind"]["text"] = json!(content);
        let bytes = serde_json::to_vec(&object).unwrap();
        assert!(bytes.len() > OBJECT_PAGE_BYTES && bytes.len() < MAX_SNAPSHOT_BYTES);
        if bytes.len() > 65536 {
            assert!(
                bytes.chunks(8192).any(|c| std::str::from_utf8(c).is_err()),
                "fixture must split multibyte UTF-8 across chunks"
            );
        }
        respond_objects(&f, Err(oversized_object(&s, 0, "大对象", bytes.len())));
        m.poll(true);
        download_object(&mut m, &f, &s, &object);
        assert!(m.boards[0].host.agent_requests.as_ref().unwrap().is_empty());
        let Phase::Reading(d) = &m.boards[0].host.task.as_ref().unwrap().phase else {
            panic!()
        };
        let snapshot = d.snapshot.as_ref().unwrap();
        assert_eq!(snapshot.total, None);
        assert_eq!(snapshot.offset, 1);
        assert!(snapshot.reading.is_none());
        assert_eq!(f.sent().last().unwrap().1, "objects.list");
        assert_eq!(f.sent().last().unwrap().2["offset"], 1);
        respond_objects(&f, Ok(object_page(&s, 1, 2, vec![edit_text("after")])));
        m.poll(true);
        let requests = m.boards[0].host.agent_requests.as_ref().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].edit_objects,
            Some(vec![object, edit_text("after")])
        );
        assert!(requests[0].images.is_empty());
        assert_eq!(
            f.sent().iter().filter(|r| r.1 == "objects.read").count(),
            bytes.len().div_ceil(8192)
        );
        assert!(!f.sent().iter().any(|r| r.1 == "resources.read"));
    }
}

#[test]
fn chunked_snapshot_accepts_exact_budget_and_rejects_one_extra_byte_before_read() {
    for extra in [0, 1] {
        let (mut m, f, s) = begin_edit();
        let mut object = edit_text("boundary");
        object["kind"]["text"] = json!("");
        let overhead = serde_json::to_vec(&object).unwrap().len() + 2;
        object["kind"]["text"] = json!("x".repeat(MAX_SNAPSHOT_BYTES - overhead + extra));
        let size = serde_json::to_vec(&object).unwrap().len();
        respond_objects(&f, Err(oversized_object(&s, 0, "boundary", size)));
        m.poll(true);
        if extra == 1 {
            assert_edit_failed(&mut m, &f);
            assert!(!f.sent().iter().any(|r| r.1 == "objects.read"));
        } else {
            download_object(&mut m, &f, &s, &object);
            respond_objects(&f, Ok(object_page(&s, 1, 1, vec![])));
            m.poll(true);
            assert_eq!(
                m.boards[0].host.agent_requests.as_ref().unwrap()[0].edit_objects,
                Some(vec![object])
            );
            assert_eq!(
                f.sent().iter().filter(|r| r.1 == "objects.read").count(),
                16
            );
        }
    }
}

#[test]
fn fallback_keeps_list_total_consistent_and_chunk_request_ids_independent() {
    let (mut m, f, s) = begin_edit();
    respond_objects(&f, Ok(object_page(&s, 0, 3, vec![edit_text("first")])));
    m.poll(true);
    let mut object = edit_text("large");
    object["kind"]["text"] = json!("x".repeat(60 * 1024));
    let bytes = serde_json::to_vec(&object).unwrap();
    respond_objects(&f, Err(oversized_object(&s, 1, "large", bytes.len())));
    m.poll(true);
    let read_id = f.sent().last().unwrap().0.clone();
    m.boards[0].next_state = Instant::now();
    m.poll(true);
    let window_id = m.boards[0].pending.as_ref().unwrap().id.clone();
    assert!(!m.boards[0]
        .host
        .response(&window_id, Ok(object_chunk(&s, "large", &bytes, 0))));
    respond_chunk(&f, Ok(object_chunk(&s, "large", &bytes, 0)));
    m.poll(true);
    assert!(!m.boards[0]
        .host
        .response(&read_id, Ok(object_chunk(&s, "large", &bytes, 0))));
    for offset in (8192..bytes.len()).step_by(8192) {
        respond_chunk(&f, Ok(object_chunk(&s, "large", &bytes, offset)));
        m.poll(true);
    }
    assert_eq!(m.boards[0].pending.as_ref().unwrap().id, window_id);
    assert_eq!(f.sent().last().unwrap().1, "objects.list");
    assert_eq!(f.sent().last().unwrap().2["offset"], 2);
    respond_objects(&f, Ok(object_page(&s, 2, 2, vec![])));
    assert_edit_failed(&mut m, &f);
}

#[test]
fn chunked_images_count_budget_but_never_send_metadata_or_fetch_assets() {
    let (mut m, f, s) = begin_edit();
    let image = json!({"id":"private-image","kind":{"type":"image",
        "asset_ref":"asset:private","metadata":"secret".repeat(10000)}});
    let bytes = serde_json::to_vec(&image).unwrap();
    respond_objects(
        &f,
        Err(oversized_object(&s, 0, "private-image", bytes.len())),
    );
    m.poll(true);
    download_object(&mut m, &f, &s, &image);
    let Phase::Reading(d) = &m.boards[0].host.task.as_ref().unwrap().phase else {
        panic!()
    };
    assert_eq!(d.snapshot.as_ref().unwrap().bytes, bytes.len() + 2);
    assert!(d.snapshot.as_ref().unwrap().objects.is_empty());
    respond_objects(&f, Ok(object_page(&s, 1, 1, vec![])));
    m.poll(true);
    let requests = m.boards[0].host.agent_requests.as_ref().unwrap();
    assert_eq!(requests[0].edit_objects, Some(vec![]));
    assert!(requests[0].images.is_empty());
    assert!(!f.sent().iter().any(|r| r.1 == "resources.read"));
}

#[test]
fn invalid_oversize_descriptors_fail_without_reads_or_skipping_objects() {
    for (key, bad) in [
        ("document_id", json!("other")),
        ("page_id", json!("other")),
        ("revision", json!(8)),
        ("object_id", json!(" ")),
        ("object_id", json!(null)),
        ("offset", json!(1)),
        ("offset", json!(-1)),
        ("next_offset", json!(0)),
        ("next_offset", json!(2)),
        ("next_offset", json!(null)),
        ("total_bytes", json!(0)),
        ("total_bytes", json!("60000")),
        ("total_bytes", json!(MAX_SNAPSHOT_BYTES)),
        ("total_bytes", json!(u64::MAX)),
    ] {
        let (mut m, f, s) = begin_edit();
        let mut e = oversized_object(&s, 0, "large", 60 * 1024);
        e.data.as_mut().unwrap()[key] = bad;
        respond_objects(&f, Err(e));
        assert_edit_failed(&mut m, &f);
        assert!(!f.sent().iter().any(|r| r.1 == "objects.read"), "{key}");
        assert_eq!(f.sent().iter().filter(|r| r.1 == "objects.list").count(), 1);
    }
    // Remaining budget, not just the single-object limit, governs admission.
    let (mut m, f, s) = begin_edit();
    let mut first = edit_text("first");
    first["kind"]["text"] = json!("x".repeat(40 * 1024));
    respond_objects(&f, Ok(object_page(&s, 0, 2, vec![first])));
    m.poll(true);
    respond_objects(&f, Err(oversized_object(&s, 1, "large", 100 * 1024)));
    assert_edit_failed(&mut m, &f);
    assert!(!f.sent().iter().any(|r| r.1 == "objects.read"));
}

#[test]
fn object_chunks_reject_bad_context_progress_encoding_and_byte_values() {
    let bytes = vec![b'x'; 60 * 1024];
    for (key, bad) in [
        ("document_id", json!("other")),
        ("page_id", json!("other")),
        ("revision", json!(8)),
        ("object_id", json!("wrong")),
        ("offset", json!(1)),
        ("next_offset", json!(0)),
        ("next_offset", json!(8193)),
        ("total_bytes", json!(bytes.len() + 1)),
        ("encoding", json!("utf8")),
        ("eof", json!(true)),
        ("bytes", json!([])),
        ("bytes", json!([1])),
        ("bytes", json!(vec![1; 8193])),
        ("bytes", json!(vec![256; 8192])),
    ] {
        let (mut m, f, s) = begin_edit();
        respond_objects(&f, Err(oversized_object(&s, 0, "large", bytes.len())));
        m.poll(true);
        let mut chunk = object_chunk(&s, "large", &bytes, 0);
        chunk[key] = bad;
        respond_chunk(&f, Ok(chunk));
        assert_edit_failed(&mut m, &f);
        assert_eq!(
            f.sent().iter().filter(|r| r.1 == "objects.read").count(),
            1,
            "{key}"
        );
    }
}

#[test]
fn chunk_reassembly_rejects_wrong_object_id_invalid_json_and_utf8() {
    for bytes in [
        serde_json::to_vec(&edit_text("wrong")).unwrap(),
        b"{invalid json}".to_vec(),
        vec![0xff],
        b"{} {}".to_vec(),
    ] {
        let (mut m, f, s) = begin_edit();
        respond_objects(&f, Err(oversized_object(&s, 0, "expected", bytes.len())));
        m.poll(true);
        respond_chunk(&f, Ok(object_chunk(&s, "expected", &bytes, 0)));
        assert_edit_failed(&mut m, &f);
    }
    // Read errors must not recursively begin another fallback or skip the object.
    let (mut m, f, s) = begin_edit();
    respond_objects(&f, Err(oversized_object(&s, 0, "large", 60000)));
    m.poll(true);
    respond_chunk(&f, Err(oversized_object(&s, 0, "another", 60000)));
    assert_edit_failed(&mut m, &f);
    assert_eq!(f.sent().iter().filter(|r| r.1 == "objects.read").count(), 1);
}

#[test]
fn cancellation_revision_and_timeout_discard_late_object_chunks() {
    for reason in ["cancel", "revision", "timeout"] {
        let (mut m, f, mut s) = begin_edit();
        let bytes = vec![b'x'; 60000];
        respond_objects(&f, Err(oversized_object(&s, 0, "large", bytes.len())));
        m.poll(true);
        respond_chunk(&f, Ok(object_chunk(&s, "large", &bytes, 0)));
        m.poll(true);
        let (id, _, _) = f.sent().last().unwrap().clone();
        let late = object_chunk(&s, "large", &bytes, 8192);
        let host = &mut m.boards[0].host;
        match reason {
            "cancel" => host.cancel(),
            "revision" => {
                s.revision += 1;
                host.check(Some(&s), s.permissions, true);
            }
            _ => {
                host.task.as_mut().unwrap().deadline = Instant::now();
                host.check(Some(&s), s.permissions, true);
            }
        }
        assert!(host.response(&id, Ok(late.clone())));
        assert_edit_failed(&mut m, &f);
        assert!(!m.boards[0].host.response(&id, Ok(late)));
        assert_eq!(f.sent().iter().filter(|r| r.1 == "objects.read").count(), 2);
        assert_eq!(f.sent().iter().filter(|r| r.1 == "objects.list").count(), 1);
    }
}

#[test]
fn consecutive_fallbacks_preserve_known_total_and_enforce_object_count() {
    let (mut m, f, s) = begin_edit();
    respond_objects(&f, Ok(object_page(&s, 0, 3, vec![edit_text("first")])));
    m.poll(true);
    for offset in 1..3 {
        let mut object = edit_text(&format!("large-{offset}"));
        object["kind"]["text"] = json!("x".repeat(50 * 1024));
        respond_objects(
            &f,
            Err(oversized_object(
                &s,
                offset,
                object["id"].as_str().unwrap(),
                serde_json::to_vec(&object).unwrap().len(),
            )),
        );
        m.poll(true);
        download_object(&mut m, &f, &s, &object);
    }
    let requests = m.boards[0].host.agent_requests.as_ref().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].edit_objects.as_ref().unwrap().len(), 3);
    // Unknown totals still cannot permit a 257th fallback (images count too).
    let (mut m, f, s) = begin_edit();
    let Phase::Reading(d) = &mut m.boards[0].host.task.as_mut().unwrap().phase else {
        panic!()
    };
    d.snapshot.as_mut().unwrap().offset = MAX_SNAPSHOT_OBJECTS;
    respond_objects(
        &f,
        Err(oversized_object(&s, MAX_SNAPSHOT_OBJECTS, "extra", 60000)),
    );
    assert_edit_failed(&mut m, &f);
    assert!(!f.sent().iter().any(|r| r.1 == "objects.read"));
}

fn edit_text(id: &str) -> Value {
    json!({"id":id,"kind":{"type":"text","position":{"x":10,"y":20},
        "text":"page text","size":24,"color":{"r":0,"g":0,"b":0,"a":255}}})
}
fn object_page(s: &State, offset: usize, total: usize, objects: Vec<Value>) -> Value {
    let next = offset + objects.len();
    json!({"document_id":s.document_id,"page_id":s.page_id,"revision":s.revision,
        "offset":offset,"total":total,"objects":objects,
        "next_offset":if next < total { Some(next) } else { None }})
}
fn respond_objects(f: &Fake, result: Reply) {
    let (id, method, _) = f
        .sent()
        .into_iter()
        .rev()
        .find(|r| r.1 == "objects.list")
        .unwrap();
    f.emit(Event::Response { id, method, result });
}
fn begin_edit() -> (DrawingManager, Fake, State) {
    let (mut m, f, s) = configured(false);
    let mut p = params(&s, false);
    p["asset_refs"] = json!([]);
    ask(&f, "runtime:edit", "host.ask_agent", p);
    m.poll(true);
    let host = &mut m.boards[0].host;
    host.agent_requests = Some(Vec::new());
    let c = host.consent().unwrap();
    c.write_back = true;
    c.structured_edit = true;
    host.authorize(true);
    m.poll(true);
    (m, f, s)
}
fn assert_edit_failed(m: &mut DrawingManager, f: &Fake) {
    m.poll(true);
    assert!(m.boards[0].host.agent_requests.as_ref().unwrap().is_empty());
    assert!(!m.boards[0].host.busy());
    assert!(f
        .replies()
        .iter()
        .any(|(id, r)| id == "runtime:edit" && r.is_err()));
}

#[test]
fn each_agent_mode_requires_local_consent_and_edit_requires_both_write_flags() {
    for (allowed, write, edit) in [
        (false, false, false),
        (true, true, false),
        (true, true, true),
        (false, true, true),
        (true, false, true),
    ] {
        let (mut m, f, s) = configured(false);
        let mut p = params(&s, false);
        p["asset_refs"] = json!([]);
        p["write_back"] = json!(allowed);
        // Runtime flags cannot substitute for Neo's independent UI consent.
        p["structured_edit"] = json!(true);
        ask(&f, "runtime:mode", "host.ask_agent", p);
        m.poll(true);
        let sent = f.sent().len();
        let host = &mut m.boards[0].host;
        host.agent_requests = Some(Vec::new());
        let c = host.consent().unwrap();
        assert!(!c.structured_edit && !c.write_back);
        c.write_back = write;
        c.structured_edit = edit;
        m.poll(true);
        assert_eq!(f.sent().len(), sent);
        assert!(m.boards[0].host.agent_requests.as_ref().unwrap().is_empty());
        m.boards[0].host.authorize(true);
        m.poll(true);
        let editing = allowed && write && edit;
        assert_eq!(f.sent().iter().any(|r| r.1 == "objects.list"), editing);
        if editing {
            respond_objects(&f, Ok(object_page(&s, 0, 0, vec![])));
            m.poll(true);
        }
        let requests = m.boards[0].host.agent_requests.as_ref().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].write_back, allowed && write);
        assert_eq!(requests[0].edit_objects, editing.then(Vec::new));
        assert!(requests[0].images.is_empty());
        assert!(!f.sent().iter().any(|r| r.1 == "resources.read"));
    }
}

#[test]
fn structured_snapshot_paginates_original_page_filters_images_and_keeps_window_map() {
    let (mut m, f, s) = begin_edit();
    let first = f.sent().last().unwrap().clone();
    assert_eq!(first.1, "objects.list");
    assert_eq!(
        first.2,
        json!({"document_id":s.document_id,"page_id":s.page_id,
        "expected_revision":s.revision,"offset":0,"limit":32,"max_bytes":OBJECT_PAGE_BYTES})
    );
    m.boards[0].next_state = Instant::now();
    m.poll(true);
    let window_id = m.boards[0].pending.as_ref().unwrap().id.clone();
    let image = json!({"id":"image-1","kind":{"type":"image","asset_ref":"asset:private"}});
    respond_objects(&f, Ok(object_page(&s, 0, 3, vec![edit_text("a"), image])));
    m.poll(true);
    assert_eq!(m.boards[0].pending.as_ref().unwrap().id, window_id);
    let second = f.sent().last().unwrap().clone();
    assert_eq!(second.1, "objects.list");
    assert_eq!(second.2["offset"], 2);
    assert_eq!(second.2["document_id"], s.document_id);
    assert_eq!(second.2["page_id"], s.page_id);
    assert_eq!(second.2["expected_revision"], s.revision);
    // Stale and unrelated request IDs cannot advance the snapshot.
    assert!(!m.boards[0]
        .host
        .response(&first.0, Ok(object_page(&s, 2, 3, vec![edit_text("b")]))));
    assert!(!m.boards[0].host.response(&window_id, Ok(json!({}))));
    respond_objects(&f, Ok(object_page(&s, 2, 3, vec![edit_text("b")])));
    m.poll(true);
    let requests = m.boards[0].host.agent_requests.as_ref().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].edit_objects,
        Some(vec![edit_text("a"), edit_text("b")])
    );
    assert!(requests[0].images.is_empty());
    assert!(!serde_json::to_string(&requests[0].edit_objects)
        .unwrap()
        .contains("asset:"));
    assert!(!f.sent().iter().any(|r| r.1 == "resources.read"));
}

#[test]
fn structured_edit_reads_only_explicit_images_then_objects_after_vision_consent() {
    let (mut m, f, s) = configured(false);
    ask(&f, "runtime:edit", "host.ask_agent", params(&s, false));
    m.poll(true);
    let before = f.sent().len();
    let host = &mut m.boards[0].host;
    host.agent_requests = Some(Vec::new());
    let c = host.consent().unwrap();
    c.write_back = true;
    c.structured_edit = true;
    host.authorize(true);
    m.poll(true);
    assert_eq!(f.sent().len(), before);
    m.boards[0].host.consent().unwrap().vision_confirmed = true;
    m.boards[0].host.authorize(true);
    m.poll(true);
    let (id, method, p) = f.sent().last().unwrap().clone();
    assert_eq!(method, "resources.read");
    assert_eq!(p["asset_ref"], "asset:one");
    let bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    f.emit(Event::Response {
        id,
        method,
        result: Ok(json!({"asset_ref":"asset:one",
        "offset":0,"total_bytes":8,"bytes":bytes,"next_offset":8,"eof":true})),
    });
    m.poll(true);
    assert_eq!(f.sent().last().unwrap().1, "objects.list");
    respond_objects(
        &f,
        Ok(object_page(
            &s,
            0,
            1,
            vec![json!({"id":"img","kind":{"type":"image","asset_ref":"asset:unapproved"}})],
        )),
    );
    m.poll(true);
    let requests = m.boards[0].host.agent_requests.as_ref().unwrap();
    assert_eq!(requests[0].images, vec![bytes]);
    assert_eq!(requests[0].edit_objects, Some(vec![]));
    assert_eq!(
        f.sent().iter().filter(|r| r.1 == "resources.read").count(),
        1
    );
}

#[test]
fn malformed_snapshot_context_and_pagination_fail_closed() {
    for (key, bad) in [
        ("document_id", json!("other")),
        ("page_id", json!("other")),
        ("revision", json!(8)),
        ("revision", json!("7")),
        ("offset", json!(1)),
        ("offset", json!(-1)),
        ("total", json!(0)),
        ("total", json!("2")),
        ("objects", json!({})),
        ("next_offset", json!(0)),
        ("next_offset", json!(2)),
        ("next_offset", Value::Null),
        ("next_offset", json!("1")),
    ] {
        let (mut m, f, s) = begin_edit();
        let mut page = object_page(&s, 0, 2, vec![edit_text("a")]);
        page[key] = bad;
        respond_objects(&f, Ok(page));
        assert_edit_failed(&mut m, &f);
        assert_eq!(
            f.sent().iter().filter(|r| r.1 == "objects.list").count(),
            1,
            "{key}"
        );
    }
    for objects in [vec![], vec![edit_text("a"); 33]] {
        let (mut m, f, s) = begin_edit();
        respond_objects(&f, Ok(object_page(&s, 0, 64, objects)));
        assert_edit_failed(&mut m, &f);
    }
    let (mut m, f, s) = begin_edit();
    let mut page = object_page(&s, 0, 0, vec![]);
    page.as_object_mut().unwrap().remove("next_offset");
    respond_objects(&f, Ok(page));
    assert_edit_failed(&mut m, &f);
}

#[test]
fn changing_total_phantom_total_and_duplicate_snapshot_ids_are_rejected() {
    for case in ["total", "phantom", "duplicate", "invalid", "asset_field"] {
        let (mut m, f, s) = begin_edit();
        respond_objects(&f, Ok(object_page(&s, 0, 2, vec![edit_text("a")])));
        m.poll(true);
        let mut second = match case {
            "total" => object_page(&s, 1, 3, vec![edit_text("b")]),
            "phantom" => object_page(&s, 1, 2, vec![]),
            "duplicate" => object_page(&s, 1, 2, vec![edit_text("a")]),
            _ => object_page(&s, 1, 2, vec![edit_text("b")]),
        };
        if case == "invalid" {
            second["objects"][0]["kind"]["size"] = json!(-1);
        }
        if case == "asset_field" {
            second["objects"][0]["kind"]["asset_ref"] = json!("asset:hidden");
        }
        respond_objects(&f, Ok(second));
        assert_edit_failed(&mut m, &f);
    }
}

#[test]
fn snapshot_budgets_include_filtered_images_and_never_truncate() {
    let (mut m, f, s) = begin_edit();
    respond_objects(&f, Ok(object_page(&s, 0, 257, vec![])));
    assert_edit_failed(&mut m, &f);
    assert_eq!(
        f.replies().last().unwrap().1.as_ref().unwrap_err().code,
        "resource_limit"
    );
    for kind in ["image", "text"] {
        let (mut m, f, s) = begin_edit();
        for offset in 0..4 {
            let mut object = edit_text(&format!("object-{offset}"));
            object["kind"]["type"] = json!(kind);
            object["kind"]["text"] = json!("x".repeat(34 * 1024));
            respond_objects(&f, Ok(object_page(&s, offset, 4, vec![object])));
            m.poll(true);
            if offset < 3 {
                assert!(f.replies().is_empty());
            }
        }
        assert_edit_failed(&mut m, &f);
        assert_eq!(
            f.replies().last().unwrap().1.as_ref().unwrap_err().code,
            "resource_limit"
        );
    }
    // A size error without a pinned object descriptor cannot authorize a read.
    let (mut m, f, _) = begin_edit();
    respond_objects(
        &f,
        Err(error(
            "object_too_large",
            "Object cannot fit the requested page",
        )),
    );
    assert_edit_failed(&mut m, &f);
    assert_eq!(
        f.replies().last().unwrap().1.as_ref().unwrap_err().code,
        "object_too_large"
    );
    assert!(!f.sent().iter().any(|r| r.1 == "objects.read"));
}

#[test]
fn all_256_images_are_scanned_even_though_the_editable_snapshot_is_empty() {
    let (mut m, f, s) = begin_edit();
    for offset in (0..256).step_by(32) {
        let objects = (offset..offset + 32).map(|n|
            json!({"id":format!("image-{n}"),"kind":{"type":"image","asset_ref":"asset:hidden"}})
        ).collect();
        respond_objects(&f, Ok(object_page(&s, offset, 256, objects)));
        m.poll(true);
    }
    assert_eq!(f.sent().iter().filter(|r| r.1 == "objects.list").count(), 8);
    let requests = m.boards[0].host.agent_requests.as_ref().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].edit_objects, Some(vec![]));
    assert!(requests[0].images.is_empty());
}

#[test]
fn cancellation_and_context_changes_discard_late_object_pages_without_agent_restart() {
    for reason in ["cancel", "document", "page", "revision", "timeout"] {
        let (mut m, f, mut s) = begin_edit();
        respond_objects(&f, Ok(object_page(&s, 0, 2, vec![edit_text("a")])));
        m.poll(true);
        let late = object_page(&s, 1, 2, vec![edit_text("b")]);
        let (id, _, _) = f.sent().last().unwrap().clone();
        match reason {
            "cancel" => m.boards[0].host.cancel(),
            "timeout" => {
                m.boards[0].host.task.as_mut().unwrap().deadline = Instant::now();
                m.boards[0].host.check(Some(&s), s.permissions, true);
            }
            _ => {
                match reason {
                    "document" => s.document_id = "other".into(),
                    "page" => s.page_id = "other".into(),
                    _ => s.revision += 1,
                }
                m.boards[0].host.check(Some(&s), s.permissions, true);
            }
        }
        assert!(m.boards[0].host.response(&id, Ok(late.clone())));
        assert_edit_failed(&mut m, &f);
        assert!(!m.boards[0].host.response(&id, Ok(late)));
        m.poll(true);
        assert_eq!(f.sent().iter().filter(|r| r.1 == "objects.list").count(), 2);
        assert!(m.boards[0].host.agent_requests.as_ref().unwrap().is_empty());
    }
}
