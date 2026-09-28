//! 仅供本机界面查看的进程内日志；不落盘、不发送网络、不进入模型上下文。
//!
//! 调用方只能传固定组件名和安全概括（例如错误类别），不能传工具参数、命令、
//! 正文、路径、原始错误或密钥。下面的保守脱敏只是纵深防护：它不识别任意编码、
//! 混淆或没有标记的秘密，不能把任意输入变成安全日志。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Instant;

pub const MAX_ENTRIES: usize = 1_000;
pub const MAX_BYTES: usize = 256 * 1024;
pub const MAX_ENTRY_BYTES: usize = 2_048;
const MAX_COMPONENT_BYTES: usize = 64;
const MAX_INPUT_BYTES: usize = 8 * 1024;
const REDACTED: &str = "[已脱敏]";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

impl Level {
    pub const ALL: [Self; 4] = [Self::Debug, Self::Info, Self::Warn, Self::Error];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Debug => "调试",
            Self::Info => "信息",
            Self::Warn => "警告",
            Self::Error => "错误",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub level: Level,
    pub component: Box<str>,
    pub message: Box<str>,
    /// 从本日志初始化开始的毫秒数，不包含系统时间或用户信息。
    pub first_ms: u64,
    pub last_ms: u64,
    /// 同级别、组件和脱敏概括的累计次数。
    pub occurrences: u64,
}

impl Entry {
    fn bytes(&self) -> usize {
        self.component.len() + self.message.len()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub received: u64,
    pub merged: u64,
    /// 容量淘汰的事件数，包含被淘汰条目中聚合的重复事件。
    pub dropped: u64,
    pub truncated: u64,
    /// 保留的 UTF-8 文本字节数；条目元数据另由 MAX_ENTRIES 限制。
    pub bytes: usize,
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    /// 按最后一次出现时间排列；不保留原始未脱敏文本。未变更的快照共享条目。
    pub entries: Arc<Vec<Entry>>,
    /// 全部计数均从最近一次 clear 开始。
    pub stats: Stats,
    pub max_entries: usize,
    pub max_bytes: usize,
    pub max_entry_bytes: usize,
}

struct Buffer {
    entries: VecDeque<Entry>,
    stats: Stats,
    max_entries: usize,
    max_bytes: usize,
    max_entry_bytes: usize,
    cached_snapshot: Option<Snapshot>,
}

impl Buffer {
    fn new(max_entries: usize, max_bytes: usize, max_entry_bytes: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            stats: Stats::default(),
            max_entries,
            max_bytes,
            max_entry_bytes,
            cached_snapshot: None,
        }
    }

    fn push(&mut self, level: Level, component: &str, message: &str, now: u64) {
        // 聚合、截断或直接丢弃也会改变计数，所有记录路径都必须失效。
        self.cached_snapshot = None;
        self.stats.received = self.stats.received.saturating_add(1);
        let (component, component_cut) =
            sanitize(component, MAX_COMPONENT_BYTES.min(self.max_entry_bytes));
        let (message, message_cut) = sanitize(
            message,
            self.max_entry_bytes.saturating_sub(component.len()),
        );
        if component_cut || message_cut {
            self.stats.truncated = self.stats.truncated.saturating_add(1);
        }
        // 在整个有界窗口内聚合，交错出现的重复错误也不会把其它条目挤掉。
        // 按脱敏后的概括聚合是刻意的：绝不为了去重保存原文或原文指纹。
        if let Some(index) = self.entries.iter().position(|entry| {
            entry.level == level && entry.component == component && entry.message == message
        }) {
            let mut entry = self.entries.remove(index).expect("已找到日志条目");
            entry.last_ms = now;
            entry.occurrences = entry.occurrences.saturating_add(1);
            self.entries.push_back(entry);
            self.stats.merged = self.stats.merged.saturating_add(1);
            return;
        }
        let entry = Entry {
            level,
            component,
            message,
            first_ms: now,
            last_ms: now,
            occurrences: 1,
        };
        let bytes = entry.bytes();
        if self.max_entries == 0 || bytes > self.max_bytes {
            self.stats.dropped = self.stats.dropped.saturating_add(1);
            return;
        }
        while self.entries.len() >= self.max_entries || self.stats.bytes > self.max_bytes - bytes {
            if let Some(old) = self.entries.pop_front() {
                self.stats.bytes -= old.bytes();
                self.stats.dropped = self.stats.dropped.saturating_add(old.occurrences);
            } else {
                break;
            }
        }
        self.stats.bytes += bytes;
        self.entries.push_back(entry);
    }

    fn snapshot(&mut self) -> Snapshot {
        self.cached_snapshot.get_or_insert_with(|| Snapshot {
            entries: Arc::new(self.entries.iter().cloned().collect()),
            stats: self.stats,
            max_entries: self.max_entries,
            max_bytes: self.max_bytes,
            max_entry_bytes: self.max_entry_bytes,
        }).clone()
    }

    fn clear(&mut self) {
        self.cached_snapshot = None;
        self.entries.clear();
        self.stats = Stats::default();
    }
}

struct Logger {
    started: Instant,
    buffer: Mutex<Buffer>,
}

impl Logger {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            buffer: Mutex::new(Buffer::new(MAX_ENTRIES, MAX_BYTES, MAX_ENTRY_BYTES)),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Buffer> {
        // 不输出锁错误，也不递归调用日志。未安装 panic hook，避免在持锁崩溃时重入。
        self.buffer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn record(&self, level: Level, component: &str, message: &str) {
        let mut buffer = self.lock();
        let now = self.started.elapsed().as_millis().min(u64::MAX as u128) as u64;
        buffer.push(level, component, message, now);
    }
}

fn logger() -> &'static Logger {
    static LOGGER: OnceLock<Logger> = OnceLock::new();
    LOGGER.get_or_init(Logger::new)
}

/// 只记录安全概括；禁止把原始错误的 Display/Debug 结果传进来。
pub fn record(level: Level, component: &str, message: &str) {
    logger().record(level, component, message);
}

/// 获取共享的有界脱敏视图；仅变更后首次读取复制条目，UI/剪贴板不持记录锁。
pub fn snapshot() -> Snapshot {
    logger().lock().snapshot()
}

/// 原子清空条目和计数；不重置进程内相对时钟。
pub fn clear() {
    logger().lock().clear();
}

/// 常见凭据标记出现时整字段隐藏，避免引号、空白和多行值造成部分泄露。
/// URL 整体隐藏（含路径、查询、片段和 userinfo），也隐藏带 ? 的相对 URL。
/// 不声称识别所有秘密；超长输入直接舍弃，避免先复制巨大的原始正文。
fn sanitize(text: &str, limit: usize) -> (Box<str>, bool) {
    if text.len() > MAX_INPUT_BYTES {
        return (clip("[输入过长，已省略]", limit).into_boxed_str(), true);
    }
    let lower = text.to_ascii_lowercase();
    let sensitive = [
        "token",
        "api_key",
        "api-key",
        "apikey",
        "api key",
        "authorization",
        "bearer",
        "basic ",
        "password",
        "passwd",
        "secret",
        "cookie",
        "credential",
        "access_key",
        "access-key",
        "private key",
        "sk-",
        "ghp_",
        "github_pat_",
        "glpat-",
        "xoxb-",
        "xoxp-",
        "aiza",
        "eyj",
        "://",
        "www.",
        "?",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
        || lower.split_whitespace().any(|word| word == "basic");
    let safe = if sensitive {
        REDACTED.to_owned()
    } else {
        // 输出始终是单行，避免换行、终端控制符和双向文本伪造日志外观。
        text.chars()
            .map(|ch| {
                if ch.is_control()
                    || matches!(ch, '\u{061c}' | '\u{200e}'..='\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
                {
                    ' '
                } else {
                    ch
                }
            })
            .collect()
    };
    let truncated = safe.len() > limit;
    (clip(&safe, limit).into_boxed_str(), truncated)
}

fn clip(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    const MARK: &str = "[截断]";
    let marker = if limit >= MARK.len() { MARK } else { "" };
    let mut end = limit.saturating_sub(marker.len()).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut output = text[..end].to_owned();
    output.push_str(marker);
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn unchanged_thousand_entry_snapshots_avoid_repeated_string_copies() {
        let mut buffer = Buffer::new(MAX_ENTRIES, MAX_BYTES, MAX_ENTRY_BYTES);
        for index in 0..MAX_ENTRIES {
            buffer.push(Level::Info, "test", &format!("安全事件 {index}"), index as u64);
        }
        let view = buffer.snapshot();
        assert_eq!(view.entries.len(), MAX_ENTRIES);
        assert!(view.entries.iter().zip(&buffer.entries).all(|(copy, original)| {
            copy.component.as_ptr() != original.component.as_ptr()
                && copy.message.as_ptr() != original.message.as_ptr()
        }));
        for repeats in [4, 60] {
            let mut legacy_copies = 0;
            let mut cached_copies = 0;
            let mut rebuilt = 0;
            for _ in 0..repeats {
                // 与优化前相同的 collect/cloned 路径，活跃分配地址不可重用。
                let legacy: Vec<_> = buffer.entries.iter().cloned().collect();
                legacy_copies += legacy.iter().zip(&buffer.entries).map(|(a, b)| {
                    usize::from(a.component.as_ptr() != b.component.as_ptr())
                        + usize::from(a.message.as_ptr() != b.message.as_ptr())
                }).sum::<usize>();
                let next = buffer.snapshot();
                rebuilt += usize::from(!Arc::ptr_eq(&view.entries, &next.entries));
                cached_copies += next.entries.iter().zip(view.entries.iter()).map(|(a, b)| {
                    usize::from(a.component.as_ptr() != b.component.as_ptr())
                        + usize::from(a.message.as_ptr() != b.message.as_ptr())
                }).sum::<usize>();
            }
            assert_eq!(legacy_copies, repeats * MAX_ENTRIES * 2);
            assert_eq!((cached_copies, rebuilt), (0, 0));
            println!("1000项，预热后{repeats}次读取：旧路径Box字符串复制={legacy_copies}，缓存复制={cached_copies}，缓存重建={rebuilt}");
        }
    }

    #[test]
    fn record_merge_eviction_and_clear_refresh_only_the_latest_snapshot() {
        let logger = Logger {
            started: Instant::now(),
            buffer: Mutex::new(Buffer::new(1, 100, 40)),
        };
        let empty = logger.lock().snapshot();
        logger.record(Level::Info, "test", "first");
        let first = logger.lock().snapshot();
        assert!(!Arc::ptr_eq(&empty.entries, &first.entries));
        let old = Arc::downgrade(&first.entries);
        logger.record(Level::Info, "test", "first");
        let merged = logger.lock().snapshot();
        assert!(!Arc::ptr_eq(&first.entries, &merged.entries));
        assert_eq!(first.entries[0].occurrences, 1);
        assert_eq!(merged.entries[0].occurrences, 2);
        assert_eq!(merged.stats.merged, 1);
        assert!(merged.entries[0].last_ms >= first.entries[0].last_ms);
        drop(first);
        assert!(old.upgrade().is_none(), "缓存不能累积历史快照");
        logger.record(Level::Error, "test", "second");
        let evicted = logger.lock().snapshot();
        assert!(!Arc::ptr_eq(&merged.entries, &evicted.entries));
        assert_eq!(evicted.stats.dropped, 2);
        assert_eq!(&*evicted.entries[0].message, "second");
        logger.lock().clear();
        let cleared = logger.lock().snapshot();
        assert!(!Arc::ptr_eq(&evicted.entries, &cleared.entries));
        assert!(cleared.entries.is_empty());
        assert_eq!(cleared.stats, Stats::default());
        assert!(Arc::ptr_eq(&cleared.entries, &logger.lock().snapshot().entries));
        logger.record(Level::Info, "test", "new");
        assert_eq!(logger.lock().snapshot().stats.received, 1);
    }

    #[test]
    fn dropped_and_truncated_records_invalidate_even_without_new_entries() {
        for (count, bytes) in [(0, 100), (3, 1)] {
            let mut buffer = Buffer::new(count, bytes, 20);
            let before = buffer.snapshot();
            buffer.push(Level::Warn, "test", &"长".repeat(100), 1);
            let after = buffer.snapshot();
            assert!(!Arc::ptr_eq(&before.entries, &after.entries));
            assert!(after.entries.is_empty());
            assert_eq!(after.stats.received, 1);
            assert_eq!(after.stats.dropped, 1);
            assert_eq!(after.stats.truncated, 1);
            assert_eq!(before.stats, Stats::default());
        }
    }

    #[test]
    fn count_and_byte_limits_evict_oldest() {
        let mut buffer = Buffer::new(2, 5, 20);
        buffer.push(Level::Info, "c", "aa", 0);
        buffer.push(Level::Info, "c", "bb", 1);
        assert_eq!(buffer.entries.len(), 1);
        assert_eq!(buffer.stats.bytes, 3);
        assert_eq!(buffer.stats.dropped, 1);
        let mut buffer = Buffer::new(2, 100, 20);
        for message in ["a", "b", "c"] {
            buffer.push(Level::Info, "c", message, 0);
        }
        assert_eq!(buffer.entries.len(), 2);
        assert_eq!(&*buffer.entries[0].message, "b");
        assert_eq!(buffer.stats.dropped, 1);
    }

    #[test]
    fn oversized_entry_is_dropped_and_zero_capacity_is_safe() {
        for (count, bytes) in [(0, 100), (3, 1)] {
            let mut buffer = Buffer::new(count, bytes, 20);
            buffer.push(Level::Info, "c", "aa", 0);
            assert!(buffer.entries.is_empty());
            assert_eq!(buffer.stats.dropped, 1);
            assert_eq!(buffer.stats.bytes, 0);
        }
    }

    #[test]
    fn unicode_truncation_and_controls_stay_bounded() {
        for limit in 0..40 {
            let mut buffer = Buffer::new(10, 100, limit);
            buffer.push(Level::Info, "组件", &"你好🦀".repeat(10), 0);
            assert!(buffer.entries[0].bytes() <= limit);
            assert_eq!(buffer.stats.truncated, 1);
        }
        assert_eq!(&*sanitize("你好\n警告\u{202e}", 100).0, "你好 警告 ");
        assert_eq!(
            &*sanitize("一\u{2028}二\u{2029}三\u{061c}\u{200e}\u{200f}", 100).0,
            "一 二 三   "
        );
        assert!(sanitize(&"中".repeat(10_000), 100).0.contains("省略"));
    }

    #[test]
    fn secrets_and_urls_are_not_retained() {
        for text in [
            "Authorization: Bearer TOPSECRET",
            "{\"api_key\":\"TOPSECRET\"}",
            "API-KEY = TOPSECRET",
            "api key: TOPSECRET",
            "access_token = TOPSECRET",
            "password=\"TOPSECRET other words\"",
            "Bearer TOPSECRET",
            "Basic TOPSECRET",
            "Basic\tTOPSECRET",
            "Basic\nTOPSECRET",
            "Basic\u{00a0}TOPSECRET",
            "sk-TOPSECRET",
            "ghp_TOPSECRET",
            "github_pat_TOPSECRET",
            "AIzaTOPSECRET",
            "eyJhbGciOiJIUzI1NiJ9.TOPSECRET.signature",
            "cookie: TOPSECRET",
            "https://user:TOPSECRET@example.org/path?value=TOPSECRET#fragment",
            "/models?value=TOPSECRET",
            "TOKEN:\nTOPSECRET",
            "组件 apiKey TOPSECRET",
        ] {
            assert_eq!(&*sanitize(text, 100).0, REDACTED, "{text}");
            let mut buffer = Buffer::new(10, 100, 100);
            buffer.push(Level::Error, text, text, 0);
            let view = buffer.snapshot();
            assert!(!format!("{view:?}").contains("TOPSECRET"));
        }
        assert_eq!(
            &*sanitize("工具执行失败：timeout", 100).0,
            "工具执行失败：timeout"
        );
    }

    #[test]
    fn interleaved_duplicates_are_aggregated_and_counted_on_eviction() {
        let mut buffer = Buffer::new(2, 100, 40);
        buffer.push(Level::Error, "tool", "失败", 1);
        buffer.push(Level::Info, "app", "启动", 2);
        buffer.push(Level::Error, "tool", "失败", 3);
        assert_eq!(buffer.entries.len(), 2);
        let entry = &buffer.entries[1];
        assert_eq!(
            (entry.first_ms, entry.last_ms, entry.occurrences),
            (1, 3, 2)
        );
        assert_eq!(buffer.stats.merged, 1);
        buffer.push(Level::Warn, "app", "警告", 4);
        buffer.push(Level::Debug, "app", "调试", 5);
        assert_eq!(buffer.stats.dropped, 3);
        assert_eq!(buffer.stats.received, 5);
    }

    #[test]
    fn clear_resets_all_counts_and_does_not_mutate_snapshots() {
        let mut buffer = Buffer::new(1, 100, 12);
        for message in ["a", "a", "很长很长很长很长很长"] {
            buffer.push(Level::Error, "c", message, 0);
        }
        let before = buffer.snapshot();
        assert!(before.stats.merged > 0 && before.stats.dropped > 0 && before.stats.truncated > 0);
        buffer.clear();
        assert!(buffer.entries.is_empty());
        assert_eq!(buffer.stats, Stats::default());
        assert_eq!(before.entries.len(), 1);
        buffer.push(Level::Info, "c", "新", 1);
        assert_eq!(buffer.stats.received, 1);
    }

    #[test]
    fn parallel_eviction_clear_and_snapshot_preserve_invariants() {
        let logger = Arc::new(Logger::new());
        std::thread::scope(|scope| {
            for worker in 0..4 {
                let logger = &logger;
                scope.spawn(move || {
                    for index in 0..600 {
                        logger.record(Level::Info, "test", &format!("安全事件 {worker}-{index}"));
                        if worker == 0 && index % 100 == 0 {
                            logger.lock().clear();
                        }
                        let view = logger.lock().snapshot();
                        assert!(view.entries.len() <= MAX_ENTRIES);
                        assert!(view.stats.bytes <= MAX_BYTES);
                        assert_eq!(view.stats.bytes, view.entries.iter().map(Entry::bytes).sum::<usize>());
                        assert_eq!(view.stats.received, view.stats.dropped + view.entries.iter().map(|entry| entry.occurrences).sum::<u64>());
                        assert!(view.entries.windows(2).all(|pair| pair[0].last_ms <= pair[1].last_ms));
                        assert!(view.entries.iter().all(|entry| entry.first_ms <= entry.last_ms));
                    }
                });
            }
        });
    }

    #[test]
    fn poisoned_lock_does_not_disable_safe_logging() {
        let logger = Arc::new(Logger::new());
        let worker = Arc::clone(&logger);
        assert!(std::thread::spawn(move || {
            let _guard = worker.lock();
            panic!("合成锁中毒");
        }).join().is_err());
        logger.record(Level::Warn, "test", "安全事件");
        assert_eq!(logger.lock().snapshot().stats.received, 1);
        logger.lock().clear();
        assert_eq!(logger.lock().snapshot().stats, Stats::default());
    }

    #[test]
    fn parallel_record_and_snapshot_are_consistent() {
        let logger = Arc::new(Logger::new());
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let logger = Arc::clone(&logger);
                std::thread::spawn(move || {
                    for _ in 0..500 {
                        logger.record(Level::Error, "tool", "执行失败：timeout");
                        let view = logger.lock().snapshot();
                        assert_eq!(
                            view.stats.bytes,
                            view.entries.iter().map(Entry::bytes).sum::<usize>()
                        );
                        assert!(view.entries.len() <= MAX_ENTRIES);
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        let view = logger.lock().snapshot();
        assert_eq!(view.stats.received, 4_000);
        assert_eq!(view.stats.merged, 3_999);
        assert_eq!(view.entries[0].occurrences, 4_000);
    }
}
