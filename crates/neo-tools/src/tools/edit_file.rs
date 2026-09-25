//! `edit_file` —— 精确替换文件里的一段文本。
//!
//! 这是**默认的改文件方式**：只动该动的地方，文件其余部分原样保留。
//!
//! 反含糊设计：
//! - `old_string` 必须**唯一命中**，命中 0 处报 `not_found`、多于一处的报
//!   `not_unique` 并附带命中位置 —— 宁可失败，也不"猜一个改掉"；
//! - 真要批量替换，必须显式 `replace_all: true`，并在确认框里看到替换次数。

use serde_json::json;

use crate::result::{Args, ErrorKind, Outcome, ToolError};
use crate::spec::Param;
use crate::Scope;

use super::limits;

pub static PARAMS: &[Param] = &[
    Param::text("path", "要修改的文件路径，相对工作区。文件必须已存在（新建请用 write_file）。"),
    Param::text(
        "old_string",
        "要被替换掉的原文，必须与文件内容**逐字符**一致（含缩进与换行）。多带几行上下文可以保证唯一命中。",
    ),
    Param::text(
        "new_string",
        "替换成的新文本。要删除这段就留空字符串。",
    ),
    Param::flag(
        "replace_all",
        "是否替换全部命中。默认 false：命中多处时直接报错，避免误伤。",
    ),
];

pub fn preview(args: &Args) -> String {
    let path = args.opt_str("path").unwrap_or_default();
    let old = args.opt_str("old_string").unwrap_or_default();
    let all = args.flag("replace_all").unwrap_or(false);
    let head: String = old.lines().next().unwrap_or("").chars().take(48).collect();
    if all {
        format!("在 {path} 中把所有「{head}」替换掉")
    } else {
        format!("在 {path} 中把「{head}」替换一次")
    }
}

pub fn run(scope: &Scope, args: &Args) -> Outcome {
    match edit(scope, args) {
        Ok(o) => o,
        Err(e) => Outcome::fail("edit_file", e),
    }
}

fn edit(scope: &Scope, args: &Args) -> Result<Outcome, ToolError> {
    let raw = args.require_str("path")?;
    let old = args.require_str("old_string")?;
    let new = args.require_present_str("new_string")?;
    let replace_all = args.flag("replace_all")?;

    if old == new {
        return Err(ToolError::new(
            ErrorKind::BadArguments,
            "`old_string` 与 `new_string` 相同，这次调用不会改变任何东西",
        )
        .with_hint("要么给出真正不同的新文本，要么不必调用本工具"));
    }
    if new.len() as u64 > limits::WRITE_BYTES {
        return Err(ToolError::new(
            ErrorKind::TooLarge,
            format!("新文本 {} 字节，超过上限", new.len()),
        ));
    }

    let path = scope.resolve(&raw)?;
    let meta = std::fs::metadata(&path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => ToolError::not_found(format!("文件不存在：{raw}"))
            .with_hint("本工具只改已存在的文件；新建请用 `write_file`"),
        _ => ToolError::io(format!("无法访问 {raw}：{e}")),
    })?;
    if meta.len() > limits::READ_BYTES {
        return Err(ToolError::new(
            ErrorKind::TooLarge,
            format!("{raw} 有 {} 字节，超过可编辑上限", meta.len()),
        )
        .with_hint("先用 `read_file` 分段确认要改的片段，大文件改用 `bash` 处理"));
    }
    let path = scope.verify_existing(&path)?;

    let bytes = std::fs::read(&path).map_err(|e| ToolError::io(format!("读取 {raw} 失败：{e}")))?;
    let text = String::from_utf8(bytes).map_err(|_| {
        ToolError::new(ErrorKind::Unsupported, format!("{raw} 不是 UTF-8 文本"))
            .with_hint("二进制或非 UTF-8 文件请用 `bash` 处理")
    })?;

    let hits: Vec<usize> = text.match_indices(&old).map(|(i, _)| i).collect();
    if hits.is_empty() {
        let head: String = old.chars().take(60).collect();
        return Err(ToolError::not_found(format!(
            "在 {raw} 里找不到 `old_string`（首行：{head}）"
        ))
        .with_hint(
            "先用 `read_file` 读一遍原文，确认缩进、全半角与换行都一致；不要凭记忆写 old_string",
        ));
    }
    if hits.len() > 1 && !replace_all {
        // 把命中所在的**行号**回给模型 —— 它据此加长上下文即可唯一定位。
        let lines: Vec<usize> = hits
            .iter()
            .map(|i| text[..*i].bytes().filter(|b| *b == b'\n').count() + 1)
            .take(20)
            .collect();
        return Err(ToolError::new(
            ErrorKind::NotUnique,
            format!(
                "`old_string` 在 {raw} 里命中 {} 处（第 {:?} 行），无法确定改哪一处",
                hits.len(),
                lines
            ),
        )
        .with_hint("多带几行上下文让旧文本唯一；确定要全部替换时把 `replace_all` 设为 true"));
    }

    let replacements = if replace_all { hits.len() } else { 1 };
    let updated = if replace_all {
        text.replace(&old, &new)
    } else {
        text.replacen(&old, &new, 1)
    };
    std::fs::write(&path, updated.as_bytes())
        .map_err(|e| ToolError::io(format!("写回 {raw} 失败：{e}")))?;

    let delta = updated.len() as i64 - text.len() as i64;
    Ok(Outcome::ok(
        "edit_file",
        format!("{raw}：替换 {replacements} 处（{:+} 字节）", delta),
        json!({
            "path": scope.display(&path),
            "replacements": replacements,
            "bytes_before": text.len(),
            "bytes_after": updated.len(),
            "old_preview": preview_text(&old),
            "new_preview": preview_text(&new),
        }),
    ))
}

/// 替换内容的短预览（单行、截断），只用于展示。
fn preview_text(s: &str) -> String {
    let mut out = String::new();
    for (i, line) in s.lines().take(3).enumerate() {
        if i > 0 {
            out.push('⏎');
        }
        out.push_str(line);
    }
    let count = out.chars().count();
    if count > 80 {
        out = out.chars().take(80).collect::<String>() + "…";
    } else if s.lines().count() > 3 {
        out.push('…');
    }
    out
}
