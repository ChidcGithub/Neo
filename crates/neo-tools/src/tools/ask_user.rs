//! `ask_user` —— 弹窗向用户提问，把点选的答案回给模型。
//!
//! 课堂场景里模型常遇到「拿不准用户要什么」的时刻：题没拍全想问要不要重拍、
//! 两种讲法想问先讲哪种。没有这个工具时它只能猜，猜错就白做一轮。
//!
//! 特殊在执行方式：**它不真正「执行」**。前端（确认窗管线）见到它时把调用挂起，
//! 弹出提问卡（问题 + 选项按钮），用户点选后由 `AppState::answer_question`
//! 直接把答案落成 [`Outcome`] —— 所以这里的 `run` 只是兜底，正常不会被调到。
//!
//! 弹窗只有按钮没有输入框（渲染层卡片不抢键盘焦点），所以选项要给全；
//! 用户想自由作答会直接说出来或在主窗补充，模型据此继续即可。
//! 风险档 [`crate::Risk::Read`]：它只是「问」，不碰机器。

use serde_json::json;

use crate::result::{Args, ErrorKind, Outcome, ToolError};
use crate::spec::Param;
use crate::Scope;

pub static PARAMS: &[Param] = &[
    Param::text("question", "要问用户的问题：一句话、具体、能靠点选选项回答。"),
    Param::opt_text(
        "options",
        "候选答案，用 | 分隔，2~4 个，第一个给最可能的（如「继续|换个思路|跳过」）。\
         选项要互斥且覆盖常见回答；不给则弹窗只有「跳过」。",
    ),
];

/// 把 options 参数解析成候选列表（去空白、去空项，最多 4 个）。
pub fn parse_options(raw: &str) -> Vec<String> {
    raw.split('|')
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .take(4)
        .collect()
}

pub fn preview(args: &Args) -> String {
    let q = args.opt_str("question").unwrap_or_default();
    format!("提问：{q}")
}

pub fn run(_scope: &Scope, _args: &Args) -> Outcome {
    // 正常路径不会走到：begin_tool_round 见到 ask_user 直接挂起等前端接管。
    // 真走到这里说明 UI 层没接管（例如无头批处理），如实告诉模型。
    Outcome::fail(
        "ask_user",
        ToolError::new(ErrorKind::Internal, "提问弹窗未被前端接管")
            .with_hint("当前环境没有提问弹窗；按最合理的假设继续，并在回答里说明这个假设"),
    )
}

/// 把用户的点选落成回灌给模型的 Outcome（前端 `answer_question` 用）。
pub fn answered(picked: Option<&str>) -> Outcome {
    match picked {
        Some(p) => Outcome::ok(
            "ask_user",
            format!("用户选择：{p}"),
            json!({"answer": p}),
        ),
        None => Outcome::ok(
            "ask_user",
            "用户跳过了这个问题（未作答）；按最合理的假设继续，并在回答里说明这个假设",
            json!({"answer": null}),
        ),
    }
}
