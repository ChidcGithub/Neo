//! `write_file` —— 写入 / 整体覆盖。
//!
//! 边界：**整文件语义**。改一小段请用 [`super::edit_file`] ——
//! 覆盖式写法会把文件里没在上下文里的内容一起抹掉，是最容易出事故的一类操作。
//! 因此 `overwrite` 默认为 `false`：目标已存在时必须显式要求覆盖。

use serde_json::json;

use crate::result::{Args, ErrorKind, Outcome, ToolError};
use crate::spec::Param;
use crate::Scope;

use super::limits;

pub static PARAMS: &[Param] = &[
    Param::text(
        "path",
        "目标文件路径，相对工作区。父目录不存在时会自动创建。",
    ),
    Param::text(
        "content",
        "要写入的完整文本。这是**整份文件内容**，不是在末尾追加。",
    ),
    Param::flag(
        "overwrite",
        "目标已存在时是否允许覆盖。默认 false —— 先 read_file 看清原内容，确有把握再改 true。",
    ),
];

pub fn preview(args: &Args) -> String {
    let path = args.opt_str("path").unwrap_or_default();
    let len = args
        .opt_str("content")
        .map(|c| c.chars().count())
        .unwrap_or(0);
    if args.flag("overwrite").unwrap_or(false) {
        format!("覆盖写入 {path}（{len} 字符）")
    } else {
        format!("写入新文件 {path}（{len} 字符）")
    }
}

pub fn run(scope: &Scope, args: &Args) -> Outcome {
    match write(scope, args) {
        Ok(o) => o,
        Err(e) => Outcome::fail("write_file", e),
    }
}

fn write(scope: &Scope, args: &Args) -> Result<Outcome, ToolError> {
    let raw = args.require_str("path")?;
    let content = args.require_present_str("content")?;
    let overwrite = args.flag("overwrite")?;

    if content.len() as u64 > limits::WRITE_BYTES {
        return Err(ToolError::new(
            ErrorKind::TooLarge,
            format!(
                "内容 {} 字节，超过 {} 字节上限",
                content.len(),
                limits::WRITE_BYTES
            ),
        )
        .with_hint("拆成多个文件，或改用 `bash` 生成大文件"));
    }

    let path = scope.resolve(&raw)?;
    let existed = path.exists();
    if existed {
        // 已存在的目标：先复查真实路径，避免顺着链接写到工作区外。
        let real = scope.verify_existing(&path)?;
        if real.is_dir() {
            return Err(
                ToolError::new(ErrorKind::Unsupported, format!("{raw} 是目录"))
                    .with_hint("给一个文件名，例如 `src/main.rs`"),
            );
        }
        if !overwrite {
            let bytes = std::fs::metadata(&real).map(|m| m.len()).unwrap_or(0);
            return Err(ToolError::new(
                ErrorKind::Conflict,
                format!("{raw} 已存在（{bytes} 字节），未允许覆盖"),
            )
            .with_hint("先 `read_file` 看原内容；确认要整体替换再把 `overwrite` 设为 true，或改用 `edit_file` 只改一段"));
        }
    } else if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ToolError::io(format!("无法创建目录 {}：{e}", scope.display(parent))))?;
    }

    let bytes = content.len() as u64;
    let lines = content.lines().count();
    std::fs::write(&path, &content).map_err(|e| ToolError::io(format!("写入 {raw} 失败：{e}")))?;

    let summary = if existed {
        format!("覆盖 {raw}（{lines} 行 / {bytes} 字节）")
    } else {
        format!("新建 {raw}（{lines} 行 / {bytes} 字节）")
    };
    Ok(Outcome::ok(
        "write_file",
        summary,
        json!({
            "path": scope.display(&path),
            "bytes": bytes,
            "lines": lines,
            "created": !existed,
            "overwritten": existed,
        }),
    ))
}
