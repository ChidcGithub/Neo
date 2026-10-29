use super::*;
use serde_json::json;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

fn args(value: serde_json::Value) -> Args<'static> {
    let tool = crate::find("wait").unwrap();
    let value: &'static serde_json::Value = Box::leak(Box::new(value));
    Args::new(tool, value)
}

#[test]
fn preview_reads_like_a_sentence() {
    assert_eq!(preview(&args(json!({"seconds": 3}))), "等待 3 秒");
}

#[test]
fn zero_seconds_returns_immediately() {
    let scope = Scope::new(std::env::temp_dir());
    let outcome = run(&scope, &args(json!({"seconds": 0})));
    assert!(outcome.is_ok(), "{:?}", outcome.error);
    assert_eq!(outcome.data["seconds"], 0);
}

#[test]
fn cancellation_stops_before_sleeping() {
    let cancel = Arc::new(AtomicBool::new(true));
    let scope = Scope::new(std::env::temp_dir()).with_cancel(cancel);
    let outcome = run(&scope, &args(json!({"seconds": 5})));
    let error = outcome.error.expect("应当被取消");
    assert_eq!(error.kind, crate::result::ErrorKind::NotAllowed);
}

#[test]
fn seconds_out_of_range_is_rejected_by_args() {
    let scope = Scope::new(std::env::temp_dir());
    let outcome = run(&scope, &args(json!({"seconds": 121})));
    assert_eq!(
        outcome.error.expect("应当失败").kind,
        crate::result::ErrorKind::BadArguments
    );
}
