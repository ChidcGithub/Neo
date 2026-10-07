use super::*;
use serde_json::json;

fn color() -> Value {
    json!({"r":0,"g":128,"b":255,"a":255})
}
fn point() -> Value {
    json!({"x":1,"y":2})
}
fn style() -> Value {
    json!({"color":color(),"width":0.1,"dashed":false})
}
fn text(id: &str) -> Value {
    json!({"id":id,"kind":{"type":"text","position":point(),"text":"板书","size":24,"color":color()}})
}
fn shape(name: &str) -> Value {
    json!({"id":"shape","kind":{"type":"shape","shape":name,"points":[point(),point()],"style":style()}})
}
fn plot(expressions: Value) -> Value {
    json!({"id":"plot","kind":{"type":"function_plot","position":point(),"width":300,"height":200,"expressions":expressions,"x_min":-10,"x_max":10,"y_min":-10,"y_max":10}})
}
fn math(layout: Value) -> Value {
    json!({"id":"math","kind":{"type":"math","position":point(),"size":24,"color":color(),"layout":layout}})
}
fn leaf(s: &str) -> Value {
    json!({"type":"text","value":s})
}
fn stroke(count: usize) -> Value {
    json!({"id":"stroke","kind":{"type":"stroke","points":vec![json!({"x":0,"y":0,"time":0,"pressure":1});count],"style":style()}})
}
fn operation(op: &str, object: Value) -> Value {
    json!({"op":op,"object":object})
}
fn response(operations: Vec<Value>, snapshot: &[Value]) -> Result<Value, String> {
    validate_edit_response(
        &json!({"answer":"完成","operations":operations}).to_string(),
        snapshot,
        "local",
    )
}
fn valid(object: Value) {
    validate_snapshot(&[object]).unwrap();
}
fn invalid(object: Value) {
    assert!(validate_snapshot(&[object]).is_err());
}

#[test]
fn all_supported_kinds_and_shape_variants() {
    let objects = vec![
        text("text"),
        shape("line"),
        plot(json!(["sin(x)", "x^2+y^2=9"])),
        math(
            json!({"type":"fraction","value":[leaf("5"),json!({"type":"radical","value":leaf("6")})]}),
        ),
        stroke(1),
        json!({"id":"axes","kind":{"type":"coordinate_system","origin":point(),"scale":10}}),
    ];
    validate_snapshot(&objects).unwrap();
    for object in &objects {
        assert!(editable_object(object));
    }
    let result = response(
        objects.into_iter().map(|o| operation("add", o)).collect(),
        &[],
    )
    .unwrap();
    assert_eq!(result["operations"].as_array().unwrap().len(), 6);
    for name in [
        "line",
        "rectangle",
        "square",
        "triangle",
        "right_triangle",
        "equilateral_triangle",
        "parallelogram",
        "rhombus",
        "ellipse",
        "circle",
        "cube",
        "cuboid",
        "cylinder",
        "cone",
        "sphere",
    ] {
        valid(shape(name));
    }
}

#[test]
fn filter_is_only_a_type_filter_and_images_are_never_authorized() {
    for object in [
        Value::Null,
        json!({"type":"text"}),
        json!({"kind":{"type":"script"}}),
        json!({"id":"image","kind":{"type":"image","position":point(),"width":10,"height":10,"asset_ref":"png"}}),
    ] {
        assert!(!editable_object(&object));
        invalid(object.clone());
        assert!(response(vec![operation("add", object.clone())], &[]).is_err());
        assert!(response(vec![json!({"op":"delete","id":"image"})], &[object]).is_err());
    }
    assert!(editable_object(&json!({"kind":{"type":"text"}})));
    invalid(json!({"kind":{"type":"text"}}));
}

#[test]
fn handwritten_is_not_editable_and_cannot_authorize_model_operations() {
    let handwritten = json!({"id":"handwritten-private","kind":{"type":"handwritten",
        "position":point(),"text":"private text","layout":leaf("private layout"),
        "strokes":[{"points":[{"x":0,"y":0,"time":0,"pressure":1}],"style":style()}]}});
    assert!(!editable_object(&handwritten));
    invalid(handwritten.clone());
    let existing = vec![
        text("text"),
        shape("line"),
        plot(json!(["x"])),
        math(leaf("5")),
        stroke(1),
        json!({"id":"axes","kind":{"type":"coordinate_system","origin":point(),"scale":10}}),
    ];
    let mut mixed = existing.clone();
    mixed.push(handwritten.clone());
    let authorized: Vec<_> = mixed.into_iter().filter(editable_object).collect();
    assert_eq!(authorized, existing);
    for op in ["add", "update"] {
        assert!(response(vec![operation(op, handwritten.clone())], &authorized).is_err());
    }
    assert!(response(
        vec![json!({"op":"delete","id":"handwritten-private"})],
        &authorized
    )
    .is_err());
    for object in &authorized {
        response(vec![operation("update", object.clone())], &authorized).unwrap();
        response(vec![json!({"op":"delete","id":object["id"]})], &authorized).unwrap();
    }
}

#[test]
fn strict_json_rejects_prose_fences_trailing_values_and_wrong_envelopes() {
    for s in [
        "",
        "null",
        "[]",
        "42",
        "```json\n{\"answer\":\"\",\"operations\":[]}\n```",
        "Here: {\"answer\":\"\",\"operations\":[]}",
        "{\"answer\":\"\",\"operations\":[]} {}",
        "{\"answer\":\"\",\"operations\":[],}",
        "{\"answer\":\"\"}",
        "{\"operations\":[]}",
        "{\"answer\":1,\"operations\":[]}",
        "{\"answer\":\"\",\"operations\":null}",
        "{\"answer\":\"\",\"operations\":[],\"rpc\":\"document.open\"}",
        "{\"answer\":\"\",/*comment*/\"operations\":[]}",
    ] {
        assert!(validate_edit_response(s, &[], "local").is_err(), "{s}");
    }
    assert_eq!(
        validate_edit_response(" \n{\"answer\":\"\",\"operations\":[]}\t", &[], "local").unwrap(),
        json!({"answer":"","operations":[]})
    );
}

#[test]
fn duplicate_keys_rejected_at_every_level_including_escaped_aliases() {
    let raw = json!({"answer":"","operations":[operation("add",text("temp"))]}).to_string();
    for (old, new) in [
        ("\"answer\":\"\"", "\"answer\":\"\",\"answer\":\"hidden\""),
        ("\"op\":\"add\"", "\"op\":\"add\",\"op\":\"add\""),
        ("\"id\":\"temp\"", "\"id\":\"temp\",\"id\":\"other\""),
        ("\"type\":\"text\"", "\"type\":\"text\",\"type\":\"text\""),
        ("\"x\":1", "\"x\":1,\"\\u0078\":2"),
        ("\"r\":0", "\"r\":0,\"r\":255"),
    ] {
        assert!(raw.contains(old));
        let error = validate_edit_response(&raw.replace(old, new), &[], "local").unwrap_err();
        assert!(error.contains("duplicate JSON key"), "{error}");
    }
    let raw = json!({"answer":"","operations":[operation("add",math(leaf("a")))]}).to_string();
    assert!(validate_edit_response(
        &raw.replace("\"value\":\"a\"", "\"value\":\"a\",\"value\":\"b\""),
        &[],
        "local"
    )
    .unwrap_err()
    .contains("duplicate JSON key"));
}

#[test]
fn snapshot_ids_match_runtime_utf8_semantics() {
    for id in ["", " \t\n", "\u{3000}"] {
        invalid(text(id));
    }
    let id = format!("板书-{}", "界".repeat(200));
    let object = text(&id);
    valid(object.clone());
    response(vec![operation("update", object.clone())], std::slice::from_ref(&object)).unwrap();
    let wire = response(vec![json!({"op":"delete","id":id})], std::slice::from_ref(&object)).unwrap();
    assert_eq!(wire["operations"][0]["id"], id);
    assert!(validate_snapshot(&[object.clone(), object]).is_err());
    assert!(response(vec![], &[text("")]).is_err());
}

#[test]
fn add_ids_are_local_deterministic_and_collision_free() {
    let snapshot = [text("local-1"), text("local-3")];
    let wire = response(
        vec![
            operation("add", text("temp")),
            operation("add", text("local-2")),
        ],
        &snapshot,
    )
    .unwrap();
    assert_eq!(wire["operations"][0]["object"]["id"], "local-4");
    assert_eq!(wire["operations"][1]["object"]["id"], "local-5");
    assert_eq!(snapshot[0]["id"], "local-1");
    assert!(response(vec![operation("add", text("local-1"))], &snapshot).is_err());
    // Deleting an authorized object never frees its ID for an add in this batch.
    let wire = response(
        vec![
            json!({"op":"delete","id":"local-1"}),
            operation("add", text("temp")),
        ],
        &snapshot,
    )
    .unwrap();
    assert_eq!(wire["operations"][1]["object"]["id"], "local-2");
}

#[test]
fn temporary_ids_and_prefixes_are_short_safe_ascii() {
    for id in [
        "".into(),
        "临时".into(),
        "a b".into(),
        "../file".into(),
        "a\n".into(),
        "x".repeat(129),
    ] {
        assert!(response(vec![operation("add", text(&id))], &[]).is_err());
        assert!(validate_edit_response("{\"answer\":\"\",\"operations\":[]}", &[], &id).is_err());
    }
    response(vec![operation("add", text(&"x".repeat(128)))], &[]).unwrap();
    let wire = validate_edit_response(
        &json!({"answer":"","operations":[operation("add",text("a"))]}).to_string(),
        &[],
        &"p".repeat(128),
    )
    .unwrap();
    assert_eq!(
        wire["operations"][0]["object"]["id"]
            .as_str()
            .unwrap()
            .len(),
        130
    );
}

#[test]
fn update_and_delete_require_original_authorized_ids_and_same_kind() {
    let snapshot = [text("known")];
    for op in [
        operation("update", text("unknown")),
        json!({"op":"delete","id":"unknown"}),
        operation(
            "update",
            json!({"id":"known","kind":{"type":"coordinate_system","origin":point(),"scale":1}}),
        ),
    ] {
        assert!(response(vec![op], &snapshot).is_err());
    }
    response(vec![operation("update", text("known"))], &snapshot).unwrap();
    response(vec![json!({"op":"delete","id":"known"})], &snapshot).unwrap();
    for follow in [
        operation("update", text("temp")),
        json!({"op":"delete","id":"temp"}),
        json!({"op":"delete","id":"local-1"}),
    ] {
        assert!(response(vec![operation("add", text("temp")), follow], &snapshot).is_err());
    }
}

#[test]
fn any_repeated_touch_is_rejected() {
    let update = operation("update", text("known"));
    let delete = json!({"op":"delete","id":"known"});
    for first in [&update, &delete] {
        for second in [&update, &delete] {
            assert!(response(vec![first.clone(), second.clone()], &[text("known")]).is_err());
        }
    }
    assert!(response(
        vec![
            operation("add", text("temp")),
            operation("add", text("temp"))
        ],
        &[]
    )
    .is_err());
}

#[test]
fn operations_and_every_schema_level_reject_unknown_or_missing_fields() {
    for op in [
        json!({"op":"rpc","method":"objects.apply"}),
        json!({"op":"delete","id":"known","object":text("known")}),
        json!({"op":"update","id":"known","object":text("known")}),
        json!({"op":"add"}),
        json!({"op":"update","object":{"id":"known","kind":{"type":"text","text":"patch"}}}),
    ] {
        assert!(response(vec![op], &[text("known")]).is_err());
    }
    let objects = [
        text("text"),
        shape("line"),
        stroke(1),
        plot(json!(["x"])),
        math(leaf("x")),
        json!({"id":"axes","kind":{"type":"coordinate_system","origin":point(),"scale":1}}),
    ];
    for object in objects {
        for pointer in ["", "/kind"] {
            let keys: Vec<_> = object
                .pointer(pointer)
                .unwrap()
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect();
            for key in keys {
                let mut bad = object.clone();
                bad.pointer_mut(pointer)
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .remove(&key);
                invalid(bad);
            }
            let mut bad = object.clone();
            bad.pointer_mut(pointer)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert("script".into(), json!("evil"));
            invalid(bad);
        }
    }
    for (mut object, pointer) in [
        (text("text"), "/kind/position"),
        (text("text"), "/kind/color"),
        (shape("line"), "/kind/style"),
        (shape("line"), "/kind/points/0"),
        (stroke(1), "/kind/points/0"),
        (math(leaf("x")), "/kind/layout"),
    ] {
        object
            .pointer_mut(pointer)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("extra".into(), json!(null));
        invalid(object);
    }
}

#[test]
fn geometry_font_color_and_style_bounds() {
    for n in [-100_000.0, 100_000.0] {
        let mut o = text("a");
        o["kind"]["position"]["x"] = json!(n);
        valid(o);
    }
    for n in [-100_000.1, 100_000.1, 1e300] {
        let mut o = text("a");
        o["kind"]["position"]["y"] = json!(n);
        invalid(o);
    }
    for n in [0.0, -1.0, 512.1, 1e-100] {
        let mut o = text("a");
        o["kind"]["size"] = json!(n);
        invalid(o);
    }
    let mut o = text("a");
    o["kind"]["size"] = json!(512);
    valid(o);
    for n in [
        json!(-1),
        json!(256),
        json!(1.5),
        json!(1.0),
        json!("0"),
        Value::Null,
    ] {
        let mut o = text("a");
        o["kind"]["color"]["r"] = n;
        invalid(o);
    }
    for n in [0.0, 0.099, 100.1] {
        let mut o = shape("line");
        o["kind"]["style"]["width"] = json!(n);
        invalid(o);
    }
    let mut o = shape("line");
    o["kind"]["style"]["width"] = json!(100);
    valid(o);
    let mut o = shape("line");
    o["kind"]["style"]["dashed"] = json!(0);
    invalid(o);
    for n in [0.0, -1.0, 10000.1, 1e-100] {
        invalid(json!({"id":"a","kind":{"type":"coordinate_system","origin":point(),"scale":n}}));
    }
    valid(json!({"id":"a","kind":{"type":"coordinate_system","origin":point(),"scale":10000}}));
    let raw = json!({"answer":"","operations":[operation("add",text("a"))]}).to_string();
    for n in ["1e999", "NaN", "Infinity"] {
        assert!(validate_edit_response(
            &raw.replace("\"x\":1", &format!("\"x\":{n}")),
            &[],
            "local"
        )
        .is_err());
    }
}

#[test]
fn shape_and_stroke_point_limits() {
    invalid(shape("polygon"));
    for count in [0, 1, 3, 256, 257] {
        let mut o = shape("line");
        o["kind"]["points"] = json!(vec![point(); count]);
        invalid(o);
    }
    for count in [2, 256] {
        let mut o = shape("rectangle");
        o["kind"]["points"] = json!(vec![point(); count]);
        valid(o);
    }
    let mut o = shape("rectangle");
    o["kind"]["points"] = json!(vec![point(); 257]);
    invalid(o);
    invalid(stroke(0));
    invalid(stroke(4097));
    // The full 4096-point object exceeds the separate snapshot byte budget.
    validate_object(&stroke(4096), &mut 0).unwrap();
    for (key, value) in [
        ("time", json!(-0.1)),
        ("pressure", json!(-0.01)),
        ("pressure", json!(1.01)),
        ("x", json!(100001)),
    ] {
        let mut o = stroke(1);
        o["kind"]["points"][0][key] = value;
        invalid(o);
    }
    let mut o = stroke(1);
    o["kind"]["points"][0]["time"] = json!(1e300);
    valid(o);
}

#[test]
fn plot_ranges_sizes_counts_and_runtime_float_precision() {
    for key in ["width", "height"] {
        for n in [0.0, -1.0, 10000.1, 1e-100] {
            let mut o = plot(json!(["x"]));
            o["kind"][key] = json!(n);
            invalid(o);
        }
        let mut o = plot(json!(["x"]));
        o["kind"][key] = json!(10000);
        valid(o);
    }
    for (lo, hi) in [
        (1.0, 1.0),
        (2.0, 1.0),
        (-1e308, 1e308),
        (-f64::MAX, f64::MAX),
    ] {
        for axis in ["x", "y"] {
            let mut o = plot(json!(["x"]));
            o["kind"][format!("{axis}_min")] = json!(lo);
            o["kind"][format!("{axis}_max")] = json!(hi);
            invalid(o.clone());
            assert!(response(vec![operation("add", o)], &[]).is_err());
        }
    }
    for (lo, hi) in [
        (1.0, 1.0000000001),
        (0.0, 1e40),
        (-3e38, 3e38),
        (0.0, 1e100),
        (-1e307, 1e307),
    ] {
        for axis in ["x", "y"] {
            let mut o = plot(json!(["x"]));
            o["kind"][format!("{axis}_min")] = json!(lo);
            o["kind"][format!("{axis}_max")] = json!(hi);
            valid(o.clone());
            response(vec![operation("add", o)], &[]).unwrap();
        }
    }
    invalid(plot(json!([])));
    invalid(plot(json!([""])));
    invalid(plot(json!([" \t"])));
    invalid(plot(json!([null])));
    invalid(plot(json!(vec!["x"; 17])));
    valid(plot(json!(vec!["x"; 16])));
}

#[test]
fn plot_restricted_tokens_allow_actual_runtime_functions_but_no_commands() {
    for expr in [
        "sin(x)+cos(x)",
        "tan(x)",
        "asin(x)+arcsin(x)",
        "acos(x)+arccos(x)",
        "atan(x)+arctan(x)",
        "ln(x)+log(x)",
        "sqrt(abs(x))",
        "exp(x)",
        "sinh(x)+cosh(x)+tanh(x)",
        "asinh(x)+acosh(x)+atanh(x)",
        "pi*x+e",
        "2x+3xy",
        ".5*x+1e-2+2E3",
        "(x-1)^2/9+(y+2)^2/4=1",
        "x=2",
    ] {
        valid(plot(json!([expr])));
    }
    for expr in [
        "alert(x)",
        "sinister(x)",
        "sin.constructor(x)",
        "import(x)",
        "eval(x)",
        "diff(x,x)",
        "integrate(x,x)",
        "simplify(x)",
        "floor(x)",
        "x;delete",
        "x[0]",
        "{x}",
        "x\\y",
        "'x'",
        "\"x\"",
        "x_1",
        "x²",
        "π*x",
        "x\0",
        "x<2",
        "1e999",
        "1..2",
        "a*x",
        "x=y=2",
        "(x=2)",
        "(x",
        "x)",
    ] {
        assert!(validate_snapshot(&[plot(json!([expr]))]).is_err(), "{expr}");
    }
    invalid(plot(json!([format!("{}x", " ".repeat(4096))])));
    invalid(plot(json!(["x+".repeat(257)])));
    invalid(plot(json!([format!(
        "{}x{}",
        "(".repeat(65),
        ")".repeat(65)
    )])));
}

#[test]
fn expressions_have_an_aggregate_budget_across_objects() {
    let padded = format!("{}x", " ".repeat(4095));
    valid(plot(json!(vec![padded.clone(); 4])));
    invalid(plot(json!(vec![padded.clone(); 5])));
    let mut second = plot(json!(["x"]));
    second["id"] = json!("second");
    assert!(validate_snapshot(&[plot(json!(vec![padded; 4])), second]).is_err());
}

#[test]
fn math_layout_exact_adjacently_tagged_schema() {
    valid(math(
        json!({"type":"row","value":[leaf("x"),json!({"type":"fraction","value":[leaf("1"),leaf("2")]}),json!({"type":"radical","value":leaf("3")})]}),
    ));
    valid(math(json!({"type":"row","value":[]})));
    for layout in [
        json!({"type":"fraction","numerator":leaf("1"),"denominator":leaf("2")}),
        json!({"type":"fraction","value":[leaf("1")]}),
        json!({"type":"fraction","value":[leaf("1"),leaf("2"),leaf("3")]}),
        json!({"type":"radical","value":[leaf("x")]}),
        json!({"type":"text","value":1}),
        json!({"type":"latex","value":"x"}),
        json!({"type":"text","value":"x","script":"run"}),
    ] {
        invalid(math(layout));
    }
}

#[test]
fn math_depth_nodes_and_utf8_text_budgets() {
    let mut layout = leaf("x");
    for _ in 1..32 {
        layout = json!({"type":"radical","value":layout});
    }
    valid(math(layout.clone()));
    response(vec![operation("add", math(layout.clone()))], &[]).unwrap();
    invalid(math(json!({"type":"radical","value":layout})));
    valid(math(json!({"type":"row","value":vec![leaf("");511]})));
    invalid(math(json!({"type":"row","value":vec![leaf("");512]})));
    valid(math(leaf(&"a".repeat(4096))));
    invalid(math(leaf(&"a".repeat(4097))));
    invalid(math(
        json!({"type":"row","value":[leaf(&"界".repeat(1000)),leaf(&"界".repeat(400))]}),
    ));
    let mut layout = leaf("x");
    for _ in 0..150 {
        layout = json!({"type":"radical","value":layout});
    }
    assert!(response(vec![operation("add", math(layout))], &[]).is_err());
}

#[test]
fn snapshot_count_and_exact_serialized_utf8_budget() {
    let objects: Vec<_> = (0..256).map(|i| text(&format!("id-{i}"))).collect();
    validate_snapshot(&objects).unwrap();
    let mut too_many = objects;
    too_many.push(text("last"));
    assert!(validate_snapshot(&too_many).is_err());
    let mut objects = vec![text("a")];
    objects[0]["kind"]["text"] = json!("");
    let overhead = serde_json::to_vec(&objects).unwrap().len();
    objects[0]["kind"]["text"] = json!("a".repeat(MAX_SNAPSHOT_BYTES - overhead));
    validate_snapshot(&objects).unwrap();

    objects[0]["kind"]["text"] = json!("a".repeat(MAX_SNAPSHOT_BYTES - overhead + 1));
    assert!(validate_snapshot(&objects).is_err());
    objects[0]["kind"]["text"] = json!("界".repeat(MAX_SNAPSHOT_BYTES / 2));
    assert!(validate_snapshot(&objects).is_err());
    // JSON escaping, not only decoded string length, consumes the budget.
    objects[0]["kind"]["text"] = json!("\0".repeat(MAX_SNAPSHOT_BYTES / 5));
    assert!(validate_snapshot(&objects).is_err());
}

#[test]
fn response_operation_count_input_and_output_byte_limits() {
    let snapshot: Vec<_> = (0..65).map(|i| text(&format!("id-{i}"))).collect();
    let deletes: Vec<_> = snapshot
        .iter()
        .map(|o| json!({"op":"delete","id":o["id"]}))
        .collect();
    response(deletes[..64].to_vec(), &snapshot).unwrap();
    assert!(response(deletes, &snapshot).is_err());
    let base = "{\"answer\":\"\",\"operations\":[]}";
    let exact = format!("{}{}", base, " ".repeat(MAX_RESPONSE_BYTES - base.len()));
    validate_edit_response(&exact, &[], "local").unwrap();
    assert!(validate_edit_response(&(exact + " "), &[], "local").is_err());
    let wire = json!({"answer":"a".repeat(MAX_WIRE_BYTES-29),"operations":[]});
    assert_eq!(serde_json::to_vec(&wire).unwrap().len(), MAX_WIRE_BYTES);
    serialized_limit(&wire, MAX_WIRE_BYTES).unwrap();
    assert!(serialized_limit(&wire, MAX_WIRE_BYTES - 1).is_err());
    // Rewriting can grow the wire; the final size is measured on rewritten IDs.
    let adds: Vec<_> = (0..64)
        .map(|i| operation("add", text(&format!("a{i}"))))
        .collect();
    let raw = json!({"answer":"","operations":adds}).to_string();
    let wire = validate_edit_response(&raw, &[], &"p".repeat(128)).unwrap();
    assert!(serde_json::to_vec(&wire).unwrap().len() > raw.len());
    assert!(serde_json::to_vec(&wire).unwrap().len() <= MAX_WIRE_BYTES);
}
