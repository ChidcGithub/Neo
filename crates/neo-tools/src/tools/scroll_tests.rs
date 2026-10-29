use super::*;
use serde_json::json;

fn args(value: serde_json::Value) -> Args<'static> {
    let tool = crate::find("scroll").unwrap();
    let value: &'static serde_json::Value = Box::leak(Box::new(value));
    Args::new(tool, value)
}

fn act(scope: &Scope, args: &Args) -> Result<Outcome, ToolError> {
    super::act(scope, args)
}

#[test]
fn preview_reads_like_a_sentence() {
    assert_eq!(
        preview(&args(
            json!({"element_id": 4, "direction": "up", "amount": 2})
        )),
        "在元素 #4 向 up 滚动 2 格"
    );
    assert_eq!(
        preview(&args(json!({"x": 1, "y": 2}))),
        "在 (1, 2) 向 down 滚动 3 格"
    );
}

#[test]
fn invalid_direction_is_rejected_before_input() {
    let scope = Scope::new(std::env::temp_dir());
    let error = act(
        &scope,
        &args(json!({"x": 1, "y": 2, "direction": "forward"})),
    )
    .unwrap_err();
    assert_eq!(error.kind, crate::result::ErrorKind::BadArguments);
    assert!(error.message.contains("direction"));
}

#[test]
fn amount_out_of_range_is_rejected_by_args() {
    let scope = Scope::new(std::env::temp_dir());
    let error = act(&scope, &args(json!({"x": 1, "y": 2, "amount": 21}))).unwrap_err();
    assert_eq!(error.kind, crate::result::ErrorKind::BadArguments);
}

#[test]
fn missing_locator_is_rejected() {
    let scope = Scope::new(std::env::temp_dir());
    let error = act(&scope, &args(json!({"direction": "down"}))).unwrap_err();
    assert!(error.message.contains("缺少滚动目标"));
}
