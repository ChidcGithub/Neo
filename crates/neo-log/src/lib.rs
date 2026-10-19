//! 进程内日志门面：统一的级别/记录/全局 sink 注册与宏，供各子项目低成本接入。
//!
//! 零依赖、不落盘、不发送网络。调用方只传固定组件名和安全概括（错误类别、
//! 计数、耗时），不能传工具参数、命令、正文、路径、原始错误或密钥。脱敏由
//! sink 侧（neo-app 诊断系统）统一执行；本门面只做长度裁剪与单行化。

use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub const MAX_COMPONENT: usize = 64;
pub const MAX_MESSAGE: usize = 8 * 1024;
pub const MAX_FIELDS: usize = 16;
pub const MAX_FIELD_KEY: usize = 32;
pub const MAX_FIELD_VALUE: usize = 256;
pub const MAX_THREAD: usize = 32;
const PRE_INIT_CAP: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Level {
    Trace = 0,
    Debug = 1,
    Info = 2,
    Warn = 3,
    Error = 4,
}

impl Level {
    pub const fn rank(self) -> u8 {
        self as u8
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Trace => "TRACE",
            Self::Debug => "DEBUG",
            Self::Info => "INFO",
            Self::Warn => "WARN",
            Self::Error => "ERROR",
        }
    }
}

/// Compile-time source only; the absolute build path is trimmed to a crate-relative suffix.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Location {
    pub file: &'static str,
    pub line: u32,
    pub column: u32,
}

impl Location {
    fn from_panic(location: &'static std::panic::Location<'static>) -> Self {
        Self {
            file: trim_file(location.file()),
            line: location.line(),
            column: location.column(),
        }
    }
}

fn trim_file(file: &str) -> &str {
    let start = file
        .find("crates/")
        .or_else(|| file.find("crates\\"))
        .or_else(|| file.find("src/"))
        .or_else(|| file.find("src\\"))
        .unwrap_or_else(|| file.rfind(['/', '\\']).map_or(0, |i| i + 1));
    &file[start..]
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    pub key: Box<str>,
    pub value: Box<str>,
}

#[derive(Clone, Debug)]
pub struct Record {
    pub level: Level,
    pub component: Box<str>,
    pub message: Box<str>,
    pub fields: Box<[Field]>,
    pub thread: Box<str>,
    /// Wall-clock milliseconds since the Unix epoch; local display/export only.
    pub wall_ms: u64,
    /// Process-relative milliseconds; the shared time base across all crates.
    pub mono_ms: u64,
    pub location: Location,
}

pub type Sink = Arc<dyn Fn(&Record) + Send + Sync>;

static SINK: RwLock<Option<Sink>> = RwLock::new(None);
static PRE_INIT: Mutex<VecDeque<Record>> = Mutex::new(VecDeque::new());
static MAX_LEVEL: AtomicU8 = AtomicU8::new(0);

static RECEIVED: AtomicU64 = AtomicU64::new(0);
static FORWARDED: AtomicU64 = AtomicU64::new(0);
static DROPPED_FILTERED: AtomicU64 = AtomicU64::new(0);
static DROPPED_UNSINKED: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub received: u64,
    pub forwarded: u64,
    pub dropped_filtered: u64,
    pub dropped_unsinked: u64,
    pub pre_init: u64,
}

pub fn stats() -> Stats {
    Stats {
        received: RECEIVED.load(Ordering::Relaxed),
        forwarded: FORWARDED.load(Ordering::Relaxed),
        dropped_filtered: DROPPED_FILTERED.load(Ordering::Relaxed),
        dropped_unsinked: DROPPED_UNSINKED.load(Ordering::Relaxed),
        pre_init: PRE_INIT.lock().map_or(0, |q| q.len() as u64),
    }
}

pub fn set_max_level(level: Level) {
    MAX_LEVEL.store(level.rank(), Ordering::Release);
}

pub fn max_level() -> Level {
    match MAX_LEVEL.load(Ordering::Acquire) {
        0 => Level::Trace,
        1 => Level::Debug,
        2 => Level::Info,
        3 => Level::Warn,
        _ => Level::Error,
    }
}

pub fn enabled(level: Level) -> bool {
    level.rank() >= MAX_LEVEL.load(Ordering::Acquire)
}

/// Installs the sink and flushes any records captured before initialization.
pub fn set_sink(sink: impl Fn(&Record) + Send + Sync + 'static) {
    let arc: Sink = Arc::new(sink);
    *SINK.write().unwrap_or_else(|e| e.into_inner()) = Some(arc.clone());
    let pending: Vec<Record> = PRE_INIT
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .drain(..)
        .collect();
    for record in pending {
        arc(&record);
        FORWARDED.fetch_add(1, Ordering::Relaxed);
    }
}

/// Removes the sink. Later records go back to the bounded pre-init ring.
pub fn take_sink() -> Option<Sink> {
    SINK.write().unwrap_or_else(|e| e.into_inner()).take()
}

#[track_caller]
pub fn emit(level: Level, component: &str, message: String, fields: &[(&str, String)]) {
    RECEIVED.fetch_add(1, Ordering::Relaxed);
    if !enabled(level) {
        DROPPED_FILTERED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    let record = Record {
        level,
        component: clip(component, MAX_COMPONENT).into_boxed_str(),
        message: clip(&message, MAX_MESSAGE).into_boxed_str(),
        fields: build_fields(fields),
        thread: thread_name(),
        wall_ms: wall_now(),
        mono_ms: mono_now(),
        location: Location::from_panic(std::panic::Location::caller()),
    };
    forward(record);
}

fn forward(record: Record) {
    let sink = SINK.read().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some(sink) = sink {
        sink(&record);
        FORWARDED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    let mut queue = PRE_INIT.lock().unwrap_or_else(|e| e.into_inner());
    if queue.len() >= PRE_INIT_CAP {
        queue.pop_front();
        DROPPED_UNSINKED.fetch_add(1, Ordering::Relaxed);
    }
    queue.push_back(record);
}

fn build_fields(fields: &[(&str, String)]) -> Box<[Field]> {
    fields
        .iter()
        .take(MAX_FIELDS)
        .map(|(key, value)| Field {
            key: clip(key, MAX_FIELD_KEY).into_boxed_str(),
            value: clip(value, MAX_FIELD_VALUE).into_boxed_str(),
        })
        .collect()
}

fn process_start() -> Instant {
    static START: OnceLock<Instant> = OnceLock::new();
    *START.get_or_init(Instant::now)
}

fn mono_now() -> u64 {
    process_start().elapsed().as_millis().min(u64::MAX as u128) as u64
}

fn wall_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().min(u64::MAX as u128) as u64)
        .unwrap_or(0)
}

fn thread_name() -> Box<str> {
    std::thread::current()
        .name()
        .unwrap_or("<unnamed>")
        .chars()
        .take(MAX_THREAD)
        .collect::<String>()
        .into_boxed_str()
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

#[macro_export]
macro_rules! info {
    ($component:expr, $($rest:tt)*) => {
        $crate::__log!($crate::Level::Info, $component, $($rest)*)
    };
}

#[macro_export]
macro_rules! warn {
    ($component:expr, $($rest:tt)*) => {
        $crate::__log!($crate::Level::Warn, $component, $($rest)*)
    };
}

#[macro_export]
macro_rules! error {
    ($component:expr, $($rest:tt)*) => {
        $crate::__log!($crate::Level::Error, $component, $($rest)*)
    };
}

#[macro_export]
macro_rules! debug {
    ($component:expr, $($rest:tt)*) => {
        $crate::__log!($crate::Level::Debug, $component, $($rest)*)
    };
}

#[macro_export]
macro_rules! trace {
    ($component:expr, $($rest:tt)*) => {
        $crate::__log!($crate::Level::Trace, $component, $($rest)*)
    };
}

#[macro_export]
macro_rules! __log {
    ($level:expr, $component:expr, $msg:literal $(, $args:expr)* ; $($key:literal => $value:expr),+ $(,)?) => {
        if $crate::enabled($level) {
            $crate::emit(
                $level,
                $component,
                ::std::format!($msg $(, $args)*),
                &[$(($key, ::std::format!("{}", $value))),+],
            );
        }
    };
    ($level:expr, $component:expr, $msg:literal $(, $args:expr)*) => {
        if $crate::enabled($level) {
            $crate::emit($level, $component, ::std::format!($msg $(, $args)*), &[]);
        }
    };
}

thread_local! {
    static TEST_SINK: RefCell<Option<Vec<Record>>> = const { RefCell::new(None) };
}

/// Test-only capture: routes records on this thread into a local buffer.
pub fn capture_for_test(run: impl FnOnce()) -> Vec<Record> {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            TEST_SINK.with(|slot| *slot.borrow_mut() = None);
        }
    }
    TEST_SINK.with(|slot| {
        assert!(slot.borrow().is_none());
        *slot.borrow_mut() = Some(Vec::new());
    });
    set_sink(|record| {
        TEST_SINK.with(|slot| {
            if let Some(buffer) = slot.borrow_mut().as_mut() {
                buffer.push(record.clone());
            }
        });
    });
    let _reset = Reset;
    run();
    take_sink();
    TEST_SINK.with(|slot| slot.borrow_mut().take().unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_max_level_is_most_verbose() {
        assert_eq!(max_level(), Level::Trace);
        assert!(enabled(Level::Trace));
        assert!(enabled(Level::Error));
    }

    #[test]
    fn max_level_filters_lower_severities() {
        set_max_level(Level::Info);
        assert!(!enabled(Level::Trace));
        assert!(!enabled(Level::Debug));
        assert!(enabled(Level::Info));
        assert!(enabled(Level::Error));
        set_max_level(Level::Trace);
    }

    #[test]
    fn records_carry_level_component_and_thread() {
        let records = capture_for_test(|| {
            info!("neo-test", "stream finished");
        });
        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert_eq!(record.level, Level::Info);
        assert_eq!(&*record.component, "neo-test");
        assert_eq!(&*record.message, "stream finished");
        assert!(record.fields.is_empty());
        assert!(record.mono_ms <= record.wall_ms);
    }

    #[test]
    fn format_args_are_rendered() {
        let records = capture_for_test(|| {
            warn!("neo-test", "attempt {} of {}", 2, 3; "model" => "neo");
        });
        assert_eq!(records.len(), 1);
        assert_eq!(&*records[0].message, "attempt 2 of 3");
        assert_eq!(records[0].fields.len(), 1);
        assert_eq!(&*records[0].fields[0].key, "model");
        assert_eq!(&*records[0].fields[0].value, "neo");
    }

    #[test]
    fn location_is_crate_relative() {
        let records = capture_for_test(|| {
            info!("neo-test", "loc");
        });
        let file = records[0].location.file;
        assert!(file.contains("neo-log") || file.contains("src/"), "{file}");
        assert!(records[0].location.line > 0);
    }

    #[test]
    fn oversized_message_is_clipped() {
        let long = "x".repeat(MAX_MESSAGE + 100);
        let records = capture_for_test(|| {
            info!("neo-test", "{}", long);
        });
        assert!(records[0].message.len() <= MAX_MESSAGE + "[截断]".len());
        assert!(records[0].message.ends_with("[截断]"));
    }

    #[test]
    fn fields_are_bounded_in_count() {
        let records = capture_for_test(|| {
            debug!("neo-test", "many"; "a" => 1, "b" => 2, "c" => 3);
        });
        assert!(records[0].fields.len() <= MAX_FIELDS);
        assert_eq!(records[0].fields.len(), 3);
    }

    #[test]
    fn trace_level_is_available() {
        let records = capture_for_test(|| {
            trace!("neo-test", "fine-grained");
        });
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].level, Level::Trace);
    }

    #[test]
    fn error_level_is_available() {
        let records = capture_for_test(|| {
            error!("neo-test", "failed");
        });
        assert_eq!(records[0].level, Level::Error);
    }

    #[test]
    fn unsunk_records_are_counted_not_stored_forever() {
        take_sink();
        let before = stats();
        info!("neo-test", "unsunk");
        let after = stats();
        assert!(after.received > before.received);
        assert!(after.forwarded <= after.received);
    }
}
