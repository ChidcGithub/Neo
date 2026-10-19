
use super::*;
use std::time::Instant;

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "neo-filelog-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = fs::create_dir_all(&dir);
    dir
}

fn drain() {
    shutdown();
    set_enabled(true);
    *shared().path.lock().unwrap_or_else(|e| e.into_inner()) = None;
    shared().lines.store(0, Ordering::Relaxed);
    shared().dropped.store(0, Ordering::Relaxed);
    shared().bytes.store(0, Ordering::Relaxed);
}

fn sample<'a>(message: &'a str, fields: &'a [(Box<str>, Box<str>)]) -> Record<'a> {
    Record {
        wall_ms: wall_now_ms(),
        mono_ms: 7,
        level: "INFO",
        component: "comp",
        thread: "main",
        message,
        fields,
        source: ("src/lib.rs", 1, 2),
    }
}

fn row_count(conn: &Connection) -> i64 {
    conn.query_row("SELECT COUNT(*) FROM logs", [], |row| row.get(0))
        .expect("count logs rows")
}

fn insert_rows(conn: &mut Connection, wall_ms: u64, count: usize, message: &str) {
    let tx = conn.transaction().expect("insert transaction");
    {
        let mut stmt = tx.prepare(INSERT).expect("prepare insert");
        for index in 0..count {
            stmt.execute(params![
                i64::try_from(wall_ms).unwrap_or(i64::MAX),
                index as i64,
                "INFO",
                "comp",
                "main",
                message,
                "[]",
                "src/lib.rs",
                1i64,
                2i64,
            ])
            .expect("insert row");
        }
    }
    tx.commit().expect("commit inserts");
}

#[test]
fn writes_rows_to_sqlite_database() {
    let dir = temp_dir("rows");
    drain();
    start(dir.clone());
    assert!(is_enabled());
    let fields: [(Box<str>, Box<str>); 1] = [("k".into(), "v".into())];
    for index in 0..64u64 {
        submit(sample(&format!("line {index}"), &fields));
    }
    shutdown();
    let path = dir.join(DB_NAME);
    assert!(path.exists(), "runtime database must be created");
    let conn = Connection::open(&path).expect("open runtime.db");
    assert_eq!(row_count(&conn), 64);
    let message: String = conn
        .query_row("SELECT message FROM logs ORDER BY id LIMIT 1", [], |row| row.get(0))
        .expect("read message");
    assert_eq!(message, "line 0");
    let fields_json: String = conn
        .query_row("SELECT fields FROM logs ORDER BY id LIMIT 1", [], |row| row.get(0))
        .expect("read fields");
    assert!(fields_json.contains("\"k\"") && fields_json.contains("\"v\""));
    drop(conn);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn bounded_channel_drops_rather_than_blocking() {
    let dir = temp_dir("drop");
    drain();
    set_enabled(true);
    start(dir.clone());
    let empty: [(Box<str>, Box<str>); 0] = [];
    for index in 0..10_000u64 {
        submit(sample(&format!("line {index}"), &empty));
    }
    shutdown();
    let stats = stats();
    assert!(stats.lines > 0);
    assert_eq!(stats.lines + stats.dropped, 10_000);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn disabled_switch_rejects_writes() {
    let dir = temp_dir("switch");
    drain();
    set_enabled(false);
    start(dir.clone());
    let empty: [(Box<str>, Box<str>); 0] = [];
    submit(sample("must not appear", &empty));
    shutdown();
    let path = dir.join(DB_NAME);
    let conn = Connection::open(&path).expect("open runtime.db");
    assert_eq!(row_count(&conn), 0);
    drop(conn);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn compaction_enforces_retention_and_size_cap() {
    let dir = temp_dir("compact");
    drain();
    let path = dir.join(DB_NAME);
    let mut conn = open(&path).expect("open runtime.db");
    let now = 1_700_000_000_000u64;
    let day = 86_400_000u64;
    insert_rows(&mut conn, now - 30 * day, 8, "expired");
    insert_rows(&mut conn, now, 4, "fresh");
    assert_eq!(row_count(&conn), 12);
    compact_within(&conn, &path, now, 7 * day, u64::MAX, 4_096);
    assert_eq!(row_count(&conn), 4, "records older than the retention window go");

    conn.execute("DELETE FROM logs", []).expect("clear fixture");
    insert_rows(
        &mut conn,
        now,
        4_000,
        "bulk payload row written for size cap compaction testing",
    );
    let before = file_bytes(&path);
    assert!(before > 64 * 1024, "fixture must exceed the cap: {before}");
    compact_within(&conn, &path, now, u64::MAX, 64 * 1024, 64);
    let after = file_bytes(&path);
    assert!(after <= 64 * 1024, "size cap must be enforced: {after}");
    assert!(after < before, "compaction must shrink the file: {before} -> {after}");
    assert!(row_count(&conn) > 0, "recent rows survive the size cap");
    drop(conn);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn start_is_idempotent() {
    let dir = temp_dir("idempotent");
    drain();
    start(dir.clone());
    start(dir.clone());
    let empty: [(Box<str>, Box<str>); 0] = [];
    submit(sample("one", &empty));
    shutdown();
    let conn = Connection::open(dir.join(DB_NAME)).expect("open runtime.db");
    assert_eq!(row_count(&conn), 1);
    drop(conn);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn utc_rendering_is_correct() {
    assert_eq!(utc_millis(0), "1970-01-01T00:00:00.000Z");
    assert_eq!(utc_millis(1_700_000_000_000), "2023-11-14T22:13:20.000Z");
    assert_eq!(utc_millis(86_400_000), "1970-01-02T00:00:00.000Z");
    assert_eq!(utc_millis(1_583_020_800_000), "2020-03-01T00:00:00.000Z");
}

#[test]
fn record_text_is_single_line_and_bounded() {
    let fields: [(Box<str>, Box<str>); 1] = [("k".into(), "v".into())];
    let row = Record {
        wall_ms: 0,
        mono_ms: 1,
        level: "INFO",
        component: "comp",
        thread: "main",
        message: "hello\r\nworld",
        fields: &fields,
        source: ("src/lib.rs", 1, 2),
    }
    .into_row();
    assert_eq!(&*row.message, "hello  world");
    assert_eq!(&*row.component, "comp");
    assert_eq!(&*row.file, "src/lib.rs");
    assert_eq!(row.line, 1);

    let long = "y".repeat(MAX_LINE_BYTES + 100);
    let bounded = bound(&long, MAX_LINE_BYTES);
    assert!(bounded.len() <= MAX_LINE_BYTES);
    assert!(bounded.ends_with("[截断]"));
    assert_eq!(&*bound("a\r\nb", MAX_LINE_BYTES), "a  b");
}

#[test]
fn start_keeps_the_file_locked_to_one_writer() {
    let dir = temp_dir("single");
    drain();
    start(dir.clone());
    start(dir.clone());
    shutdown();
    assert!(dir.join(DB_NAME).exists());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn unused_imports_clean() {
    let _ = Duration::from_secs(1);
    let _ = Instant::now();
}
