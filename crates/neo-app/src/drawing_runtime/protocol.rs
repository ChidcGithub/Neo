use super::{BoardKind, Permissions, MAX_LINE};
use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::fmt;

#[derive(Clone, Debug, PartialEq)]
pub struct RpcError {
    pub code: String,
    pub message: String,
    pub data: Option<Value>,
}

impl RpcError {
    pub(super) fn local(code: &str, message: &str) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            data: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct State {
    pub visible: bool,
    pub hidden_confirmed: bool,
    pub has_window: bool,
    pub dirty: bool,
    pub closed: bool,
    pub configured: bool,
    pub document_id: String,
    pub page_id: String,
    pub revision: u64,
    pub desired_visible: bool,
    pub effective_visible: bool,
    pub close_pending: bool,
    pub connected: bool,
    pub permissions: Permissions,
}

impl State {
    pub(super) fn parse(value: &Value, kind: BoardKind) -> Result<Self, String> {
        let obj = value.as_object().ok_or("state must be an object")?;
        if value["app"].as_str() != Some(kind.code()) || value["revision_scope"] != "document" {
            return Err("invalid state app/revision_scope".into());
        }
        let boolean = |key: &str| {
            obj.get(key)
                .and_then(Value::as_bool)
                .ok_or_else(|| format!("invalid state {key}"))
        };
        let text = |key: &str| {
            obj.get(key)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty() && s.len() <= 256)
                .map(str::to_owned)
                .ok_or_else(|| format!("invalid state {key}"))
        };
        let permissions = parse_permissions(&value["permissions"])?;
        let state = Self {
            visible: boolean("visible")?,
            hidden_confirmed: boolean("hidden_confirmed")?,
            has_window: boolean("has_window")?,
            dirty: boolean("dirty")?,
            closed: boolean("closed")?,
            configured: boolean("configured")?,
            document_id: text("document_id")?,
            page_id: text("page_id")?,
            revision: value["revision"].as_u64().ok_or("invalid state revision")?,
            desired_visible: boolean("desired_visible")?,
            effective_visible: boolean("effective_visible")?,
            close_pending: boolean("close_pending")?,
            connected: boolean("connected")?,
            permissions,
        };
        if (state.hidden_confirmed && (!state.has_window || state.visible))
            || (state.visible && !state.has_window)
        {
            return Err("contradictory window state".into());
        }
        if !matches!(
            value["window_status"].as_str(),
            Some("no_window" | "pending" | "visible" | "hidden")
        ) {
            return Err("invalid window_status".into());
        }
        Ok(state)
    }
}

fn parse_permissions(value: &Value) -> Result<Permissions, String> {
    let object = value.as_object().ok_or("permissions must be an object")?;
    if object.len() != 3 {
        return Err("permissions require exactly three boolean fields".into());
    }
    let boolean = |key: &str| {
        value[key]
            .as_bool()
            .ok_or_else(|| format!("invalid permission {key}"))
    };
    let safe = boolean("classroom_safe")?;
    let capture = boolean("desktop_capture_allowed")?;
    let agent = boolean("agent_allowed")?;
    if safe && capture {
        return Err("classroom_safe forbids desktop capture".into());
    }
    Ok(Permissions::new(safe, capture, agent))
}

pub(super) fn validate_outbound(method: &str, params: &Value) -> Result<(), String> {
    let object = params.as_object().ok_or("params must be an object")?;
    if method.is_empty() || method.len() > 256 || method.chars().any(char::is_control) {
        return Err("invalid method".into());
    }
    match method {
        "configure" => {
            parse_permissions(params)?;
        }
        "show" | "hide" | "close" | "get_state" if !object.is_empty() => {
            return Err(format!("{method} requires {{}}"))
        }

        _ => {}
    }
    Ok(())
}

pub(super) fn encode(value: &Value) -> Result<Vec<u8>, String> {
    let mut bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    if bytes.len() > MAX_LINE {
        return Err("JSONL frame exceeds 65536 bytes".into());
    }
    bytes.push(b'\n');
    Ok(bytes)
}

pub(super) fn request_frame(id: &str, method: &str, params: &Value) -> Result<Vec<u8>, String> {
    encode(&json!({"version":1,"type":"request","id":id,"method":method,"params":params}))
}

pub(super) enum Envelope {
    Response {
        id: String,
        result: Result<Value, RpcError>,
    },
    Request {
        id: String,
        method: String,
        params: Value,
    },
    Event {
        name: String,
        data: Value,
    },
}

// Keep duplicate names before converting to Value: serde_json::Value alone silently
// overwrites them, including escaped spellings. Business objects are not restricted.
struct Fields(Vec<(String, Value)>, bool);
struct ErrorFields(Vec<(String, Value)>);

#[derive(Deserialize)]
#[serde(untagged)]
enum ErrorValue {
    Object(ErrorFields),
    Other(Value),
}

impl<'de> Deserialize<'de> for ErrorFields {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct ObjectVisitor;
        impl<'de> Visitor<'de> for ObjectVisitor {
            type Value = ErrorFields;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("error object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut fields = Vec::new();
                while let Some(key) = map.next_key::<String>()? {
                    fields.push((key, map.next_value()?));
                }
                Ok(ErrorFields(fields))
            }
        }
        d.deserialize_map(ObjectVisitor)
    }
}

impl<'de> Deserialize<'de> for Fields {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct ObjectVisitor;
        impl<'de> Visitor<'de> for ObjectVisitor {
            type Value = Fields;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("protocol object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut fields = Vec::new();
                let mut duplicate_error = false;
                while let Some(key) = map.next_key::<String>()? {
                    let value = if key == "error" {
                        match map.next_value::<ErrorValue>()? {
                            ErrorValue::Object(error) => {
                                duplicate_error |=
                                    reject_duplicates(&error.0, &["code", "message", "data"])
                                        .is_err();
                                Value::Object(error.0.into_iter().collect())
                            }
                            ErrorValue::Other(value) => value,
                        }
                    } else {
                        map.next_value()?
                    };
                    fields.push((key, value));
                }
                Ok(Fields(fields, duplicate_error))
            }
        }
        d.deserialize_map(ObjectVisitor)
    }
}

fn reject_duplicates(fields: &[(String, Value)], known: &[&str]) -> Result<(), String> {
    let mut seen = HashSet::new();
    for (name, _) in fields {
        if known.contains(&name.as_str()) && !seen.insert(name) {
            return Err(format!("duplicate protocol field {name}"));
        }
    }
    Ok(())
}

pub(super) fn valid_id(id: &str, prefix: &str) -> bool {
    id.len() <= 256
        && id.strip_prefix(prefix).is_some_and(|s| !s.is_empty())
        && !id.chars().any(|c| c.is_whitespace() || c.is_control())
}

pub(super) fn decode(bytes: &[u8]) -> Result<Envelope, String> {
    if bytes.len() > MAX_LINE {
        return Err("line_too_long".into());
    }
    let fields: Fields =
        serde_json::from_slice(bytes).map_err(|e| format!("invalid protocol JSON: {e}"))?;
    reject_duplicates(&fields.0, &["version", "type"])?;
    let kind = fields
        .0
        .iter()
        .find(|(k, _)| k == "type")
        .and_then(|(_, v)| v.as_str())
        .ok_or("missing message type")?;
    let known: &[&str] = match kind {
        "response" => &["id", "ok", "result", "error"],
        "request" => &["id", "method", "params"],
        "event" => &["event", "data"],
        _ => return Err("unknown message type".into()),
    };
    reject_duplicates(&fields.0, known)?;
    if kind == "response" && fields.1 {
        return Err("duplicate error field".into());
    }
    let obj: Map<String, Value> = fields.0.into_iter().collect();
    if obj.get("version").and_then(Value::as_u64) != Some(1) {
        return Err("unsupported_version".into());
    }
    let text = |key: &str| {
        obj.get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| format!("invalid {key}"))
    };
    match obj["type"].as_str().unwrap() {
        "response" => {
            let id = text("id")?;
            if !valid_id(&id, "neo:") {
                return Err("invalid response ID".into());
            }
            let result = match obj.get("ok").and_then(Value::as_bool) {
                Some(true) if obj.contains_key("result") && !obj.contains_key("error") => {
                    Ok(obj["result"].clone())
                }
                Some(false) if obj.contains_key("error") && !obj.contains_key("result") => {
                    let error = &obj["error"];
                    let code = error["code"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .ok_or("invalid error code")?;
                    let message = error["message"].as_str().ok_or("invalid error message")?;
                    Err(RpcError {
                        code: code.into(),
                        message: message.into(),
                        data: error.get("data").cloned(),
                    })
                }
                _ => return Err("response must contain exactly result or error matching ok".into()),
            };
            Ok(Envelope::Response { id, result })
        }
        "request" => {
            let id = text("id")?;
            if !valid_id(&id, "runtime:") {
                return Err("invalid runtime request ID".into());
            }
            let method = text("method")?;
            let params = obj
                .get("params")
                .filter(|v| v.is_object())
                .ok_or("params must be an object")?
                .clone();
            Ok(Envelope::Request { id, method, params })
        }
        _ => Ok(Envelope::Event {
            name: text("event")?,
            data: obj.get("data").ok_or("missing event data")?.clone(),
        }),
    }
}

pub(super) fn ready(data: &Value, kind: BoardKind) -> Result<HashSet<String>, String> {
    if data["app"].as_str() != Some(kind.code())
        || data["headless"] != false
        || data["has_window"] != true
        || data["max_line_bytes"].as_u64() != Some(MAX_LINE as u64)
        || data["revision_scope"] != "document"
    {
        return Err("incompatible hosted ready capabilities".into());
    }
    let list = data["methods"]
        .as_array()
        .ok_or("ready.methods must be an array")?;
    let mut methods = HashSet::new();
    for value in list {
        let method = value
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 256)
            .ok_or("invalid ready method")?;
        if !methods.insert(method.to_owned()) {
            return Err("duplicate ready method".into());
        }
    }
    if !["configure", "get_state", "show", "hide", "close"]
        .iter()
        .all(|m| methods.contains(*m))
    {
        return Err("missing required runtime methods".into());
    }
    Ok(methods)
}

pub(super) fn host_response(id: &str, result: Result<Value, RpcError>) -> Result<Vec<u8>, String> {
    if !valid_id(id, "runtime:") {
        return Err("invalid runtime request ID".into());
    }
    let mut response = json!({"version":1,"type":"response","id":id,"ok":result.is_ok()});
    match result {
        Ok(value) => response["result"] = value,
        Err(error) => {
            if error.code.is_empty() {
                return Err("invalid error code".into());
            }
            response["error"] = json!({"code":error.code,"message":error.message});
            if let Some(data) = error.data {
                response["error"]["data"] = data;
            }
        }
    }
    encode(&response)
}

pub(super) fn state_result(
    method: &str,
    result: &Value,
    kind: BoardKind,
) -> Result<Option<State>, String> {
    if matches!(
        method,
        "configure"
            | "get_state"
            | "show"
            | "hide"
            | "close"
            | "window.suspend"
            | "window.resume"
            | "document.new"
            | "document.open"
            | "document.save"
            | "pages.delete"
            | "pages.select"
            | "objects.apply"
    ) {
        return State::parse(result, kind).map(Some);
    }
    if let Some(state) = result.get("state") {
        return State::parse(state, kind).map(Some);
    }
    Ok(None)
}

pub(super) struct Pending {
    pub request: super::Request,
    pub handshake: bool,
}

pub(super) struct Session {
    pub kind: BoardKind,
    pub generation: u64,
    pub permissions: Permissions,
    pub methods: Option<HashSet<String>>,
    pub configured: bool,
    pub closed: bool,
    pub pending: HashMap<String, Pending>,
}
