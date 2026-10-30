//! `present_result` —— 把题目讲解、长任务结果或成果卡显示在屏幕中央。
//!
//! 它不真正「执行」：`begin_tool_round` 见到它就把卡片写进 `AppState`，
//! 由结果窗渲染，直到用户点关闭。这里只负责参数校验与兜底。
//!
//! 普通问答不该弹卡；只有用户发来题目、长任务或明确需要成果展示时才用。

use serde_json::json;

use crate::Scope;
use crate::result::{Args, ErrorKind, Outcome, ToolError};
use crate::spec::Param;

/// 标题上限（字符）。卡片标题只有一行，超长由界面省略。
pub const MAX_TITLE_CHARS: usize = 200;
/// 正文上限（字符）。结果卡支持滚动，但不接受无上限输入。
pub const MAX_CONTENT_CHARS: usize = 32_000;

pub static PARAMS: &[Param] = &[
    Param::text(
        "title",
        "卡片标题：一句话说明展示内容，如「勾股定理证明」「本周学习计划」。",
    ),
    Param::text(
        "content",
        "完整结果，Markdown；可用标题、列表、表格、代码块和 $公式$。上限 32000 字符。",
    ),
];

/// 一次展示请求的纯数据。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Card {
    pub title: String,
    pub content: String,
}

/// 解析并校验展示卡。`begin_tool_round` 与兜底 `run` 共用。
pub fn card(args: &Args) -> Result<Card, ToolError> {
    args.reject_unknown()?;
    let title = args.require_str("title")?;
    // 卡片标题只有一行；模型偶尔把副标题塞进 title，取首行即可。
    let title = title.trim().lines().next().unwrap_or("").trim().to_owned();
    if title.chars().count() > MAX_TITLE_CHARS {
        return Err(
            ToolError::bad_args(format!("title 最多 {MAX_TITLE_CHARS} 字符"))
                .with_hint("把标题压成一句话，细节放进 content"),
        );
    }
    let content = args.require_str("content")?;
    if content.chars().count() > MAX_CONTENT_CHARS {
        return Err(
            ToolError::bad_args(format!("content 最多 {MAX_CONTENT_CHARS} 字符"))
                .with_hint("精简内容，或分成多张结果卡展示"),
        );
    }
    Ok(Card { title, content })
}

pub fn preview(args: &Args) -> String {
    let title = args.opt_str("title").unwrap_or_default();
    format!("展示：{title}")
}

pub fn run(_scope: &Scope, _args: &Args) -> Outcome {
    // 正常路径不会走到：begin_tool_round 见到 present_result 直接落卡。
    Outcome::fail(
        "present_result",
        ToolError::new(ErrorKind::Internal, "结果卡片未被前端接管")
            .with_hint("当前环境没有结果窗口；直接把结果写在回答里"),
    )
}

/// 成功回灌给模型的结果（不含正文，避免重复占用上下文）。
pub fn presented(card: &Card) -> Outcome {
    Outcome::ok(
        "present_result",
        format!("已在屏幕中央展示「{}」，等待用户关闭", card.title),
        json!({
            "title": card.title,
            "chars": card.content.chars().count(),
        }),
    )
}

#[cfg(test)]
#[path = "present_result_tests.rs"]
mod tests;
