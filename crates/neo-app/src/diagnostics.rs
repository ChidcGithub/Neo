//! 仅供本机界面查看的进程内日志；不落盘、不发送网络、不进入模型上下文。
//!
//! 调用方只能传固定组件名和安全概括（例如错误类别），不能传工具参数、命令、
//! 正文、路径、原始错误或密钥。下面的保守脱敏只是纵深防护：它不识别任意编码、
//! 混淆或没有标记的秘密，不能把任意输入变成安全日志。

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
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
    #[cfg(test)]
    if TEST_BUFFER.with(|slot| {
        if let Some(buffer) = slot.borrow_mut().as_mut() {
            buffer.push(level, component, message, 0);
            true
        } else { false }
    }) { return; }
    logger().record(level, component, message);
}

#[cfg(test)]
thread_local! {
    static TEST_BUFFER: std::cell::RefCell<Option<Buffer>> = const { std::cell::RefCell::new(None) };
}

/// 调用点测试隔离日志，避免并行 UI 清空日志导致偶发失败；不接管其它线程。
#[cfg(test)]
pub(crate) fn capture_for_test(run: impl FnOnce()) -> Snapshot {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) { TEST_BUFFER.with(|slot| *slot.borrow_mut() = None); }
    }
    TEST_BUFFER.with(|slot| {
        assert!(slot.borrow().is_none());
        *slot.borrow_mut() = Some(Buffer::new(MAX_ENTRIES, MAX_BYTES, MAX_ENTRY_BYTES));
    });
    let _reset = Reset;
    run();
    TEST_BUFFER.with(|slot| slot.borrow_mut().as_mut().unwrap().snapshot())
}

/// 仅本进程分配的关联 ID；不能使用模型 call_id、数据库 ID 或原文指纹。
#[derive(Clone, Copy, Debug)]
pub struct Span {
    pub id: u64,
    parent: Option<u64>,
    component: &'static str,
    started: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Started, Ready, AwaitingConfirm, Approved, Denied, Answered, Skipped,
    Dispatched, WorkerFinished, Delivered, ResultDiscarded, CancelRequested,
    Completed, Failed, Rejected, Abandoned,
}

impl Phase {
    fn label(self) -> &'static str {
        match self {
            Self::Started => "started", Self::Ready => "ready",
            Self::AwaitingConfirm => "awaiting_confirm", Self::Approved => "approved",
            Self::Denied => "denied", Self::Answered => "answered", Self::Skipped => "skipped",
            Self::Dispatched => "dispatched", Self::WorkerFinished => "worker_finished",
            Self::Delivered => "delivered", Self::ResultDiscarded => "result_discarded",
            Self::CancelRequested => "cancel_requested", Self::Completed => "completed",
            Self::Failed => "failed", Self::Rejected => "rejected", Self::Abandoned => "abandoned",
        }
    }
}

/// 非结构化错误只允许映射到固定类别，不能把原文作为字段传入。
#[derive(Clone, Copy, Debug)]
pub enum Failure {
    Tool(neo_tools::ErrorKind),
    HttpClient, HttpServer, HttpOther, Transport, ModelOther,
    Spawn, Disconnected, Budget, ToolLimit, Storage, Configuration,
}

impl Failure {
    fn label(self) -> &'static str {
        match self {
            Self::Tool(kind) => kind.as_str(),
            Self::HttpClient => "http_4xx", Self::HttpServer => "http_5xx",
            Self::HttpOther => "http_other", Self::Transport => "transport",
            Self::ModelOther => "model_other", Self::Spawn => "thread_spawn",
            Self::Disconnected => "channel_disconnected", Self::Budget => "budget",
            Self::ToolLimit => "tool_limit", Self::Storage => "storage",
            Self::Configuration => "configuration",
        }
    }
}

/// 只识别 neo-llm 固定前缀后的三位状态码，不扫描 URL、响应正文或任意数字。
pub fn model_failure(error: &str) -> Failure {
    let status = error.strip_prefix("接口返回 ")
        .or_else(|| error.strip_prefix("模型列表接口返回 HTTP "));
    if let Some(status) = status {
        let bytes = status.as_bytes();
        if bytes.len() > 3 && bytes[..3].iter().all(u8::is_ascii_digit)
            && (status[3..].starts_with(' ') || status[3..].starts_with('，') || status[3..].starts_with('：'))
        {
            return match bytes[0] {
                b'4' => Failure::HttpClient, b'5' => Failure::HttpServer,
                b'1'..=b'3' => Failure::HttpOther, _ => Failure::ModelOther,
            };
        }
    }
    if error.starts_with("请求失败：") || error == "模型列表请求失败，请检查接口地址、密钥或网络" {
        Failure::Transport
    } else {
        Failure::ModelOther
    }
}

#[derive(Clone, Copy, Debug)]
pub struct UiaFailure {
    stage: &'static str,
    hresult: u32,
}

/// 只读受控 UIA 错误封套中的第一个已知 stage/HRESULT 对；不保留 hint/窗口名。
/// 未知格式降级为 ErrorKind，不尝试泛化解析 Windows Display 文本。
fn uia_failure(tool: &str, error: &neo_tools::ToolError) -> Option<UiaFailure> {
    if !matches!(tool, "click" | "screen_elements" | "screen_element_search")
        || error.message.len() > MAX_INPUT_BYTES
        || !error.message.starts_with("UIA 即时核验拒绝输入：")
    { return None; }
    const STAGES: &[&str] = &[
        "ElementFromHandle", "window.GetRuntimeId", "ElementFromPoint", "CurrentIsPassword",
        "CurrentControlType", "CurrentName", "window.CurrentName", "CurrentBoundingRectangle",
        "hit.identity", "CurrentIsEnabled", "CurrentIsOffscreen", "CurrentIsKeyboardFocusable",
        "GetCurrentPattern", "GetCurrentPattern(Invoke)", "GetCurrentPattern(Toggle)",
        "GetCurrentPattern(SelectionItem)", "GetCurrentPattern(ExpandCollapse)",
        "passive_hit", "passive_hit.CurrentControlType", "passive_hit.CurrentIsKeyboardFocusable",
        "passive_hit.CurrentIsPassword", "RawViewWalker.GetParentElement", "CoInitializeEx",
        "CoCreateInstance", "RawViewWalker", "DwmGetWindowAttribute", "EnumDisplayMonitors", "CompareElements",
    ];
    for part in error.message.split("stage=").skip(1).take(16) {
        let Some((stage, rest)) = part.split_once(", HRESULT=0x") else { continue; };
        let Some(&stage) = STAGES.iter().find(|&&known| known == stage) else { continue; };
        let hex = rest.as_bytes().get(..8)?;
        if !hex.iter().all(u8::is_ascii_hexdigit)
            || !rest[8..].starts_with(')')
        { continue; }
        // 原 API 名含凭据标记，映射安全别名，避免纵深脱敏隐藏整条事件。
        let stage = match stage {
            "CurrentIsPassword" => "sensitive_field_check",
            "passive_hit.CurrentIsPassword" => "passive_hit.sensitive_field_check",
            _ => stage,
        };
        return Some(UiaFailure { stage, hresult: u32::from_str_radix(&rest[..8], 16).ok()? });
    }
    None
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Details {
    pub tool: Option<&'static str>,
    pub failure: Option<Failure>,
    pub background: Option<bool>,
    pub cancel_observed: Option<bool>,
    pub count: Option<usize>,
    pub uia: Option<UiaFailure>,
}

pub fn known_tool(name: &str) -> &'static str {
    neo_tools::find(name).map(|tool| tool.name).unwrap_or("unknown")
}

impl Details {
    pub fn tool(name: &str) -> Self {
        Self { tool: Some(known_tool(name)), ..Self::default() }
    }

    pub fn outcome(outcome: &neo_tools::Outcome) -> Self {
        let mut details = Self::tool(outcome.tool);
        if let Some(error) = &outcome.error {
            details.failure = Some(Failure::Tool(error.kind));
            details.uia = uia_failure(outcome.tool, error);
        }
        if outcome.is_ok() && matches!(outcome.tool, "powershell" | "bash") {
            // 只接受 JSON bool；不记录 PID、command、cwd、stdout 或任意其它 data。
            details.background = outcome.data.get("background").and_then(serde_json::Value::as_bool);
        }
        details
    }
}

impl Span {
    pub fn new(component: &'static str, parent: Option<u64>) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let component = match component {
            "task" | "tool" | "model" | "demo" | "model_list" | "compaction" => component,
            _ => "diagnostic",
        };
        let id = NEXT.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .expect("diagnostic id exhausted");
        Self { id, parent, component, started: Instant::now() }
    }

    fn message(self, phase: Phase, details: Details) -> String {
        let elapsed = self.started.elapsed().as_millis().min(u64::MAX as u128) as u64;
        let mut message = format!("event={} id={} elapsed_ms={elapsed}", phase.label(), self.id);
        if let Some(parent) = self.parent { let _ = write!(message, " parent={parent}"); }
        if let Some(tool) = details.tool { let _ = write!(message, " tool={}", known_tool(tool)); }
        if let Some(failure) = details.failure { let _ = write!(message, " kind={}", failure.label()); }
        if let Some(value) = details.background { let _ = write!(message, " background={value}"); }
        if let Some(value) = details.cancel_observed { let _ = write!(message, " cancel_observed={value}"); }
        if let Some(count) = details.count { let _ = write!(message, " count={count}"); }
        if let Some(uia) = details.uia { let _ = write!(message, " uia_stage={} hresult=0x{:08X}", uia.stage, uia.hresult); }
        message
    }

    pub fn event(self, phase: Phase, details: Details) {
        let level = if details.failure.is_some() || matches!(phase, Phase::Failed | Phase::Rejected) {
            Level::Warn
        } else { Level::Info };
        record(level, self.component, &self.message(phase, details));
    }
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
#[path = "diagnostics_tests.rs"]
mod tests;
