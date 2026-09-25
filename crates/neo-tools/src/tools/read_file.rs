//! `read_file` —— 查看文本文件。
//!
//! 边界：**只处理文本**。图片交给 [`super::view_image`]，Office 文档交给 [`super::read_document`]。
//! 分段读取（`offset` / `limit`）是为了让"文件太大"永远有出路 ——
//! 报错的 hint 里会直接写清楚该怎么分段，模型不需要猜。

use serde_json::json;

use crate::result::{Args, ErrorKind, Outcome, ToolError};
use crate::spec::Param;
use crate::Scope;

use super::limits;

pub static PARAMS: &[Param] = &[
    Param::text(
        "path",
        "文件路径，相对工作区；例如 `src/main.rs`。不要写工作区之外的路径。",
    ),
    Param::opt_int(
        "offset",
        "起始行号（从 0 开始）。大文件分段读时，用上次返回的 next_offset。",
        0,
        0,
        10_000_000,
    ),
    Param::opt_int(
        "limit",
        "最多返回多少行。默认 400，上限 2000。",
        400,
        1,
        2000,
    ),
];

pub fn preview(args: &Args) -> String {
    let path = args.opt_str("path").unwrap_or_default();
    let offset = args.opt_int("offset").unwrap_or(0);
    let limit = args.opt_int("limit").unwrap_or(400);
    if offset > 0 {
        format!("读取 {path} 第 {offset} 行起最多 {limit} 行")
    } else {
        format!("读取 {path}（最多 {limit} 行）")
    }
}

pub fn run(scope: &Scope, args: &Args) -> Outcome {
    match read(scope, args) {
        Ok(out) => out,
        Err(e) => Outcome::fail("read_file", e),
    }
}

fn read(scope: &Scope, args: &Args) -> Result<Outcome, ToolError> {
    let raw = args.require_str("path")?;
    let offset = args.opt_int("offset")? as usize;
    let limit = args.opt_int("limit")? as usize;

    let path = scope.resolve(&raw)?;
    let meta = std::fs::metadata(&path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => ToolError::not_found(format!("文件不存在：{raw}"))
            .with_hint("先用 `bash` 跑 `ls` 看看目录里有什么；路径不要带工作区前缀"),
        _ => ToolError::io(format!("无法访问 {raw}：{e}")),
    })?;
    if meta.is_dir() {
        return Err(
            ToolError::new(ErrorKind::Unsupported, format!("{raw} 是目录，不是文件"))
                .with_hint("用 `bash` 执行 `ls` 列目录；read_file 只读单个文件"),
        );
    }
    let path = scope.verify_existing(&path)?;
    let extension = path
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();
    if matches!(extension.as_str(), "doc" | "docx" | "ppt" | "pptx") {
        return Err(
            ToolError::new(ErrorKind::Unsupported, "Office 文档不是纯文本文件").with_hint(
                "请用 `read_document` 提取 DOC/DOCX 或 PPT/PPTX 正文，再用 next_offset 分页续读",
            ),
        );
    }
    if meta.len() > limits::READ_BYTES {
        return Err(ToolError::new(
            ErrorKind::TooLarge,
            format!(
                "{raw} 有 {} 字节，超过 {} 字节上限",
                meta.len(),
                limits::READ_BYTES
            ),
        )
        .with_hint("用 offset/limit 分段读，或用 `bash` 里的 grep/head 先定位"));
    }

    let bytes = std::fs::read(&path).map_err(|e| ToolError::io(format!("读取 {raw} 失败：{e}")))?;
    let text = String::from_utf8(bytes).map_err(|_| {
        ToolError::new(ErrorKind::Unsupported, format!("{raw} 不是 UTF-8 文本")).with_hint(
            "DOC/DOCX、PPT/PPTX 或 GBK/UTF-16 文本请用 `read_document`；图片请用 `view_image`",
        )
    })?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);

    let total: Vec<&str> = text.lines().collect();
    let lines_total = total.len();
    let start = offset.min(lines_total);
    let end = (start + limit).min(lines_total);
    let slice = &total[start..end];
    let content = slice.join("\n");
    let has_more = end < lines_total;

    let mut data = json!({
        "path": scope.display(&path),
        "bytes": meta.len(),
        "lines_total": lines_total,
        "lines_returned": slice.len(),
        "offset": start,
        "truncated": has_more,
        "content": content,
    });
    if has_more {
        data["next_offset"] = json!(end);
    }

    let summary = if has_more {
        format!(
            "读取 {raw} 第 {start}–{} 行（共 {lines_total} 行）",
            end.saturating_sub(1)
        )
    } else if start > 0 {
        format!("读取 {raw} 第 {start}–{lines_total} 行（文件末尾）")
    } else {
        format!("读取 {raw}（{lines_total} 行）")
    };
    Ok(Outcome::ok("read_file", summary, data))
}
