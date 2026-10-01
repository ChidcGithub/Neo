//! `note_day` / `recall_day` —— 每日记忆：agent 的「当天速记」与回查。
//!
//! 与长期记忆（[`super::memory`]）的分工：长期记忆装跨对话一直有效的用户档案；
//! 每日记忆装**只对当天/某天有意义**的流水 —— 典型来源是静默观察
//! （看到疑似课堂无关内容时记一条，不声张），事后用户问起再回查。
//!
//! 存储与索引规则见 [`crate::dailylog`] 模块头（先查索引再翻文件）。
//! 写归 `Risk::Open`（与 remember 同理：改的是 Neo 自己的记忆文件）；
//! 读归 `Risk::Read`。

use serde_json::json;

use crate::dailylog;
use crate::result::{Args, Outcome, ToolError};
use crate::spec::Param;
use crate::Scope;

pub static NOTE_PARAMS: &[Param] = &[Param::text(
    "content",
    "要记的内容：一两句话说清「看到/约定了什么」（≤500 字，越短越好）。\
     时间由工具自动盖章，不用自己写。",
)];

pub static RECALL_PARAMS: &[Param] = &[Param::opt_text(
    "date",
    "要查的日期（YYYY-MM-DD）。省略 = 只返回索引（哪天有什么的一句话列表，\
     先查它）；给日期 = 返回那天的全部速记与当天的课堂总结。",
)];

pub fn note_preview(args: &Args) -> String {
    let c = args.opt_str("content").unwrap_or_default();
    let head: String = c.chars().take(24).collect();
    format!("记录当天记忆：{head}")
}

pub fn note_run(_scope: &Scope, args: &Args) -> Outcome {
    let content = match args.require_str("content") {
        Ok(c) => c,
        Err(e) => return Outcome::fail("note_day", e),
    };
    match dailylog::append(&content) {
        Ok(note) => {
            let time = dailylog::now_hhmm();
            Outcome::ok(
                "note_day",
                format!("已记录（今天 {time}）"),
                json!({"date": crate::classlog::today_key(), "time": time, "id": note.id}),
            )
        }
        Err(e) => Outcome::fail("note_day", e),
    }
}

pub fn recall_preview(args: &Args) -> String {
    let date = args.opt_str("date").unwrap_or_default();
    if date.trim().is_empty() {
        "查看每日记忆索引".to_owned()
    } else {
        format!("读取 {date} 的记录")
    }
}

pub fn recall_run(_scope: &Scope, args: &Args) -> Outcome {
    let date = args.opt_str("date").unwrap_or_default().trim().to_owned();
    if date.is_empty() {
        return recall_index();
    }
    // 轻校验：YYYY-MM-DD 形状（不查日历合法性 —— 没有那天的记录就是空结果）。
    let shape_ok = date.len() == 10
        && date
            .chars()
            .enumerate()
            .all(|(i, c)| if i == 4 || i == 7 { c == '-' } else { c.is_ascii_digit() });
    if !shape_ok {
        return Outcome::fail(
            "recall_day",
            ToolError::bad_args(format!("日期格式不对：{date}"))
                .with_hint("用 YYYY-MM-DD，如 2026-09-27；省略 date 则返回索引"),
        );
    }
    recall_day(&date)
}

/// 索引模式：哪天有什么的一句话列表（最近的在最后）。
fn recall_index() -> Outcome {
    let index = dailylog::load_index();
    let summary = if index.is_empty() {
        "每日记忆为空（还没有任何一天的记录）".to_owned()
    } else {
        format!(
            "共 {} 天有记录（最早 {}，最新 {}）；要看某天全文请带 date 再调一次",
            index.len(),
            index.first().map(|e| e.date.as_str()).unwrap_or(""),
            index.last().map(|e| e.date.as_str()).unwrap_or(""),
        )
    };
    Outcome::ok(
        "recall_day",
        summary,
        json!({"index": index, "hint": "带 date=YYYY-MM-DD 再调一次读当天全文"}),
    )
}

/// 单日模式：当天速记 + 课堂总结（课堂语音转写原文太长，不在此列）。
fn recall_day(date: &str) -> Outcome {
    let notes = dailylog::load_day(date);
    let class = crate::classlog::load_day(date);
    let class_briefs: Vec<_> = class
        .iter()
        .map(|c| {
            json!({
                "subject": c.subject,
                "summary": c.summary,
                "screen_notes": c.screen_notes.len(),
                "transcript_lines": c.transcript.len(),
            })
        })
        .collect();
    let notes_json: Vec<_> = notes
        .iter()
        .map(|n| {
            json!({
                "time": ts_hhmm(n.ts_ms),
                "content": n.content,
            })
        })
        .collect();
    let summary = if notes.is_empty() && class_briefs.is_empty() {
        format!("{date} 没有任何记录")
    } else {
        format!("{date}：{} 条速记 + {} 节课", notes.len(), class_briefs.len())
    };
    Outcome::ok(
        "recall_day",
        summary,
        json!({"date": date, "notes": notes_json, "class": class_briefs}),
    )
}

#[cfg(any(windows, test))]
fn unix_ms_to_filetime(ms: i64) -> Option<u64> {
    u64::try_from(i128::from(ms) * 10_000 + 116_444_736_000_000_000).ok()
}

#[cfg(test)]
#[path = "daily_tests.rs"]
mod tests;

/// Unix 毫秒 → 本地「HH:MM」（Windows 取本地时区；非 Windows 退回 UTC，仅保编译）。
#[cfg(windows)]
fn ts_hhmm(ms: i64) -> String {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::Foundation::SYSTEMTIME;
    use windows_sys::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
    // FILETIME = 100ns 自 1601 年；Unix ms → FILETIME 的换算系数。
    let Some(ft64) = unix_ms_to_filetime(ms) else {
        return "??:??".to_owned();
    };
    let ft = FILETIME {
        dwLowDateTime: (ft64 & 0xFFFF_FFFF) as u32,
        dwHighDateTime: (ft64 >> 32) as u32,
    };
    unsafe {
        let mut utc: SYSTEMTIME = std::mem::zeroed();
        let mut local: SYSTEMTIME = std::mem::zeroed();
        if FileTimeToSystemTime(&ft, &mut utc) != 0
            && SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) != 0
        {
            return format!("{:02}:{:02}", local.wHour, local.wMinute);
        }
    }
    "??:??".to_owned()
}

#[cfg(not(windows))]
fn ts_hhmm(ms: i64) -> String {
    let mins = (ms / 60_000).rem_euclid(1440);
    format!("{:02}:{:02}", mins / 60, mins % 60)
}
