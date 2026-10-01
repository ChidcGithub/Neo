//! 每日记忆：agent 自己的「当天速记」，与课堂分记忆（[`crate::classlog`]）同约定存放。
//!
//! 目录：`{NEO_HOME|%APPDATA%\Neo|当前目录}/daily/`：
//! - `YYYY-MM-DD.json`：一天的全部速记（[`DailyNote`]，时间升序）；
//! - `index.json`：「哪天有什么」的一句话索引（[`DailyIndexEntry`]）。
//!
//! **索引先行**是这个模块存在的核心理由：查「某天发生过什么」先读索引
//! （小、恒定快），锁定日期后再翻当天文件 —— 逐日全文扫描会随天数线性变慢。
//! 写速记时索引同锁更新；索引失败留下恢复标记，下次读取或追加时补齐。

use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::classlog::{today_key, valid_date};
use crate::result::ToolError;
use crate::tools::memory::{load_json, lock_data_file, memories_path, next_id, now_ms, save_json};

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
    if !valid_date(date) { return Vec::new(); }
    let _guard = FILE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let Ok(_disk) = lock_data_file(&index_path()) else { return Vec::new() };
    load_notes_unlocked(&day_path(date)).unwrap_or_default()
}

/// 读索引（日期升序，最近的在最后）。
pub fn load_index() -> Vec<DailyIndexEntry> {
    let _guard = FILE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let Ok(_disk) = lock_data_file(&index_path()) else { return Vec::new() };
    let _ = recover_index_unlocked();
    load_index_unlocked().unwrap_or_default()
}

/// 往今天追加一条速记；正文成功后索引故障由恢复标记补齐，不重复追加正文。
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
    let _guard = FILE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _disk = lock_data_file(&index_path())?;
    recover_index_unlocked()?;
    let date = today_key();
    let path = day_path(&date);
    let mut notes = load_notes_unlocked(&path)?;
    let note = DailyNote {
        id: next_id(notes.iter().map(|n| n.id))?,
        ts_ms: now_ms(),
        content: content.to_owned(),
    };
    notes.push(note.clone());
    // 先留下恢复意图。正文提交后即算成功，避免索引失败诱发调用方重复追加。
    save_json(&pending_index_path(), &Some(date))?;
    save_json(&path, &notes)?;
    let _ = recover_index_unlocked();
    Ok(note)
}

fn load_notes_unlocked(path: &std::path::Path) -> Result<Vec<DailyNote>, ToolError> {
    load_json(path)
}

fn pending_index_path() -> PathBuf {
    daily_dir().join("index.pending.json")
}

fn recover_index_unlocked() -> Result<(), ToolError> {
    let pending: Option<String> = load_json(&pending_index_path())?;
    if let Some(date) = pending {
        if !valid_date(&date) {
            return Err(ToolError::io("待恢复索引日期无效"));
        }
        let notes = load_notes_unlocked(&day_path(&date))?;
        update_index_unlocked(&date, &notes)?;
        std::fs::remove_file(pending_index_path()).map_err(|e| ToolError::io(e.to_string()))?;
    }
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
        if candidate.chars().count() > 80 || (i + 1 >= 4 && notes.len() > 4) {
            let suffix = format!("…等 {} 条", notes.len());
            let keep = 80usize.saturating_sub(suffix.chars().count());
            return candidate.chars().take(keep).collect::<String>() + &suffix;
        }
        gist = candidate;
    }
    gist
}

fn update_index_unlocked(date: &str, notes: &[DailyNote]) -> Result<(), ToolError> {
    let mut index = load_index_unlocked()?;
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
        .map(weight)
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
    save_json(&index_path(), &index)
}

fn load_index_unlocked() -> Result<Vec<DailyIndexEntry>, ToolError> {
    load_json(&index_path())
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
#[path = "dailylog_tests.rs"]
mod tests;
