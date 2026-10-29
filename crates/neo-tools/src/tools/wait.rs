//! `wait` —— 可取消的短暂等待。

use std::time::{Duration, Instant};

use serde_json::json;

use crate::result::{Args, Outcome};
use crate::spec::Param;
use crate::Scope;

pub static PARAMS: &[Param] = &[Param::opt_int(
    "seconds",
    "等待秒数，默认 1，范围 0–120。",
    1,
    0,
    120,
)];

pub fn preview(args: &Args) -> String {
    format!("等待 {} 秒", args.opt_int("seconds").unwrap_or(1))
}

pub fn run(scope: &Scope, args: &Args) -> Outcome {
    match act(scope, args) {
        Ok(outcome) => outcome,
        Err(error) => Outcome::fail("wait", error),
    }
}

fn act(scope: &Scope, args: &Args) -> Result<Outcome, crate::result::ToolError> {
    let seconds = args.opt_int("seconds")?;
    let started = Instant::now();
    let deadline = started + Duration::from_secs(seconds as u64);
    while Instant::now() < deadline {
        if scope.is_cancelled() {
            return Err(crate::cancelled_error());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        std::thread::sleep(remaining.min(Duration::from_millis(50)));
    }
    Ok(Outcome::ok(
        "wait",
        format!("已等待 {seconds} 秒"),
        json!({ "seconds": seconds, "elapsed_ms": started.elapsed().as_millis() as u64 }),
    ))
}

#[cfg(test)]
#[path = "wait_tests.rs"]
mod tests;
