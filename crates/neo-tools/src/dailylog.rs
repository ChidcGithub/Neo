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
    fn failed_index_write_is_recovered_without_duplicate_note() {
        let _lock = crate::NEO_HOME_TEST_LOCK.lock().unwrap();
        let (_g, dir) = temp_home("index-recovery");
        std::fs::create_dir_all(index_path()).unwrap();
        let saved = append("正文成功，索引失败").unwrap();
        assert!(pending_index_path().exists());
        assert_eq!(load_day(&today_key()).len(), 1);
        assert!(append("恢复前不提交下一条").is_err());
        std::fs::remove_dir(index_path()).unwrap();
        let index = load_index();
        assert_eq!(index.len(), 1);
        assert!(index[0].gist.contains("正文成功"));
        assert!(!pending_index_path().exists());
        assert_eq!(load_day(&today_key())[0].id, saved.id);
        append("下一条").unwrap();
        assert_eq!(load_day(&today_key()).len(), 2);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn unreadable_index_preserves_history_and_pending_recovery() {
        use std::os::windows::fs::OpenOptionsExt;
        let _lock = crate::NEO_HOME_TEST_LOCK.lock().unwrap();
        let (_g, dir) = temp_home("index-sharing");
        let history = vec![DailyNote { id: 1, ts_ms: 0, content: "历史记录".into() }];
        save_json(&day_path("2024-02-29"), &history).unwrap();
        update_index_unlocked("2024-02-29", &history).unwrap();
        let before = std::fs::read(index_path()).unwrap();
        let held = std::fs::OpenOptions::new().read(true).share_mode(0).open(index_path()).unwrap();
        let note = append("正文已提交").unwrap();
        let body = std::fs::read(day_path(&today_key())).unwrap();
        assert!(pending_index_path().exists());
        assert!(append("不可重复写入").is_err());
        assert!(load_index().is_empty());
        assert_eq!(std::fs::read(day_path(&today_key())).unwrap(), body);
        drop(held);
        assert_eq!(std::fs::read(index_path()).unwrap(), before);
        for _ in 0..2 {
            let index = load_index();
            assert_eq!(index.len(), 2);
            assert_eq!(index[0].date, "2024-02-29");
            assert_eq!(index[0].gist, "历史记录");
            assert_eq!(index[1].gist, "正文已提交");
        }
        assert!(!pending_index_path().exists());
        let notes = load_day(&today_key());
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].id, note.id);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn failed_body_and_pending_cleanup_recover_idempotently() {
        use std::os::windows::fs::OpenOptionsExt;
        let _lock = crate::NEO_HOME_TEST_LOCK.lock().unwrap();
        let (_g, dir) = temp_home("commit-stages");
        let first = append("原记录").unwrap();
        let path = day_path(&today_key());
        let before = std::fs::read(&path).unwrap();
        let held = std::fs::OpenOptions::new().read(true).share_mode(1).open(&path).unwrap();
        assert!(append("稍后重试").is_err());
        assert!(pending_index_path().exists());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        drop(held);
        assert_eq!(load_index()[0].gist, "原记录");
        assert!(!pending_index_path().exists());
        assert_eq!(append("稍后重试").unwrap().id, first.id + 1);
        let before = std::fs::read(&path).unwrap();
        save_json(&pending_index_path(), &Some(today_key())).unwrap();
        let held = std::fs::OpenOptions::new().read(true).share_mode(1).open(pending_index_path()).unwrap();
        for _ in 0..2 {
            let index = load_index();
            assert_eq!(index.len(), 1);
            assert_eq!(index[0].gist, "原记录；稍后重试");
            assert!(pending_index_path().exists());
        }
        assert!(append("清理失败时不追加").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        drop(held);
        assert_eq!(load_index().len(), 1);
        assert!(!pending_index_path().exists());
        assert_eq!(load_day(&today_key()).len(), 2);
        std::fs::remove_dir_all(dir).unwrap();
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
    fn pending_index_recovery_uses_recorded_day_and_gist_stays_bounded() {
        let _lock = crate::NEO_HOME_TEST_LOCK.lock().unwrap();
        let (_g, dir) = temp_home("past-day");
        let notes: Vec<_> = (1..=100).map(|id| DailyNote {
            id, ts_ms: 0, content: "字".repeat(20),
        }).collect();
        assert!(gist_of(&notes).chars().count() <= 80);
        save_json(&day_path("2024-02-29"), &notes).unwrap();
        save_json(&pending_index_path(), &Some("2024-02-29")).unwrap();
        let index = load_index();
        assert_eq!(index.len(), 1);
        assert_eq!(index[0].date, "2024-02-29");
        assert_eq!(load_day("2024-02-29").len(), 100);
        assert!(load_day("../memories").is_empty());
        std::fs::remove_dir_all(dir).unwrap();
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
            update_index_unlocked(&date, &notes).unwrap();
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
