use super::*;
use serde_json::json;

fn args(value: serde_json::Value) -> Args<'static> {
    let tool = crate::find("desktop_snapshot").unwrap();
    let value: &'static serde_json::Value = Box::leak(Box::new(value));
    Args::new(tool, value)
}

#[test]
fn preview_reads_like_a_sentence() {
    assert_eq!(preview(&args(json!({}))), "查看焦点窗口的元素");
    assert_eq!(
        preview(&args(json!({"window": "记事本", "use_vision": true}))),
        "查看指定窗口的元素，并截图"
    );
    assert_eq!(
        preview(&args(json!({"include_elements": false}))),
        "查看焦点窗口的窗口列表"
    );
}

#[test]
fn max_elements_out_of_range_is_rejected_before_uia() {
    let scope = Scope::new(std::env::temp_dir());
    let outcome = run(&scope, &args(json!({"max_elements": 0})));
    assert_eq!(
        outcome.error.expect("应当失败").kind,
        crate::result::ErrorKind::BadArguments
    );
}
