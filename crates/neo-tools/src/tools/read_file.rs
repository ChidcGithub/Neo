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
        "最多返回多少行。默认 400，上限 2000；受完整 JSON 预算约束可能更少。content 保留内部原始换行，省略末行终止符。",
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

pub(crate) fn read_bounded(path: &std::path::Path, limit: u64) -> Result<Vec<u8>, ToolError> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|e| ToolError::io(e.to_string()).with_source(&e))?;
    let meta = file.metadata().map_err(|e| ToolError::io(e.to_string()).with_source(&e))?;
    if !meta.is_file() {
        return Err(ToolError::new(ErrorKind::Unsupported, "只支持普通文件"));
    }
    if meta.len() > limit {
        return Err(ToolError::new(ErrorKind::TooLarge, "文件超过读取上限"));
    }
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1)).read_to_end(&mut bytes)
        .map_err(|e| ToolError::io(e.to_string()).with_source(&e))?;
    if bytes.len() as u64 > limit {
        return Err(ToolError::new(ErrorKind::TooLarge, "文件读取期间超过上限"));
    }
    Ok(bytes)
}

fn read(scope: &Scope, args: &Args) -> Result<Outcome, ToolError> {
    let raw = args.require_str("path")?;
    let offset = args.opt_int("offset")? as usize;
    let limit = args.opt_int("limit")? as usize;

    let path = scope.resolve(&raw)?;
    let meta = std::fs::metadata(&path).map_err(|e| {
        let error = match e.kind() {
            std::io::ErrorKind::NotFound => ToolError::not_found(format!("文件不存在：{raw}"))
                .with_hint("先用 `bash` 跑 `ls` 看看目录里有什么；路径不要带工作区前缀"),
            _ => ToolError::io(format!("无法访问 {raw}：{e}")),
        };
        error.with_source(&e)
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

    let bytes = read_bounded(&path, limits::READ_BYTES)?;
    let text = String::from_utf8(bytes).map_err(|_| {
        ToolError::new(ErrorKind::Unsupported, format!("{raw} 不是 UTF-8 文本")).with_hint(
            "DOC/DOCX、PPT/PPTX 或 GBK/UTF-16 文本请用 `read_document`；图片请用 `view_image`",
        )
    })?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);

    let lines_total = text.lines().count();
    let start = offset.min(lines_total);
    let mut lines = text.split_inclusive('\n');
    let start_byte: usize = lines.by_ref().take(start).map(str::len).sum();
    let mut ends = vec![start_byte];
    for line in lines.take(limit) {
        ends.push(ends.last().unwrap() + line.len());
    }
    let make_page = |count: usize| {
        let end = start + count;
        let slice = &text[start_byte..ends[count]];
        // 只省略页面末行的终止符；内部 CRLF/LF 和孤立 CR 必须逐字节保留。
        let content = match slice.strip_suffix('\n') {
            Some(line) => line.strip_suffix('\r').unwrap_or(line),
            None => slice,
        };
        let has_more = end < lines_total;
        let mut data = json!({
            "path": scope.display(&path),
            "bytes": meta.len(),
            "lines_total": lines_total,
            "lines_returned": count,
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
        Outcome::ok("read_file", summary, data)
    };
    let fits = |out: &Outcome| {
        let json = out.to_model_json(usize::MAX);
        json.chars().count() <= limits::MODEL_JSON_CHARS && json.len() <= 32 * 1024
    };
    let requested = make_page(ends.len() - 1);
    if fits(&requested) {
        return Ok(requested);
    }
    // 按最终完整 Outcome 预算选择完整行前缀，保留 next_offset，不能用通用 head 截断。
    let (mut low, mut high) = (0, ends.len() - 1);
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        if fits(&make_page(middle)) {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    if low == 0 {
        return Err(ToolError::new(
            ErrorKind::TooLarge,
            format!("第 {start} 行或页面元数据超过完整 JSON 输出预算，无法返回完整行"),
        )
        .with_hint("减小 limit 无法拆分单行；请先将长行拆分为短行，或用 read_document 按字符分页读取支持的文本文件（不能据其规范化文本直接精确替换）"));
    }
    Ok(make_page(low))
}

#[cfg(test)]
#[path = "read_file_tests.rs"]
mod tests;
