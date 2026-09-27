//! 每日记忆：agent 自己的「当天速记」，与课堂分记忆（[`crate::classlog`]）同约定存放。
//!
//! 目录：`{NEO_HOME|%APPDATA%\Neo|当前目录}/daily/`：
//! - `YYYY-MM-DD.json`：一天的全部速记（[`DailyNote`]，时间升序）；
//! - `index.json`：「哪天有什么」的一句话索引（[`DailyIndexEntry`]）。
//!
//! **索引先行**是这个模块存在的核心理由：查「某天发生过什么」先读索引
//! （小、恒定快），锁定日期后再翻当天文件 —— 逐日全文扫描会随天数线性变慢。
//! 写速记时索引同锁同步更新，两边不可能脱节。

use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::classlog::today_key;
use crate::result::ToolError;
use crate::tools::memory::{memories_path, now_ms};

/// 一条速记。内容约定一两句话（工具层限制 500 字符），时间由存储层盖章。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DailyNote {
    pub id: u64,
    /// Unix 毫秒（本地时区的「当时」靠它换算）。
    pub ts_ms: i64,
    pub content: String,
}

/// 索引条目：一天一句话。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DailyIndexEntry {
    pub date: String,
    /// 各条速记的开头拼接（「；」分隔，80 字符封顶），只够定位，不够答题。
    pub gist: String,
}

/// 索引最多保留的天数（超出从最早的丢 —— 索引只为定位，历史文件本身不删）。
const INDEX_KEEP_DAYS: usize = 120;
/// 索引总字符上限（日期 + gist 合计）：超出同样从最早的丢。
/// 索引会整体进模型的上下文，必须保持「一眼扫完」的体积。
const INDEX_MAX_CHARS: usize = 4000;

/// 进程内读改写串行化（工具线程可能并发记两条）。
static FILE_LOCK: Mutex<()> = Mutex::new(());

pub fn daily_dir() -> PathBuf {
    memories_path()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
        .join("daily")
}

pub fn day_path(date: &str) -> PathBuf {
    daily_dir().join(format!("{date}.json"))
}

pub fn index_path() -> PathBuf {
    daily_dir().join("index.json")
}

/// 读某天全部速记；文件不存在或损坏返回空（损坏留档 `.bad`，与记忆文件同款）。
pub fn load_day(date: &str) -> Vec<DailyNote> {
    let _guard = FILE_LOCK.lock().unwrap();
    load_notes_unlocked(&day_path(date))
}

/// 读索引（日期升序，最近的在最后）。
pub fn load_index() -> Vec<DailyIndexEntry> {
    let _guard = FILE_LOCK.lock().unwrap();
    let path = index_path();
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    match serde_json::from_str::<Vec<DailyIndexEntry>>(&raw) {
        Ok(list) => list,
        Err(e) => {
            eprintln!("[neo] 每日记忆索引损坏（{e}），已留档为 index.json.bad");
            let _ = std::fs::rename(&path, path.with_extension("json.bad"));
            Vec::new()
        }
    }
}

/// 往今天追加一条速记，并同步更新索引（同一把锁，索引与文件不会脱节）。
pub fn append(content: &str) -> Result<DailyNote, ToolError> {
    let content = content.trim();
    if content.is_empty() {
        return Err(ToolError::bad_args("速记内容不能为空"));
    }
    if content.chars().count() > 500 {
        return Err(ToolError::new(
            crate::ErrorKind::TooLarge,
            "速记超过 500 字符",
        )
        .with_hint("浓缩到一两句再记；长内容不属于每日记忆"));
    }
    let _guard = FILE_LOCK.lock().unwrap();
    let date = today_key();
    let path = day_path(&date);
    let mut notes = load_notes_unlocked(&path);
    let note = DailyNote {
        id: notes.iter().map(|n| n.id).max().unwrap_or(0) + 1,
        ts_ms: now_ms(),
        content: content.to_owned(),
    };
    notes.push(note.clone());
    save_unlocked(&path, &notes)?;
    update_index_unlocked(&date, &notes);
    Ok(note)
}

fn load_notes_unlocked(path: &std::path::Path) -> Vec<DailyNote> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    match serde_json::from_str::<Vec<DailyNote>>(&raw) {
        Ok(list) => list,
        Err(e) => {
            eprintln!("[neo] 每日记忆文件损坏（{}：{e}），已留档 .bad", path.display());
            let _ = std::fs::rename(path, path.with_extension("json.bad"));
            Vec::new()
        }
    }
}

/// 写回（先写临时文件再改名，写一半断电不会留半个 JSON）。
fn save_unlocked(path: &std::path::Path, notes: &[DailyNote]) -> Result<(), ToolError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ToolError::io(format!("创建目录 {} 失败：{e}", parent.display())))?;
    }
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_string_pretty(notes)
        .map_err(|e| ToolError::io(format!("序列化每日记忆失败：{e}")))?;
    std::fs::write(&tmp, body).map_err(|e| ToolError::io(format!("写 {} 失败：{e}", tmp.display())))?;
    std::fs::rename(&tmp, path)
        .map_err(|e| ToolError::io(format!("落盘 {} 失败：{e}", path.display())))?;
    Ok(())
}

/// 索引 gist：各条速记的开头（每条 ≤20 字符）「；」相接，80 字符封顶，
/// 装不下的尾巴换成「…等 N 条」—— 只够定位「这天值不值得翻」，不追求完整。
fn gist_of(notes: &[DailyNote]) -> String {
    let mut gist = String::new();
    for (i, n) in notes.iter().enumerate() {
        let head: String = n.content.chars().take(20).collect();
        let head = head.trim().replace('\n', " ");
        let candidate = if gist.is_empty() {
            head
        } else {
            format!("{gist}；{head}")
        };
        if candidate.chars().count() > 80 {
            gist = format!("{gist}…等 {} 条", notes.len());
            return gist;
        }
        gist = candidate;
        if i + 1 >= 4 && notes.len() > 4 {
            gist = format!("{gist}…等 {} 条", notes.len());
            return gist;
        }
    }
    gist
}

fn update_index_unlocked(date: &str, notes: &[DailyNote]) {
    let mut index = load_index_unlocked();
    let gist = gist_of(notes);
    match index.iter_mut().find(|e| e.date == date) {
        Some(e) => e.gist = gist,
        None => index.push(DailyIndexEntry {
            date: date.to_owned(),
            gist,
        }),
    }
    // 日期串字典序即时间序；超帽（天数 / 总字符）从最早的丢，至少保住最新一条。
    index.sort_by(|a, b| a.date.cmp(&b.date));
    if index.len() > INDEX_KEEP_DAYS {
        index.drain(..index.len() - INDEX_KEEP_DAYS);
    }
    let weight = |e: &DailyIndexEntry| e.date.len() + e.gist.chars().count();
    let mut overflow = index
        .iter()
        .map(|e| weight(e))
        .sum::<usize>()
        .saturating_sub(INDEX_MAX_CHARS);
    let mut drop_n = 0;
    while overflow > 0 && drop_n < index.len().saturating_sub(1) {
        overflow = overflow.saturating_sub(weight(&index[drop_n]));
        drop_n += 1;
    }
    if drop_n > 0 {
        index.drain(..drop_n);
    }
    let path = index_path();
    // 目录可能还没建（比如测试直接喂索引更新）：自己建，不依赖 append 先行。
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = path.with_extension("json.tmp");
    let body = match serde_json::to_string_pretty(&index) {
        Ok(b) => b,
        Err(_) => return, // 序列化不会失败；真失败也不该拖垮速记本身
    };
    if std::fs::write(&tmp, body).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

fn load_index_unlocked() -> Vec<DailyIndexEntry> {
    let path = index_path();
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    serde_json::from_str::<Vec<DailyIndexEntry>>(&raw).unwrap_or_default()
}

/// 当前本地时间「HH:MM」（速记回执里给模型确认用；Windows 取本地时区）。
#[cfg(windows)]
pub fn now_hhmm() -> String {
    use windows_sys::Win32::Foundation::SYSTEMTIME;
    use windows_sys::Win32::System::SystemInformation::GetLocalTime;
    let mut st: SYSTEMTIME = unsafe { std::mem::zeroed() };
    unsafe { GetLocalTime(&mut st) };
    format!("{:02}:{:02}", st.wHour, st.wMinute)
}

/// 非 Windows 只是编译保活，退回 UTC 时间。
#[cfg(not(windows))]
pub fn now_hhmm() -> String {
    let mins = (now_ms() / 60_000).rem_euclid(1440);
    format!("{:02}:{:02}", mins / 60, mins % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 隔离：指向临时 NEO_HOME，跑完恢复（跨模块靠 NEO_HOME_TEST_LOCK 串行）。
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
        let dir = std::env::temp_dir().join(format!("neo-dailylog-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let guard = EnvGuard::set(&dir);
        (guard, dir)
    }

    #[test]
    fn append_and_load_roundtrip_updates_index() {
        let _lock = crate::NEO_HOME_TEST_LOCK.lock().unwrap();
        let (_g, _dir) = temp_home("roundtrip");
        let n1 = append(" 10:32 看到学生打游戏 ").unwrap();
        assert_eq!(n1.content, "10:32 看到学生打游戏");
        let n2 = append("14:05 语文课《岳阳楼记》").unwrap();
        assert_eq!(n2.id, n1.id + 1, "id 应递增");

        let day = load_day(&today_key());
        assert_eq!(day.len(), 2);
        assert_eq!(day[0].content, "10:32 看到学生打游戏");

        let index = load_index();
        assert_eq!(index.len(), 1);
        assert_eq!(index[0].date, today_key());
        assert!(index[0].gist.contains("打游戏"), "索引应含 gist：{}", index[0].gist);
        assert!(index[0].gist.contains("岳阳楼记"), "索引应含两条：{}", index[0].gist);
    }

    #[test]
    fn append_rejects_empty_and_oversize() {
        let _lock = crate::NEO_HOME_TEST_LOCK.lock().unwrap();
        let (_g, _dir) = temp_home("reject");
        assert!(append("   ").is_err());
        let long = "长".repeat(501);
        let err = append(&long).unwrap_err();
        assert_eq!(err.kind, crate::ErrorKind::TooLarge);
    }

    #[test]
    fn corrupt_day_file_is_quarantined() {
        let _lock = crate::NEO_HOME_TEST_LOCK.lock().unwrap();
        let (_g, _dir) = temp_home("corrupt");
        let path = day_path(&today_key());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{ not json").unwrap();
        assert!(load_day(&today_key()).is_empty());
        assert!(path.with_extension("json.bad").exists());
    }

    #[test]
    fn index_char_cap_drops_oldest_but_keeps_newest() {
        let _lock = crate::NEO_HOME_TEST_LOCK.lock().unwrap();
        let (_g, _dir) = temp_home("cap");
        // 60 天、每天 4 条顶格 gist（~80 字符）：总重远超 4000 上限。
        for day in 1..=60 {
            let date = format!("2026-01-{day:02}");
            let notes: Vec<DailyNote> = (1..=4)
                .map(|i| DailyNote {
                    id: i,
                    ts_ms: 0,
                    content: format!("第 {i} 条很长的记录内容，撑满单条 20 字符的上限"),
                })
                .collect();
            update_index_unlocked(&date, &notes);
        }
        let index = load_index();
        let total: usize = index
            .iter()
            .map(|e| e.date.len() + e.gist.chars().count())
            .sum();
        assert!(total <= INDEX_MAX_CHARS, "索引超重: {total}");
        assert_eq!(index.last().unwrap().date, "2026-01-60", "最新一条必须保住");
        assert!(index.len() < 60, "最早的应被丢弃，剩 {} 条", index.len());
        // 窗口必须连续（从某天起到最后一天）。
        let first: usize = index.first().unwrap().date[8..].parse().unwrap();
        assert_eq!(first + index.len() - 1, 60, "丢弃后日期应连续");
    }
}
