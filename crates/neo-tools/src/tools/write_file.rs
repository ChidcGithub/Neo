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
    // symlink_metadata 不跟随链接：词法路径本身是链接时，无论目标在不在，
    // 都要走真实路径化解（dangling 链接 exists() 为 false，但写入会顺链接
    // 落到链接目标 —— 目标是工作区外的文件就逃逸了）。
    let is_link = std::fs::symlink_metadata(&path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false);
    let existed = path.exists();
    let path = if existed || is_link {
        // 先复查真实路径；后续使用化解后的路径，提交前仍需再次检查。
        let real = scope.verify_existing(&path)?;
        if real.is_dir() {
            return Err(
                ToolError::new(ErrorKind::Unsupported, format!("{raw} 是目录"))
                    .with_hint("给一个文件名，例如 `src/main.rs`"),
            );
        }
        if existed && !overwrite {
            let bytes = std::fs::metadata(&real).map(|m| m.len()).unwrap_or(0);
            return Err(ToolError::new(
                ErrorKind::Conflict,
                format!("{raw} 已存在（{bytes} 字节），未允许覆盖"),
            )
            .with_hint("先 `read_file` 看原内容；确认要整体替换再把 `overwrite` 设为 true，或改用 `edit_file` 只改一段"));
        }
        real
    } else {
        // 新建：目标不存在，但祖先目录可能是指向工作区外的链接。
        scope.verify_new(&path)?;
        if scope.is_cancelled() {
            return Err(crate::cancelled_error());
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ToolError::io(format!("无法创建目录 {}：{e}", scope.display(parent))).with_source(&e))?;
            // 锁与编辑工具使用同一个真实路径，不能保留祖先目录的链接别名。
            scope.verify_existing(parent)?.join(path.file_name().unwrap())
        } else {
            path
        }
    };

    let _disk = super::memory::lock_data_file_scoped(&path, Some(scope))?;
    if path.exists() || std::fs::symlink_metadata(&path).is_ok() {
        if scope.verify_existing(&path)? != path {
            return Err(ToolError::new(ErrorKind::Conflict, "等待期间文件路径已变化，请重新读取"));
        }
    } else {
        scope.verify_new(&path)?;
        if let Some(parent) = path.parent() {
            if scope.verify_existing(parent)? != parent {
                return Err(ToolError::new(ErrorKind::Conflict, "等待期间父目录已变化，请重试"));
            }
        }
    }
    if scope.is_cancelled() {
        return Err(crate::cancelled_error());
    }
    let bytes = content.len() as u64;
    let lines = content.lines().count();
    if overwrite {
        super::memory::atomic_write(&path, content.as_bytes())?;
    } else {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&path)
            .map_err(|e| {
                let error = if e.kind() == std::io::ErrorKind::AlreadyExists {
                    ToolError::new(ErrorKind::Conflict, "目标已存在，未允许覆盖")
                } else { ToolError::io(format!("写入 {raw} 失败：{e}")) };
                error.with_source(&e)
            })?;
        let result = file.write_all(content.as_bytes()).and_then(|_| file.sync_all());
        drop(file);
        if let Err(e) = result {
            let _ = std::fs::remove_file(&path);
            return Err(ToolError::io(format!("写入 {raw} 失败：{e}")).with_source(&e));
        }
    }

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

#[cfg(test)]
#[path = "write_file_tests.rs"]
mod tests;
