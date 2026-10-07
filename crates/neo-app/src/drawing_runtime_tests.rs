use super::protocol::{self, Session};
use super::transport::{self, Output, WriteFrame};
use super::*;
use serde_json::json;
use std::io::{BufReader, Cursor};

fn state() -> Value {
    json!({"app":"drawing","document_id":"actual-document","page_id":"actual-page","revision":0,
        "revision_scope":"document","dirty":false,"configured":true,"closed":false,
        "connected":true,"close_pending":false,"desired_visible":true,"effective_visible":true,
        "visible":false,"has_window":true,"window_status":"pending","hidden_confirmed":false,
        "permissions":Permissions::restricted(true).params()})
}

fn ready() -> Value {
    json!({"version":1,"type":"event","event":"ready","data":{
        "app":"drawing","methods":["configure","get_state","show","hide","close"],
        "headless":false,"has_window":true,"max_line_bytes":65536,"revision_scope":"document"}})
}

struct Harness {
    session: Session,
    output: Output,
    events: mpsc::Receiver<Event>,
    writer: mpsc::SyncSender<WriteFrame>,
    writes: mpsc::Receiver<WriteFrame>,
}

impl Harness {
    fn new() -> Self {
        let (events_tx, events) = mpsc::sync_channel(EVENT_QUEUE);
        let (failure, _) = mpsc::sync_channel(1);
        let (writer, writes) = mpsc::sync_channel(QUEUE);
        Self {
            session: Session {
                kind: BoardKind::Drawing,
                generation: 42,
                permissions: Permissions::restricted(true),
                methods: None,
                configured: false,
                closed: false,
                pending: Default::default(),
            },
            output: Output {
                events: events_tx,
                failure,
                ctx: egui::Context::default(),
                shared: Arc::new(Shared {
                    stop: AtomicBool::new(false),
                    usable: Arc::new(AtomicBool::new(true)),
                    host_requests: Mutex::default(),
                    alive: AtomicBool::new(true),
                    exited: AtomicBool::new(false),
                }),
            },
            events,
            writer,
            writes,
        }
    }
    fn feed(&mut self, value: Value) -> Result<(), String> {
        self.session.frame(
            &serde_json::to_vec(&value).unwrap(),
            &self.writer,
            &self.output,
        )
    }
    fn request(&mut self, n: u64, method: &str) {
        self.session
            .accept(
                Request {
                    id: format!("neo:42:{n}"),
                    method: method.into(),
                    params: json!({}),
                    deadline: Instant::now() + REQUEST_TIMEOUT,
                },
                &self.writer,
                &self.output,
            )
            .unwrap();
    }
    fn handle(&self) -> (RuntimeHandle, mpsc::Receiver<Command>) {
        let (requests, incoming) = mpsc::sync_channel(QUEUE);
        let (_, events) = mpsc::sync_channel(EVENT_QUEUE);
        let (_, failure) = mpsc::sync_channel(1);
        (
            RuntimeHandle {
                generation: 42,
                next_request: 1,
                requests,
                events,
                failure,
                shared: self.output.shared.clone(),
                exit_delivered: false,
            },
            incoming,
        )
    }

    fn host(&mut self, id: &str, method: &str) -> Result<(), String> {
        self.feed(json!({"version":1,"type":"request","id":id,"method":method,"params":{}}))
    }

    fn configured() -> Self {
        let mut h = Self::new();
        h.feed(ready()).unwrap();
        let configure: Value = serde_json::from_slice(&h.writes.try_recv().unwrap().bytes).unwrap();
        assert_eq!(configure["params"], Permissions::restricted(true).params());
        assert_eq!(configure["id"], "neo:42:0");
        assert!(h.events.try_recv().is_err());
        h.feed(json!({"version":1,"type":"response","id":"neo:42:0","ok":true,"result":state()}))
            .unwrap();
        assert!(
            matches!(h.events.try_recv().unwrap(), Event::Ready(s) if s.configured && !s.visible)
        );
        h
    }
}

#[test]
fn framing_exact_limit_crlf_eof_and_utf8_bytes() {
    for ending in ["\n", "\r\n", ""] {
        let mut input = vec![b' '; MAX_LINE];
        input.extend_from_slice(ending.as_bytes());
        let mut reader = BufReader::with_capacity(7, Cursor::new(input));
        assert_eq!(
            transport::read_frame(&mut reader)
                .unwrap()
                .unwrap()
                .unwrap()
                .len(),
            MAX_LINE
        );
        assert!(transport::read_frame(&mut reader).unwrap().is_none());
    }
    let mut input = "字".repeat(MAX_LINE / 3 + 1).into_bytes();
    input.extend_from_slice(b"\n{}\n");
    let mut reader = BufReader::with_capacity(11, Cursor::new(input));
    assert!(transport::read_frame(&mut reader)
        .unwrap()
        .unwrap()
        .is_err());
    assert_eq!(
        transport::read_frame(&mut reader)
            .unwrap()
            .unwrap()
            .unwrap(),
        b"{}"
    );
    let mut no_newline = Cursor::new(vec![b'x'; MAX_LINE * 10]);
    assert!(transport::read_frame(&mut no_newline)
        .unwrap()
        .unwrap()
        .is_err());
}

#[test]
fn rejects_invalid_envelopes_and_escaped_duplicates() {
    for line in [
        r#"{"version":1,"version":1,"type":"event","event":"ready","data":{}}"#,
        r#"{"version":1,"type":"response","id":"neo:1","i\u0064":"neo:1","ok":true,"result":null}"#,
        r#"{"version":1,"type":"response","id":"neo:1","ok":false,"error":{"code":"x","co\u0064e":"x","message":""}}"#,
        r#"{"version":1,"type":"response","id":"neo:1","ok":true,"result":null,"error":null}"#,
        r#"{"version":1,"type":"response","id":"neo:1","ok":false,"result":null,"error":{"code":"x","message":""}}"#,
        r#"{"version":1,"type":"response","id":"neo:1","ok":true}"#,
        r#"{"version":1,"type":"response","id":"neo:","ok":true,"result":null}"#,
        r#"{"version":1,"type":"response","id":"neo:a b","ok":true,"result":null}"#,
        r#"{"version":2,"type":"response","id":"neo:1","ok":true,"result":null}"#,
        r#"{"version":1,"type":"request","id":"runtime:x","method":"x","params":[]}"#,
        r#"{"version":1,"type":"request","id":"neo:x","method":"x","params":{}}"#,
    ] {
        assert!(protocol::decode(line.as_bytes()).is_err(), "{line}");
    }
    assert!(protocol::decode(b"\xff").is_err());
    assert!(protocol::decode(
        br#"{"version":1,"type":"response","id":"neo:1","ok":true,"result":null}"#
    )
    .is_ok());
    assert!(protocol::decode(br#"{"version":1,"type":"event","event":"x","data":{"x":1,"x":2},"unknown":1,"unknown":2,"error":null}"#).is_ok());
    assert!(!protocol::valid_id(
        &format!("neo:{}", "x".repeat(253)),
        "neo:"
    ));
}

#[test]
fn handshake_uses_actual_methods_and_restricted_permissions() {
    let mut h = Harness::configured();
    assert!(h.feed(ready()).is_err());
    h.request(1, "not.advertised");
    assert!(
        matches!(h.events.try_recv().unwrap(), Event::Response { result: Err(e), .. } if e.code == "method_not_found")
    );
    assert!(h.writes.try_recv().is_err());
    for (key, value) in [
        ("app", json!("blackboard")),
        ("headless", json!(true)),
        ("has_window", json!(false)),
        ("max_line_bytes", json!(999)),
        ("revision_scope", json!("page")),
    ] {
        let mut r = ready();
        r["data"][key] = value;
        assert!(Harness::new().feed(r).is_err());
    }
    let mut r = ready();
    r["data"]["host_methods"] = r["data"]["methods"].clone();
    r["data"]["methods"] = json!([]);
    assert!(Harness::new().feed(r).is_err());
}

#[test]
fn ready_jsonl_v1_accepts_child_v3_document_and_object_capabilities() {
    let mut r = ready();
    r["data"]["document_file_versions"] = json!([1, 2, 3]);
    r["data"]["resource_persistence"] = json!("board-session-package-v1");
    r["data"]["resource_persistence_versions"] = json!([
        "board-session-package-v1",
        "board-session-package-v2",
        "board-session-package-v3"
    ]);
    r["data"]["object_types"] = json!([
        "stroke",
        "shape",
        "text",
        "math",
        "handwritten",
        "image",
        "coordinate_system",
        "function_plot"
    ]);
    r["data"]["object_chunk_encoding"] = json!("utf8_json_u8_array");
    r["data"]["object_chunk_bytes"] = json!(8192);
    let mut h = Harness::new();
    h.feed(r.clone()).unwrap();
    let configure: Value = serde_json::from_slice(&h.writes.try_recv().unwrap().bytes).unwrap();
    assert_eq!(configure["version"], 1);
    assert_eq!(configure["method"], "configure");
    assert!(h.events.try_recv().is_err());
    h.feed(json!({"version":1,"type":"response","id":"neo:42:0","ok":true,"result":state()}))
        .unwrap();
    assert!(matches!(h.events.try_recv().unwrap(), Event::Ready(_)));
    assert!(h.writes.try_recv().is_err());
    // File/package v3 does not authorize a JSONL v3 envelope.
    r["version"] = json!(3);
    assert!(Harness::new().feed(r).is_err());
}

#[test]
fn configure_errors_and_timeouts_never_emit_ready() {
    let mut h = Harness::new();
    h.request(1, "show");
    assert!(
        matches!(h.events.try_recv().unwrap(), Event::Response { result: Err(e), .. } if e.code == "not_configured")
    );
    h.feed(ready()).unwrap();
    assert!(h
        .session
        .expire(
            Instant::now() + REQUEST_TIMEOUT + Duration::from_secs(1),
            &h.output
        )
        .is_err());
    assert!(h.events.try_recv().is_err());
    let mut h = Harness::new();
    h.feed(ready()).unwrap();
    let mut bad = state();
    bad["permissions"]["agent_allowed"] = json!(true);
    assert!(h
        .feed(json!({"version":1,"type":"response","id":"neo:42:0","ok":true,"result":bad}))
        .is_err());
}

#[test]
fn dirty_close_error_keeps_session_and_close_timeout_is_not_success() {
    let mut h = Harness::configured();
    h.request(1, "close");
    let wire: Value = serde_json::from_slice(&h.writes.try_recv().unwrap().bytes).unwrap();
    assert_eq!(wire["params"], json!({}));
    h.feed(
        json!({"version":1,"type":"response","id":"neo:42:1","ok":false,
        "error":{"code":"unsaved_changes","message":"save first"}}),
    )
    .unwrap();
    assert!(
        matches!(h.events.try_recv().unwrap(), Event::Response { result: Err(e), .. } if e.code == "unsaved_changes")
    );
    assert!(h.session.configured && !h.session.closed);
    h.request(2, "close");
    h.session
        .expire(Instant::now() + CLOSE_TIMEOUT, &h.output)
        .unwrap();
    assert!(
        matches!(h.events.try_recv().unwrap(), Event::Response { result: Err(e), .. } if e.code == "timeout")
    );
    let mut closed = state();
    closed["closed"] = json!(true);
    h.feed(json!({"version":1,"type":"response","id":"neo:42:2","ok":true,"result":closed}))
        .unwrap();
    assert!(h.events.try_recv().is_err());
    assert!(!h.session.closed);
}

#[test]
fn generations_reordering_and_same_revision_state_events() {
    let mut h = Harness::configured();
    h.request(1, "show");
    h.request(2, "hide");
    h.feed(json!({"version":1,"type":"response","id":"neo:41:1","ok":true,"result":state()}))
        .unwrap();
    assert_eq!(h.session.pending.len(), 2);
    for n in [2, 1] {
        h.feed(json!({"version":1,"type":"response","id":format!("neo:42:{n}"),"ok":true,"result":state()})).unwrap();
        assert!(matches!(
            h.events.try_recv().unwrap(),
            Event::StateChanged(_)
        ));
        assert!(
            matches!(h.events.try_recv().unwrap(), Event::Response { id, .. } if id == format!("neo:42:{n}"))
        );
    }
    for name in ["state_changed", "document_changed"] {
        let mut s = state();
        s["page_id"] = json!(name);
        h.feed(json!({"version":1,"type":"event","event":name,"data":s}))
            .unwrap();
        assert!(
            matches!(h.events.try_recv().unwrap(), Event::StateChanged(s) if s.page_id == name && s.revision == 0)
        );
    }
}

#[test]
fn host_requests_dont_block_pending_responses_or_fabricate_services() {
    let mut h = Harness::configured();
    h.request(1, "get_state");
    h.writes.try_recv().unwrap();
    for method in [
        "host.ask_agent",
        "host.capture_region",
        "resources.read",
        "resources.release",
        "future.method",
        "jobs.cancel",
    ] {
        let id = format!("runtime:{method}");
        let params = json!({"job_id":"job-1","request_id":"runtime:host.ask_agent"});
        h.feed(json!({"version":1,"type":"request","id":id,"method":method,"params":params}))
            .unwrap();
        assert_eq!(
            h.events.try_recv().unwrap(),
            Event::HostRequest {
                id,
                method: method.into(),
                params
            }
        );
        assert!(h.writes.try_recv().is_err());
    }
    h.feed(json!({"version":1,"type":"response","id":"neo:42:1","ok":true,"result":state()}))
        .unwrap();
    assert!(h.session.pending.is_empty());
    assert!(matches!(
        h.events.try_recv().unwrap(),
        Event::StateChanged(_)
    ));
    assert!(matches!(
        h.events.try_recv().unwrap(),
        Event::Response { .. }
    ));
    assert_eq!(h.output.shared.host_requests.lock().unwrap().inflight, 6);
}

#[test]
fn bidirectional_resources_are_reentrant_and_share_one_writer() {
    let mut h = Harness::configured();
    h.session
        .methods
        .as_mut()
        .unwrap()
        .insert("resources.read".into());
    let (mut handle, commands) = h.handle();
    h.host("runtime:agent", "host.ask_agent").unwrap();
    assert!(matches!(
        h.events.try_recv().unwrap(),
        Event::HostRequest { .. }
    ));
    let read_id = handle
        .request(
            "resources.read",
            json!({"asset_ref":"asset:board","offset":0,"length":1}),
        )
        .unwrap();
    h.session
        .command(commands.try_recv().unwrap(), &h.writer, &h.output)
        .unwrap();
    let read: Value = serde_json::from_slice(&h.writes.try_recv().unwrap().bytes).unwrap();
    assert_eq!(read["id"], read_id);
    h.host("runtime:read", "resources.read").unwrap();
    h.host("runtime:release", "resources.release").unwrap();
    for id in ["runtime:read", "runtime:release"] {
        assert!(matches!(
            h.events.try_recv().unwrap(),
            Event::HostRequest { .. }
        ));
        handle
            .reply_host(id, Ok(json!({"test_resource":true})))
            .unwrap();
    }
    h.feed(json!({"version":1,"type":"response","id":read_id,"ok":true,"result":{"bytes":[1]}}))
        .unwrap();
    assert!(matches!(
        h.events.try_recv().unwrap(),
        Event::Response { result: Ok(_), .. }
    ));
    handle
        .reply_host("runtime:agent", Ok(json!({"answer":"done"})))
        .unwrap();
    for id in ["runtime:read", "runtime:release", "runtime:agent"] {
        h.session
            .command(commands.try_recv().unwrap(), &h.writer, &h.output)
            .unwrap();
        let frame = h.writes.try_recv().unwrap();
        assert!(frame.deadline.is_none());
        let reply: Value = serde_json::from_slice(&frame.bytes).unwrap();
        assert_eq!(reply["id"], id);
        assert_eq!(reply["ok"], true);
        assert!(reply.get("error").is_none());
        assert!(handle.reply_host(id, Ok(Value::Null)).is_err());
    }
    assert_eq!(h.output.shared.host_requests.lock().unwrap().inflight, 0);
}

#[test]
fn host_ids_cannot_replay_before_or_after_reply_and_reject_before_ready() {
    for ready_received in [false, true] {
        let mut h = Harness::new();
        if ready_received {
            h.feed(ready()).unwrap();
        }
        assert!(h.host("runtime:early", "host.capture_region").is_err());
        assert!(h.events.try_recv().is_err());
    }
    for replied in [false, true] {
        let mut h = Harness::configured();
        let (handle, commands) = h.handle();
        h.host("runtime:once", "host.ask_agent").unwrap();
        h.events.try_recv().unwrap();
        if replied {
            handle.reply_host("runtime:once", Ok(Value::Null)).unwrap();
            h.session
                .command(commands.try_recv().unwrap(), &h.writer, &h.output)
                .unwrap();
        }
        assert!(h.host("runtime:once", "host.capture_region").is_err());
        assert!(h.events.try_recv().is_err());
        assert!(handle
            .reply_host("runtime:unknown", Ok(Value::Null))
            .is_err());
        assert!(handle.reply_host("neo:42:1", Ok(Value::Null)).is_err());
    }
}

#[test]
fn host_inflight_and_session_id_budgets_are_bounded() {
    let mut h = Harness::configured();
    for n in 0..QUEUE {
        h.host(&format!("runtime:{n}"), "resources.read").unwrap();
        h.events.try_recv().unwrap();
    }
    assert!(h.host("runtime:overflow", "resources.read").is_err());
    assert_eq!(
        h.output.shared.host_requests.lock().unwrap().ids.len(),
        QUEUE
    );
    let mut h = Harness::configured();
    let (handle, commands) = h.handle();
    for n in 0..HOST_ID_BUDGET {
        let id = format!("runtime:{n}");
        h.host(&id, "resources.read").unwrap();
        h.events.try_recv().unwrap();
        handle.reply_host(&id, Ok(Value::Null)).unwrap();
        h.session
            .command(commands.try_recv().unwrap(), &h.writer, &h.output)
            .unwrap();
        h.writes.try_recv().unwrap();
    }
    assert!(h.host("runtime:overflow", "resources.read").is_err());
    assert!(h.host("runtime:0", "resources.read").is_err());
    assert!(handle.reply_host("runtime:0", Ok(Value::Null)).is_err());
}

#[test]
fn guarded_reply_revocation_at_each_queue_emits_only_cancelled_wire() {
    // Revoke before command enqueue, before writer enqueue, or while serialized in
    // the writer queue. Exercise errors too: their data may also require authorization.
    for revoke_at in 0..3 {
        for success in [false, true] {
            let mut h = Harness::configured();
            let (mut handle, commands) = h.handle();
            let valid = Arc::new(AtomicBool::new(revoke_at != 0));
            h.host("runtime:guarded", "host.ask_agent").unwrap();
            h.events.try_recv().unwrap();
            let result = if success {
                Ok(json!({"answer":"sensitive"}))
            } else {
                Err(RpcError {
                    code: "service_error".into(),
                    message: "sensitive".into(),
                    data: Some(json!({"detail":"sensitive"})),
                })
            };
            handle
                .reply_host_guarded("runtime:guarded", result, valid.clone())
                .unwrap();
            assert!(handle
                .reply_host("runtime:guarded", Ok(Value::Null))
                .is_err());
            let command = commands.try_recv().unwrap();
            let Command::HostReply { bytes, .. } = &command else {
                panic!("expected host reply")
            };
            let queued: Value = serde_json::from_slice(bytes).unwrap();
            assert_eq!(queued["error"]["code"] == "cancelled", revoke_at == 0);
            if revoke_at == 1 {
                valid.store(false, Ordering::Release);
            }
            h.session.command(command, &h.writer, &h.output).unwrap();
            let frame = h.writes.try_recv().unwrap();
            let queued: Value = serde_json::from_slice(&frame.bytes).unwrap();
            assert_eq!(queued["error"]["code"] == "cancelled", revoke_at < 2);
            assert_eq!(h.output.shared.host_requests.lock().unwrap().inflight, 0);
            valid.store(false, Ordering::Release);
            let mut wire = Vec::new();
            frame.write_to(&mut wire, &AtomicBool::new(false)).unwrap();
            assert_eq!(wire.iter().filter(|b| **b == b'\n').count(), 1);
            let reply: Value = serde_json::from_slice(&wire).unwrap();
            assert_eq!(reply["id"], "runtime:guarded");
            assert_eq!(reply["ok"], false);
            assert_eq!(reply["error"]["code"], "cancelled");
            assert!(reply.get("result").is_none());
            assert!(reply["error"].get("data").is_none());
            assert!(!String::from_utf8(wire).unwrap().contains("sensitive"));
            assert!(h.writes.try_recv().is_err());

            // Neither normal Neo requests nor the unguarded compatibility API inherit
            // another response's revoked authorization token.
            handle.request("get_state", json!({})).unwrap();
            h.session
                .command(commands.try_recv().unwrap(), &h.writer, &h.output)
                .unwrap();
            let mut wire = Vec::new();
            h.writes
                .try_recv()
                .unwrap()
                .write_to(&mut wire, &AtomicBool::new(false))
                .unwrap();
            let request: Value = serde_json::from_slice(&wire).unwrap();
            assert_eq!(request["type"], "request");
            assert_eq!(request["method"], "get_state");
            h.host("runtime:plain", "resources.release").unwrap();
            handle
                .reply_host("runtime:plain", Ok(json!({"released":true})))
                .unwrap();
            h.session
                .command(commands.try_recv().unwrap(), &h.writer, &h.output)
                .unwrap();
            let mut wire = Vec::new();
            h.writes
                .try_recv()
                .unwrap()
                .write_to(&mut wire, &AtomicBool::new(false))
                .unwrap();
            let reply: Value = serde_json::from_slice(&wire).unwrap();
            assert_eq!(reply["ok"], true);
            assert_eq!(reply["result"]["released"], true);
        }
    }
}

#[test]
fn guarded_reply_queue_full_can_retry_after_revocation() {
    let mut h = Harness::configured();
    let (mut handle, commands) = h.handle();
    let valid = Arc::new(AtomicBool::new(true));
    h.host("runtime:retry", "host.ask_agent").unwrap();
    for _ in 0..QUEUE {
        handle.request("get_state", json!({})).unwrap();
    }
    assert!(handle
        .reply_host_guarded("runtime:retry", Ok(Value::Null), valid.clone())
        .is_err());
    assert!(
        !h.output.shared.host_requests.lock().unwrap().ids["runtime:retry"]
    );
    for _ in 0..QUEUE {
        commands.try_recv().unwrap();
    }
    valid.store(false, Ordering::Release);
    handle
        .reply_host_guarded("runtime:retry", Ok(Value::Null), valid.clone())
        .unwrap();
    assert!(handle
        .reply_host_guarded("runtime:retry", Ok(Value::Null), valid)
        .is_err());
    h.session
        .command(commands.try_recv().unwrap(), &h.writer, &h.output)
        .unwrap();
    let mut wire = Vec::new();
    h.writes
        .try_recv()
        .unwrap()
        .write_to(&mut wire, &AtomicBool::new(false))
        .unwrap();
    let reply: Value = serde_json::from_slice(&wire).unwrap();
    assert_eq!(reply["error"]["code"], "cancelled");
}

#[test]
fn guarded_reply_does_not_replace_a_partially_written_json_frame() {
    use std::io::{self, Write};
    struct PartialWriter {
        wire: Vec<u8>,
        valid: Arc<AtomicBool>,
    }
    impl Write for PartialWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.wire.push(bytes[0]);
            self.valid.store(false, Ordering::Release);
            Ok(1)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut h = Harness::configured();
    let (handle, commands) = h.handle();
    let valid = Arc::new(AtomicBool::new(true));
    h.host("runtime:started", "host.ask_agent").unwrap();
    handle
        .reply_host_guarded(
            "runtime:started",
            Ok(json!({"answer":"started"})),
            valid.clone(),
        )
        .unwrap();
    h.session
        .command(commands.try_recv().unwrap(), &h.writer, &h.output)
        .unwrap();
    let frame = h.writes.try_recv().unwrap();
    let expected = frame.bytes.clone();
    let mut writer = PartialWriter {
        wire: Vec::new(),
        valid: valid.clone(),
    };
    frame
        .write_to(&mut writer, &AtomicBool::new(false))
        .unwrap();
    assert!(!valid.load(Ordering::Acquire));
    assert_eq!(writer.wire, expected);
    let reply: Value = serde_json::from_slice(&writer.wire).unwrap();
    assert_eq!(reply["ok"], true);
    assert_eq!(reply["result"]["answer"], "started");
}

#[test]
fn host_reply_validation_queue_retry_and_unknown_method_error() {
    let mut h = Harness::configured();
    let (mut handle, commands) = h.handle();
    h.host("runtime:future", "future.method").unwrap();
    h.events.try_recv().unwrap();
    assert!(handle
        .reply_host("runtime:future", Ok(json!("x".repeat(MAX_LINE))))
        .is_err());
    assert!(handle
        .reply_host("runtime:future", Err(RpcError::local("", "bad")))
        .is_err());
    for _ in 0..QUEUE {
        handle.request("get_state", json!({})).unwrap();
    }
    assert!(handle
        .reply_host("runtime:future", Ok(Value::Null))
        .is_err());
    for _ in 0..QUEUE {
        commands.try_recv().unwrap();
    }
    handle
        .reply_host(
            "runtime:future",
            Err(RpcError {
                code: "method_not_found".into(),
                message: "not implemented".into(),
                data: Some(json!({"method":"future.method"})),
            }),
        )
        .unwrap();
    assert!(handle
        .reply_host("runtime:future", Ok(Value::Null))
        .is_err());
    h.session
        .command(commands.try_recv().unwrap(), &h.writer, &h.output)
        .unwrap();
    let reply: Value = serde_json::from_slice(&h.writes.try_recv().unwrap().bytes).unwrap();
    assert_eq!(reply["error"]["code"], "method_not_found");
    assert_eq!(reply["error"]["data"]["method"], "future.method");
    assert!(reply.get("result").is_none());
}

#[test]
fn permissions_boolean_matrix_and_post_ready_configuration() {
    for safe in [false, true] {
        for capture in [false, true] {
            for agent in [false, true] {
                let p = Permissions::new(safe, capture, agent);
                assert_eq!(p.classroom_safe(), safe);
                assert_eq!(p.capture_allowed(), capture && !safe);
                assert_eq!(p.agent_allowed(), agent);
                let mut s = state();
                s["permissions"] = p.params();
                assert_eq!(State::parse(&s, BoardKind::Drawing).unwrap().permissions, p);
                s["permissions"]["desktop_capture_allowed"] = json!(capture);
                assert_eq!(
                    State::parse(&s, BoardKind::Drawing).is_err(),
                    safe && capture
                );
                assert_eq!(
                    protocol::validate_outbound("configure", &s["permissions"]).is_err(),
                    safe && capture
                );
            }
        }
    }
    for key in ["classroom_safe", "desktop_capture_allowed", "agent_allowed"] {
        for bad in [Value::Null, json!(1), json!("true")] {
            let mut s = state();
            s["permissions"][key] = bad;
            assert!(State::parse(&s, BoardKind::Drawing).is_err());
        }
        let mut s = state();
        s["permissions"].as_object_mut().unwrap().remove(key);
        assert!(State::parse(&s, BoardKind::Drawing).is_err());
    }
    let mut s = state();
    s["permissions"]["extra"] = json!(false);
    assert!(State::parse(&s, BoardKind::Drawing).is_err());
    let mut h = Harness::configured();
    let (mut handle, commands) = h.handle();
    for method in ["capture.request", "agent.request"] {
        assert!(protocol::validate_outbound(method, &json!({})).is_ok());
    }
    let p = Permissions::new(false, true, true);
    let id = handle.request("configure", p.params()).unwrap();
    h.session
        .command(commands.try_recv().unwrap(), &h.writer, &h.output)
        .unwrap();
    let mut s = state();
    s["permissions"] = p.params();
    h.feed(json!({"version":1,"type":"response","id":id,"ok":true,"result":s}))
        .unwrap();
    assert!(matches!(h.events.try_recv().unwrap(), Event::StateChanged(s) if s.permissions == p));
    assert!(matches!(
        h.events.try_recv().unwrap(),
        Event::Response { result: Ok(_), .. }
    ));
}

#[test]
fn failguard_precedes_stdin_cleanup_and_rejects_capture_result() {
    use std::io::{self, Write};
    struct Probe {
        live: Arc<AtomicBool>,
        cleaned: Arc<AtomicBool>,
        fail_flush: bool,
    }
    impl Write for Probe {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.fail_flush {
                Ok(bytes.len())
            } else {
                Err(io::ErrorKind::BrokenPipe.into())
            }
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
    }
    impl Drop for Probe {
        fn drop(&mut self) {
            assert!(
                !self.live.load(Ordering::Acquire),
                "stdin closed before guard invalidation"
            );
            self.cleaned.store(true, Ordering::Release);
        }
    }
    for fail_flush in [false, true] {
        let mut h = Harness::configured();
        let (handle, commands) = h.handle();
        h.host("runtime:capture", "host.capture_region").unwrap();
        let live = handle.connection_live();
        let cleaned = Arc::new(AtomicBool::new(false));
        let mut writer = transport::DisconnectWriter {
            inner: Probe {
                live: live.clone(),
                cleaned: cleaned.clone(),
                fail_flush,
            },
            live: live.clone(),
        };
        if fail_flush {
            writer.write_all(b"response").unwrap();
            assert!(writer.flush().is_err());
        } else {
            assert!(writer.write_all(b"response").is_err());
        }
        assert!(!live.load(Ordering::Acquire));
        assert!(!cleaned.load(Ordering::Acquire));
        // No Failed event has been consumed; even an already-started capture cannot publish.
        assert!(handle
            .reply_host("runtime:capture", Ok(json!({"asset_ref":"asset:late"})))
            .is_err());
        assert!(commands.try_recv().is_err());
        drop(writer);
        assert!(cleaned.load(Ordering::Acquire));
    }
    let live = Arc::new(AtomicBool::new(true));
    drop(transport::DisconnectWriter {
        inner: Probe {
            live: live.clone(),
            cleaned: Arc::new(AtomicBool::new(false)),
            fail_flush: false,
        },
        live: live.clone(),
    });
    assert!(!live.load(Ordering::Acquire));
}

#[test]
fn output_writer_pending_and_request_budgets_are_bounded() {
    let mut h = Harness::configured();
    for n in 1..=QUEUE as u64 {
        h.request(n, "show");
    }
    h.request(100, "hide");
    assert!(
        matches!(h.events.try_recv().unwrap(), Event::Response { result: Err(e), .. } if e.code == "busy")
    );
    assert_eq!(h.session.pending.len(), QUEUE);
    // Host routing does not need room in the outbound writer queue.
    h.feed(json!({"version":1,"type":"request","id":"runtime:x","method":"x","params":{}}))
        .unwrap();
    assert!(matches!(
        h.events.try_recv().unwrap(),
        Event::HostRequest { .. }
    ));
    let mut h = Harness::configured();
    for _ in 0..EVENT_QUEUE {
        h.output
            .events
            .try_send(Event::StateChanged(
                State::parse(&state(), BoardKind::Drawing).unwrap(),
            ))
            .unwrap();
    }
    assert!(h
        .feed(json!({"version":1,"type":"event","event":"state_changed","data":state()}))
        .is_err());
    assert!(protocol::request_frame("neo:1", "x", &json!({"text":"字".repeat(MAX_LINE)})).is_err());
}

#[test]
fn safe_api_and_state_validation() {
    assert_eq!(BoardKind::Blackboard.code(), "blackboard");
    assert!(protocol::validate_outbound("close", &json!({"discard_unsaved":true})).is_err());
    for method in ["show", "hide", "close", "get_state"] {
        assert!(protocol::validate_outbound(method, &json!({})).is_ok());
    }
    assert!(
        protocol::validate_outbound("configure", &Permissions::restricted(false).params()).is_ok()
    );
    assert!(protocol::validate_outbound(
        "configure",
        &json!({"classroom_safe":true,"desktop_capture_allowed":true,"agent_allowed":true})
    )
    .is_err());
    assert!(protocol::validate_outbound("show", &Value::Null).is_err());
    for key in [
        "visible",
        "dirty",
        "configured",
        "document_id",
        "page_id",
        "revision",
    ] {
        let mut s = state();
        s.as_object_mut().unwrap().remove(key);
        assert!(State::parse(&s, BoardKind::Drawing).is_err());
    }
    let mut s = state();
    s["has_window"] = json!(false);
    s["hidden_confirmed"] = json!(true);
    assert!(State::parse(&s, BoardKind::Drawing).is_err());
}

#[test]
fn drop_and_poll_are_nonblocking_and_sessions_are_isolated() {
    let make = |generation| {
        let (requests, receiver) = mpsc::sync_channel(QUEUE);
        let (sender, events) = mpsc::sync_channel(EVENT_QUEUE);
        let (_, failure) = mpsc::sync_channel(1);
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            usable: Arc::new(AtomicBool::new(true)),
            host_requests: Mutex::default(),
            alive: AtomicBool::new(true),
            exited: AtomicBool::new(false),
        });
        (
            RuntimeHandle {
                generation,
                next_request: 1,
                requests,
                events,
                failure,
                shared: shared.clone(),
                exit_delivered: false,
            },
            receiver,
            sender,
            shared,
        )
    };
    let (mut first, _requests, events, shared) = make(501);
    let (mut second, _requests2, _events2, _) = make(502);
    assert_ne!(
        first.request("show", json!({})).unwrap(),
        second.request("show", json!({})).unwrap()
    );
    assert!(second.try_recv().is_none());
    assert_eq!(first.generation(), 501);
    assert!(first.is_alive());
    for _ in 1..QUEUE {
        first.request("hide", json!({})).unwrap();
    }
    assert!(first.request("hide", json!({})).is_err());
    let live = first.connection_live();
    assert!(live.load(Ordering::Acquire));
    drop(first);
    assert!(!live.load(Ordering::Acquire));
    assert!(shared.stop.load(Ordering::Acquire));
    assert!(shared.alive.load(Ordering::Acquire));
    assert!(events.try_send(Event::Exited).is_err());
    assert!(second.try_recv().is_none());
}

/// Compile drawing_runtime/fake_stdio.rs to target/drawing-runtime-fake-stdio.exe,
/// then explicitly run with --ignored. Never resolves/starts a deployed board.
#[test]
#[ignore = "explicit synthetic stdio subprocess smoke only"]
fn fake_stdio_process_drains_stderr_and_reaps_after_stdin_disconnect() {
    fake_process_smoke("stdio");
}

#[test]
#[ignore = "explicit synthetic blocked-stdin subprocess smoke only"]
fn fake_stdio_drop_with_blocked_writer_still_reaps() {
    fake_process_smoke("blocked");
}

#[test]
#[ignore = "explicit synthetic malformed-envelope/EOF subprocess smoke only"]
fn fake_stdio_failure_invalidates_token_without_ui_poll() {
    fake_process_smoke("malformed");
    fake_process_smoke("eof");
}

static PROCESS_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn cleanup_permits_bound_retries_until_last_pipe_owner_finishes() {
    let _lock = PROCESS_TEST_LOCK.lock().unwrap();
    let permits: Vec<_> = (0..4)
        .map(|_| transport::WorkerPermit::acquire().unwrap())
        .collect();
    assert!(transport::WorkerPermit::acquire().is_err());
    let pipe_owners = permits.clone();
    drop(permits);
    assert!(transport::WorkerPermit::acquire().is_err());
    drop(pipe_owners);
    assert!(transport::WorkerPermit::acquire().is_ok());
}

fn fake_process_smoke(mode: &str) {
    let blocked = mode == "blocked";
    let _lock = PROCESS_TEST_LOCK.lock().unwrap();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(format!("target/drawing-runtime-fake-{mode}.exe"));
    assert!(
        path.is_file(),
        "compile the dedicated fake_stdio.rs peer first"
    );
    let (requests, incoming) = mpsc::sync_channel(QUEUE);
    let (events_tx, events) = mpsc::sync_channel(EVENT_QUEUE);
    let (failure_tx, failure) = mpsc::sync_channel(1);
    let shared = Arc::new(Shared {
        stop: AtomicBool::new(false),
        usable: Arc::new(AtomicBool::new(true)),
        host_requests: Mutex::default(),
        alive: AtomicBool::new(true),
        exited: AtomicBool::new(false),
    });
    let mut handle = RuntimeHandle {
        generation: 900,
        next_request: 1,
        requests,
        events,
        failure,
        shared: shared.clone(),
        exit_delivered: false,
    };
    let output = Output {
        events: events_tx,
        failure: failure_tx,
        shared: shared.clone(),
        ctx: egui::Context::default(),
    };
    let permit = transport::WorkerPermit::acquire().unwrap();
    let lifetime = Arc::downgrade(&permit);
    let worker = std::thread::spawn(move || {
        transport::run(
            BoardKind::Drawing,
            path,
            900,
            Permissions::restricted(true),
            incoming,
            output,
            permit,
        )
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match handle.try_recv() {
            Some(Event::Ready(s)) => {
                assert!(s.dirty);
                break;
            }
            Some(other) => panic!("unexpected handshake event: {other:?}"),
            None => {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
    if matches!(mode, "malformed" | "eof") {
        let live = handle.connection_live();
        while live.load(Ordering::Acquire) {
            assert!(
                Instant::now() < deadline,
                "failure guard requires UI polling"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(handle.request("get_state", json!({})).is_err());
        assert!(handle
            .reply_host("runtime:late", Ok(json!({"asset_ref":"asset:late"})))
            .is_err());
        // The malformed peer exits only after stdin cleanup. EOF exits independently.
        while !shared.exited.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(matches!(handle.try_recv(), Some(Event::Failed(_))));
        assert_eq!(handle.try_recv(), Some(Event::Exited));
        drop(handle);
        worker.join().unwrap();
        while lifetime.upgrade().is_some() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        return;
    }
    if blocked {
        // Larger than the Windows anonymous pipe buffer; the fake has stopped reading.
        handle
            .request("math.calculate", json!({"expression":"x".repeat(60_000)}))
            .unwrap();
        std::thread::sleep(Duration::from_millis(100));
        let before = Instant::now();
        drop(handle);
        assert!(before.elapsed() < Duration::from_millis(100));
        #[cfg(windows)]
        {
            // Supervisor + stdout + stderr remain; the fourth owner (writer) must
            // release its permit before the fake's two-second natural exit.
            let cancel_deadline = Instant::now() + Duration::from_secs(1);
            while lifetime.strong_count() > 3 {
                assert!(
                    Instant::now() < cancel_deadline,
                    "blocked stdin was not cancelled"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(
                shared.alive.load(Ordering::Acquire),
                "peer exited before writer cancellation was tested"
            );
        }
        while !shared.exited.load(Ordering::Acquire) {
            assert!(
                Instant::now() < deadline,
                "blocked writer prevented child reap"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        worker.join().unwrap();
        while lifetime.upgrade().is_some() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        return;
    }
    handle.request("close", json!({})).unwrap();
    loop {
        match handle.try_recv() {
            Some(Event::Response { result: Err(e), .. }) => {
                assert_eq!(e.code, "unsaved_changes");
                break;
            }
            Some(other) => panic!("unexpected close event: {other:?}"),
            None => {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
    assert!(handle.is_alive());
    drop(handle);
    while !shared.exited.load(Ordering::Acquire) {
        assert!(
            Instant::now() < deadline,
            "fake peer was not reaped after stdin EOF"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    worker.join().unwrap();
    while lifetime.upgrade().is_some() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!shared.alive.load(Ordering::Acquire));
}

#[test]
fn paths_reject_relative_traversal_and_wrong_filename_without_launching() {
    assert!(paths::validate_path(
        Path::new("apps/drawing/neo-drawing.exe"),
        BoardKind::Drawing
    )
    .is_err());
    assert!(paths::validate_path(
        &std::env::temp_dir().join("../neo-drawing.exe"),
        BoardKind::Drawing
    )
    .is_err());
    assert!(
        paths::validate_path(&std::env::temp_dir().join("cmd.exe"), BoardKind::Drawing).is_err()
    );
}
