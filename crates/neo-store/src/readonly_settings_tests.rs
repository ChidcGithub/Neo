use super::*;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("neo-readonly-{}-{id}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn path(&self) -> PathBuf {
        self.0.join("设置 # % database.db")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn readonly_settings_reads_legacy_schema_without_writes_or_migrations() {
    let fixture = Fixture::new();
    let path = fixture.path();
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE settings(key TEXT PRIMARY KEY, value TEXT NOT NULL);
        INSERT INTO settings VALUES('language','en-US'),('classroom_safe','0');",
    )
    .unwrap();
    drop(conn);
    let before = std::fs::read(&path).unwrap();
    let settings = ReadOnlySettings::open(&path).unwrap();
    assert_eq!(
        settings.setting("language").unwrap().as_deref(),
        Some("en-US")
    );
    assert_eq!(
        settings.setting("classroom_safe").unwrap().as_deref(),
        Some("0")
    );
    assert_eq!(settings.setting("missing").unwrap(), None);
    assert_eq!(settings.setting("language' OR 1=1 --").unwrap(), None);
    assert!(settings
        .conn
        .execute("UPDATE settings SET value='zh-CN'", [])
        .is_err());
    let journal: String = settings
        .conn
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(journal, "delete");
    let tables: Vec<String> = settings
        .conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table'")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(tables, ["settings"]);
    drop(settings);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(std::fs::read_dir(&fixture.0).unwrap().count(), 1);
}

#[test]
fn readonly_settings_missing_table_defaults_without_schema_changes() {
    let fixture = Fixture::new();
    let path = fixture.path();
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TABLE unrelated(value TEXT);")
        .unwrap();
    drop(conn);
    let before = std::fs::read(&path).unwrap();
    let settings = ReadOnlySettings::open(&path).unwrap();
    for key in ["language", "classroom_safe", "silent_startup_errors"] {
        assert_eq!(settings.setting(key).unwrap(), None);
    }
    drop(settings);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(std::fs::read_dir(&fixture.0).unwrap().count(), 1);
}

#[test]
fn readonly_settings_never_creates_missing_database_or_parent() {
    let fixture = Fixture::new();
    let path = fixture.0.join("missing").join("neo.db");
    assert!(ReadOnlySettings::open(&path).is_err());
    assert!(!path.parent().unwrap().exists());
    assert!(ReadOnlySettings::open(&fixture.path()).is_err());
    assert!(!fixture.path().exists());
    std::fs::write(fixture.path(), b"not sqlite").unwrap();
    assert!(ReadOnlySettings::open(&fixture.path()).is_err());
    assert_eq!(std::fs::read(fixture.path()).unwrap(), b"not sqlite");
}

#[test]
fn readonly_settings_reads_current_wal_database() {
    let fixture = Fixture::new();
    let store = Store::open(&fixture.path()).unwrap();
    store.set_setting("language", "en-US").unwrap();
    let settings = ReadOnlySettings::open(&fixture.path()).unwrap();
    assert_eq!(
        settings.setting("language").unwrap().as_deref(),
        Some("en-US")
    );
    assert!(settings
        .conn
        .execute_batch("CREATE TABLE forbidden(value TEXT)")
        .is_err());
}
