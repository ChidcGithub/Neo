use super::*;
use serde_json::json;

// Kept in tracked tests: the intentionally ignored local docs must not be build dependencies.
const MANIFEST: &str = r#"{
  "api_version": 1,
  "id": "org.example",
  "revision": 1,
  "executable": "bin/example.exe",
  "capabilities": ["workspace_read"],
  "limits": {
    "timeout_ms": 5000,
    "memory_mib": 64,
    "max_in_flight": 1,
    "max_message_bytes": 65536,
    "max_result_bytes": 32768
  },
  "tools": [{
    "name": "echo",
    "description": "回显显式提供的文本（仅合约示例）",
    "input_schema": {
      "type": "object",
      "properties": {"text": {"type": "string"}},
      "required": ["text"],
      "additionalProperties": false
    },
    "output_schema": {
      "type": "object",
      "properties": {"text": {"type": "string"}},
      "required": ["text"],
      "additionalProperties": false
    }
  }]
}"#;
const ENVELOPES: &str = r#"[
  {"type":"handshake","api_version":1,"mod_id":"org.example"},
  {"type":"request","api_version":1,"id":"r1","tool":"mod_org_example__echo","params":{"text":"你好"}},
  {"type":"result","api_version":1,"id":"r1","tool":"mod_org_example__echo","outcome":{"status":"success","data":{"text":"你好"}}},
  {"type":"result","api_version":1,"id":"r1","tool":"mod_org_example__echo","outcome":{"status":"error","code":"cancelled"}},
  {"type":"cancel","api_version":1,"id":"r1"}
]"#;
fn manifest() -> ValidatedManifest {
    ValidatedManifest::parse(MANIFEST.as_bytes()).unwrap()
}
fn raw() -> Manifest {
    manifest().manifest().clone()
}
fn wire() -> Vec<Value> {
    serde_json::from_str(ENVELOPES).unwrap()
}
fn bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap()
}
fn parse_message(value: &Value) -> Check<Message> {
    Message::parse(&bytes(value), &manifest())
}

#[test]
fn examples_roundtrip_and_correlate() {
    let m = manifest();
    let messages: Vec<_> = wire().iter().map(|v| parse_message(v).unwrap()).collect();
    for (message, expected) in messages.iter().zip(wire()) {
        let encoded = message.to_bytes(&m).unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&encoded).unwrap(), expected);
        Message::parse(&encoded, &m).unwrap();
    }
    for reply in &messages[2..] {
        reply.validate_reply_to(&messages[1], &m).unwrap();
    }
    assert_eq!(m.manifest().id, "org.example");
    assert_eq!(m.manifest().capabilities, vec![Capability::WorkspaceRead]);
}

#[test]
fn strict_bounded_manifest_parser() {
    for invalid in [b"".as_slice(), b"null", b"[]", b"{}", b"\xff", b"{}{}"] {
        assert!(ValidatedManifest::parse(invalid).is_err());
    }
    assert!(ValidatedManifest::parse(&vec![b' '; MAX_MANIFEST_BYTES + 1]).is_err());
    let padded = format!(
        "{}{}",
        MANIFEST,
        " ".repeat(MAX_MANIFEST_BYTES - MANIFEST.len())
    );
    ValidatedManifest::parse(padded.as_bytes()).unwrap();
    for replacement in [
        "\"api_version\": 2",
        "\"api_version\": 0",
        "\"api_version\": 1.0",
        "\"api_version\": 1, \"api_version\": 1",
        "\"api_version\": 1, \"secret\": \"x\"",
    ] {
        assert!(ValidatedManifest::parse(
            MANIFEST
                .replace("\"api_version\": 1", replacement)
                .as_bytes()
        )
        .is_err());
    }
    for (from, to) in [
        ("\"revision\": 1", "\"revision\": 0"),
        ("\"workspace_read\"", "\"unknown_capability\""),
        (
            "\"workspace_read\"",
            "\"workspace_read\", \"workspace_read\"",
        ),
        (
            "\"type\": \"string\"",
            "\"type\": \"string\", \"type\": \"string\"",
        ),
        (
            "\"type\": \"string\"",
            "\"type\": \"string\", \"pattern\": \".*\"",
        ),
        ("\"type\": \"string\"", "\"type\": \"array\""),
        (
            "\"additionalProperties\": false",
            "\"additionalProperties\": true",
        ),
        (
            "\"required\": [\"text\"]",
            "\"required\": [\"text\", \"text\"]",
        ),
        ("\"required\": [\"text\"]", "\"required\": [\"absent\"]"),
        ("\"timeout_ms\": 5000", "\"timeout_ms\": 5000, \"extra\": 1"),
        ("\"description\":", "\"extra\": 1, \"description\":"),
    ] {
        assert!(
            ValidatedManifest::parse(MANIFEST.replace(from, to).as_bytes()).is_err(),
            "{to}"
        );
    }
    let deep = format!("{}0{}", "[".repeat(200), "]".repeat(200));
    assert!(ValidatedManifest::parse(deep.as_bytes()).is_err());
}

#[test]
fn conservative_executable_paths_are_platform_independent() {
    for path in [
        "",
        "/bin/mod",
        "../mod",
        "a/../b",
        "a/./b",
        "./mod",
        "a//b",
        "bin/",
        "C:/mod.exe",
        "C:mod.exe",
        "bin\\mod.exe",
        "..\\mod.exe",
        "\\\\host\\mod.exe",
        "bin/mod.exe:payload",
        "bin/mod.",
        "bin/mod ",
        "bin/con.exe",
        "NUL",
        "aux.txt",
        "PRN/log",
        "COM1.exe",
        "lpt9",
        "COM0",
        "LPT¹.txt",
        "CLOCK$",
        "bin/cOn.payload.exe",
        "bin/CON .exe",
        "CONIN$",
        "CONOUT$",
        "//server/share/mod.exe",
        "\\\\?\\C:\\mod.exe",
        "\\\\.\\pipe\\mod",
        "bin/mod?.exe",
        "bin/mod*.exe",
        "bin/mod<.exe",
        "bin/mod|.exe",
        "a\nb",
        "a\0b",
        "%TEMP%/mod.exe",
        "~user/mod",
        "bin/模块.exe",
    ] {
        let mut m = raw();
        m.executable = path.into();
        assert!(m.validate().is_err(), "{path:?}");
    }
    for path in [
        "bin/example.exe",
        "plugin",
        "a-b/mod_1.exe",
        "bin/com10.exe",
        "bin/.hidden.exe",
    ] {
        let mut m = raw();
        m.executable = path.into();
        m.validate().unwrap();
    }
    let mut m = raw();
    m.executable = "a".repeat(240);
    m.clone().validate().unwrap();
    m.executable.push('a');
    assert!(m.validate().is_err());
}

#[test]
fn names_are_stable_injective_and_not_builtins() {
    assert_eq!(
        tool_name("org.example", "echo").unwrap(),
        "mod_org_example__echo"
    );
    for id in [
        "single", "org..x", "Org.x", "org.x_y", "org.x-y", ".org", "org.",
    ] {
        assert!(tool_name(id, "echo").is_err());
    }
    for local in ["", "Echo", "read_file", "a.b", "a__b", "1abc"] {
        assert!(tool_name("org.example", local).is_err());
    }
    let name = tool_name("abcdefghijklmnop.abcdefghijklmno", &"a".repeat(24)).unwrap();
    assert!(name.len() <= 64);
    assert!(crate::registry().iter().all(|tool| tool.name != name));
    assert_ne!(
        tool_name("a.bc", "d").unwrap(),
        tool_name("a.b", "cd").unwrap()
    );
}

#[test]
fn registration_is_disabled_and_rejections_are_atomic() {
    let mut catalog = Catalog::default();
    assert!(catalog.is_empty());
    catalog.register(manifest()).unwrap();
    let before = catalog.names.clone();
    assert!(catalog.register(manifest()).is_err());
    assert_eq!(catalog.len(), 1);
    assert_eq!(catalog.names, before);
    let entry = catalog.get("org.example").unwrap();
    assert!(!entry.enabled());
    assert_eq!(entry.manifest().manifest().revision, 1);
    let mut duplicate = raw();
    duplicate.tools.push(duplicate.tools[0].clone());
    assert!(duplicate.validate().is_err());
    assert_eq!(catalog.names, before);
    let mut upgrade = raw();
    upgrade.revision = 2;
    assert!(catalog.register(upgrade.validate().unwrap()).is_err());
    assert_eq!(
        catalog
            .get("org.example")
            .unwrap()
            .manifest()
            .manifest()
            .revision,
        1
    );
    assert_eq!(catalog.names, before);
    // Distinct IDs cannot collide with today's injective mapping; exercise the guard directly.
    let mut other = raw();
    other.id = "org.other".into();
    let mut second = other.tools[0].clone();
    second.name = "later".into();
    other.tools.push(second);
    catalog.names.insert(tool_name(&other.id, "later").unwrap());
    let before = catalog.names.clone();
    assert!(catalog.register(other.validate().unwrap()).is_err());
    assert_eq!(catalog.names, before);
    assert_eq!(catalog.len(), 1);
    assert!(catalog.get("org.other").is_none());
}

#[test]
fn catalog_count_is_bounded() {
    let mut catalog = Catalog::default();
    for index in 0..MAX_CATALOG {
        let mut m = raw();
        m.id = format!("org.m{index}");
        catalog.register(m.validate().unwrap()).unwrap();
    }
    let before = catalog.names.clone();
    assert!(catalog.register(manifest()).is_err());
    assert_eq!(catalog.len(), MAX_CATALOG);
    assert_eq!(catalog.names, before);
    assert!(catalog.get("org.example").is_none());
}

#[test]
fn policy_never_autoapproves_even_declared_read_only_mods() {
    assert_eq!(
        manifest().manifest().capabilities,
        vec![Capability::WorkspaceRead]
    );
    for auto_approve in [false, true] {
        for classroom_safe in [false, true] {
            for read_only in [false, true] {
                let policy = crate::Policy {
                    auto_approve,
                    classroom_safe,
                    read_only,
                    ..Default::default()
                };
                assert_eq!(
                    analyze_execution(&policy),
                    if classroom_safe || read_only {
                        ExecutionReview::Denied
                    } else {
                        ExecutionReview::RequiresExplicitUserApproval
                    }
                );
            }
        }
    }
}

#[test]
fn schemas_validate_values_not_just_declarations() {
    let schema: Schema = serde_json::from_value(json!({
        "type":"object", "properties": {
            "text":{"type":"string"}, "count":{"type":"integer"},
            "ratio":{"type":"number"}, "flag":{"type":"boolean"}
        }, "required":["text", "count", "ratio", "flag"], "additionalProperties":false
    }))
    .unwrap();
    let good = json!({"text":"你好", "count":3, "ratio":1.5, "flag":true});
    schema.validate_value(&good).unwrap();
    for (key, value) in [
        ("text", json!(null)),
        ("count", json!(1.0)),
        ("ratio", json!("1")),
        ("flag", json!(1)),
        ("text", json!([])),
        ("text", json!({})),
        ("text", json!("界".repeat(MAX_STRING_BYTES / 3 + 1))),
    ] {
        let mut bad = good.clone();
        bad[key] = value;
        assert!(schema.validate_value(&bad).is_err(), "{key}");
    }
    assert!(schema.validate_value(&json!({})).is_err());
    let mut bad = good;
    bad["extra"] = json!(true);
    assert!(schema.validate_value(&bad).is_err());
    let mut request = wire()[1].clone();
    request["params"] = json!({"text": 1});
    assert!(parse_message(&request).is_err());
}

#[test]
fn resource_bounds_reject_instead_of_clamping() {
    for (field, value) in [
        ("timeout_ms", 0),
        ("timeout_ms", 60_001),
        ("memory_mib", 0),
        ("memory_mib", 257),
        ("max_in_flight", 0),
        ("max_in_flight", 5),
        ("max_message_bytes", 0),
        ("max_message_bytes", MAX_MESSAGE_BYTES + 1),
        ("max_result_bytes", 0),
        ("max_result_bytes", MAX_RESULT_BYTES + 1),
        ("max_message_bytes", MAX_RESULT_BYTES - 1),
    ] {
        let mut value_json: Value = serde_json::from_str(MANIFEST).unwrap();
        value_json["limits"][field] = json!(value);
        assert!(
            ValidatedManifest::parse(&bytes(&value_json)).is_err(),
            "{field}={value}"
        );
    }
    let mut m = raw();
    m.tools.clear();
    assert!(m.validate().is_err());
    let mut m = raw();
    m.tools = (0..=MAX_TOOLS)
        .map(|i| {
            let mut t = m.tools[0].clone();
            t.name = format!("t{i}");
            t
        })
        .collect();
    assert!(m.validate().is_err());
}

#[test]
fn wire_rejects_unknown_fields_versions_diagnostics_and_mismatches() {
    for mut value in wire() {
        value["extra"] = json!("sensitive trace");
        assert!(parse_message(&value).is_err());
        value.as_object_mut().unwrap().remove("extra");
        value["api_version"] = json!(2);
        assert!(parse_message(&value).is_err());
    }
    let mut handshake = wire()[0].clone();
    handshake["mod_id"] = json!("org.other");
    assert!(parse_message(&handshake).is_err());
    let mut error = wire()[3].clone();
    error["outcome"]["trace"] = json!("sensitive trace");
    assert!(parse_message(&error).is_err());
    error["outcome"].as_object_mut().unwrap().remove("trace");
    error["outcome"]["code"] = json!("unknown");
    assert!(parse_message(&error).is_err());
    let mut success = wire()[2].clone();
    success["outcome"]["data"] = json!({"text":1});
    assert!(parse_message(&success).is_err());
    let request = parse_message(&wire()[1]).unwrap();
    for index in [2, 4] {
        let mut value = wire()[index].clone();
        value["id"] = json!("r2");
        assert!(parse_message(&value)
            .unwrap()
            .validate_reply_to(&request, &manifest())
            .is_err());
        value["id"] = json!("x".repeat(65));
        assert!(parse_message(&value).is_err());
    }
    assert!(request.validate_reply_to(&request, &manifest()).is_err());
    let duplicate = r#"{"type":"request","api_version":1,"id":"r1","tool":"mod_org_example__echo","params":{"text":"a","text":"b"}}"#;
    assert_eq!(
        Message::parse(duplicate.as_bytes(), &manifest()).unwrap_err(),
        Error("invalid JSON")
    );
    let mut m = raw();
    let mut second = m.tools[0].clone();
    second.name = "other".into();
    m.tools.push(second);
    let m = m.validate().unwrap();
    let mut reply = wire()[2].clone();
    reply["tool"] = json!("mod_org_example__other");
    let reply = Message::parse(&bytes(&reply), &m).unwrap();
    assert!(reply.validate_reply_to(&request, &m).is_err());
}

#[test]
fn message_result_and_parameter_byte_limits_apply_to_parsed_and_owned_values() {
    let values = wire();
    let result_bytes = bytes(&values[2]);
    let mut m = raw();
    m.limits.max_result_bytes = result_bytes.len();
    let exact = m.clone().validate().unwrap();
    let result = Message::parse(&result_bytes, &exact).unwrap();
    result.to_bytes(&exact).unwrap();
    let mut padded = result_bytes.clone();
    padded.push(b' ');
    assert!(Message::parse(&padded, &exact).is_err());
    m.limits.max_result_bytes -= 1;
    let smaller = m.validate().unwrap();
    assert!(Message::parse(&result_bytes, &smaller).is_err());
    assert!(result.to_bytes(&smaller).is_err());
    assert!(Message::parse(&vec![b' '; MAX_MESSAGE_BYTES + 1], &manifest()).is_err());
    let mut m = raw();
    let mut params = serde_json::Map::new();
    for key in ["a", "b", "c", "d"] {
        m.tools[0]
            .input_schema
            .properties
            .insert(key.into(), Scalar::String {});
        params.insert(key.into(), json!("x".repeat(MAX_STRING_BYTES)));
    }
    params.insert("text".into(), json!("ok"));
    let m = m.validate().unwrap();
    let request = Message::Request {
        api_version: 1,
        id: "r1".into(),
        tool: "mod_org_example__echo".into(),
        params: Value::Object(params),
    };
    assert!(request.to_bytes(&m).is_err());
    assert!(Message::parse(&serde_json::to_vec(&request).unwrap(), &m).is_err());
}

#[test]
fn strict_json_checks_depth_and_decoded_duplicate_keys_before_contracts() {
    // serde_json's default recursion budget permits 127 nested containers, not 128.
    let nested = |n| format!("{}0{}", "[".repeat(n), "]".repeat(n));
    parse::<Value>(nested(127).as_bytes(), MAX_MESSAGE_BYTES).unwrap();
    assert_eq!(
        parse::<Value>(nested(128).as_bytes(), MAX_MESSAGE_BYTES).unwrap_err(),
        Error("invalid JSON")
    );
    let deep_request = format!(
        r#"{{"type":"request","api_version":1,"id":"r1","tool":"mod_org_example__echo","params":{}}}"#,
        nested(128)
    );
    assert_eq!(
        Message::parse(deep_request.as_bytes(), &manifest()).unwrap_err(),
        Error("invalid JSON")
    );
    for input in [
        r#"{"a":0,"\u0061":1}"#,
        r#"{"outer":[{"key":0,"key":1}]}"#,
        r#"{"type":"cancel","type":"cancel","api_version":1,"id":"r1"}"#,
        r#"{"n":1e999}"#,
        r#"{"n":NaN}"#,
        r#"{"n":01}"#,
        r#"{"n":0,}"#,
        "{}{}",
    ] {
        assert_eq!(
            parse::<Value>(input.as_bytes(), MAX_MESSAGE_BYTES).unwrap_err(),
            Error("invalid JSON")
        );
    }
    let duplicate_schema = MANIFEST.replace(
        "\"properties\": {\"text\": {\"type\": \"string\"}}",
        r#""properties": {"text": {"type": "string"}, "te\u0078t": {"type": "string"}}"#,
    );
    assert_eq!(
        ValidatedManifest::parse(duplicate_schema.as_bytes()).unwrap_err(),
        Error("invalid JSON")
    );
}

#[test]
fn namespace_mapping_and_catalog_keep_distinct_ids_separate() {
    let mut names = BTreeSet::new();
    let mut catalog = Catalog::default();
    for id in ["a.b", "a.bc", "ab.c", "a.b.c", "org.example", "org.other"] {
        for local in ["a", "bc", "echo", "echo1"] {
            let name = tool_name(id, local).unwrap();
            assert!(names.insert(name.clone()));
            assert!(crate::find(&name).is_none());
        }
        let mut m = raw();
        m.id = id.into();
        catalog.register(m.validate().unwrap()).unwrap();
        assert!(!catalog.get(id).unwrap().enabled());
    }
    assert_eq!(catalog.len(), 6);
    assert_eq!(catalog.names.len(), 6);
    assert!(tool_name(&format!("{}.{}", "a".repeat(16), "b".repeat(16)), "echo").is_err());
    assert!(tool_name("org.example", &"a".repeat(25)).is_err());
}

#[test]
fn message_limits_are_inclusive_for_every_kind_and_owned_validation() {
    for value in wire() {
        let input = bytes(&value);
        let mut raw = raw();
        raw.limits.max_message_bytes = input.len();
        raw.limits.max_result_bytes = input.len();
        let m = raw.clone().validate().unwrap();
        let message = Message::parse(&input, &m).unwrap();
        message.validate(&m).unwrap();
        message.to_bytes(&m).unwrap();
        let mut padded = input.clone();
        padded.push(b' ');
        assert!(Message::parse(&padded, &m).is_err());
        raw.limits.max_message_bytes -= 1;
        raw.limits.max_result_bytes -= 1;
        let smaller = raw.validate().unwrap();
        assert!(Message::parse(&input, &smaller).is_err());
        assert!(message.validate(&smaller).is_err());
        assert!(message.to_bytes(&smaller).is_err());
    }
    let mut raw = raw();
    raw.tools[0].input_schema.properties = ["a", "b", "c", "d"]
        .into_iter()
        .map(|name| (name.into(), Scalar::String {}))
        .collect();
    raw.tools[0].input_schema.required.clear();
    let m = raw.validate().unwrap();
    // Four keys cost 29 bytes in compact JSON; the final string fills the exact boundary.
    let mut params = json!({"a":"x".repeat(4096), "b":"x".repeat(4096), "c":"x".repeat(4096), "d":"x".repeat(4067)});
    assert_eq!(bytes(&params).len(), MAX_PARAMS_BYTES);
    let mut request = wire()[1].clone();
    request["params"] = params.clone();
    Message::parse(&bytes(&request), &m)
        .unwrap()
        .validate(&m)
        .unwrap();
    params["d"] = json!("x".repeat(4068));
    request["params"] = params;
    assert!(Message::parse(&bytes(&request), &m).is_err());
    let owned: Message = serde_json::from_value(request).unwrap();
    assert!(owned.validate(&m).is_err());
}

#[test]
fn schema_and_correlation_reject_invalid_owned_values_and_wrong_kinds() {
    let m = manifest();
    let messages: Vec<_> = wire().iter().map(|v| parse_message(v).unwrap()).collect();
    for non_request in [&messages[0], &messages[2], &messages[3], &messages[4]] {
        assert!(messages[2].validate_reply_to(non_request, &m).is_err());
    }
    assert!(messages[0].validate_reply_to(&messages[1], &m).is_err());
    for id in ["", "R1", "r_1", "r-1", "1r", "请求", "r\n"] {
        for index in [1, 2, 3, 4] {
            let mut value = wire()[index].clone();
            value["id"] = json!(id);
            let owned: Message = serde_json::from_value(value.clone()).unwrap();
            assert!(owned.validate(&m).is_err());
            assert!(parse_message(&value).is_err());
        }
    }
    for tool in ["echo", "read_file", "mod_org_other__echo"] {
        for index in [1, 2, 3] {
            let mut value = wire()[index].clone();
            value["tool"] = json!(tool);
            assert!(parse_message(&value).is_err());
        }
    }
    for index in [1, 2] {
        let mut value = wire()[index].clone();
        let data = if index == 1 {
            &mut value["params"]
        } else {
            &mut value["outcome"]["data"]
        };
        *data = json!({"text":"x".repeat(MAX_STRING_BYTES)});
        parse_message(&value).unwrap();
    }
    let mut bad_request = messages[1].clone();
    if let Message::Request { params, .. } = &mut bad_request {
        *params = json!({"text":false});
    }
    assert!(messages[2].validate_reply_to(&bad_request, &m).is_err());
    let mut schema = raw().tools.remove(0).input_schema;
    schema.required.clear();
    schema.validate_value(&json!({})).unwrap();
    schema.additional_properties = true;
    assert!(schema.validate_value(&json!({})).is_err());
}

#[test]
fn wire_enums_require_strings_not_serde_external_tags() {
    let mut m: Value = serde_json::from_str(MANIFEST).unwrap();
    m["capabilities"] = json!([{"workspace_read": null}]);
    assert!(ValidatedManifest::parse(&bytes(&m)).is_err());
    let mut m: Value = serde_json::from_str(MANIFEST).unwrap();
    m["tools"][0]["input_schema"]["type"] = json!({"object": null});
    assert!(ValidatedManifest::parse(&bytes(&m)).is_err());
    let mut result = wire()[3].clone();
    result["outcome"]["code"] = json!({"cancelled": null});
    assert!(parse_message(&result).is_err());
    for name in [
        "workspace_read",
        "workspace_write",
        "network",
        "desktop_read",
        "desktop_input",
        "process_spawn",
    ] {
        let mut m: Value = serde_json::from_str(MANIFEST).unwrap();
        m["capabilities"] = json!([name]);
        let validated = ValidatedManifest::parse(&bytes(&m)).unwrap();
        assert_eq!(serde_json::to_value(validated.manifest()).unwrap(), m);
        m["capabilities"] = json!([{(name): null}]);
        assert!(ValidatedManifest::parse(&bytes(&m)).is_err());
    }
    for code in ["invalid_params", "failed", "cancelled", "limit_exceeded"] {
        let mut result = wire()[3].clone();
        result["outcome"]["code"] = json!(code);
        let message = parse_message(&result).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&message.to_bytes(&manifest()).unwrap()).unwrap(),
            result
        );
        result["outcome"]["code"] = json!({(code): null});
        assert!(parse_message(&result).is_err());
    }
}

#[test]
fn required_fields_and_scalar_keywords_are_not_silently_defaulted() {
    let m: Value = serde_json::from_str(MANIFEST).unwrap();
    for pointer in [
        "",
        "/limits",
        "/tools/0",
        "/tools/0/input_schema",
        "/tools/0/output_schema",
    ] {
        for key in m.pointer(pointer).unwrap().as_object().unwrap().keys() {
            let mut bad = m.clone();
            bad.pointer_mut(pointer)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove(key);
            assert!(
                ValidatedManifest::parse(&bytes(&bad)).is_err(),
                "{pointer}/{key}"
            );
        }
    }
    for value in wire() {
        for key in value.as_object().unwrap().keys() {
            let mut bad = value.clone();
            bad.as_object_mut().unwrap().remove(key);
            assert!(parse_message(&bad).is_err(), "{key}");
        }
    }
    for index in [2, 3] {
        let value = wire()[index].clone();
        for key in value["outcome"].as_object().unwrap().keys() {
            let mut bad = value.clone();
            bad["outcome"].as_object_mut().unwrap().remove(key);
            assert!(parse_message(&bad).is_err(), "outcome/{key}");
        }
    }
    for kind in ["string", "integer", "number", "boolean"] {
        let mut m = m.clone();
        m["tools"][0]["input_schema"]["properties"]["text"] = json!({"type":kind});
        ValidatedManifest::parse(&bytes(&m)).unwrap();
        m["tools"][0]["input_schema"]["properties"]["text"]["default"] = json!(null);
        assert!(ValidatedManifest::parse(&bytes(&m)).is_err());
    }
}

// Parse the original array strictly before converting its display-only entries to wire messages.
fn audit_envelopes(input: &[u8], manifest: &ValidatedManifest) -> Check<Vec<Value>> {
    let values: Vec<Value> = parse(input, MAX_MESSAGE_BYTES)?;
    ensure(values.len() == 5, "fixture message count")?;
    let messages = values
        .iter()
        .map(|v| Message::parse(&bytes(v), manifest))
        .collect::<Check<Vec<_>>>()?;
    ensure(
        matches!(messages[0], Message::Handshake { .. }),
        "fixture handshake",
    )?;
    for reply in &messages[2..] {
        reply.validate_reply_to(&messages[1], manifest)?;
    }
    for message in messages {
        Message::parse(&message.to_bytes(manifest)?, manifest)?;
    }
    Ok(values)
}

fn documented_envelopes(doc: &str) -> Check<String> {
    let mut lines = doc.lines(); // Accept both LF and CRLF without altering JSON string contents.
    let mut blocks = Vec::new();
    while let Some(line) = lines.next() {
        if line == "```json" {
            let mut block = String::new();
            let mut closed = false;
            for line in lines.by_ref() {
                if line == "```" {
                    closed = true;
                    break;
                }
                block.push_str(line);
                block.push('\n');
            }
            ensure(closed, "unclosed JSON fence")?;
            blocks.push(block);
        }
    }
    ensure(blocks.len() == 1, "expected one JSON block")?;
    Ok(blocks.remove(0))
}

#[test]
fn doc_audit_helpers_handle_line_endings_and_reject_invalid_fixtures() {
    for newline in ["\n", "\r\n"] {
        let doc = format!("# Example\n\n```json\n{ENVELOPES}\n```\n").replace('\n', newline);
        let example = documented_envelopes(&doc).unwrap();
        assert_eq!(
            audit_envelopes(example.as_bytes(), &manifest()).unwrap(),
            wire()
        );
    }
    for doc in ["", "```json\n[]", "```json\n[]\n```\n```json\n[]\n```"] {
        assert!(documented_envelopes(doc).is_err());
    }
    for bad in [
        ENVELOPES.replace("\"text\":\"你好\"", "\"text\":\"你好\",\"text\":\"你好\""),
        ENVELOPES.replace("\"text\":\"你好\"", "\"text\":0"),
        ENVELOPES.replace(
            "\"status\":\"success\"",
            "\"status\":\"success\",\"trace\":\"secret\"",
        ),
        ENVELOPES.replace(
            "\"type\":\"cancel\",\"api_version\":1,\"id\":\"r1\"",
            "\"type\":\"cancel\",\"api_version\":1,\"id\":\"r2\"",
        ),
    ] {
        assert!(audit_envelopes(bad.as_bytes(), &manifest()).is_err());
    }
}

#[test]
#[ignore = "manual audit: requires public docs/mod-api.md and private docs-pri/mod-example fixtures"]
fn local_docs_match_validated_fixtures() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let docs = root.join("docs-pri");
    // Missing files must fail an explicitly requested audit, never silently skip validation.
    let actual =
        std::fs::read(docs.join("mod-example/manifest.json")).expect("read local manifest fixture");
    let m = ValidatedManifest::parse(&actual).expect("validate local manifest fixture");
    assert_eq!(
        serde_json::to_value(m.manifest()).unwrap(),
        serde_json::from_str::<Value>(MANIFEST).unwrap()
    );
    let actual = std::fs::read(docs.join("mod-example/envelopes.json"))
        .expect("read local envelope fixtures");
    assert_eq!(audit_envelopes(&actual, &m).unwrap(), wire());
    let doc = std::fs::read_to_string(root.join("docs/mod-api.md")).expect("read public documentation");
    let example = documented_envelopes(&doc).unwrap();
    assert_eq!(audit_envelopes(example.as_bytes(), &m).unwrap(), wire());
}
