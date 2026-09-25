//! `read_document` —— 共享附件解析器，按 Unicode 字符分页读取文档。

use serde_json::json;

use crate::{documents, Args, ErrorKind, Outcome, Param, Scope, ToolError};

use super::limits;

pub static PARAMS: &[Param] = &[
    Param::text(
        "path",
        "工作区内的 DOC/DOCX、PPT/PPTX 或 TXT/MD/CSV/JSON/LOG/TSV 文件路径。图片请用 view_image。",
    ),
    Param::opt_int(
        "offset",
        "已提取文本的起始字符位置（从 0 开始，不是字节或行号）。继续读取请用返回的 next_offset。",
        0,
        0,
        80_000,
    ),
    Param::opt_int(
        "limit",
        "最多返回多少个 Unicode 字符，默认 4000，最大 8000；受 JSON 预算约束可能少于此值。",
        4000,
        1,
        8000,
    ),
];

pub fn preview(args: &Args) -> String {
    let path = args.opt_str("path").unwrap_or_default();
    let offset = args.opt_int("offset").unwrap_or(0);
    let limit = args.opt_int("limit").unwrap_or(4000);
    format!("读取文档 {path}（从第 {offset} 个字符起，最多 {limit} 字符）")
}

pub fn run(scope: &Scope, args: &Args) -> Outcome {
    match read(scope, args) {
        Ok(out) => out,
        Err(mut error) => {
            // Paths and parser diagnostics may contain model-controlled text.
            // Bound failures too, so generic JSON truncation never swallows kind/hint.
            error.message = bounded_diagnostic(&error.message);
            error.hint = error.hint.map(|hint| bounded_diagnostic(&hint));
            Outcome::fail("read_document", error)
        }
    }
}

fn bounded_diagnostic(text: &str) -> String {
    let mut chars = text.chars();
    let mut result: String = chars.by_ref().take(1000).collect();
    if chars.next().is_some() {
        result.push_str(" [诊断过长，已截短]");
    }
    result
}

fn read(scope: &Scope, args: &Args) -> Result<Outcome, ToolError> {
    args.reject_unknown()?;
    let raw = args.require_str("path")?;
    let offset = args.opt_int("offset")? as usize;
    let limit = args.opt_int("limit")? as usize;
    let path = scope.resolve(&raw)?;
    let meta = std::fs::metadata(&path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => ToolError::not_found(format!("文件不存在：{raw}"))
            .with_hint("先确认工作区内的文件路径，再调用 read_document"),
        _ => ToolError::io(format!("无法访问 {raw}：{error}")),
    })?;
    let path = scope.verify_existing(&path)?;
    if !meta.is_file() {
        return Err(ToolError::new(
            ErrorKind::Unsupported,
            "read_document 只读取普通文件",
        ));
    }
    if meta.len() > documents::MAX_FILE_BYTES {
        return Err(ToolError::new(ErrorKind::TooLarge, "文档超过 32 MiB 上限")
            .with_hint("请先拆分文件；offset/limit 仅限制返回文字，不能绕过文件体积上限"));
    }
    let extension = path
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();
    if matches!(
        extension.as_str(),
        "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp"
    ) {
        return Err(
            ToolError::new(ErrorKind::Unsupported, "图片不通过 read_document 读取")
                .with_hint("请调用 view_image，并设置 include_data=true 让模型查看图片"),
        );
    }
    let document = documents::load(&path).map_err(|message| {
        let kind = if message.starts_with("无法打开附件：")
            || message.starts_with("无法读取附件信息：")
            || message.starts_with("读取附件失败：")
        {
            ErrorKind::Io
        } else if message.contains("上限") || message.contains("过大") {
            ErrorKind::TooLarge
        } else {
            ErrorKind::Unsupported
        };
        ToolError::new(kind, message)
            .with_hint("支持未加密 DOC/DOCX、PPT/PPTX 及文本；扫描件请转成图片后调用 view_image")
    })?;
    let chars: Vec<char> = document.text.chars().collect();
    let total = chars.len();
    let start = offset.min(total);
    let requested_end = (start + limit).min(total);
    let make_page = |end: usize| {
        let has_more = end < total;
        Outcome::ok(
            "read_document",
            format!(
                "读取 {}（字符 {start}–{end} / 已提取 {total} 字符）",
                document.name
            ),
            json!({
                "content": chars[start..end].iter().collect::<String>(),
                "name": document.name,
                "type": document.kind,
                "total_chars": total,
                "offset": start,
                "next_offset": if has_more { Some(end) } else { None },
                "has_more": has_more,
                "warning": document.warning,
            }),
        )
    };
    let fits = |out: &Outcome| {
        let json = out.to_model_json(usize::MAX);
        json.chars().count() <= limits::MODEL_JSON_CHARS && json.len() <= 32 * 1024
    };
    let requested = make_page(requested_end);
    if fits(&requested) {
        return Ok(requested);
    }
    // 按完整 JSON 的字符及字节预算缩页，保留结构化分页字段，不能依赖通用截断。
    let (mut low, mut high) = (start, requested_end);
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        if fits(&make_page(middle)) {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    Ok(make_page(low))
}
