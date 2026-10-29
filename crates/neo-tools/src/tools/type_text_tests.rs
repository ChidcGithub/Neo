use super::*;
use serde_json::json;

fn act(scope: &Scope, args: &Args) -> Result<Outcome, ToolError> {
    super::act(scope, args)
}

fn args(value: serde_json::Value) -> Args<'static> {
    let tool = crate::find("type_text").unwrap();
    // The tool registry is static; leaking the value keeps the test helper simple.
    let value: &'static serde_json::Value = Box::leak(Box::new(value));
    Args::new(tool, value)
}

#[test]
fn preview_reads_like_a_sentence() {
    let value = args(json!({"element_id": 7, "text": "hello world"}));
    assert_eq!(preview(&value), "在元素 #7 输入「hello world」");
    let value =
        args(json!({"x": 10, "y": 20, "text": "hello", "clear": true, "press_enter": true}));
    assert_eq!(
        preview(&value),
        "在 (10, 20) 输入「hello」（先清空）（回车）"
    );
}

#[test]
fn empty_text_without_action_is_rejected() {
    let scope = Scope::new(std::env::temp_dir());
    let error = act(&scope, &args(json!({"x": 1, "y": 2, "text": ""}))).unwrap_err();
    assert_eq!(error.kind, crate::result::ErrorKind::BadArguments);
    assert!(error.message.contains("没有可执行的动作"));
}

#[test]
fn invalid_caret_is_rejected_before_input() {
    let scope = Scope::new(std::env::temp_dir());
    let error = act(
        &scope,
        &args(json!({"x": 1, "y": 2, "text": "x", "caret": "middle"})),
    )
    .unwrap_err();
    assert_eq!(error.kind, crate::result::ErrorKind::BadArguments);
    assert!(error.message.contains("caret"));
}

#[test]
fn missing_locator_is_rejected() {
    let scope = Scope::new(std::env::temp_dir());
    let error = act(&scope, &args(json!({"text": "hello"}))).unwrap_err();
    assert!(error.message.contains("缺少输入目标"));
}

#[test]
fn mixed_locator_is_rejected() {
    let scope = Scope::new(std::env::temp_dir());
    let error = act(
        &scope,
        &args(json!({"element_id": 1, "x": 1, "y": 1, "text": "x"})),
    )
    .unwrap_err();
    assert!(error.message.contains("不可混用"));
}

#[test]
fn stale_element_is_rejected_before_input() {
    let scope = Scope::new(std::env::temp_dir());
    let error = act(
        &scope,
        &args(json!({"element_id": 9999, "snapshot_id": "gone", "text": "x"})),
    )
    .unwrap_err();
    assert!(error.message.contains("9999"));
    assert!(error.hint.unwrap().contains("screen_elements"));
}
