//! Standalone validation of the deliberately small, non-image drawing edit wire.
//!
//! The host must supply an authorized, revision-pinned snapshot and a fresh local
//! prefix (unique across the document, including objects outside this snapshot).
//! This module neither performs RPC nor grants image, file, or script access.

use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Number, Value};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io::{self, Write};

pub const MAX_SNAPSHOT_OBJECTS: usize = 256;
pub const MAX_SNAPSHOT_BYTES: usize = 128 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 16 * 1024;
pub const MAX_WIRE_BYTES: usize = 60 * 1024;
pub const MAX_OPERATIONS: usize = 64;
/// Shared across all plots in a snapshot or response, not per expression.
pub const MAX_EXPRESSION_BYTES: usize = 16 * 1024;

/// Type filter only, NOT validation or authorization. Filter before model upload;
/// `validate_snapshot` rejects images and all other unsupported/malformed objects.
pub fn editable_object(object: &Value) -> bool {
    matches!(
        object
            .get("kind")
            .and_then(|k| k.get("type"))
            .and_then(Value::as_str),
        Some("text" | "shape" | "function_plot" | "coordinate_system" | "math" | "stroke")
    )
}

/// IDs retain runtime semantics: nonblank UTF-8, bounded by the snapshot budget.
/// Values already parsed by the host cannot retain evidence of duplicate keys;
/// untrusted model JSON must go through `validate_edit_response` instead.
pub fn validate_snapshot(objects: &[Value]) -> Result<(), String> {
    ensure(
        objects.len() <= MAX_SNAPSHOT_OBJECTS,
        "too many snapshot objects",
    )?;
    let mut ids = HashSet::new();
    let mut expressions = 0;
    for object in objects {
        let (id, _) = validate_object(object, &mut expressions)?;
        ensure(ids.insert(id), "duplicate snapshot ID")?;
    }
    serialized_limit(objects, MAX_SNAPSHOT_BYTES)
}

/// Accept exactly one JSON object with required `answer` and `operations` fields.
/// Updates are full replacements of the same kind. Each model ID may be touched
/// only once; newly added IDs cannot be referenced by updates/deletes. Add IDs
/// are always rewritten to `prefix-counter` (counter starts at 1, skips snapshot
/// and model IDs). Prefix must be 1..128 ASCII alphanumeric / `_` / `-` bytes.
/// Returns only the validated wire, limited AFTER rewriting to 60 KiB.
pub fn validate_edit_response(
    text: &str,
    objects: &[Value],
    prefix: &str,
) -> Result<Value, String> {
    ensure(
        text.len() <= MAX_RESPONSE_BYTES,
        "model response exceeds 16 KiB",
    )?;
    validate_snapshot(objects)?;
    ensure(short_id(prefix), "invalid local ID prefix")?;
    let mut de = serde_json::Deserializer::from_str(text);
    let mut response = StrictValue::deserialize(&mut de)
        .map_err(|e| e.to_string())?
        .0;
    de.end().map_err(|e| e.to_string())?;
    exact(&response, &["answer", "operations"])?;
    string(&response["answer"])?;
    let operations = array(&response["operations"])?;
    ensure(operations.len() <= MAX_OPERATIONS, "too many operations")?;
    let existing: HashMap<&str, &str> = objects
        .iter()
        .map(|o| {
            (
                o["id"].as_str().unwrap(),
                o["kind"]["type"].as_str().unwrap(),
            )
        })
        .collect();
    let mut touched = HashSet::new();
    let mut expressions = 0;
    for operation in operations {
        let op = string(&operation["op"])?;
        let id = match op {
            "add" | "update" => {
                exact(operation, &["op", "object"])?;
                let (id, kind) = validate_object(&operation["object"], &mut expressions)?;
                if op == "add" {
                    ensure(short_id(id), "invalid temporary add ID")?;
                    ensure(!existing.contains_key(id), "add ID already exists")?;
                } else {
                    ensure(
                        existing.get(id).copied() == Some(kind),
                        "unauthorized ID or changed object kind",
                    )?;
                }
                id
            }
            "delete" => {
                exact(operation, &["op", "id"])?;
                let id = string(&operation["id"])?;
                ensure(existing.contains_key(id), "unauthorized delete ID")?;
                id
            }
            _ => return Err("unsupported operation".into()),
        };
        ensure(touched.insert(id), "an ID may only be touched once")?;
    }
    // Reserve all original IDs before mutating, including IDs later in the batch.
    let mut reserved: HashSet<String> = existing
        .keys()
        .chain(touched.iter())
        .map(|id| (*id).to_owned())
        .collect();
    let mut counter = 0usize;
    for operation in response["operations"].as_array_mut().unwrap() {
        if operation["op"] == "add" {
            loop {
                counter += 1;
                let id = format!("{prefix}-{counter}");
                if reserved.insert(id.clone()) {
                    operation["object"]["id"] = Value::String(id);
                    break;
                }
            }
        }
    }
    serialized_limit(&response, MAX_WIRE_BYTES)?;
    Ok(response)
}

fn short_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

fn ensure(ok: bool, message: &str) -> Result<(), String> {
    if ok {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn exact(value: &Value, fields: &[&str]) -> Result<(), String> {
    let object = value.as_object().ok_or("expected object")?;
    ensure(
        object.len() == fields.len() && fields.iter().all(|key| object.contains_key(*key)),
        "missing or unknown field",
    )
}

fn string(value: &Value) -> Result<&str, String> {
    value.as_str().ok_or_else(|| "expected string".into())
}

fn array(value: &Value) -> Result<&[Value], String> {
    value
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| "expected array".into())
}

fn number(value: &Value) -> Result<f64, String> {
    let n = value.as_f64().ok_or("expected number")?;
    ensure(n.is_finite(), "nonfinite number")?;
    Ok(n)
}

fn bounded(value: &Value, min: f64, max: f64) -> Result<f64, String> {
    let n = number(value)?;
    ensure((min..=max).contains(&n), "number out of bounds")?;
    Ok(n)
}

fn positive(value: &Value, max: f64) -> Result<(), String> {
    let n = bounded(value, 0.0, max)?;
    ensure(
        n > 0.0 && (n as f32) > 0.0,
        "size must be positive in runtime precision",
    )
}

fn point(value: &Value) -> Result<(), String> {
    exact(value, &["x", "y"])?;
    coordinates(value)
}

fn coordinates(value: &Value) -> Result<(), String> {
    bounded(&value["x"], -100_000.0, 100_000.0)?;
    bounded(&value["y"], -100_000.0, 100_000.0)?;
    Ok(())
}

fn color(value: &Value) -> Result<(), String> {
    exact(value, &["r", "g", "b", "a"])?;
    for key in ["r", "g", "b", "a"] {
        ensure(
            value[key].as_u64().is_some_and(|n| n <= 255),
            "color channel must be u8",
        )?;
    }
    Ok(())
}

fn style(value: &Value) -> Result<(), String> {
    exact(value, &["color", "width", "dashed"])?;
    color(&value["color"])?;
    bounded(&value["width"], 0.1, 100.0)?;
    ensure(value["dashed"].is_boolean(), "dashed must be boolean")
}

fn validate_object<'a>(
    object: &'a Value,
    expressions: &mut usize,
) -> Result<(&'a str, &'a str), String> {
    exact(object, &["id", "kind"])?;
    let id = string(&object["id"])?;
    ensure(!id.trim().is_empty(), "blank object ID")?;
    let k = &object["kind"];
    let kind = string(&k["type"])?;
    match kind {
        "text" | "math" => {
            let content = if kind == "text" { "text" } else { "layout" };
            exact(k, &["type", "position", content, "size", "color"])?;
            point(&k["position"])?;
            positive(&k["size"], 512.0)?;
            color(&k["color"])?;
            if kind == "text" {
                string(&k["text"])?;
            } else {
                math_layout(&k["layout"])?;
            }
        }
        "coordinate_system" => {
            exact(k, &["type", "origin", "scale"])?;
            point(&k["origin"])?;
            positive(&k["scale"], 10_000.0)?;
        }
        "shape" => {
            exact(k, &["type", "shape", "points", "style"])?;
            let shape = string(&k["shape"])?;
            ensure(
                matches!(
                    shape,
                    "line"
                        | "rectangle"
                        | "square"
                        | "triangle"
                        | "right_triangle"
                        | "equilateral_triangle"
                        | "parallelogram"
                        | "rhombus"
                        | "ellipse"
                        | "circle"
                        | "cube"
                        | "cuboid"
                        | "cylinder"
                        | "cone"
                        | "sphere"
                ),
                "unsupported shape",
            )?;
            let points = array(&k["points"])?;
            ensure(
                (2..=256).contains(&points.len()) && (shape != "line" || points.len() == 2),
                "invalid shape point count",
            )?;
            for p in points {
                point(p)?;
            }
            style(&k["style"])?;
        }
        "stroke" => {
            exact(k, &["type", "points", "style"])?;
            let points = array(&k["points"])?;
            ensure(
                (1..=4096).contains(&points.len()),
                "invalid stroke point count",
            )?;
            for p in points {
                exact(p, &["x", "y", "time", "pressure"])?;
                coordinates(p)?;
                ensure(number(&p["time"])? >= 0.0, "negative stroke time")?;
                bounded(&p["pressure"], 0.0, 1.0)?;
            }
            style(&k["style"])?;
        }
        "function_plot" => {
            exact(
                k,
                &[
                    "type",
                    "position",
                    "width",
                    "height",
                    "expressions",
                    "x_min",
                    "x_max",
                    "y_min",
                    "y_max",
                ],
            )?;
            point(&k["position"])?;
            positive(&k["width"], 10_000.0)?;
            positive(&k["height"], 10_000.0)?;
            for (lo, hi) in [("x_min", "x_max"), ("y_min", "y_max")] {
                let lo = number(&k[lo])?;
                let hi = number(&k[hi])?;
                ensure(lo < hi && (hi - lo).is_finite(), "invalid plot range")?;
            }
            let items = array(&k["expressions"])?;
            ensure((1..=16).contains(&items.len()), "invalid expression count")?;
            for item in items {
                let text = string(item)?;
                ensure(
                    !text.trim().is_empty() && text.len() <= 4096,
                    "invalid expression length",
                )?;
                *expressions += text.len();
                ensure(
                    *expressions <= MAX_EXPRESSION_BYTES,
                    "total expression budget exceeded",
                )?;
                expression(text)?;
            }
        }
        _ => return Err("unsupported object type (images are not editable)".into()),
    }
    Ok((id, kind))
}

fn math_layout(root: &Value) -> Result<(), String> {
    let mut stack = vec![(root, 1usize)];
    let (mut nodes, mut bytes) = (0usize, 0usize);
    while let Some((node, depth)) = stack.pop() {
        nodes += 1;
        ensure(
            depth <= 32 && nodes <= 512,
            "math layout depth/node budget exceeded",
        )?;
        exact(node, &["type", "value"])?;
        let value = &node["value"];
        match string(&node["type"])? {
            "text" => {
                bytes = bytes.saturating_add(string(value)?.len());
                ensure(bytes <= 4096, "math text budget exceeded")?;
            }
            "row" | "fraction" => {
                let children = array(value)?;
                ensure(
                    node["type"] != "fraction" || children.len() == 2,
                    "fraction needs two children",
                )?;
                ensure(
                    children.len() <= 512usize.saturating_sub(nodes + stack.len()),
                    "math node budget exceeded",
                )?;
                stack.extend(children.iter().map(|child| (child, depth + 1)));
            }
            "radical" => stack.push((value, depth + 1)),
            _ => return Err("unsupported math layout".into()),
        }
    }
    Ok(())
}

/// Lexical safety check only, not a CAS or promise of runtime sampleability.
/// Implicit multiplication is allowed. No commands, strings, escapes or Unicode.
fn expression(text: &str) -> Result<(), String> {
    ensure(text.is_ascii(), "expression must be ASCII")?;
    let b = text.as_bytes();
    let (mut i, mut tokens, mut depth, mut equals) = (0, 0, 0usize, 0);
    while i < b.len() {
        if matches!(b[i], b' ' | b'\t' | b'\r' | b'\n') {
            i += 1;
            continue;
        }
        tokens += 1;
        ensure(tokens <= 512, "expression token budget exceeded")?;
        match b[i] {
            b'0'..=b'9' | b'.' => {
                let start = i;
                while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
                    i += 1;
                }
                if i < b.len() && matches!(b[i], b'e' | b'E') {
                    let mut end = i + 1;
                    if end < b.len() && matches!(b[end], b'+' | b'-') {
                        end += 1;
                    }
                    if end < b.len() && b[end].is_ascii_digit() {
                        i = end + 1;
                        while i < b.len() && b[i].is_ascii_digit() {
                            i += 1;
                        }
                    }
                }
                let n = text[start..i]
                    .parse::<f64>()
                    .map_err(|_| "invalid expression number")?;
                ensure(n.is_finite(), "nonfinite expression literal")?;
            }
            b'x' | b'y' => {
                i += 1;
            }
            b'a'..=b'z' | b'A'..=b'Z' => {
                let start = i;
                while i < b.len() && b[i].is_ascii_alphabetic() {
                    i += 1;
                }
                let name = &text[start..i];
                ensure(
                    matches!(
                        name,
                        "pi" | "e"
                            | "sin"
                            | "cos"
                            | "tan"
                            | "asin"
                            | "arcsin"
                            | "acos"
                            | "arccos"
                            | "atan"
                            | "arctan"
                            | "ln"
                            | "log"
                            | "sqrt"
                            | "abs"
                            | "exp"
                            | "sinh"
                            | "cosh"
                            | "tanh"
                            | "asinh"
                            | "acosh"
                            | "atanh"
                    ),
                    "expression identifier not allowed",
                )?;
            }
            b'+' | b'-' | b'*' | b'/' | b'^' => {
                i += 1;
            }
            b'(' => {
                depth += 1;
                ensure(depth <= 64, "expression nesting exceeded")?;
                i += 1;
            }
            b')' => {
                ensure(depth > 0, "unbalanced expression")?;
                depth -= 1;
                i += 1;
            }
            b'=' => {
                equals += 1;
                ensure(equals <= 1 && depth == 0, "invalid expression equation")?;
                i += 1;
            }
            _ => return Err("expression character not allowed".into()),
        }
    }
    ensure(depth == 0 && tokens > 0, "unbalanced or empty expression")
}

// Streaming byte count avoids allocating a second oversized snapshot/wire.
fn serialized_limit<T: Serialize + ?Sized>(value: &T, max: usize) -> Result<(), String> {
    struct Limit(usize);
    impl Write for Limit {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0 = self
                .0
                .checked_sub(bytes.len())
                .ok_or_else(|| io::Error::other("serialized byte budget exceeded"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Limit(max), value).map_err(|e| e.to_string())
}

// Value's default deserializer silently replaces duplicate keys. Reject them at
// every nesting level, including equivalent escaped keys, before schema checks.
struct StrictValue(Value);
impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct StrictVisitor;
        impl<'de> Visitor<'de> for StrictVisitor {
            type Value = StrictValue;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("JSON with unique object keys")
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Self::Value, E> {
                Ok(StrictValue(v.into()))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
                Ok(StrictValue(v.into()))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
                Ok(StrictValue(v.into()))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Self::Value, E> {
                Number::from_f64(v)
                    .map(|n| StrictValue(Value::Number(n)))
                    .ok_or_else(|| E::custom("nonfinite number"))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(StrictValue(v.into()))
            }
            fn visit_string<E: de::Error>(self, v: String) -> Result<Self::Value, E> {
                Ok(StrictValue(v.into()))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(StrictValue(Value::Null))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(StrictValue(value)) = seq.next_element()? {
                    values.push(value);
                }
                Ok(StrictValue(Value::Array(values)))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut values = Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    if values.contains_key(&key) {
                        return Err(de::Error::custom("duplicate JSON key"));
                    }
                    let StrictValue(value) = map.next_value()?;
                    values.insert(key, value);
                }
                Ok(StrictValue(Value::Object(values)))
            }
        }
        deserializer.deserialize_any(StrictVisitor)
    }
}

#[cfg(test)]
#[path = "drawing_objects_tests.rs"]
mod tests;
