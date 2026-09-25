//! 长期记忆：`remember` / `forget` 工具 + 纯 JSON 文件存储。
//!
//! 不落 SQLite、走独立文件的原因：工具执行线程碰不到 app 的 Store
//! （方向相反，拿过来就是循环依赖），而 JSON 文件三方都好碰 ——
//! 工具写、app 设置页读写、用户手动编辑 / 导入导出，都是它。

use std::path::PathBuf;
use std::sync::Mutex;

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

/// 读全部记忆；文件不存在或损坏时返回空（损坏文件改名留档 `.bad`）。
pub fn load_memories() -> Vec<Memory> {
    let _guard = FILE_LOCK.lock().unwrap();
    load_unlocked()
}

fn load_unlocked() -> Vec<Memory> {
    let path = memories_path();
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    match serde_json::from_str::<Vec<Memory>>(&raw) {
        Ok(list) => list,
        Err(e) => {
            eprintln!("[neo] 记忆文件损坏（{e}），已留档为 memories.json.bad");
            let _ = std::fs::rename(&path, path.with_extension("json.bad"));
            Vec::new()
        }
    }
}

/// 写回（先写临时文件再改名，写一半断电不会留半个 JSON）。
fn save_unlocked(list: &[Memory]) -> Result<(), ToolError> {
    let path = memories_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ToolError::io(format!("创建目录 {} 失败：{e}", parent.display())))?;
    }
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_string_pretty(list)
        .map_err(|e| ToolError::io(format!("序列化记忆失败：{e}")))?;
    std::fs::write(&tmp, body).map_err(|e| ToolError::io(format!("写 {} 失败：{e}", tmp.display())))?;
    std::fs::rename(&tmp, &path)
        .map_err(|e| ToolError::io(format!("落盘 {} 失败：{e}", path.display())))?;
    Ok(())
}

/// 新增一条；内容与已有完全相同时不重复，只刷新时间戳。
/// 返回 `(条目, 是否新增)`。
pub fn add_memory(content: &str) -> Result<(Memory, bool), ToolError> {
    let _guard = FILE_LOCK.lock().unwrap();
    let mut list = load_unlocked();
    let now = now_ms();
    if let Some(existing) = list.iter_mut().find(|m| m.content.trim() == content.trim()) {
        existing.updated_ms = now;
        let m = existing.clone();
        save_unlocked(&list)?;
        return Ok((m, false));
    }
    let id = list.iter().map(|m| m.id).max().unwrap_or(0) + 1;
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
    let _guard = FILE_LOCK.lock().unwrap();
    let mut list = load_unlocked();
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
    let _guard = FILE_LOCK.lock().unwrap();
    let mut list = load_unlocked();
    let needle = query.trim().trim_start_matches('#');
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
    let _guard = FILE_LOCK.lock().unwrap();
    let mut list = load_unlocked();
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
        m.id = list.iter().map(|e| e.id).max().unwrap_or(0) + 1;
        m.updated_ms = now;
        list.push(m);
        added += 1;
    }
    save_unlocked(&list)?;
    Ok((added, skipped))
}

/// 导出到指定路径（JSON，格式即存储格式，可直接再导入）。
pub fn export_memories(path: &std::path::Path) -> Result<usize, ToolError> {
    let _guard = FILE_LOCK.lock().unwrap();
    let list = load_unlocked();
    let body = serde_json::to_string_pretty(&list)
        .map_err(|e| ToolError::io(format!("序列化记忆失败：{e}")))?;
    std::fs::write(path, body)
        .map_err(|e| ToolError::io(format!("写 {} 失败：{e}", path.display())))?;
    Ok(list.len())
}

fn now_ms() -> i64 {
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

pub fn remember_run(_scope: &Scope, args: &Args) -> Outcome {
    let content = match args.require_str("content") {
        Ok(c) => c.trim().to_owned(),
        Err(e) => return Outcome::fail("remember", e),
    };
    if content.is_empty() {
        return Outcome::fail("remember", ToolError::bad_args("内容为空"));
    }
    match add_memory(&content) {
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

pub fn forget_run(_scope: &Scope, args: &Args) -> Outcome {
    let query = match args.require_str("query") {
        Ok(q) => q,
        Err(e) => return Outcome::fail("forget", e),
    };
    match forget_memory(&query) {
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
mod tests {
    use super::*;

    /// 隔离：指向临时 NEO_HOME，跑完恢复。
    struct EnvGuard(Option<String>);
    impl EnvGuard {
        fn set(dir: &std::path::Path) -> Self {
            let old = std::env::var("NEO_HOME").ok();
            std::env::set_var("NEO_HOME", dir);
            Self(old)
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.0 {
                Some(v) => std::env::set_var("NEO_HOME", v),
                None => std::env::remove_var("NEO_HOME"),
            }
        }
    }

    fn temp_home(tag: &str) -> (EnvGuard, PathBuf) {
        let dir = std::env::temp_dir().join(format!("neo-mem-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        (EnvGuard::set(&dir), dir)
    }

    /// 环境变量是进程级的：两个测试并行会互相串台，合成一个跑。
    #[test]
    fn add_dedup_forget_and_import_roundtrip() {
        let (_g, dir) = temp_home("roundtrip");
        let (m1, fresh) = add_memory(" 用户教高二物理 ").unwrap();
        assert!(fresh);
        assert_eq!(m1.content, "用户教高二物理");
        // 完全相同内容不重复记
        let (m2, fresh2) = add_memory("用户教高二物理").unwrap();
        assert!(!fresh2);
        assert_eq!(m1.id, m2.id);
        assert_eq!(load_memories().len(), 1);
        // 关键词删
        let removed = forget_memory("高二物理").unwrap().unwrap();
        assert_eq!(removed.id, m1.id);
        assert!(load_memories().is_empty());
        assert!(forget_memory("不存在的东西").unwrap().is_none());

        // 导入合并：同内容跳过、新内容重排 id
        add_memory("甲").unwrap();
        let src = dir.join("in.json");
        std::fs::write(
            &src,
            r#"[{"id":1,"content":"甲","updated_ms":0},{"id":2,"content":"乙","updated_ms":0}]"#,
        )
        .unwrap();
        let (added, skipped) = import_memories(&src).unwrap();
        assert_eq!((added, skipped), (1, 1));
        let list = load_memories();
        assert_eq!(list.len(), 2);
        assert_eq!(list.iter().map(|m| m.id).max(), Some(2));
    }
}
