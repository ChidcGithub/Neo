//! 长期记忆：`remember` / `forget` 工具 + 纯 JSON 文件存储。
//!
//! 不落 SQLite、走独立文件的原因：工具执行线程碰不到 app 的 Store
//! （方向相反，拿过来就是循环依赖），而 JSON 文件三方都好碰 ——
//! 工具写、app 设置页读写、用户手动编辑 / 导入导出，都是它。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{atomic::{AtomicU64, Ordering}, Mutex};

use serde::{Deserialize, Serialize};

use crate::result::{Args, Outcome, ToolError};
use crate::spec::Param;
use crate::Scope;

/// 一条长期记忆。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Memory {
    pub id: u64,
    pub content: String,
    /// Unix 毫秒；新增与内容被修改都会刷新它。
    pub updated_ms: i64,
}

/// 记忆文件路径：与 neo-store 的 `neo.db` 同一目录约定
/// （`NEO_HOME` > `%APPDATA%\Neo` > 当前目录），文件名 `memories.json`。
pub fn memories_path() -> PathBuf {
    if let Ok(home) = std::env::var("NEO_HOME") {
        if !home.trim().is_empty() {
            return PathBuf::from(home).join("memories.json");
        }
    }
    if let Ok(appdata) = std::env::var("APPDATA") {
        if !appdata.trim().is_empty() {
            return PathBuf::from(appdata).join("Neo").join("memories.json");
        }
    }
    PathBuf::from("memories.json")
}

/// 进程内读改写串行化（工具线程与 app UI 可能同时动手）。
static FILE_LOCK: Mutex<()> = Mutex::new(());

/// 读全部记忆；保留 Vec 接口，读取失败不允许后续写操作覆盖旧数据。
pub fn load_memories() -> Vec<Memory> {
    let _guard = FILE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let result = (|| {
        let _disk = lock_data_file(&memories_path())?;
        load_unlocked()
    })();
    result.unwrap_or_default()
}

fn load_unlocked() -> Result<Vec<Memory>, ToolError> {
    load_json(&memories_path())
}

static TEMP_ID: AtomicU64 = AtomicU64::new(0);

/// 与同机其它 Neo 进程串行读改写，文件句柄关闭时自动解锁。
pub(crate) fn lock_data_file(path: &Path) -> Result<std::fs::File, ToolError> {
    lock_data_file_scoped(path, None)
}

pub(crate) fn lock_data_file_scoped(path: &Path, scope: Option<&Scope>) -> Result<std::fs::File, ToolError> {
    if scope.is_some_and(Scope::is_cancelled) {
        return Err(crate::cancelled_error());
    }
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| ToolError::io(e.to_string()))?;
    }
    let mut name = path.as_os_str().to_os_string();
    name.push(".lock");
    let file = std::fs::OpenOptions::new().read(true).write(true).create(true)
        .truncate(false).open(PathBuf::from(name))
        .map_err(|e| ToolError::io(format!("打开数据锁失败：{e}")))?;
    if let Some(scope) = scope {
        loop {
            if scope.is_cancelled() {
                return Err(crate::cancelled_error());
            }
            match file.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(std::fs::TryLockError::Error(e)) => {
                    return Err(ToolError::io(format!("锁定数据失败：{e}")));
                }
            }
        }
        if scope.is_cancelled() {
            return Err(crate::cancelled_error());
        }
    } else {
        file.lock().map_err(|e| ToolError::io(format!("锁定数据失败：{e}")))?;
    }
    Ok(file)
}

/// 只有 NotFound 表示空数据；损坏内容必须成功留档后才能重建。
pub(crate) fn load_json<T: serde::de::DeserializeOwned + Default>(path: &Path) -> Result<T, ToolError> {
    let raw = match std::fs::read(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(T::default()),
        Err(e) => return Err(ToolError::io(format!("读取 {} 失败：{e}", path.display()))),
    };
    match serde_json::from_slice(&raw) {
        Ok(value) => Ok(value),
        Err(_) => {
            let mut backup = path.with_extension("json.bad");
            loop {
                match std::fs::OpenOptions::new().write(true).create_new(true).open(&backup) {
                    Ok(mut file) => {
                        let saved = file.write_all(&raw).and_then(|_| file.sync_all());
                        drop(file);
                        if let Err(e) = saved {
                            let _ = std::fs::remove_file(&backup);
                            return Err(ToolError::io(format!("损坏数据留档失败：{e}")));
                        }
                        std::fs::remove_file(path).map_err(|e| ToolError::io(format!("隔离损坏数据失败：{e}")))?;
                        return Ok(T::default());
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                        backup = path.with_extension(format!("json.{}-{}.bad", std::process::id(), TEMP_ID.fetch_add(1, Ordering::Relaxed)));
                    }
                    Err(e) => return Err(ToolError::io(format!("损坏数据留档失败：{e}"))),
                }
            }
        }
    }
}

/// 同目录暂存、同步后替换；任何失败都不先删除或截断原文件。
pub(crate) fn atomic_write(path: &Path, body: &[u8]) -> Result<(), ToolError> {
    if std::fs::metadata(path).is_ok_and(|m| m.permissions().readonly()) {
        return Err(ToolError::io("目标文件为只读，未修改原文件"));
    }
    let (tmp, mut file) = loop {
        let tmp = path.with_file_name(format!(".neo-{}-{}.tmp", std::process::id(), TEMP_ID.fetch_add(1, Ordering::Relaxed)));
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp) {
            Ok(file) => break (tmp, file),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(ToolError::io(format!("创建暂存文件失败：{e}"))),
        }
    };
    let result = file.write_all(body).and_then(|_| {
        match std::fs::metadata(path) {
            Ok(meta) => file.set_permissions(meta.permissions())?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
            Err(e) => return Err(e),
        }
        file.sync_all()
    });
    drop(file);
    let result = result.and_then(|_| std::fs::rename(&tmp, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result.map_err(|e| ToolError::io(format!("落盘 {} 失败：{e}", path.display())))
}

pub(crate) fn save_json<T: Serialize + ?Sized>(path: &Path, value: &T) -> Result<(), ToolError> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| ToolError::io(e.to_string()))?;
    }
    let body = serde_json::to_vec_pretty(value).map_err(|e| ToolError::io(e.to_string()))?;
    atomic_write(path, &body)
}

fn save_unlocked(list: &[Memory]) -> Result<(), ToolError> {
    save_json(&memories_path(), list)
}

pub(crate) fn next_id(ids: impl Iterator<Item = u64>) -> Result<u64, ToolError> {
    ids.max().unwrap_or(0).checked_add(1)
        .ok_or_else(|| ToolError::io("数据编号已耗尽，未修改原数据"))
}

/// 新增一条；内容与已有完全相同时不重复，只刷新时间戳。
/// 返回 `(条目, 是否新增)`。
pub fn add_memory(content: &str) -> Result<(Memory, bool), ToolError> {
    add_memory_scoped(content, None)
}

fn add_memory_scoped(content: &str, scope: Option<&Scope>) -> Result<(Memory, bool), ToolError> {
    let _guard = FILE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _disk = lock_data_file_scoped(&memories_path(), scope)?;
    let mut list = load_unlocked()?;
    if scope.is_some_and(Scope::is_cancelled) {
        return Err(crate::cancelled_error());
    }
    let now = now_ms();
    if let Some(existing) = list.iter_mut().find(|m| m.content.trim() == content.trim()) {
        existing.updated_ms = now;
        let m = existing.clone();
        save_unlocked(&list)?;
        return Ok((m, false));
    }
    let id = next_id(list.iter().map(|m| m.id))?;
    let memory = Memory {
        id,
        content: content.trim().to_owned(),
        updated_ms: now,
    };
    list.push(memory.clone());
    save_unlocked(&list)?;
    Ok((memory, true))
}

/// 按 id 或内容片段更新内容；找不到返回 `None`。
pub fn edit_memory(id: u64, content: &str) -> Result<Option<Memory>, ToolError> {
    let _guard = FILE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _disk = lock_data_file(&memories_path())?;
    let mut list = load_unlocked()?;
    let Some(existing) = list.iter_mut().find(|m| m.id == id) else {
        return Ok(None);
    };
    existing.content = content.trim().to_owned();
    existing.updated_ms = now_ms();
    let m = existing.clone();
    save_unlocked(&list)?;
    Ok(Some(m))
}

/// 删除：`query` 是 `#3` / `3` 按 id 精确删；否则按内容包含匹配，
/// 多条命中时删最近更新的那条（模型通常想删的是刚记错的那条）。
pub fn forget_memory(query: &str) -> Result<Option<Memory>, ToolError> {
    forget_memory_scoped(query, None)
}

fn forget_memory_scoped(query: &str, scope: Option<&Scope>) -> Result<Option<Memory>, ToolError> {
    let needle = query.trim().trim_start_matches('#').trim();
    if needle.is_empty() {
        return Err(ToolError::bad_args("要删除的记忆不能为空")
            .with_hint("请给出 #id（如 #3）或非空的内容关键词"));
    }
    let _guard = FILE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _disk = lock_data_file_scoped(&memories_path(), scope)?;
    let mut list = load_unlocked()?;
    if scope.is_some_and(Scope::is_cancelled) {
        return Err(crate::cancelled_error());
    }
    let idx = if let Ok(id) = needle.parse::<u64>() {
        list.iter().position(|m| m.id == id)
    } else {
        let lower = needle.to_lowercase();
        list.iter()
            .enumerate()
            .filter(|(_, m)| m.content.to_lowercase().contains(&lower))
            .max_by_key(|(_, m)| m.updated_ms)
            .map(|(i, _)| i)
    };
    let Some(idx) = idx else {
        return Ok(None);
    };
    let removed = list.remove(idx);
    save_unlocked(&list)?;
    Ok(Some(removed))
}

/// 从 JSON 文件导入合并（按内容去重），返回 `(新增数, 跳过数)`。
pub fn import_memories(path: &std::path::Path) -> Result<(usize, usize), ToolError> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| ToolError::io(format!("读 {} 失败：{e}", path.display())))?;
    let incoming: Vec<Memory> = serde_json::from_str(&raw).map_err(|e| {
        ToolError::bad_args(format!("{} 不是有效的记忆文件：{e}", path.display()))
            .with_hint("导入用本应用导出的 JSON（一个 memories 数组）")
    })?;
    let _guard = FILE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _disk = lock_data_file(&memories_path())?;
    let mut list = load_unlocked()?;
    let mut added = 0;
    let mut skipped = 0;
    let now = now_ms();
    for mut m in incoming {
        m.content = m.content.trim().to_owned();
        if m.content.is_empty() {
            skipped += 1;
            continue;
        }
        if list.iter().any(|e| e.content == m.content) {
            skipped += 1;
            continue;
        }
        m.id = next_id(list.iter().map(|e| e.id))?;
        m.updated_ms = now;
        list.push(m);
        added += 1;
    }
    save_unlocked(&list)?;
    Ok((added, skipped))
}

/// 导出到指定路径（JSON，格式即存储格式，可直接再导入）。
pub fn export_memories(path: &std::path::Path) -> Result<usize, ToolError> {
    let _guard = FILE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _disk = lock_data_file(&memories_path())?;
    let list = load_unlocked()?;
    save_json(path, &list)?;
    Ok(list.len())
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---------------- 工具入口 ----------------

pub static REMEMBER_PARAMS: &[Param] = &[Param::text(
    "content",
    "要记住的内容：一句完整、独立、长期有效的话（用户偏好 / 身份信息 / 常用设定），\
     不要记一次性的任务指令。",
)];

pub static FORGET_PARAMS: &[Param] = &[Param::text(
    "query",
    "要删除的记忆：#id（如 #3）或内容里的关键词。",
)];

pub fn remember_preview(args: &Args) -> String {
    format!("记住：{}", args.opt_str("content").unwrap_or_default())
}

pub fn forget_preview(args: &Args) -> String {
    format!("忘掉：{}", args.opt_str("query").unwrap_or_default())
}

pub fn remember_run(scope: &Scope, args: &Args) -> Outcome {
    let content = match args.require_str("content") {
        Ok(c) => c.trim().to_owned(),
        Err(e) => return Outcome::fail("remember", e),
    };
    if content.is_empty() {
        return Outcome::fail("remember", ToolError::bad_args("内容为空"));
    }
    match add_memory_scoped(&content, Some(scope)) {
        Ok((m, true)) => Outcome::ok(
            "remember",
            format!("已记住（#{}）：{}", m.id, m.content),
            serde_json::json!({"id": m.id, "created": true}),
        ),
        Ok((m, false)) => Outcome::ok(
            "remember",
            format!("这条已经记过（#{}），无需重复", m.id),
            serde_json::json!({"id": m.id, "created": false}),
        ),
        Err(e) => Outcome::fail("remember", e),
    }
}

pub fn forget_run(scope: &Scope, args: &Args) -> Outcome {
    let query = match args.require_str("query") {
        Ok(q) => q,
        Err(e) => return Outcome::fail("forget", e),
    };
    match forget_memory_scoped(&query, Some(scope)) {
        Ok(Some(m)) => Outcome::ok(
            "forget",
            format!("已忘掉（#{}）：{}", m.id, m.content),
            serde_json::json!({"id": m.id, "removed": true}),
        ),
        Ok(None) => Outcome::fail(
            "forget",
            ToolError::not_found(format!("没有匹配「{query}」的记忆"))
                .with_hint("先用记忆列表里的 #id 精确删除；当前可见的记忆在系统提示词里"),
        ),
        Err(e) => Outcome::fail("forget", e),
    }
}

#[cfg(test)]
#[path = "memory_tests.rs"]
mod tests;
