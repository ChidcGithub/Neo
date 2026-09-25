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
use crate::tools::memory::{memories_path, now_ms};

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
    let _guard = FILE_LOCK.lock().unwrap();
    let path = class_path(date);
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    match serde_json::from_str::<Vec<ClassNote>>(&raw) {
        Ok(list) => list,
        Err(e) => {
            eprintln!("[neo] 分记忆文件损坏（{e}），已留档：{}", path.display());
            let _ = std::fs::rename(&path, path.with_extension("json.bad"));
            Vec::new()
        }
    }
}

/// 追加一节课到当天文件，返回分配好 id 的完整条目。
pub fn append_class(note: NewClassNote) -> Result<ClassNote, ToolError> {
    let _guard = FILE_LOCK.lock().unwrap();
    let date = today_key();
    let path = class_path(&date);
    let mut list = {
        // 不存在 = 当天第一节课；坏了就留档重建（与 load_day 同款处理）。
        match std::fs::read_to_string(&path) {
            Err(_) => Vec::new(),
            Ok(raw) => match serde_json::from_str::<Vec<ClassNote>>(&raw) {
                Ok(list) => list,
                Err(e) => {
                    eprintln!("[neo] 分记忆文件损坏（{e}），已留档：{}", path.display());
                    let _ = std::fs::rename(&path, path.with_extension("json.bad"));
                    Vec::new()
                }
            },
        }
    };
    let chars = note.summary.chars().count();
    let full = ClassNote {
        id: list.iter().map(|c| c.id).max().unwrap_or(0) + 1,
        started_ms: note.started_ms,
        ended_ms: now_ms(),
        subject: note.subject,
        summary: note.summary,
        over_limit: chars > SUMMARY_LIMIT,
        screen_notes: note.screen_notes,
        transcript: note.transcript,
    };
    list.push(full.clone());
    // 原子写：先 tmp 再改名，写一半断电不留半个 JSON。
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ToolError::io(format!("创建目录 {} 失败：{e}", parent.display())))?;
    }
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_string_pretty(&list)
        .map_err(|e| ToolError::io(format!("序列化分记忆失败：{e}")))?;
    std::fs::write(&tmp, body)
        .map_err(|e| ToolError::io(format!("写 {} 失败：{e}", tmp.display())))?;
    std::fs::rename(&tmp, &path)
        .map_err(|e| ToolError::io(format!("落盘 {} 失败：{e}", path.display())))?;
    Ok(full)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 隔离：指向临时 NEO_HOME，跑完恢复（与 memory.rs 同款，测试串行跑）。
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

    fn fixture(content: &str) -> NewClassNote {
        NewClassNote {
            started_ms: 1_790_000_000_000,
            subject: "数学".into(),
            summary: content.into(),
            screen_notes: vec!["板书：三角函数定义".into()],
            transcript: vec!["同学们看这里".into()],
        }
    }

    /// 两个场景共享 NEO_HOME 环境变量，必须合在一个测试里串行跑
    /// （memory.rs 已经踩过并行串台的坑）。
    #[test]
    fn append_roundtrip_and_corruption_handling() {
        let dir = std::env::temp_dir().join(format!("neo-classlog-{}", std::process::id()));
        let _g = EnvGuard::set(&dir);

        // 同一天写两节课，id 递增、落在同一个当天文件里。
        let a = append_class(fixture("诱导公式")).unwrap();
        let b = append_class(fixture("图象变换")).unwrap();
        assert_eq!(b.id, a.id + 1);

        let day = load_day(&today_key());
        assert_eq!(day.len(), 2);
        assert_eq!(day[0].subject, "数学");
        assert_eq!(day[0].summary, "诱导公式");
        assert_eq!(day[0].transcript, vec!["同学们看这里".to_string()]);
        assert!(!day[0].over_limit);
        assert!(day[0].ended_ms >= day[0].started_ms);

        // 超限标记：1501 个字符 → over_limit。
        let long = "字".repeat(SUMMARY_LIMIT + 1);
        let c = append_class(fixture(&long)).unwrap();
        assert!(c.over_limit);

        // 损坏文件：改名 .bad 留档，读出来是空而不是崩。
        std::fs::write(class_path(&today_key()), b"{ not json").unwrap();
        assert!(load_day(&today_key()).is_empty());
        assert!(class_path(&today_key()).with_extension("json.bad").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
