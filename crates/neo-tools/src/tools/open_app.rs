//! `open_app` —— 把 Neo 主界面唤回到屏幕上。
//!
//! 语音唤醒走的是全程后台静默（进度看角落小窗），但有些结果用户终究要
//! 亲眼看到、亲手操作 —— 模型说一句「我打开主界面给你看」时就调它。
//!
//! 工具执行层碰不到 eframe 的窗口句柄，只能置一个信号位；
//! app 主循环每帧轮询，见到新时间戳就 `show_window`。
//! 「唤回自己的窗口」可见而无破坏，风险等级 [`crate::Risk::Open`]。

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::json;

use crate::result::{Args, Outcome};
use crate::Scope;

/// 「唤回主窗」信号（毫秒，Unix epoch；0 = 无请求）。
pub static SHOW_WINDOW_AT: AtomicU64 = AtomicU64::new(0);

pub fn preview(_args: &Args) -> String {
    "打开 Neo 主界面".to_owned()
}

pub fn run(_scope: &Scope, _args: &Args) -> Outcome {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    SHOW_WINDOW_AT.store(millis, Ordering::Relaxed);
    Outcome::ok("open_app", "已唤回 Neo 主界面", json!({"at": millis}))
}
