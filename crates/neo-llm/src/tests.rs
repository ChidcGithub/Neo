use super::*;

#[test]
fn context_configuration_restore_and_million_default() {
    assert_eq!(Config::deepseek("k").context_tokens, 1_000_000);
    for value in [None, Some("invalid"), Some("0"), Some("4000001")] {
        assert_eq!(restored_context_tokens(value), 1_000_000);
    }
    assert_eq!(restored_context_tokens(Some("32768")), 32768);
    let cfg = Config::deepseek("k");
    let mut messages = vec![Msg::new(Role::User, "x".repeat(100_000))];
    budget_messages(&cfg, &mut messages, &[]).unwrap();
    assert_eq!(build_wire(&cfg, &messages, vec![]).max_tokens, OUTPUT_TOKENS);
    let smaller = Config { context_tokens: 32 * 1024, ..cfg };
    assert!(budget_messages(&smaller, &mut messages, &[]).is_err());
    assert_eq!(messages[0].content.len(), 100_000);
}

#[test]
fn context_compaction_input_keeps_previous_summary_and_every_history_record() {
    let cfg = Config { context_tokens: 32 * 1024, ..Config::deepseek("k") };
    let history = serde_json::json!([
        {"previous_summary": "此前目标与约束"},
        {"role": "user", "content": "原始用户目标"},
        {"role": "assistant", "content": "x".repeat(20_000)},
        {"role": "tool", "content": "旧工具结果"}
    ]).to_string();
    let mut messages = vec![Msg::new(Role::System, "只总结历史"), Msg::new(Role::User, history.clone())];
    budget_messages(&cfg, &mut messages, &[]).unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[1].content, history);
    assert_eq!(request_body(&cfg, &messages, vec![])["messages"][1]["content"], history);
    messages.push(Msg::new(Role::User, "y".repeat(20_000)));
    let before = request_body(&cfg, &messages, vec![]);
    assert!(budget_messages(&cfg, &mut messages, &[]).is_err());
    assert_eq!(request_body(&cfg, &messages, vec![]), before);
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[1].content, history);
}

#[test]
fn context_multiround_output_reserve_matches_wire_and_stays_bounded() {
    let tools = neo_tools::tool_declarations();
    let schema = serialized_size(&tools, MAX_REQUEST_BYTES).unwrap();
    let fixed = PROTOCOL_TOKENS + TOOL_RESERVE.max(schema);
    // Keep the message/output allowance stable as real tool descriptions grow.
    // Four rounds fit, but must reduce output below the normal 4096-token cap.
    let context_tokens = fixed + 12 * 1024;
    let cfg = Config { context_tokens, ..Config::deepseek("k") };
    let mut messages = vec![Msg::new(Role::System, "s".repeat(2000)), Msg::new(Role::User, "original intent")];
    for round in 0..4 {
        let id = format!("c-{round}");
        messages.push(Msg::assistant_with_tools("", vec![ToolCall {
            id: id.clone(), name: "read_file".into(), arguments: format!("{{\"path\":\"a.txt\",\"offset\":{round}}}"),
        }]).with_reasoning("complete reasoning"));
        messages.push(Msg::tool_result(id, serde_json::json!({"ok":true, "tool":"read_file",
            "data":{"content":"x".repeat(1800), "offset":round, "next_offset":round+1, "truncated":true}}).to_string()));
        budget_messages(&cfg, &mut messages, &tools).unwrap_or_else(|error| {
            panic!("round={round}, schema_bytes={}, required_budget={}: {error}",
                serialized_size(&tools, MAX_REQUEST_BYTES).unwrap(),
                context_usage(&cfg, &messages, &tools).unwrap());
        });
        assert_eq!(messages.len(), 2 + (round + 1) * 2);
        let wire = build_wire(&cfg, &messages, tools.clone());
        let used: usize = messages.iter().map(|m| message_tokens(m, true).unwrap()).sum();
        assert!(used + fixed + wire.max_tokens <= context_tokens);
        assert!((MIN_OUTPUT_TOKENS..=OUTPUT_TOKENS).contains(&wire.max_tokens));
        if round == 0 { assert_eq!(wire.max_tokens, OUTPUT_TOKENS); }
        if round == 3 { assert!(wire.max_tokens < OUTPUT_TOKENS); }
    }
    let used: usize = messages.iter().map(|m| message_tokens(m, true).unwrap()).sum();
    let spare = context_tokens - fixed - MIN_OUTPUT_TOKENS - used;
    messages[1].content.push_str(&"x".repeat(spare));
    budget_messages(&cfg, &mut messages, &tools).unwrap();
    assert_eq!(context_usage(&cfg, &messages, &tools).unwrap(), context_tokens);
    assert_eq!(build_wire(&cfg, &messages, tools.clone()).max_tokens, MIN_OUTPUT_TOKENS);
    messages[1].content.push('x');
    assert_eq!(context_usage(&cfg, &messages, &tools).unwrap(), context_tokens + 1);
    let before = request_body(&cfg, &messages, tools.clone());
    assert!(budget_messages(&cfg, &mut messages, &tools).is_err());
    assert_eq!(request_body(&cfg, &messages, tools), before);
}

#[test]
fn context_pairing_failure_does_not_trim_input() {
    let mut messages = vec![Msg::new(Role::User, "旧".repeat(20_000)),
        Msg::new(Role::User, "最新"), Msg::tool_result("orphan", "{}")];
    assert!(budget_messages(&Config::deepseek("k"), &mut messages, &[]).is_err());
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0].content.len(), 60_000);
}

#[test]
fn tool_images_follow_all_parallel_results() {
    let calls = ["a", "b"].map(|id| ToolCall { id: id.into(), name: "view_image".into(), arguments: "{}".into() });
    let mut messages = vec![Msg::new(Role::User, "看图"),
        Msg::assistant_with_tools("", calls.to_vec()),
        Msg::tool_result("a", "{}").with_image(png_url(2, 3, 0)),
        Msg::tool_result("b", "{}")];
    budget_messages(&Config::deepseek("k"), &mut messages, &[]).unwrap();
    let body = request_body(&Config::deepseek("k"), &messages, vec![]);
    assert_eq!(body["messages"][2]["role"], "tool");
    assert_eq!(body["messages"][3]["role"], "tool");
    assert!(body["messages"][2]["content"].is_string());
    assert_eq!(body["messages"][4]["role"], "user");
    assert_eq!(body["messages"][4]["content"][1]["text"], "a");
    assert_eq!(body["messages"][4]["content"][2]["type"], "image_url");
    assert_eq!(messages.len(), 4);
}

#[test]
fn context_keeps_complete_turns_and_reasoning() {
    let call = ToolCall {
        id: "c".into(),
        name: "read".into(),
        arguments: "{}".into(),
    };
    let mut messages = vec![
        Msg::new(Role::System, "system"),
        Msg::new(Role::User, "旧".repeat(9000)),
        Msg::assistant_with_tools("", vec![call.clone()]).with_reasoning("旧推理"),
        Msg::tool_result("c", "{}"),
        Msg::new(Role::User, "最新意图"),
        Msg::assistant_with_tools("", vec![call]).with_reasoning("完整推理"),
        Msg::tool_result("c", "{\"ok\":true}"),
    ];
    let cfg = Config { context_tokens: 32 * 1024, ..Config::deepseek("k") };
    assert!(budget_messages(&cfg, &mut messages, &[serde_json::json!({})]).is_err());
    assert_eq!(messages.len(), 7, "超限不能冒充摘要而删除旧轮");
    assert_eq!(messages[0].content, "system");
    assert_eq!(messages[1].content, "旧".repeat(9000));
    assert_eq!(messages[4].content, "最新意图");
    assert_eq!(messages[5].reasoning.as_deref(), Some("完整推理"));
    assert_eq!(messages[6].content, "{\"ok\":true}");
}

#[test]
fn context_rejects_large_latest_system_schema_and_unpaired_tools() {
    assert_eq!(estimate_text_tokens("中文"), 6);
    assert_eq!(estimate_text_tokens("abcd"), 4);
    let cfg = Config { context_tokens: 32 * 1024, ..Config::deepseek("k") };
    for mut messages in [
        vec![Msg::new(Role::User, "中文".repeat(10_000))],
        vec![
            Msg::new(Role::System, "x".repeat(100_000)),
            Msg::new(Role::User, "hi"),
        ],
        vec![
            Msg::new(Role::User, "hi"),
            Msg::tool_result("missing", "{}"),
        ],
        vec![Msg::rejected("preflight rejected")],
    ] {
        assert!(budget_messages(&cfg, &mut messages, &[]).is_err());
    }
    let mut messages = vec![Msg::new(Role::User, "hi")];
    assert!(budget_messages(
        &cfg,
        &mut messages,
        &[serde_json::json!({"description":"文".repeat(20_000)})]
    )
    .is_err());
    assert!(
        budget_messages(&cfg, &mut messages, &neo_tools::tool_declarations()).is_ok(),
        "实际工具声明必须仍容纳简单问题"
    );
}

fn png_url(width: u32, height: u32, extra: usize) -> String {
    let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    bytes.extend(width.to_be_bytes());
    bytes.extend(height.to_be_bytes());
    bytes.resize(bytes.len() + extra, 0);
    encode_image("png", &bytes)
}

fn encode_image(mime: &str, bytes: &[u8]) -> String {
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = format!("data:image/{mime};base64,");
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16
            | (chunk.get(1).copied().unwrap_or(0) as u32) << 8
            | chunk.get(2).copied().unwrap_or(0) as u32;
        for i in 0..4 {
            out.push(if i > chunk.len() {
                '='
            } else {
                alphabet[((n >> (18 - i * 6)) & 63) as usize] as char
            });
        }
    }
    out
}

#[test]
fn image_budget_matches_tools_and_preserves_4k_boundary() {
    use neo_tools::tools::view_image;
    assert_eq!(MAX_MODEL_IMAGE_BYTES, view_image::MAX_MODEL_IMAGE_BYTES);
    assert_eq!(MAX_MODEL_IMAGE_EDGE, view_image::MAX_MODEL_IMAGE_EDGE);
    assert_eq!(MAX_MODEL_IMAGE_PIXELS, view_image::MAX_MODEL_IMAGE_PIXELS);
    assert_eq!(3840u64 * 2160, 8_294_400);
    assert_eq!(MAX_MODEL_IMAGE_PIXELS, 8_847_360);
    assert_eq!(MAX_MODEL_IMAGE_EDGE, 4096);
    assert_eq!(MAX_MODEL_IMAGE_BYTES, 4 * 1024 * 1024);
    assert_eq!(view_image::MAX_SOURCE_IMAGE_BYTES, 32 * 1024 * 1024);
    for (w, h) in [(3840, 2160), (4096, 2160), (2160, 4096), (4096, 1)] {
        assert!(validate_image(&png_url(w, h, 0)).is_ok());
    }
    for (w, h) in [(4096, 2161), (2161, 4096), (4097, 1), (1, 4097)] {
        assert!(validate_image(&png_url(w, h, 0)).is_err());
    }
    assert!(validate_image(&png_url(1, 1, MAX_MODEL_IMAGE_BYTES - 24)).is_ok());
    assert!(validate_image(&png_url(1, 1, MAX_MODEL_IMAGE_BYTES - 23)).is_err());
}

#[test]
fn image_metadata_and_count_budget_not_base64_tokens() {
    let url = png_url(1280, 720, 1024 * 1024);
    assert!(validate_image(&url).is_ok());
    let mut messages = vec![Msg::new(Role::User, "看图").with_image(url)];
    budget_messages(&Config::deepseek("k"), &mut messages, &[]).unwrap();
    for url in [
        png_url(0, 100, 0),
        png_url(5000, 100, 0),
        png_url(4096, 4096, 0),
        png_url(100, 100, 4 * 1024 * 1024),
        "data:image/png;base64,AAA=".into(),
    ] {
        assert!(validate_image(&url).is_err());
    }
    let jpeg = encode_image("jpeg", &[255, 216, 255, 192, 0, 8, 8, 2, 208, 5, 0, 1]);
    assert!(validate_image(&jpeg).is_ok());
    let mut msg = Msg::new(Role::User, "图片");
    msg.images = vec![png_url(1, 1, 0); 5];
    assert!(budget_messages(&Config::deepseek("k"), &mut vec![msg], &[]).is_err());
}

#[test]
fn serialized_budget_counts_escapes_without_large_copy() {
    let value = "\0".repeat(100);
    assert!(serialized_size(&value, 101).is_err());
    assert_eq!(serialized_size(&value, 602).unwrap(), 602);
    let body = request_body(
        &Config::deepseek("k"),
        &[Msg::new(Role::User, "hi")],
        vec![],
    );
    assert_eq!(body["max_tokens"], OUTPUT_TOKENS);
}

#[test]
fn cumulative_text_reasoning_args_and_events_fail_without_done() {
    for field in ["content", "reasoning_content", "arguments"] {
        let text = "x".repeat(MAX_OUTPUT_BYTES / 2 + 1);
        let delta = if field == "arguments" {
            serde_json::json!({"tool_calls":[{"index":0,"function":{"name":"read", "arguments":text}}]})
        } else {
            serde_json::json!({field:text})
        };
        let chunk = serde_json::json!({"choices":[{"delta":delta}]});
        let body = format!("data: {chunk}\n\ndata: {chunk}\n\ndata: [DONE]\n");
        assert_failed(&reader_events(body.as_bytes()));
    }
    let body = format!("{}data: [DONE]\n", ": ping\n".repeat(MAX_STREAM_EVENTS + 1));
    assert_failed(&reader_events(body.as_bytes()));
}

#[test]
fn start_rejects_over_budget_before_local_http() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let cfg = Config {
        base_url: format!("http://{}", listener.local_addr().unwrap()),
        ..Config::deepseek("test")
    };
    let cfg = Config { context_tokens: 32 * 1024, ..cfg };
    let stream = start(cfg, vec![Msg::new(Role::User, "字".repeat(100_000))]);
    assert!(matches!(
        stream.rx.recv_timeout(Duration::from_secs(3)).unwrap(),
        Event::Failed(_)
    ));
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn start_rejects_escaped_wire_size_and_invalid_image_before_http() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let cfg = Config {
        base_url: format!("http://{}", listener.local_addr().unwrap()),
        ..Config::deepseek("test")
    };
    // model 不参与上下文估算，仍必须受真实转义后 JSON 字节上限约束。
    let mut huge_config = cfg.clone();
    huge_config.model = "\0".repeat(MAX_REQUEST_BYTES / 6 + 1);
    for (config, message) in [
        (huge_config, Msg::new(Role::User, "hi")),
        (
            cfg,
            Msg::new(Role::User, "看图").with_image(png_url(5000, 1, 0)),
        ),
    ] {
        let stream = start(config, vec![message]);
        assert!(matches!(
            stream.rx.recv_timeout(Duration::from_secs(3)).unwrap(),
            Event::Failed(_)
        ));
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
}

#[test]
fn local_http_total_timeout_bounds_stalled_headers() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut bytes = [0; 1024];
        let _ = socket.read(&mut bytes);
        std::thread::sleep(Duration::from_millis(500));
    });
    let result = http_client(Duration::from_millis(100))
        .unwrap()
        .get(format!("http://{addr}/"))
        .send();
    assert!(result.unwrap_err().is_timeout());
    server.join().unwrap();
}

fn reader_events(reader: impl Read) -> Vec<Event> {
    let (line_tx, line_rx) = mpsc::channel();
    let cancel = AtomicBool::new(false);
    read_lines(reader, &line_tx, &cancel);
    drop(line_tx);
    let (tx, rx) = mpsc::channel();
    consume_lines(line_rx, &tx, &cancel);
    drop(tx);
    rx.into_iter().collect()
}

fn assert_failed(events: &[Event]) {
    assert!(
        matches!(events.last(), Some(Event::Failed(_))),
        "{events:?}"
    );
    assert!(!events.iter().any(|e| matches!(e, Event::Done { .. })));
}

struct SingleByteReader<'a>(&'a [u8]);

impl Read for SingleByteReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let len = buf.len().min(1);
        self.0.read(&mut buf[..len])
    }
}

#[test]
fn sse_single_byte_bom_chinese_and_body_bom_are_preserved() {
    let content = "中文\u{feff}正文";
    let chunk = serde_json::json!({"choices":[{"delta":{
        "content":content, "reasoning_content":"\u{feff}思考"
    }}]});
    for prefix in ["\u{feff}", "\u{feff}: keepalive\r\n\r\n"] {
        let body = format!("{prefix}data: {chunk}\r\n\r\ndata: [DONE]\r\n\r\n");
        let events = reader_events(SingleByteReader(body.as_bytes()));
        assert_eq!(events.len(), 2, "{events:?}");
        assert!(matches!(&events[0], Event::Delta { content: c, reasoning }
            if c == content && reasoning == "\u{feff}思考"));
        assert!(matches!(events[1], Event::Done { tool_calls: false }));
    }
    for body in [
        "data: \u{feff}{}\n\ndata: [DONE]\n\n",
        "data: [DONE]\u{feff}\n\n",
        "\u{feff}\u{feff}data: [DONE]\n\n",
        ": first\n\u{feff}data: [DONE]\n\n",
    ] {
        assert_failed(&reader_events(SingleByteReader(body.as_bytes())));
    }
}

#[test]
fn sse_multiline_json_comments_crlf_and_empty_fields() {
    let body = concat!(
        ": heartbeat\r\n\r\nevent: message\r\nid: 1\r\n",
        "data\r\ndata: {\r\n: between data lines\r\n",
        "data: \"choices\": [\r\nretry: 1000\r\n",
        "data:{\"delta\": {\"content\": \"你好\",\r\n",
        "data: \"reasoning_content\": \"思考\"}}]}\r\ndata:\r\n\r\n",
        ": heartbeat\r\n\r\ndata: [DONE]\r\n\r\n"
    );
    let events = reader_events(SingleByteReader(body.as_bytes()));
    assert_eq!(events.len(), 2, "{events:?}");
    assert!(matches!(&events[0], Event::Delta { content, reasoning }
        if content == "你好" && reasoning == "思考"));
    assert!(matches!(events[1], Event::Done { tool_calls: false }));
    // 每个 data 值必须由换行连接，不能直接拼接或按行独立解析。
    for body in [
        "data: {\"choices\":[{\"delta\":{\"content\":\"a\ndata: b\"}}]}\n\ndata: [DONE]\n\n",
        "data: {}\ndata: {}\n\ndata: [DONE]\n\n",
        "data: [DONE]\ndata: {}\n\n",
    ] {
        assert_failed(&reader_events(body.as_bytes()));
    }
}

#[test]
fn sse_data_removes_only_one_ascii_space() {
    for value in ["[DONE]", " [DONE]"] {
        let events = reader_events(format!("data:{value}\n\n").as_bytes());
        assert!(matches!(events.last(), Some(Event::Done { tool_calls: false })));
    }
    for value in ["  [DONE]", "\t[DONE]", "\u{a0}[DONE]", " [DONE] ", " [DONE]\t"] {
        assert_failed(&reader_events(format!("data:{value}\n\n").as_bytes()));
    }
}

#[test]
fn sse_eof_dispatch_keeps_finish_reason_and_tool_safety() {
    for ending in ["", "\n", "\r\n", "\n\n"] {
        let events = reader_events(format!("data: [DONE]{ending}").as_bytes());
        assert!(matches!(events.last(), Some(Event::Done { tool_calls: false })));
        for reason in ["stop", "tool_calls"] {
            for args in ["{}", "{", "", "[]", "{}{}"] {
                let chunk = serde_json::json!({"choices":[{"delta":{"tool_calls":[
                    {"function":{"name":"read_file","arguments":args}}
                ]},"finish_reason":reason}]});
                let body = format!("data: {{\ndata: \"choices\":{} }}{ending}", chunk["choices"]);
                let events = reader_events(body.as_bytes());
                if args == "{}" {
                    assert!(matches!(events.last(), Some(Event::Done { tool_calls: true })), "{events:?}");
                } else {
                    assert_failed(&events);
                }
            }
        }
        let body = format!("data: {{\"choices\":[{{\"delta\":{{\"content\":\"partial\"}}}}]}}{ending}");
        assert_failed(&reader_events(body.as_bytes()));
        assert_failed(&reader_events(format!("data: {{\ndata: \"choices\": [{ending}").as_bytes()));
    }
}

#[test]
fn sse_multiline_event_limit_includes_joined_newlines() {
    let half = " ".repeat(MAX_EVENT_BYTES / 2);
    for extra in [0, 1] {
        // 2 字节 JSON + 两行间换行 + padding，恰好上限时仍可解析。
        let body = format!("data: {{}}{half}\ndata: {}\n\ndata: [DONE]\n\n",
            " ".repeat(MAX_EVENT_BYTES / 2 - 3 + extra));
        let events = reader_events(body.as_bytes());
        if extra == 0 {
            assert!(matches!(events.last(), Some(Event::Done { tool_calls: false })), "{events:?}");
        } else {
            assert_failed(&events);
            assert!(matches!(events.last(), Some(Event::Failed(error)) if error.contains("事件过长")));
        }
    }
    let body = format!("{}data: [DONE]\n\n", format!("data: {half}\n").repeat(3));
    assert_failed(&reader_events(body.as_bytes()));
}

#[test]
fn sse_line_stream_and_output_event_limits_remain_enforced() {
    let events = reader_events("x".repeat(MAX_LINE_BYTES as usize + 1).as_bytes());
    assert_failed(&events);
    assert!(matches!(events.last(), Some(Event::Failed(error)) if error.contains("行过长")));
    let comment = format!(":{}\n", "x".repeat(1022));
    let events = reader_events(comment.repeat(MAX_STREAM_BYTES / comment.len() + 1).as_bytes());
    assert_failed(&events);
    assert!(matches!(events.last(), Some(Event::Failed(error)) if error.contains("累计字节或事件数")));
    let calls: Vec<_> = (0..MAX_TOOL_CALLS).map(|i|
        serde_json::json!({"index":i,"function":{"arguments":" "}})
    ).collect();
    let chunk = serde_json::json!({"choices":[{"delta":{"tool_calls":calls}}]});
    let body = format!("{}data: [DONE]\n\n",
        format!("data: {chunk}\n\n").repeat(MAX_STREAM_EVENTS / (MAX_TOOL_CALLS + 1) + 1));
    let events = reader_events(body.as_bytes());
    assert_failed(&events);
    assert!(matches!(events.last(), Some(Event::Failed(error)) if error.contains("累计超过预算")));
}

#[test]
fn sse_pending_event_is_not_dispatched_on_failure_disconnect_or_cancel() {
    for end in [None, Some(LineEvent::Failed("mock reset".into()))] {
        let (line_tx, line_rx) = mpsc::channel();
        line_tx.send(LineEvent::Line("data: [DONE]\n".into())).unwrap();
        if let Some(end) = end {
            line_tx.send(end).unwrap();
        }
        drop(line_tx);
        let (tx, rx) = mpsc::channel();
        consume_lines(line_rx, &tx, &AtomicBool::new(false));
        drop(tx);
        assert_failed(&rx.into_iter().collect::<Vec<_>>());
    }
    let (line_tx, line_rx) = mpsc::channel();
    line_tx.send(LineEvent::Line("data: [DONE]\n\n".into())).unwrap();
    let (tx, rx) = mpsc::channel();
    consume_lines(line_rx, &tx, &AtomicBool::new(true));
    drop(tx);
    assert!(rx.into_iter().next().is_none());
}

#[test]
fn reader_eof_without_terminator_fails() {
    for body in [
        "",
        "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n",
    ] {
        assert_failed(&reader_events(body.as_bytes()));
    }
    let (line_tx, line_rx) = mpsc::channel();
    drop(line_tx);
    let (tx, rx) = mpsc::channel();
    consume_lines(line_rx, &tx, &AtomicBool::new(false));
    drop(tx);
    assert_failed(&rx.into_iter().collect::<Vec<_>>());
}

#[test]
fn reader_accepts_either_normal_terminator() {
    for tail in [
        "data: [DONE]\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
    ] {
        let events = reader_events(tail.as_bytes());
        assert!(matches!(
            events.last(),
            Some(Event::Done { tool_calls: false })
        ));
    }
}

#[test]
fn incomplete_tools_never_complete() {
    for args in ["{", "{}"] {
        let chunk = serde_json::json!({"choices":[{"delta":{"tool_calls":[
            {"index":0,"function":{"name":"bash","arguments":args}}
        ]}}]});
        let body = format!("data: {chunk}\n\n");
        assert_failed(&reader_events(body.as_bytes()));
        for reason in ["length", "content_filter"] {
            let tail = format!("data: {{\"choices\":[{{\"finish_reason\":\"{reason}\"}}]}}\n\ndata: [DONE]\n\n");
            assert_failed(&reader_events(format!("{body}{tail}").as_bytes()));
        }
        for tail in [
            "data: [DONE]\n\n",
            "data: {\"choices\":[{\"finish_reason\":\"tool_calls\"}]}\n\n",
        ] {
            let events = reader_events(format!("{body}{tail}").as_bytes());
            if args == "{}" {
                assert!(matches!(
                    events.last(),
                    Some(Event::Done { tool_calls: true })
                ));
            } else {
                assert_failed(&events);
            }
        }
    }
    let unnamed = b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"function\":{\"arguments\":\"{}\"}}]}}]}\n\ndata: [DONE]\n\n";
    assert_failed(&reader_events(&unnamed[..]));
}

#[test]
fn duplicate_tool_ids_never_emit_executable_done() {
    for tail in ["data: [DONE]\n", "data: {\"choices\":[{\"finish_reason\":\"tool_calls\"}]}\n"] {
        let chunk = serde_json::json!({"choices":[{"delta":{"tool_calls":[
            {"index":0,"id":"dup","function":{"name":"read_file","arguments":"{}"}},
            {"index":1,"id":"dup","function":{"name":"read_file","arguments":"{}"}}
        ]}}]});
        let events = reader_events(format!("data: {chunk}\n\n{tail}").as_bytes());
        assert_failed(&events);
        assert!(matches!(events.last(), Some(Event::Failed(error)) if error.contains("ID 重复")));
    }
}

#[test]
fn missing_tool_ids_avoid_explicit_and_fallback_collisions() {
    let args = "{ \"path\": \"a.txt\" }";
    let chunk = serde_json::json!({"choices":[{"delta":{"tool_calls":[
        {"index":3,"id":"call_1_1","function":{"name":"read_file","arguments":args}},
        {"index":2,"function":{"name":"read_file","arguments":args}},
        {"index":1,"function":{"name":"read_file","arguments":args}},
        {"index":0,"id":"call_1","function":{"name":"read_file","arguments":args}}
    ]}}]});
    let events = reader_events(format!("data: {chunk}\n\ndata: [DONE]\n").as_bytes());
    assert!(matches!(events.last(), Some(Event::Done { tool_calls: true })));
    let frags: Vec<_> = events.into_iter().filter_map(|e| match e { Event::ToolCall(f) => Some(f), _ => None }).collect();
    let calls = assemble(&frags);
    assert_eq!(calls.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["call_1", "call_1_2", "call_2", "call_1_1"]);
    assert!(calls.iter().all(|c| c.arguments == args));
}

#[test]
fn empty_finish_reason_is_not_a_terminator() {
    for reason in [
        serde_json::Value::Null,
        serde_json::json!(""),
        serde_json::json!(" "),
    ] {
        let chunk = serde_json::json!({"choices":[{"delta":{"content":"partial"},"finish_reason":reason}]});
        let body = format!("data: {chunk}\n\n");
        assert_failed(&reader_events(body.as_bytes()));
        let events = reader_events(format!("{body}data: [DONE]\n\n").as_bytes());
        assert!(
            matches!(events.first(), Some(Event::Delta { content, .. }) if content == "partial")
        );
        assert!(matches!(
            events.last(),
            Some(Event::Done { tool_calls: false })
        ));
    }
}

#[test]
fn tool_completion_accepts_stop_but_never_non_object_arguments() {
    for args in ["{}", "", "null", "[]", "{", "{}{}"] {
        let chunk = serde_json::json!({"choices":[{"delta":{"tool_calls":[
            {"index":0,"function":{"name":"bash","arguments":args}}
        ]},"finish_reason":"stop"}]});
        let events = reader_events(format!("data: {chunk}\n\n").as_bytes());
        if args == "{}" {
            assert!(matches!(
                events.last(),
                Some(Event::Done { tool_calls: true })
            ));
        } else {
            assert_failed(&events);
        }
    }
}

#[test]
fn reader_io_failure_is_not_done() {
    struct BrokenReader;
    impl Read for BrokenReader {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "mock reset",
            ))
        }
    }
    assert_failed(&reader_events(BrokenReader));
}

#[test]
fn local_http_eof_and_normal_completion() {
    use std::io::Write;
    use std::net::TcpListener;
    for terminated in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = BufReader::new(socket.try_clone().unwrap());
            loop {
                let mut line = String::new();
                if request.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                    break;
                }
            }
            let body = if terminated {
                "data: [DONE]\n\n"
            } else {
                "data: {}\n\n"
            };
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        });
        let response = reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap()
            .get(format!("http://{address}/"))
            .send()
            .unwrap();
        let events = reader_events(response);
        server.join().unwrap();
        if terminated {
            assert!(matches!(
                events.last(),
                Some(Event::Done { tool_calls: false })
            ));
        } else {
            assert_failed(&events);
        }
    }
}

#[test]
fn parses_openai_style_delta() {
    let payload = r#"{"id":"1","choices":[{"delta":{"content":"你好"}}]}"#;
    let c = parse_chunk(payload).unwrap();
    assert_eq!(c.content, "你好");
    assert_eq!(c.reasoning, "");
    assert!(c.tool_calls.is_empty());
}

#[test]
fn parses_reasoning_delta() {
    let payload = r#"{"choices":[{"delta":{"reasoning_content":"先想想"}}]}"#;
    let c = parse_chunk(payload).unwrap();
    assert_eq!(c.content, "");
    assert_eq!(c.reasoning, "先想想");
}

#[test]
fn ignores_usage_only_chunk() {
    let payload = r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#;
    let c = parse_chunk(payload).unwrap();
    assert!(c.content.is_empty() && c.reasoning.is_empty() && c.tool_calls.is_empty());
    assert_eq!(c.finish_reason.as_deref(), Some("stop"));
}

#[test]
fn surfaces_server_error() {
    let payload = r#"{"error":{"message":"Authentication Fails"}}"#;
    assert_eq!(
        parse_chunk(payload).unwrap_err(),
        "Authentication Fails".to_owned()
    );
}

#[test]
fn parses_tool_call_fragments() {
    // 真实协议里首片带 id/name，后续片只有 arguments 增量。
    let first = r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"read_file","arguments":"{\"pa"}}]}}]}"#;
    let second = r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"th\":\"a.txt\"}"}}]},"finish_reason":"tool_calls"}]}"#;

    let a = parse_chunk(first).unwrap();
    assert_eq!(a.tool_calls.len(), 1);
    assert_eq!(a.tool_calls[0].index, 0);
    assert_eq!(a.tool_calls[0].id.as_deref(), Some("call_1"));
    assert_eq!(a.tool_calls[0].name.as_deref(), Some("read_file"));

    let b = parse_chunk(second).unwrap();
    assert_eq!(b.tool_calls[0].index, 0);
    assert!(b.tool_calls[0].id.is_none());
    assert_eq!(b.finish_reason.as_deref(), Some("tool_calls"));

    let calls = assemble(&[a.tool_calls[0].clone(), b.tool_calls[0].clone()]);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "read_file");
    assert_eq!(calls[0].arguments, r#"{"path":"a.txt"}"#);
    assert_eq!(
        calls[0].parse_arguments().unwrap()["path"],
        serde_json::json!("a.txt")
    );
}

#[test]
fn assemble_handles_parallel_calls_and_missing_ids() {
    let calls = assemble(&[
        ToolCallFrag {
            index: 0,
            id: Some("a".into()),
            name: Some("read_file".into()),
            args: "{}".into(),
        },
        ToolCallFrag {
            index: 1,
            id: None,
            name: Some("bash".into()),
            args: "{}".into(),
        },
    ]);
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].id, "a");
    assert_eq!(calls[1].id, "call_1", "缺 id 时补合成 id，保证可配对");
}

#[test]
fn bad_arguments_are_reported_not_panicked() {
    let call = ToolCall {
        id: "1".into(),
        name: "read_file".into(),
        arguments: "{not json".into(),
    };
    assert!(call.parse_arguments().is_err());
}

#[test]
fn poll_distinguishes_empty_from_finished() {
    let (tx, rx) = mpsc::channel::<Event>();
    let stream = Stream::new_for_test(rx);
    // 空 Vec ≠ 结束：只是暂时没有新事件。
    assert!(stream.poll().is_empty());
    assert!(!stream.is_finished());
    drop(tx); // 线程结束（正常或 panic 都一样断开）
    assert!(stream.poll().is_empty());
    assert!(stream.is_finished(), "发送端断开后必须能感知");
}

#[test]
fn astronomical_tool_call_index_is_rejected() {
    // assemble 按 index 扩容槽位：天文序号 = 远程 DoS，解析入口必须拒掉。
    let payload = r#"{"choices":[{"delta":{"tool_calls":[{"index":1000000000,"function":{"name":"bash","arguments":"{}"}}]}}]}"#;
    let err = parse_chunk(payload).unwrap_err();
    assert!(err.contains("越界"), "{err}");

    // 边界内（含缺省兜底）仍然正常。
    let ok = parse_chunk(
        r#"{"choices":[{"delta":{"tool_calls":[{"index":3,"function":{"name":"bash","arguments":"{"}},{"function":{"arguments":"}"}}]}}]}"#,
    )
    .unwrap();
    assert_eq!(ok.tool_calls.len(), 2);
    assert_eq!(ok.tool_calls[0].index, 3);
    assert_eq!(ok.tool_calls[1].index, 1, "缺 index 时用数组下标兜底");
}

#[test]
fn wire_message_carries_tool_plumbing() {
    let msg = Msg::tool_result("call_1", r#"{"ok":true}"#);
    assert_eq!(msg.role.as_str(), "tool");
    assert_eq!(msg.tool_call_id.as_deref(), Some("call_1"));

    let msg = Msg::assistant_with_tools(
        "",
        vec![ToolCall {
            id: "call_1".into(),
            name: "bash".into(),
            arguments: "{}".into(),
        }],
    );
    let wire = serde_json::to_value(WireMessage {
        role: msg.role.as_str(),
        content: wire_content(&msg),
        tool_calls: msg
            .tool_calls
            .iter()
            .map(|c| WireToolCall {
                id: &c.id,
                kind: "function",
                function: WireFunction {
                    name: &c.name,
                    arguments: &c.arguments,
                },
            })
            .collect(),
        tool_call_id: None,
        reasoning_content: None,
    })
    .unwrap();
    assert_eq!(wire["tool_calls"][0]["id"], "call_1");
    assert_eq!(wire["tool_calls"][0]["type"], "function");
    assert_eq!(wire["tool_calls"][0]["function"]["name"], "bash");
    assert!(wire.get("tool_call_id").is_none(), "空字段不序列化");
}

/// **回归测试**：`tools` 必须是扁平的函数数组。
///
/// 曾经写成 `vec![openai_tools()]`（而 `openai_tools()` 本身已是数组），
/// 于是线上请求体成了 `tools: [[…]]`，服务端直接
/// `422 tools[0][0].function: invalid type: map, expected unit`。
#[test]
fn wire_tools_are_a_flat_array_of_functions() {
    let cfg = Config::deepseek("sk-x");
    let msgs = vec![Msg::new(Role::User, "hi")];
    let tools = neo_tools::tool_declarations();
    assert_eq!(tools.len(), neo_tools::registry().len());

    let body = serde_json::to_value(build_wire(&cfg, &msgs, tools)).unwrap();
    let arr = body["tools"].as_array().expect("tools 应是数组");
    assert_eq!(arr.len(), neo_tools::registry().len());
    for (i, t) in arr.iter().enumerate() {
        assert!(t.is_object(), "tools[{i}] 必须是单个工具对象，不能是数组");
        assert_eq!(t["type"], "function", "tools[{i}] 缺 type=function");
        assert!(t["function"]["name"].is_string(), "tools[{i}] 缺函数名");
        assert!(
            t["function"]["parameters"]["properties"].is_object(),
            "tools[{i}] 缺 JSON Schema"
        );
    }
    // 注册表里的工具都要出现 —— 数量跟着注册表走，别写死：
    // 写死只会变成"加一个工具就要改一次测试"，那是文档同步测试该管的事。
    assert_eq!(
        arr.len(),
        neo_tools::registry().len(),
        "工具数量与注册表不一致：{:?}",
        neo_tools::tool_names()
    );
    assert!(arr.len() >= 6);

    // ---- 服务端视角的复核 ----
    // 下面这两个结构体只描述服务端**期待**的形状。把我们的请求体反序列化
    // 进去，等价于在本地跑一遍服务端的解析 —— 嵌套数组会在这里失败，
    // 报错形状与线上那条 422 一致（`invalid type: map, expected unit`）。
    #[derive(serde::Deserialize)]
    struct ServerTool {
        #[serde(rename = "type")]
        kind: String,
        function: ServerFunction,
    }
    #[derive(serde::Deserialize)]
    struct ServerFunction {
        name: String,
        #[allow(dead_code)]
        description: String,
        parameters: serde_json::Value,
    }

    let parsed: Vec<ServerTool> = serde_json::from_value(body["tools"].clone())
        .expect("服务端形状解析失败：多半是 tools 被多包了一层数组");
    assert_eq!(parsed.len(), neo_tools::registry().len());
    for t in &parsed {
        assert_eq!(t.kind, "function");
        assert!(!t.function.name.is_empty());
        // properties 必须是对象；空不空跟参数声明走 —— 无参数工具
        // （open_app）的空 properties 是合法 schema，不是拼错。
        let props = t.function.parameters["properties"]
            .as_object()
            .unwrap_or_else(|| panic!("{} 缺 properties", t.function.name));
        let declared = neo_tools::find(&t.function.name).unwrap();
        assert_eq!(
            props.is_empty(),
            declared.params.is_empty(),
            "{} 的 properties 与参数声明不符",
            t.function.name
        );
    }
}

// ---- 多模态（图片输入） ----

/// 没有图的消息**仍然是纯字符串** —— 别让多模态改动波及最常见的路径。
#[test]
fn messages_without_images_keep_plain_string_content() {
    let body = wire_json(Thinking::Model, Vec::new(), &[Msg::new(Role::User, "你好")]);
    assert_eq!(body["messages"][0]["content"], "你好");
    assert!(
        body["messages"][0]["content"].is_string(),
        "无图时 content 必须是字符串：{body}"
    );
}

/// **服务端视角**：带图时 content 是 blocks 数组，且 tag 与字段名都对得上。
#[test]
fn message_with_image_becomes_content_blocks() {
    #[derive(serde::Deserialize)]
    struct ServerMsg {
        /// 只用来核对角色，不读取内容。
        #[allow(dead_code)]
        role: String,
        content: Vec<ServerPart>,
    }
    #[derive(serde::Deserialize, Debug)]
    #[serde(tag = "type")]
    enum ServerPart {
        #[serde(rename = "text")]
        Text { text: String },
        #[serde(rename = "image_url")]
        Image { image_url: ServerImageUrl },
    }
    #[derive(serde::Deserialize, Debug)]
    struct ServerImageUrl {
        url: String,
    }

    let url = "data:image/png;base64,AAA=";
    let msgs = vec![Msg::new(Role::User, "看看这张图").with_image(url)];
    let body = wire_json(Thinking::Model, Vec::new(), &msgs);
    let parsed: Vec<ServerMsg> =
        serde_json::from_value(body["messages"].clone()).expect("content blocks 形状");
    let parts = &parsed[0].content;
    assert_eq!(parts.len(), 2);
    match &parts[0] {
        ServerPart::Text { text } => assert_eq!(text, "看看这张图"),
        other => panic!("第一块应当是 text：{other:?}"),
    }
    match &parts[1] {
        ServerPart::Image { image_url } => assert_eq!(image_url.url, url),
        _ => panic!("第二块应当是 image_url"),
    }
}

/// 只带图、没有文字时不能凭空多出一个空的 text 块。
#[test]
fn image_only_message_has_no_empty_text_block() {
    let msgs = vec![Msg::new(Role::User, "").with_image("data:image/png;base64,AAA=")];
    let body = wire_json(Thinking::Model, Vec::new(), &msgs);
    let parts = body["messages"][0]["content"].as_array().unwrap();
    assert_eq!(parts.len(), 1, "只该有图块：{body}");
    assert_eq!(parts[0]["type"], "image_url");
}

/// 工具结果也能带图（截屏 / 看图之后把图交给模型的那条路）。
#[test]
fn tool_result_can_carry_an_image() {
    let msgs =
        vec![Msg::tool_result("call_1", r#"{"ok":true}"#)
            .with_image("data:image/png;base64,AAA=")];
    let body = wire_json(Thinking::Model, Vec::new(), &msgs);
    let msg = &body["messages"][0];
    assert_eq!(msg["role"], "tool");
    assert_eq!(msg["tool_call_id"], "call_1");
    assert!(msg["content"].is_string());
    assert_eq!(body["messages"][1]["role"], "user");
    assert_eq!(body["messages"][1]["content"][2]["type"], "image_url");
}

/// 空 data URL 不该被塞进请求（多发一个空字段只会让服务端困惑）。
#[test]
fn empty_image_url_is_ignored() {
    let msgs = vec![Msg::new(Role::User, "hi").with_image("")];
    let body = wire_json(Thinking::Model, Vec::new(), &msgs);
    assert!(body["messages"][0]["content"].is_string(), "{body}");
}

// ---- 思考模式 ----

/// 组装一次请求体，返回 JSON（测试用）。
fn wire_json(
    thinking: Thinking,
    tools: Vec<serde_json::Value>,
    msgs: &[Msg],
) -> serde_json::Value {
    let cfg = Config {
        thinking,
        ..Config::deepseek("k")
    };
    serde_json::to_value(build_wire(&cfg, msgs, tools)).unwrap()
}

/// 默认档「跟随模型」**不发送任何思考字段** —— 与加这个功能之前的行为一致。
#[test]
fn thinking_model_sends_no_control_fields() {
    let body = wire_json(Thinking::Model, Vec::new(), &[Msg::new(Role::User, "hi")]);
    assert!(body.get("thinking").is_none(), "不该发 thinking：{body}");
    assert!(
        body.get("reasoning_effort").is_none(),
        "不该发 effort：{body}"
    );
}

/// 关闭思考只发开关、不发强度（发了也是同一个效果）。
#[test]
fn thinking_off_sends_only_the_toggle() {
    let body = wire_json(Thinking::Off, Vec::new(), &[Msg::new(Role::User, "hi")]);
    assert_eq!(body["thinking"]["type"], "disabled");
    assert!(body.get("reasoning_effort").is_none());
}

/// 三档强度：开关开 + 强度值分别是 low/high/max。
#[test]
fn thinking_levels_map_to_low_high_max() {
    for (level, want) in [
        (Thinking::Low, "low"),
        (Thinking::High, "high"),
        (Thinking::Max, "max"),
    ] {
        let body = wire_json(level, Vec::new(), &[Msg::new(Role::User, "hi")]);
        assert_eq!(body["thinking"]["type"], "enabled", "{level:?}");
        assert_eq!(body["reasoning_effort"], want, "{level:?}");
    }
}

/// **服务端视角**：带 `tools` 时，历史推理必须原样回传，否则 400。
#[test]
fn reasoning_is_passed_back_when_tools_are_present() {
    #[derive(serde::Deserialize)]
    struct ServerMsg {
        role: String,
        #[serde(default)]
        reasoning_content: Option<String>,
    }

    let msgs = vec![
        Msg::new(Role::User, "看一下 a.txt"),
        Msg::assistant_with_tools("我查一下", Vec::new()).with_reasoning("用户要看 a.txt"),
    ];

    // 带 tools → 回传
    let with_tools = wire_json(
        Thinking::High,
        vec![serde_json::json!({"type": "function"})],
        &msgs,
    );
    let parsed: Vec<ServerMsg> =
        serde_json::from_value(with_tools["messages"].clone()).expect("messages 形状");
    let assistant = parsed.iter().find(|m| m.role == "assistant").unwrap();
    assert_eq!(
        assistant.reasoning_content.as_deref(),
        Some("用户要看 a.txt")
    );

    // 不带 tools → 服务端会忽略，我们干脆不发
    let no_tools = wire_json(Thinking::High, Vec::new(), &msgs);
    let parsed: Vec<ServerMsg> =
        serde_json::from_value(no_tools["messages"].clone()).expect("messages 形状");
    let assistant = parsed.iter().find(|m| m.role == "assistant").unwrap();
    assert!(assistant.reasoning_content.is_none());
}

/// 明确关闭思考时，历史推理也不再回传（上下文不该自相矛盾）。
#[test]
fn reasoning_is_dropped_when_thinking_is_off() {
    let msgs = vec![
        Msg::new(Role::User, "a"),
        Msg::new(Role::Assistant, "b").with_reasoning("想过"),
    ];
    let body = wire_json(
        Thinking::Off,
        vec![serde_json::json!({"type": "function"})],
        &msgs,
    );
    let raw = body.to_string();
    assert!(
        !raw.contains("reasoning_content"),
        "关闭思考后不该回传：{raw}"
    );
}

/// 空推理不该发出去（`reasoning_content: ""` 是没意义的字段）。
#[test]
fn empty_reasoning_is_never_sent() {
    let msgs = vec![Msg::new(Role::Assistant, "b").with_reasoning("")];
    let body = wire_json(
        Thinking::High,
        vec![serde_json::json!({"type": "function"})],
        &msgs,
    );
    assert!(!body.to_string().contains("reasoning_content"));
}

/// 加了思考字段之后，`tools` 仍然是**扁平数组** —— 别把上次那个 422 弄回来。
#[test]
fn thinking_does_not_change_tools_shape() {
    #[derive(serde::Deserialize)]
    struct ServerTool {
        #[serde(rename = "type")]
        #[allow(dead_code)]
        kind: String,
        #[allow(dead_code)]
        function: serde_json::Value,
    }
    let body = wire_json(
        Thinking::Max,
        neo_tools::tool_declarations(),
        &[Msg::new(Role::User, "hi")],
    );
    let tools: Vec<ServerTool> =
        serde_json::from_value(body["tools"].clone()).expect("tools 必须是扁平函数数组");
    assert!(!tools.is_empty());
    assert_eq!(body["reasoning_effort"], "max");
}

/// 档位的界面名 ↔ 落库键 必须一一对应，不能有重复键。
#[test]
fn thinking_keys_round_trip_and_are_unique() {
    let mut keys: Vec<&str> = Thinking::ALL.iter().map(|t| t.key()).collect();
    let count = keys.len();
    keys.sort_unstable();
    keys.dedup();
    assert_eq!(keys.len(), count, "档位键重复了");
    for t in Thinking::ALL {
        assert_eq!(Thinking::from_key(t.key()), t, "{t:?} 往返失败");
    }
    assert_eq!(Thinking::from_key("没见过的值"), Thinking::Model);
}

/// 把当时那个错误写法**钉在测试里**：嵌套数组服务端解析不出来。
///
/// 这条不是为了测 `build_wire`，而是为了让"为什么不能多包一层"这件事
/// 有一个可执行的解释 —— 后人想改回去时会先看到它失败。
#[test]
fn nested_tools_shape_is_rejected() {
    #[derive(serde::Deserialize)]
    struct ServerTool {
        #[serde(rename = "type")]
        #[allow(dead_code)]
        kind: String,
        #[allow(dead_code)]
        function: serde_json::Value,
    }

    // 出事的形状：tools = [[{…}]]（`openai_tools()` 本身已是数组，又套了一层）
    let nested = vec![neo_tools::openai_tools()];
    let body = serde_json::to_value(build_wire(
        &Config::deepseek("k"),
        &[Msg::new(Role::User, "hi")],
        nested,
    ))
    .unwrap();
    assert!(
        serde_json::from_value::<Vec<ServerTool>>(body["tools"].clone()).is_err(),
        "嵌套数组必须解析失败 —— 这正是线上那条 422"
    );
}

/// 工具对话的消息形状（服务端视角）。
///
/// 两个最容易写错、且一写错就 422 的点，这里都用**严格结构体**钉住：
/// 1. `function.arguments` 必须是**字符串**（不是 JSON 对象）；
/// 2. `role=tool` 必须带 `tool_call_id`，且能对上前面 assistant 的调用 id。
#[test]
fn wire_tool_conversation_matches_server_shape() {
    #[derive(serde::Deserialize)]
    struct ServerCallFn {
        name: String,
        /// 注意类型是 String —— 服务端期待字符串化的 JSON
        arguments: String,
    }
    #[derive(serde::Deserialize)]
    struct ServerCall {
        id: String,
        #[serde(rename = "type")]
        #[allow(dead_code)]
        kind: String,
        function: ServerCallFn,
    }
    #[derive(serde::Deserialize)]
    struct ServerMsg {
        role: String,
        #[allow(dead_code)]
        content: String,
        #[serde(default)]
        tool_calls: Vec<ServerCall>,
        #[serde(default)]
        tool_call_id: Option<String>,
    }

    let msgs = vec![
        Msg::new(Role::User, "读一下 a.txt"),
        Msg::assistant_with_tools(
            "",
            vec![ToolCall {
                id: "call_1".into(),
                name: "read_file".into(),
                arguments: r#"{"path":"a.txt"}"#.into(),
            }],
        ),
        Msg::tool_result("call_1", r#"{"ok":true}"#),
    ];
    let body = serde_json::to_value(build_wire(
        &Config::deepseek("k"),
        &msgs,
        neo_tools::tool_declarations(),
    ))
    .unwrap();

    let parsed: Vec<ServerMsg> =
        serde_json::from_value(body["messages"].clone()).expect("消息形状不符合服务端期待");
    assert_eq!(parsed.len(), 3);
    assert_eq!(parsed[1].role, "assistant");
    assert_eq!(parsed[1].tool_calls.len(), 1);
    assert_eq!(parsed[1].tool_calls[0].id, "call_1");
    assert_eq!(parsed[1].tool_calls[0].kind, "function");
    assert_eq!(parsed[1].tool_calls[0].function.name, "read_file");
    // 参数是字符串，且能再解析成对象
    let args: serde_json::Value =
        serde_json::from_str(&parsed[1].tool_calls[0].function.arguments).unwrap();
    assert_eq!(args["path"], "a.txt");

    assert_eq!(parsed[2].role, "tool");
    assert_eq!(parsed[2].tool_call_id.as_deref(), Some("call_1"));
    assert_eq!(
        parsed[1].tool_calls[0].id,
        parsed[2].tool_call_id.clone().unwrap()
    );
    // 普通消息不该凭空长出 tool 字段
    assert!(parsed[0].tool_calls.is_empty());
    assert!(parsed[0].tool_call_id.is_none());
}

/// 没有工具时，`tools` 字段必须整个不出现（不支持 function calling 的服务端
/// 不会因为一个空数组报错）。
#[test]
fn wire_omits_tools_when_empty() {
    let cfg = Config::deepseek("sk-x");
    let body =
        serde_json::to_value(build_wire(&cfg, &[Msg::new(Role::User, "hi")], vec![])).unwrap();
    assert!(body.get("tools").is_none());
}

#[test]
fn config_gate() {
    let mut cfg = Config::deepseek("");
    assert!(!cfg.is_configured());
    cfg.api_key = "sk-x".to_owned();
    assert!(cfg.is_configured());
}
