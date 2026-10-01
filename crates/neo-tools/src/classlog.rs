//! 课堂分记忆：「当天、当节」的课堂记录，与总记忆（`tools::memory`）分离存放。
//!
//! 目录约定：与 `memories.json` 同目录下的 `class/{YYYY-MM-DD}.json`，
//! 一天一个文件、一节课一条。分记忆只装课堂内容（科目 / 总结 / 原始素材），
//! 不回读总记忆；反向的索引（总记忆里留一句话指向当天的课）由 app 在
//! 总结完成后追加 —— 单向引用，互不缠绕。

use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::result::ToolError;
use crate::tools::memory::{load_json, lock_data_file, memories_path, next_id, now_ms, save_json};

/// 总结的目标字数上限：超过只是提醒（`over_limit`），不强制截断。
pub const SUMMARY_LIMIT: usize = 1500;

/// 一节课的分记忆。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClassNote {
    pub id: u64,
    /// Unix 毫秒：进入最大化（开课）时刻。
    pub started_ms: i64,
    /// Unix 毫秒：总结完成（收课）时刻。
    pub ended_ms: i64,
    /// 科目（首次截屏的视觉分析识别；识别不出为空串）。
    pub subject: String,
    /// 打磨后的课堂总结（目标 ≤ [`SUMMARY_LIMIT`] 字）。
    pub summary: String,
    /// 总结是否超了上限（UI 提醒用，不强制）。
    pub over_limit: bool,
    /// 原始素材：屏幕笔记（每次截图分析一条）。
    pub screen_notes: Vec<String>,
    /// 原始素材：课堂语音转写（逐句）。
    pub transcript: Vec<String>,
}

/// 待落盘的一节课（id / over_limit 由存储层分配与计算）。
pub struct NewClassNote {
    pub started_ms: i64,
    pub subject: String,
    pub summary: String,
    pub screen_notes: Vec<String>,
    pub transcript: Vec<String>,
}

/// 分记忆目录：`{NEO_HOME|%APPDATA%\Neo|当前目录}/class/`（与记忆文件同约定）。
pub fn class_dir() -> PathBuf {
    memories_path()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
        .join("class")
}

/// 某一天的分记忆文件。
pub fn class_path(date: &str) -> PathBuf {
    class_dir().join(format!("{date}.json"))
}

pub(crate) fn valid_date(date: &str) -> bool {
    let b = date.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-'
        || !b.iter().enumerate().all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit()) {
        return false;
    }
    let year: u32 = date[..4].parse().unwrap();
    let month: u32 = date[5..7].parse().unwrap();
    let day: u32 = date[8..].parse().unwrap();
    let days = match month {
        2 if year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400)) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        _ => return false,
    };
    year > 0 && day > 0 && day <= days
}

/// 当天日期串（本地时区，`YYYY-MM-DD`）。
#[cfg(windows)]
pub fn today_key() -> String {
    use windows_sys::Win32::Foundation::SYSTEMTIME;
    use windows_sys::Win32::System::SystemInformation::GetLocalTime;
    let mut st: SYSTEMTIME = unsafe { std::mem::zeroed() };
    unsafe { GetLocalTime(&mut st) };
    format!("{:04}-{:02}-{:02}", st.wYear, st.wMonth, st.wDay)
}

/// 非 Windows 只是编译保活（教学部署全是 Windows），退回 UTC 日期。
#[cfg(not(windows))]
pub fn today_key() -> String {
    let (y, m, d) = civil_from_days(now_ms().div_euclid(86_400_000));
    format!("{y:04}-{m:02}-{d:02}")
}

/// Howard Hinnant 的 days → 年月日算法（UTC）。
#[cfg(not(windows))]
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// 进程内读改写串行化（分析 worker 与 UI 可能同时动手）。
static FILE_LOCK: Mutex<()> = Mutex::new(());

/// 读某一天的分记忆；文件不存在返回空，损坏文件改名 `.bad` 留档。
pub fn load_day(date: &str) -> Vec<ClassNote> {
    if !valid_date(date) {
        return Vec::new();
    }
    let _guard = FILE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let path = class_path(date);
    let Ok(_disk) = lock_data_file(&path) else { return Vec::new() };
    load_json(&path).unwrap_or_default()
}

/// 追加一节课到当天文件，返回分配好 id 的完整条目。
pub fn append_class(note: NewClassNote) -> Result<ClassNote, ToolError> {
    append_class_on(&today_key(), note)
}

/// 追加到固定目标日；调用方重试时必须复用同一日期，不能重新取当天日期。
pub fn append_class_on(date: &str, note: NewClassNote) -> Result<ClassNote, ToolError> {
    if !valid_date(date) {
        return Err(ToolError::bad_args("课堂日期必须是有效的 YYYY-MM-DD"));
    }
    let _guard = FILE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let path = class_path(date);
    let _disk = lock_data_file(&path)?;
    let mut list: Vec<ClassNote> = load_json(&path)?;
    // 同一次收课回执丢失后重试不产生第二份；同起点但不同内容仍可保存。
    if let Some(saved) = list.iter().find(|saved| {
        saved.started_ms == note.started_ms && saved.subject == note.subject
            && saved.summary == note.summary && saved.screen_notes == note.screen_notes
            && saved.transcript == note.transcript
    }) {
        return Ok(saved.clone());
    }
    let chars = note.summary.chars().count();
    let full = ClassNote {
        id: next_id(list.iter().map(|c| c.id))?,
        started_ms: note.started_ms,
        ended_ms: now_ms(),
        subject: note.subject,
        summary: note.summary,
        over_limit: chars > SUMMARY_LIMIT,
        screen_notes: note.screen_notes,
        transcript: note.transcript,
    };
    list.push(full.clone());
    save_json(&path, &list)?;
    Ok(full)
}

#[cfg(test)]
#[path = "classlog_tests.rs"]
mod tests;
