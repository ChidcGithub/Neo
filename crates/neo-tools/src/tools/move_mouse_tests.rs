use super::*;
use serde_json::json;

fn args(value: serde_json::Value) -> Args<'static> {
    let tool = crate::find("move_mouse").unwrap();
    let value: &'static serde_json::Value = Box::leak(Box::new(value));
    Args::new(tool, value)
}

fn act(scope: &Scope, args: &Args) -> Result<Outcome, ToolError> {
    super::act(scope, args)
}

#[test]
fn preview_reads_like_a_sentence() {
    assert_eq!(
        preview(&args(json!({"element_id": 8, "duration_ms": 200}))),
        "把鼠标移到元素 #8（200 ms）"
    );
    assert_eq!(
        preview(&args(json!({"x": 10, "y": 20}))),
        "把鼠标移到 (10, 20)（0 ms）"
    );
}

#[test]
fn missing_locator_is_rejected() {
    let scope = Scope::new(std::env::temp_dir());
    let error = act(&scope, &args(json!({"duration_ms": 100}))).unwrap_err();
    assert!(error.message.contains("缺少鼠标移动目标"));
}

#[test]
fn duration_out_of_range_is_rejected_by_args() {
    let scope = Scope::new(std::env::temp_dir());
    let error = act(&scope, &args(json!({"x": 1, "y": 2, "duration_ms": 10001}))).unwrap_err();
    assert_eq!(error.kind, crate::result::ErrorKind::BadArguments);
}
