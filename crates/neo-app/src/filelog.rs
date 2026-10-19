//! 本地 SQLite 落盘日志：默认开启的详细落盘，后台线程批量写盘、有界通道、自动压缩、可开关。
//!
//! 与内存诊断共用一条摄入路径（`diagnostics::record`/`forward` 会同时投递到此
//! 处的通道）。所有文本在进入通道前已由 `diagnostics::sanitize` 脱敏；
//! 此模块只负责有界缓冲、批量事务写入与压缩，绝不调用日志宏以免递归。
//!
//! 每条事件单独一行（与内存视图的聚合不同），时间线完整；容量上限与时间保留
//! 双策略在后台线程中执行，`auto_vacuum=FULL` 让删除立即回收文件空间。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection};

pub const MAX_LINE_BYTES: usize = 4 * 1024;
pub const MAX_DB_BYTES: u64 = 32 * 1024 * 1024;
pub const RETENTION_MS: u64 = 7 * 24 * 60 * 60 * 1000;
const CHANNEL: usize = 2048;
const COMPACT_EVERY: u64 = 2_000;
const DELETE_BATCH: u64 = 4_096;
const COMPACT_PASSES: u32 = 512;
const FIELD_LIMIT: usize = 64;
const FIELD_BYTES: usize = 4 * 1024;
const AUTO_VACUUM_FULL: i64 = 1;
const DB_NAME: &str = "runtime.db";

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS logs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    wall_ms INTEGER NOT NULL,
    mono_ms INTEGER NOT NULL,
    level TEXT NOT NULL,
    component TEXT NOT NULL,
    thread TEXT NOT NULL,
    message TEXT NOT NULL,
    fields TEXT NOT NULL,
    file TEXT NOT NULL,
    line INTEGER NOT NULL,
    col INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS logs_wall_ms ON logs (wall_ms);
";

const INSERT: &str = "INSERT INTO logs (wall_ms, mono_ms, level, component, thread, message, fields, file, line, col) \
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)";

/// One already-sanitized record. Built from borrowed data so callers pay nothing
/// when the sink is disabled or not started.
pub struct Record<'a> {
    pub wall_ms: u64,
    pub mono_ms: u64,
    pub level: &'a str,
    pub component: &'a str,
    pub thread: &'a str,
    pub message: &'a str,
    pub fields: &'a [(Box<str>, Box<str>)],
    pub source: (&'a str, u32, u32),
}

struct Row {
    wall_ms: u64,
    mono_ms: u64,
    level: Box<str>,
    component: Box<str>,
    thread: Box<str>,
    message: Box<str>,
    fields: Box<[(Box<str>, Box<str>)]>,
    file: Box<str>,
    line: u32,
    col: u32,
}

impl Record<'_> {
    fn into_row(self) -> Row {
        Row {
            wall_ms: self.wall_ms,
            mono_ms: self.mono_ms,
            level: bound(self.level, MAX_LINE_BYTES),
            component: bound(self.component, MAX_LINE_BYTES),
            thread: bound(self.thread, MAX_LINE_BYTES),
            message: bound(self.message, MAX_LINE_BYTES),
            fields: self
                .fields
                .iter()
                .take(FIELD_LIMIT)
                .map(|(key, value)| (bound(key, FIELD_BYTES), bound(value, FIELD_BYTES)))
                .collect(),
            file: bound(self.source.0, MAX_LINE_BYTES),
            line: self.source.1,
            col: self.source.2,
        }
    }
}

struct Shared {
    enabled: AtomicBool,
    lines: AtomicU64,
    dropped: AtomicU64,
    bytes: AtomicU64,
    path: Mutex<Option<PathBuf>>,
}

static SHARED: OnceLock<Shared> = OnceLock::new();
static TX: Mutex<Option<mpsc::SyncSender<Row>>> = Mutex::new(None);
static WRITER: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);

fn shared() -> &'static Shared {
    SHARED.get_or_init(|| Shared {
        enabled: AtomicBool::new(true),
        lines: AtomicU64::new(0),
        dropped: AtomicU64::new(0),
        bytes: AtomicU64::new(0),
        path: Mutex::new(None),
    })
}

/// Starts the writer thread on `dir`. Idempotent; safe to call once at install.
pub fn start(dir: PathBuf) {
    if TX.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
        return;
    }
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join(DB_NAME);
    let (tx, rx) = mpsc::sync_channel::<Row>(CHANNEL);
    let writer_path = path.clone();
    let Ok(handle) = std::thread::Builder::new()
        .name("neo-filelog".into())
        .spawn(move || writer_loop(rx, writer_path))
    else {
        return;
    };
    *shared().path.lock().unwrap_or_else(|e| e.into_inner()) = Some(path);
    shared().bytes.store(0, Ordering::Relaxed);
    *TX.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
    *WRITER.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle);
}

fn writer_loop(rx: mpsc::Receiver<Row>, path: PathBuf) {
    let mut conn = match open(&path) {
        Ok(conn) => conn,
        Err(_) => {
            let lost = rx.iter().count() as u64;
            shared().dropped.fetch_add(lost, Ordering::Relaxed);
            return;
        }
    };
    shared().bytes.store(file_bytes(&path), Ordering::Relaxed);
    let mut since_compact = 0u64;
    while let Ok(row) = rx.recv() {
        let mut batch = vec![row];
        while let Ok(next) = rx.try_recv() {
            batch.push(next);
        }
        match flush(&mut conn, &batch) {
            Ok(count) => {
                shared().lines.fetch_add(count, Ordering::Relaxed);
                since_compact += count;
            }
            Err(_) => {
                shared().dropped.fetch_add(batch.len() as u64, Ordering::Relaxed);
            }
        }
        if since_compact >= COMPACT_EVERY {
            compact(&conn, &path, wall_now_ms());
            since_compact = 0;
        }
        shared().bytes.store(file_bytes(&path), Ordering::Relaxed);
    }
    compact(&conn, &path, wall_now_ms());
    shared().bytes.store(file_bytes(&path), Ordering::Relaxed);
}

fn open(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(Duration::from_secs(2))?;
    conn.execute_batch(SCHEMA)?;
    let vacuum: i64 = conn
        .query_row("PRAGMA auto_vacuum", [], |row| row.get(0))
        .unwrap_or(0);
    if vacuum != AUTO_VACUUM_FULL {
        conn.execute_batch("PRAGMA auto_vacuum=FULL;")?;
        conn.execute_batch("VACUUM;")?;
    }
    Ok(conn)
}

fn flush(conn: &mut Connection, batch: &[Row]) -> rusqlite::Result<u64> {
    let tx = conn.transaction()?;
    {
        let mut stmt = tx.prepare(INSERT)?;
        for row in batch {
            let fields = serde_json::to_string(&row.fields).unwrap_or_else(|_| "[]".into());
            stmt.execute(params![
                i64::try_from(row.wall_ms).unwrap_or(i64::MAX),
                i64::try_from(row.mono_ms).unwrap_or(i64::MAX),
                &*row.level,
                &*row.component,
                &*row.thread,
                &*row.message,
                fields,
                &*row.file,
                i64::from(row.line),
                i64::from(row.col),
            ])?;
        }
    }
    tx.commit()?;
    Ok(batch.len() as u64)
}

fn compact(conn: &Connection, path: &Path, now_ms: u64) {
    compact_within(conn, path, now_ms, RETENTION_MS, MAX_DB_BYTES, DELETE_BATCH);
}

fn compact_within(
    conn: &Connection,
    path: &Path,
    now_ms: u64,
    retention_ms: u64,
    max_bytes: u64,
    batch: u64,
) {
    let cutoff = i64::try_from(now_ms.saturating_sub(retention_ms)).unwrap_or(i64::MAX);
    let _ = conn.execute("DELETE FROM logs WHERE wall_ms < ?1", params![cutoff]);
    let batch = i64::try_from(batch).unwrap_or(i64::MAX);
    let mut passes = 0;
    while file_bytes(path) > max_bytes && passes < COMPACT_PASSES {
        passes += 1;
        let removed = conn
            .execute(
                "DELETE FROM logs WHERE id IN (SELECT id FROM logs ORDER BY id ASC LIMIT ?1)",
                params![batch],
            )
            .unwrap_or(0);
        if removed == 0 {
            break;
        }
    }
}

fn file_bytes(path: &Path) -> u64 {
    fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
}

fn wall_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis().min(u64::MAX as u128) as u64)
        .unwrap_or(0)
}

pub fn set_enabled(enabled: bool) {
    shared().enabled.store(enabled, Ordering::Relaxed);
}

pub fn is_enabled() -> bool {
    shared().enabled.load(Ordering::Relaxed)
}

/// Submits one already-sanitized record. Never blocks the caller.
pub fn submit(record: Record<'_>) {
    if !is_enabled() {
        return;
    }
    let tx = TX.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let Some(tx) = tx else { return; };
    if tx.try_send(record.into_row()).is_err() {
        shared().dropped.fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub lines: u64,
    pub dropped: u64,
    pub bytes: u64,
    pub enabled: bool,
}

pub fn stats() -> Stats {
    let shared = shared();
    Stats {
        lines: shared.lines.load(Ordering::Relaxed),
        dropped: shared.dropped.load(Ordering::Relaxed),
        bytes: shared.bytes.load(Ordering::Relaxed),
        enabled: shared.enabled.load(Ordering::Relaxed),
    }
}

pub fn path() -> Option<PathBuf> {
    shared()
        .path
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// Stops the writer thread and flushes. Safe to call multiple times.
pub fn shutdown() {
    let tx = TX.lock().unwrap_or_else(|e| e.into_inner()).take();
    drop(tx);
    if let Some(handle) = WRITER
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
    {
        let _ = handle.join();
    }
}

/// Collapses CR/LF and enforces `limit` in bytes, marking the cut with `[截断]`.
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

/// Single-line, byte-bounded ownership of one sanitized field.
fn bound(text: &str, limit: usize) -> Box<str> {
    if !text.contains(['\r', '\n']) && text.len() <= limit {
        return text.into();
    }
    let flat = text.replace(['\r', '\n'], " ");
    if flat.len() <= limit {
        return flat.into_boxed_str();
    }
    clip(&flat, limit).into_boxed_str()
}

/// UTC wall-clock rendering from epoch milliseconds (local display only).
pub fn utc_millis(ms: u64) -> String {
    let secs = ms / 1000;
    let millis = ms % 1000;
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (year, month, day) = civil_from_days(days as i64);
    let hour = rem / 3600;
    let minute = (rem % 3600) / 60;
    let second = rem % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

/// Howard Hinnant's civil_from_days: days since 1970-01-01 to (year, month, day).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
#[path = "filelog_tests.rs"]
mod tests;
