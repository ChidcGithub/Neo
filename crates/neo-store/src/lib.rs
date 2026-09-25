//! # neo-store
//!
//! Neo 的本地持久化。三张表：
//!
//! | 表 | 存什么 |
//! |---|---|
//! | `sessions` | 会话（标题 + 时间戳） |
//! | `messages` | 消息（含思考过程 `reasoning`） |
//! | `settings` | 键值设置（主题 / 观看距离 / API 配置） |
//!
//! 设计取舍：
//!
//! - **单连接 + `Mutex`**。Neo 是单窗口应用，写入频率低（每条消息一次），
//!   不值得为它上连接池或异步驱动。所有写入都发生在主线程，
//!   LLM 流式线程只通过通道回报增量，由主线程落库。
//! - **错误不 panic**。教室场景下丢一条历史不该让整块屏幕黑掉，
//!   调用方拿到 `Result` 自行决定（通常是忽略并继续）。
//! - 数据库默认放 `%APPDATA%\Neo\neo.db`，可用环境变量 `NEO_HOME` 改到别处
//!   （一体机常见的"绿色部署"需求）。

use std::path::PathBuf;
use std::sync::Mutex;

use rusqlite::{params, Connection, OptionalExtension};

/// 一条会话（列表行）。
#[derive(Clone, Debug)]
pub struct SessionRow {
    pub id: i64,
    pub title: String,
    /// Unix 毫秒。用毫秒而不是秒：同秒内创建的两个会话顺序会不稳定。
    pub updated_ms: i64,
}

/// 一条消息。
#[derive(Clone, Debug)]
pub struct MessageRow {
    /// `user` 或 `assistant`。
    pub role: String,
    pub content: String,
    /// 模型的思考过程（DeepSeek-R1 的 `reasoning_content`），可能为空。
    pub reasoning: String,
    /// 展示用元信息（模型名 · 耗时）。
    pub meta: String,
    /// 附件 JSON；图片数据随附件保存，空附件为 `[]`。
    pub attachments: String,
}

pub type Result<T> = std::result::Result<T, rusqlite::Error>;

/// SQLite 存储。
pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    /// 用默认路径打开（`%APPDATA%\Neo\neo.db`，可被 `NEO_HOME` 覆盖）。
    pub fn open_default() -> Result<Self> {
        Self::open(&default_db_path())
    }

    /// 在指定路径打开，必要时建表。
    pub fn open(path: &std::path::Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let conn = Connection::open(path)?;
        // WAL 让"边聊边写"不阻塞读；synchronous=NORMAL 在 WAL 下是安全的折中。
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(SCHEMA)?;
        let has_attachments = {
            let mut stmt = conn.prepare("PRAGMA table_info(messages)")?;
            let columns = stmt
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<Result<Vec<_>>>()?;
            columns.iter().any(|name| name == "attachments")
        };
        if !has_attachments {
            conn.execute(
                "ALTER TABLE messages ADD COLUMN attachments TEXT NOT NULL DEFAULT '[]'",
                [],
            )?;
        }
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// 会话列表，最近更新的在前。
    pub fn sessions(&self) -> Result<Vec<SessionRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare_cached(
            "SELECT id, title, updated_ms FROM sessions ORDER BY updated_ms DESC, id DESC",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(SessionRow {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    updated_ms: r.get(2)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// 新建会话，返回 id。
    pub fn create_session(&self, title: &str) -> Result<i64> {
        let now = now();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO sessions (title, created_ms, updated_ms) VALUES (?1, ?2, ?2)",
            params![title, now],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// 更新会话标题与 `updated_ms`。
    pub fn rename_session(&self, id: i64, title: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE sessions SET title = ?1, updated_ms = ?2 WHERE id = ?3",
            params![title, now(), id],
        )?;
        Ok(())
    }

    /// 仅刷新 `updated_ms`（用于把会话顶到列表最前）。
    pub fn touch_session(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE sessions SET updated_ms = ?1 WHERE id = ?2",
            params![now(), id],
        )?;
        Ok(())
    }

    /// 删除会话及其全部消息。
    pub fn delete_session(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM messages WHERE session_id = ?1", params![id])?;
        conn.execute("DELETE FROM sessions WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// 某个会话的全部消息，按写入顺序。
    pub fn messages(&self, session_id: i64) -> Result<Vec<MessageRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare_cached(
            "SELECT role, content, reasoning, meta, attachments FROM messages \
             WHERE session_id = ?1 ORDER BY id ASC",
        )?;
        let rows = stmt
            .query_map(params![session_id], |r| {
                Ok(MessageRow {
                    role: r.get(0)?,
                    content: r.get(1)?,
                    reasoning: r.get(2)?,
                    meta: r.get(3)?,
                    attachments: r.get(4)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// 追加一条消息。
    pub fn append_message(
        &self,
        session_id: i64,
        role: &str,
        content: &str,
        reasoning: &str,
        meta: &str,
    ) -> Result<i64> {
        self.append_message_with_attachments(session_id, role, content, reasoning, meta, "[]")
    }

    /// 追加含附件的消息，与刷新会话时间一起提交。
    pub fn append_message_with_attachments(
        &self,
        session_id: i64,
        role: &str,
        content: &str,
        reasoning: &str,
        meta: &str,
        attachments: &str,
    ) -> Result<i64> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let timestamp = now();
        tx.execute(
            "INSERT INTO messages (session_id, role, content, reasoning, meta, attachments, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![session_id, role, content, reasoning, meta, attachments, timestamp],
        )?;
        let message_id = tx.last_insert_rowid();
        tx.execute(
            "UPDATE sessions SET updated_ms = ?1 WHERE id = ?2",
            params![timestamp, session_id],
        )?;
        tx.commit()?;
        Ok(message_id)
    }

    /// 读取设置；不存在返回 `None`。
    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let v = conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()?;
        Ok(v)
    }

    /// 写入设置（upsert）。
    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }
}

/// Unix 毫秒。
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS sessions (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    title      TEXT    NOT NULL DEFAULT '新对话',
    created_ms INTEGER NOT NULL,
    updated_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS messages (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    role       TEXT    NOT NULL,
    content    TEXT    NOT NULL,
    reasoning  TEXT    NOT NULL DEFAULT '',
    meta       TEXT    NOT NULL DEFAULT '',
    attachments TEXT   NOT NULL DEFAULT '[]',
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id, id);

CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
";

/// 默认数据库路径。
///
/// 优先级：`NEO_HOME` > `%APPDATA%\Neo` > 当前目录。
/// 最后一档是兜底（例如 AppData 不可写的受控环境），正常情况下不会走到。
pub fn default_db_path() -> PathBuf {
    if let Ok(home) = std::env::var("NEO_HOME") {
        if !home.trim().is_empty() {
            return PathBuf::from(home).join("neo.db");
        }
    }
    if let Ok(appdata) = std::env::var("APPDATA") {
        if !appdata.trim().is_empty() {
            return PathBuf::from(appdata).join("Neo").join("neo.db");
        }
    }
    PathBuf::from("neo.db")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(tag: &str) -> (Store, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("neo-store-test-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::open(&dir.join("neo.db")).expect("打开测试库");
        (store, dir)
    }

    #[test]
    fn session_roundtrip() {
        let (store, dir) = temp_store("roundtrip");
        let id = store.create_session("楞次定律").unwrap();
        store
            .append_message(id, "user", "讲讲楞次定律", "", "")
            .unwrap();
        store
            .append_message(
                id,
                "assistant",
                "好，我们分三步…",
                "先回忆…",
                "deepseek-chat · 1.2s",
            )
            .unwrap();

        let sessions = store.sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].title, "楞次定律");

        let msgs = store.messages(id).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[1].role, "assistant");
        assert_eq!(msgs[1].reasoning, "先回忆…");
        assert_eq!(msgs[0].attachments, "[]");
        assert_eq!(msgs[1].attachments, "[]");

        // 中文与换行不丢
        assert_eq!(msgs[0].content, "讲讲楞次定律");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn attachments_survive_reopen() {
        let (store, dir) = temp_store("attachments");
        let id = store.create_session("附件").unwrap();
        let attachments = r#"[{"name":"题目.png","kind":"image","data_url":"data:image/png;base64,aGVsbG8="},{"name":"笔记.txt","kind":"text","text":"中文\n第二行"}]"#;
        let message_id = store
            .append_message_with_attachments(id, "user", "解释附件", "思考", "元信息", attachments)
            .unwrap();
        assert!(message_id > 0);
        {
            let conn = store.conn.lock().unwrap();
            let (created_at, updated_ms): (i64, i64) = conn
                .query_row(
                    "SELECT messages.created_at, sessions.updated_ms FROM messages \
                     JOIN sessions ON messages.session_id = sessions.id WHERE messages.id = ?1",
                    params![message_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(created_at, updated_ms);
        }
        drop(store);

        let store = Store::open(&dir.join("neo.db")).unwrap();
        let messages = store.messages(id).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].content, "解释附件");
        assert_eq!(messages[0].reasoning, "思考");
        assert_eq!(messages[0].meta, "元信息");
        assert_eq!(messages[0].attachments, attachments);
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn old_database_migrates_attachments_once() {
        let dir = std::env::temp_dir().join(format!("neo-store-migration-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("neo.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE sessions (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    title TEXT NOT NULL DEFAULT '新对话',
                    created_ms INTEGER NOT NULL,
                    updated_ms INTEGER NOT NULL
                );
                CREATE TABLE messages (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                    role TEXT NOT NULL,
                    content TEXT NOT NULL,
                    reasoning TEXT NOT NULL DEFAULT '',
                    meta TEXT NOT NULL DEFAULT '',
                    created_at INTEGER NOT NULL
                );
                INSERT INTO sessions (title, created_ms, updated_ms) VALUES ('旧会话', 1, 2);
                INSERT INTO messages (session_id, role, content, reasoning, meta, created_at)
                VALUES (1, 'assistant', '旧消息', '旧思考', '旧元信息', 2);",
            )
            .unwrap();
        }
        let attachments = r#"[{"name":"附件.txt","text":"迁移后附件"}]"#;
        {
            let store = Store::open(&path).unwrap();
            let messages = store.messages(1).unwrap();
            assert_eq!(messages.len(), 1);
            assert_eq!(messages[0].content, "旧消息");
            assert_eq!(messages[0].reasoning, "旧思考");
            assert_eq!(messages[0].meta, "旧元信息");
            assert_eq!(messages[0].attachments, "[]");
            assert_eq!(store.sessions().unwrap()[0].title, "旧会话");
            store
                .append_message(1, "user", "旧接口仍可用", "", "")
                .unwrap();
            store
                .append_message_with_attachments(1, "user", "新接口", "", "", attachments)
                .unwrap();
        }
        {
            let store = Store::open(&path).unwrap();
            let messages = store.messages(1).unwrap();
            assert_eq!(messages.len(), 3);
            assert_eq!(messages[0].attachments, "[]");
            assert_eq!(messages[1].attachments, "[]");
            assert_eq!(messages[2].attachments, attachments);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failed_session_touch_rolls_back_message() {
        let (store, dir) = temp_store("rollback");
        let id = store.create_session("回滚").unwrap();
        {
            let conn = store.conn.lock().unwrap();
            conn.execute_batch(
                "UPDATE sessions SET updated_ms = 0;
                CREATE TRIGGER reject_session_touch BEFORE UPDATE OF updated_ms ON sessions
                BEGIN SELECT RAISE(FAIL, '拒绝刷新会话时间'); END;",
            )
            .unwrap();
        }
        assert!(store
            .append_message_with_attachments(id, "user", "失败消息", "", "", "[]")
            .is_err());
        assert!(store.messages(id).unwrap().is_empty());
        assert_eq!(store.sessions().unwrap()[0].updated_ms, 0);
        {
            let conn = store.conn.lock().unwrap();
            conn.execute_batch("DROP TRIGGER reject_session_touch;")
                .unwrap();
        }
        store
            .append_message(id, "user", "重试消息", "", "")
            .unwrap();
        let messages = store.messages(id).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "重试消息");
        assert!(store.sessions().unwrap()[0].updated_ms > 0);
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn ordering_is_most_recent_first() {
        let (store, dir) = temp_store("ordering");
        let a = store.create_session("甲").unwrap();
        // 毫秒精度下也留一点间隔，避免同毫秒内顺序由 id 决定。
        std::thread::sleep(std::time::Duration::from_millis(5));
        let b = store.create_session("乙").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        // 甲刚被使用过，应当排到最前。
        store.touch_session(a).unwrap();

        let titles: Vec<String> = store
            .sessions()
            .unwrap()
            .into_iter()
            .map(|s| s.title)
            .collect();
        assert_eq!(titles, vec!["甲".to_string(), "乙".to_string()]);
        let _ = b;
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn delete_cascades_messages() {
        let (store, dir) = temp_store("delete");
        let id = store.create_session("会话").unwrap();
        store.append_message(id, "user", "hi", "", "").unwrap();
        store.delete_session(id).unwrap();
        assert!(store.sessions().unwrap().is_empty());
        assert!(store.messages(id).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn settings_upsert() {
        let (store, dir) = temp_store("settings");
        assert_eq!(store.setting("api_key").unwrap(), None);
        store.set_setting("api_key", "sk-中文测试").unwrap();
        store.set_setting("api_key", "sk-second").unwrap();
        assert_eq!(
            store.setting("api_key").unwrap().as_deref(),
            Some("sk-second")
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn reopen_keeps_data() {
        let dir = std::env::temp_dir().join(format!("neo-store-reopen-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("neo.db");
        {
            let store = Store::open(&path).unwrap();
            let id = store.create_session("持久").unwrap();
            store.append_message(id, "user", "内容", "", "").unwrap();
        }
        {
            let store = Store::open(&path).unwrap();
            assert_eq!(store.sessions().unwrap().len(), 1);
            assert_eq!(store.messages(1).unwrap().len(), 1);
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}
