use super::*;
use serde_json::json;

fn args(value: serde_json::Value) -> Args<'static> {
    let tool = crate::find("app_control").unwrap();
    let value: &'static serde_json::Value = Box::leak(Box::new(value));
    Args::new(tool, value)
}

#[test]
fn preview_reads_like_a_sentence() {
    assert_eq!(
        preview(&args(json!({"mode": "launch", "name": "记事本"}))),
        "启动应用「记事本」"
    );
    assert_eq!(
        preview(&args(
            json!({"mode": "launch_executable", "executable": "C:\\a.exe"})
        )),
        "启动可执行文件「C:\\a.exe」"
    );
    assert_eq!(
        preview(&args(
            json!({"mode": "resize", "name": "记事本", "width": 800, "height": 600})
        )),
        "缩放窗口「记事本」到 800×600"
    );
}

#[test]
fn invalid_args_json_is_rejected() {
    let error = parse_args("[not json").unwrap_err();
    assert_eq!(error.kind, crate::result::ErrorKind::BadArguments);
}

#[test]
fn unknown_mode_is_rejected() {
    let scope = Scope::new(std::env::temp_dir());
    let outcome = run(&scope, &args(json!({"mode": "teleport"})));
    assert_eq!(
        outcome.error.expect("应当失败").kind,
        crate::result::ErrorKind::BadArguments
    );
}

#[test]
fn resize_requires_all_four_rect_params() {
    let scope = Scope::new(std::env::temp_dir());
    let outcome = run(
        &scope,
        &args(json!({"mode": "resize", "name": "definitely-not-a-window", "width": 800})),
    );
    assert!(outcome.error.is_some());
}
