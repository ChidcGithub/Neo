use super::*;
use serde_json::json;

fn args(value: serde_json::Value) -> Args<'static> {
    let tool = crate::find("shortcut").unwrap();
    let value: &'static serde_json::Value = Box::leak(Box::new(value));
    Args::new(tool, value)
}

fn act(scope: &Scope, args: &Args) -> Result<Outcome, ToolError> {
    super::act(scope, args)
}

#[test]
fn preview_reads_like_a_sentence() {
    assert_eq!(preview(&args(json!({"keys": "ctrl+c"}))), "按 ctrl+c");
    assert_eq!(
        preview(&args(json!({"keys": "alt+tab", "repeat": 3}))),
        "按 alt+tab（3 次）"
    );
}

#[test]
fn invalid_chord_is_rejected_before_input() {
    let scope = Scope::new(std::env::temp_dir());
    let error = act(&scope, &args(json!({"keys": "ctrl+nope"}))).unwrap_err();
    assert_eq!(error.kind, crate::result::ErrorKind::BadArguments);
    assert!(error.message.contains("无法识别"));
}

#[test]
fn empty_chord_is_rejected() {
    let scope = Scope::new(std::env::temp_dir());
    let error = act(&scope, &args(json!({"keys": "  "}))).unwrap_err();
    assert_eq!(error.kind, crate::result::ErrorKind::BadArguments);
}

#[test]
fn repeat_out_of_range_is_rejected_by_args() {
    let scope = Scope::new(std::env::temp_dir());
    let error = act(&scope, &args(json!({"keys": "ctrl+c", "repeat": 11}))).unwrap_err();
    assert_eq!(error.kind, crate::result::ErrorKind::BadArguments);
}
