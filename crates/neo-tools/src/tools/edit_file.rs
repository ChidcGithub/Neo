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
    std::fs::metadata(&path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => ToolError::not_found(format!("文件不存在：{raw}"))
            .with_hint("本工具只改已存在的文件；新建请用 `write_file`"),
        _ => ToolError::io(format!("无法访问 {raw}：{e}")),
    })?;
    let path = scope.verify_existing(&path)?;
    let _disk = super::memory::lock_data_file_scoped(&path, Some(scope))?;
    // 等锁期间路径可能已变化；不能拿旧路径的锁去修改新的链接目标。
    if scope.verify_existing(&path)? != path {
        return Err(ToolError::new(ErrorKind::Conflict, "等待期间文件路径已变化，请重新读取"));
    }
    let meta = std::fs::metadata(&path).map_err(|e| ToolError::io(format!("无法访问 {raw}：{e}")))?;
    if meta.len() > limits::READ_BYTES {
        return Err(ToolError::new(
            ErrorKind::TooLarge,
            format!("{raw} 有 {} 字节，超过可编辑上限", meta.len()),
        )
        .with_hint("先用 `read_file` 分段确认要改的片段，大文件改用 `bash` 处理"));
    }

    let bytes = super::read_file::read_bounded(&path, limits::READ_BYTES)?;
    let text = String::from_utf8(bytes).map_err(|_| {
        ToolError::new(ErrorKind::Unsupported, format!("{raw} 不是 UTF-8 文本"))
            .with_hint("二进制或非 UTF-8 文件请用 `bash` 处理")
    })?;

    let hits = text.matches(&old).count();
    if hits == 0 {
        let head: String = old.chars().take(60).collect();
        return Err(ToolError::not_found(format!(
            "在 {raw} 里找不到 `old_string`（首行：{head}）"
        ))
        .with_hint(
            "先用 `read_file` 读一遍原文，确认缩进、全半角与换行都一致；不要凭记忆写 old_string",
        ));
    }
    if hits > 1 && !replace_all {
        // 把命中所在的**行号**回给模型 —— 它据此加长上下文即可唯一定位。
        let lines: Vec<usize> = text.match_indices(&old)
            .map(|(i, _)| text[..i].bytes().filter(|b| *b == b'\n').count() + 1)
            .take(20)
            .collect();
        return Err(ToolError::new(
            ErrorKind::NotUnique,
            format!(
                "`old_string` 在 {raw} 里命中 {} 处（第 {:?} 行），无法确定改哪一处",
                hits,
                lines
            ),
        )
        .with_hint("多带几行上下文让旧文本唯一；确定要全部替换时把 `replace_all` 设为 true"));
    }

    let replacements = if replace_all { hits } else { 1 };
    checked_replacement_len(text.len(), old.len(), new.len(), replacements, limits::WRITE_BYTES)?;
    let updated = if replace_all {
        text.replace(&old, &new)
    } else {
        text.replacen(&old, &new, 1)
    };
    if scope.is_cancelled() {
        return Err(crate::cancelled_error());
    }
    if scope.verify_existing(&path)? != path {
        return Err(ToolError::new(ErrorKind::Conflict, "编辑期间文件路径已变化，请重新读取"));
    }
    super::memory::atomic_write(&path, updated.as_bytes())?;

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

fn checked_replacement_len(
    text: usize,
    old: usize,
    new: usize,
    count: usize,
    limit: u64,
) -> Result<usize, ToolError> {
    let len = old.checked_mul(count)
        .and_then(|removed| text.checked_sub(removed))
        .and_then(|remaining| new.checked_mul(count).and_then(|added| remaining.checked_add(added)));
    match len {
        Some(len) if len as u64 <= limit => Ok(len),
        _ => Err(ToolError::new(ErrorKind::TooLarge, "替换后的文件超过可写入上限")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_wait_for_other_writers_and_cancellation_prevents_commit() {
        use std::sync::{atomic::{AtomicBool, Ordering}, Arc, mpsc};
        use std::time::Duration;
        let root = std::env::temp_dir().join(format!("neo-edit-lock-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("fixture.txt");
        std::fs::write(&path, "first second").unwrap();
        let token = Arc::new(AtomicBool::new(false));
        let scope = Scope::new(&root).with_cancel(token.clone());
        for cancel in [false, true] {
            let held = super::super::memory::lock_data_file(&path).unwrap();
            std::thread::scope(|s| {
                let (tx, rx) = mpsc::channel();
                let scope = &scope;
                s.spawn(move || {
                    tx.send(crate::dispatch(scope, "edit_file", &json!({
                        "path": "fixture.txt", "old_string": "first", "new_string": "edited"
                    }))).unwrap();
                });
                assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
                if cancel {
                    token.store(true, Ordering::Release);
                    let out = rx.recv_timeout(Duration::from_secs(5)).unwrap();
                    assert_eq!(out.error.unwrap().kind, ErrorKind::NotAllowed);
                    drop(held);
                } else {
                    std::fs::write(&path, "first other-update").unwrap();
                    drop(held);
                    assert!(rx.recv_timeout(Duration::from_secs(5)).unwrap().error.is_none());
                }
            });
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "edited other-update");
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_target_stays_not_found_without_creating_lock() {
        let root = std::env::temp_dir().join(format!("neo-edit-missing-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let out = crate::dispatch(&Scope::new(&root), "edit_file", &json!({
            "path": "missing.txt", "old_string": "old", "new_string": "new"
        }));
        assert_eq!(out.error.unwrap().kind, ErrorKind::NotFound);
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn expanded_edit_rejects_without_changing_file() {
        let root = std::env::temp_dir().join(format!("neo-edit-budget-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("fixture.txt");
        std::fs::write(&path, "aa").unwrap();
        let result = crate::dispatch(&Scope::new(&root), "edit_file", &json!({
            "path": "fixture.txt", "old_string": "a",
            "new_string": "x".repeat(limits::WRITE_BYTES as usize), "replace_all": true,
        }));
        let actual = std::fs::read(&path).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(result.error.unwrap().kind, ErrorKind::TooLarge);
        assert_eq!(actual, b"aa");
    }

    #[test]
    fn replacement_size_is_checked_before_allocation() {
        assert_eq!(checked_replacement_len(4, 1, 3, 4, 12).unwrap(), 12);
        assert_eq!(checked_replacement_len(4, 1, 3, 4, 11).unwrap_err().kind, ErrorKind::TooLarge);
        assert_eq!(checked_replacement_len(12, 3, 0, 4, 1).unwrap(), 0);
        assert_eq!(checked_replacement_len(10, 1, 2, 1, 10).unwrap_err().kind, ErrorKind::TooLarge);
        assert_eq!(checked_replacement_len(2, 1, usize::MAX, 2, u64::MAX).unwrap_err().kind, ErrorKind::TooLarge);
        assert_eq!(checked_replacement_len(usize::MAX, 1, 2, 1, u64::MAX).unwrap_err().kind, ErrorKind::TooLarge);
        assert_eq!(checked_replacement_len(6, "汉".len(), "文文".len(), 2, 12).unwrap(), 12);
    }
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
