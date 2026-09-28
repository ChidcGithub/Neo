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

    #[test]
    fn waiting_memory_tools_cancel_without_writing() {
        use std::sync::{atomic::AtomicBool, Arc, mpsc};
        use std::time::Duration;
        let _lock = crate::NEO_HOME_TEST_LOCK.lock().unwrap();
        let (_g, dir) = temp_home("cancel");
        add_memory("keep").unwrap();
        let before = std::fs::read(memories_path()).unwrap();
        for (tool, args) in [
            ("remember", serde_json::json!({"content": "cancelled"})),
            ("forget", serde_json::json!({"query": "keep"})),
        ] {
            let held = lock_data_file(&memories_path()).unwrap();
            let token = Arc::new(AtomicBool::new(false));
            let scope = Scope::new(&dir).with_cancel(token.clone());
            std::thread::scope(|s| {
                let (tx, rx) = mpsc::channel();
                s.spawn(move || { tx.send(crate::dispatch(&scope, tool, &args)).unwrap(); });
                assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
                token.store(true, Ordering::Release);
                let out = rx.recv_timeout(Duration::from_secs(5)).unwrap();
                assert_eq!(out.error.unwrap().kind, crate::ErrorKind::NotAllowed);
            });
            drop(held);
            assert_eq!(std::fs::read(memories_path()).unwrap(), before);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn cross_process_storage_writers() {
        const CHILD: &str = "NEO_STORAGE_TEST_CHILD";
        if std::env::var_os(CHILD).is_some() {
            let root = PathBuf::from(std::env::var_os("NEO_HOME").unwrap());
            assert!(root.starts_with(std::env::temp_dir()));
            let path = root.join("counter.json");
            for i in 0..8 {
                let _disk = lock_data_file(&path).unwrap();
                let value: u64 = load_json(&path).unwrap();
                std::thread::sleep(std::time::Duration::from_millis(2));
                save_json(&path, &(value + 1)).unwrap();
                drop(_disk);
                add_memory(&format!("process {} note {i}", std::process::id())).unwrap();
                crate::dailylog::append(&format!("process {} note {i}", std::process::id())).unwrap();
                let out = crate::dispatch(&Scope::new(&root), "edit_file", &serde_json::json!({
                    "path": "edits.txt", "old_string": "count=", "new_string": "count=x"
                }));
                assert!(out.error.is_none(), "{:?}", out.error);
            }
            return;
        }
        let _lock = crate::NEO_HOME_TEST_LOCK.lock().unwrap();
        let (_g, dir) = temp_home("processes");
        std::fs::write(dir.join("edits.txt"), "count=").unwrap();
        let mut children: Vec<_> = (0..3).map(|_| {
            std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "tools::memory::tests::cross_process_storage_writers", "--nocapture"])
                .env(CHILD, "1").env("NEO_HOME", &dir)
                .stdout(std::process::Stdio::null()).spawn().unwrap()
        }).collect();
        for child in &mut children {
            assert!(child.wait().unwrap().success());
        }
        assert_eq!(load_json::<u64>(&dir.join("counter.json")).unwrap(), 24);
        assert_eq!(load_memories().len(), 24);
        assert_eq!(crate::dailylog::load_day(&crate::classlog::today_key()).len(), 24);
        assert_eq!(crate::dailylog::load_index().len(), 1);
        assert_eq!(std::fs::read_to_string(dir.join("edits.txt")).unwrap(), format!("count={}", "x".repeat(24)));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn persistence_failures_preserve_data_and_retries_recover() {
        let _lock = crate::NEO_HOME_TEST_LOCK.lock().unwrap();
        let (_g, dir) = temp_home("failures");
        add_memory("保留数据").unwrap();
        let path = memories_path();
        let before = std::fs::read(&path).unwrap();
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // 不共享 DELETE：模拟其它程序持有目标文件，原子替换必须失败。
            let held = std::fs::OpenOptions::new().read(true).share_mode(1).open(&path).unwrap();
            assert!(add_memory("失败重试").is_err());
            assert_eq!(std::fs::read(&path).unwrap(), before);
            drop(held);
            add_memory("失败重试").unwrap();
            assert_eq!(load_memories().len(), 2);
            // 不共享 READ：不能误判为空集合并覆盖。
            let held = std::fs::OpenOptions::new().read(true).share_mode(0).open(&path).unwrap();
            assert!(add_memory("不可读").is_err());
            drop(held);
            assert_eq!(load_memories().len(), 2);
        }
        #[cfg(not(windows))]
        assert_eq!(std::fs::read(&path).unwrap(), before);
        std::fs::write(&path, b"bad one").unwrap();
        assert!(load_memories().is_empty());
        std::fs::write(&path, [0xff, 0xfe]).unwrap();
        assert!(load_memories().is_empty());
        assert_eq!(std::fs::read(path.with_extension("json.bad")).unwrap(), b"bad one");
        assert_eq!(std::fs::read_dir(&dir).unwrap().filter(|e| e.as_ref().unwrap().path().extension().is_some_and(|e| e == "bad")).count(), 2);
        save_json(&path, &vec![Memory { id: u64::MAX, content: "编号耗尽".into(), updated_ms: 0 }]).unwrap();
        let before = std::fs::read(&path).unwrap();
        assert!(add_memory("不能溢出").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn concurrent_memory_writers_do_not_lose_updates() {
        let _lock = crate::NEO_HOME_TEST_LOCK.lock().unwrap();
        let (_g, dir) = temp_home("concurrent");
        std::thread::scope(|s| {
            for i in 0..12 {
                s.spawn(move || { add_memory(&format!("合成记录 {i}")).unwrap(); });
            }
        });
        let mut ids: Vec<_> = load_memories().iter().map(|m| m.id).collect();
        ids.sort_unstable();
        assert_eq!(ids, (1..=12).collect::<Vec<_>>());
        // 不经过 FILE_LOCK，验证独立文件句柄锁确实串行化读改写。
        let path = dir.join("counter.json");
        std::thread::scope(|s| {
            for _ in 0..8 {
                let path = &path;
                s.spawn(move || {
                    let _guard = lock_data_file(path).unwrap();
                    let value: u64 = load_json(path).unwrap();
                    save_json(path, &(value + 1)).unwrap();
                });
            }
        });
        assert_eq!(load_json::<u64>(&path).unwrap(), 8);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn forget_rejects_empty_normalized_query_without_changing_memories() {
        let _lock = crate::NEO_HOME_TEST_LOCK.lock().unwrap();
        let (_g, dir) = temp_home("empty-forget");
        let (first, _) = add_memory("保留第一条").unwrap();
        add_memory("保留最近一条").unwrap();
        let before = std::fs::read(memories_path()).unwrap();
        for query in ["#", "###", "  #  ", "", "   "] {
            assert_eq!(forget_memory(query).unwrap_err().kind, crate::ErrorKind::BadArguments);
            assert_eq!(std::fs::read(memories_path()).unwrap(), before);
        }
        assert_eq!(forget_memory(&format!("#{}", first.id)).unwrap().unwrap().id, first.id);
        assert_eq!(load_memories().len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// 环境变量是进程级的：两个测试并行会互相串台，合成一个跑。
    /// 跨模块（classlog 也改 NEO_HOME）靠 `NEO_HOME_TEST_LOCK` 串行。
    #[test]
    fn add_dedup_forget_and_import_roundtrip() {
        let _lock = crate::NEO_HOME_TEST_LOCK.lock().unwrap();
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
