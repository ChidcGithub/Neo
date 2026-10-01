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
    pub tool_calls: String,
    pub tool_call_id: String,
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
        // SQLite 默认不强制外键（且不持久化、必须逐连接开启）——不开的话
        // schema 里的 REFERENCES ... ON DELETE CASCADE 是死代码，还能写进
        // 不属于任何会话的孤儿消息。
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        let has_attachments = {
            let mut stmt = conn.prepare("PRAGMA table_info(messages)")?;
            let columns = stmt
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<Result<Vec<_>>>()?;
            columns.iter().any(|name| name == "attachments")
        };
        if !has_attachments {
            // 双开同库时两个进程可能同时走到这里：一个是「列已存在」的错误，
            // 不是真失败 —— 另一个进程已经替我们迁移好了。
            if let Err(e) = conn.execute(
                "ALTER TABLE messages ADD COLUMN attachments TEXT NOT NULL DEFAULT '[]'",
                [],
            ) {
                if !e.to_string().contains("duplicate column") {
                    return Err(e);
                }
            }
        }
        for (name, default) in [("tool_calls", "[]"), ("tool_call_id", "")] {
            let columns = conn.prepare("PRAGMA table_info(messages)")?
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<Result<Vec<_>>>()?;
            if !columns.iter().any(|column| column == name) {
                if let Err(error) = conn.execute(&format!(
                    "ALTER TABLE messages ADD COLUMN {name} TEXT NOT NULL DEFAULT '{default}'"
                ), []) {
                    if !error.to_string().contains("duplicate column") { return Err(error); }
                }
            }
        }
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// 取连接。锁中毒（某次持锁期间 panic 过）不该让存储层永久瘫痪：
    /// 连接本身没有并发损坏的风险，取出继续用。
    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// 会话列表，最近更新的在前。
    pub fn sessions(&self) -> Result<Vec<SessionRow>> {
        let conn = self.conn();
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
        let conn = self.conn();
        conn.execute(
            "INSERT INTO sessions (title, created_ms, updated_ms) VALUES (?1, ?2, ?2)",
            params![title, now],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// 更新会话标题与 `updated_ms`。
    pub fn rename_session(&self, id: i64, title: &str) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "UPDATE sessions SET title = ?1, updated_ms = ?2 WHERE id = ?3",
            params![title, now(), id],
        )?;
        Ok(())
    }

    /// 仅刷新 `updated_ms`（用于把会话顶到列表最前）。
    pub fn touch_session(&self, id: i64) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "UPDATE sessions SET updated_ms = ?1 WHERE id = ?2",
            params![now(), id],
        )?;
        Ok(())
    }

    /// 删除会话及其全部消息。两条 DELETE 必须同事务：中途失败
    /// （磁盘满 / 进程崩溃）会留下"会话还在、消息全丢"的损坏态。
    pub fn delete_session(&self, id: i64) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM messages WHERE session_id = ?1", params![id])?;
        tx.execute("DELETE FROM sessions WHERE id = ?1", params![id])?;
        tx.commit()?;
        Ok(())
    }

    /// 某个会话的全部消息，按写入顺序。
    pub fn messages(&self, session_id: i64) -> Result<Vec<MessageRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare_cached(
            "SELECT role, content, reasoning, meta, attachments, tool_calls, tool_call_id FROM messages \
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
                    tool_calls: r.get(5)?,
                    tool_call_id: r.get(6)?,
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
        self.append_message_with_protocol(session_id, role, content, reasoning, meta, attachments, "[]", "")
    }

    /// 原文与工具协议同事务保存，避免重开后丢失配对结果。
    #[allow(clippy::too_many_arguments)]
    pub fn append_message_with_protocol(
        &self,
        session_id: i64,
        role: &str,
        content: &str,
        reasoning: &str,
        meta: &str,
        attachments: &str,
        tool_calls: &str,
        tool_call_id: &str,
    ) -> Result<i64> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let timestamp = now();
        tx.execute(
            "INSERT INTO messages (session_id, role, content, reasoning, meta, attachments, created_at, tool_calls, tool_call_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![session_id, role, content, reasoning, meta, attachments, timestamp, tool_calls, tool_call_id],
        )?;
        let message_id = tx.last_insert_rowid();
        tx.execute(
            "UPDATE sessions SET updated_ms = ?1 WHERE id = ?2",
            params![timestamp, session_id],
        )?;
        tx.commit()?;
        Ok(message_id)
    }

    /// 检查点只允许覆盖已持久化消息；原消息不被删除。
    pub fn save_checkpoint(&self, session: i64, covered: usize, data: &str) -> Result<()> {
        let conn = self.conn();
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM messages WHERE session_id=?1", [session], |r| r.get(0))?;
        if covered == 0 || covered > count as usize {
            return Err(rusqlite::Error::InvalidParameterName("checkpoint boundary".into()));
        }
        conn.execute("INSERT INTO context_checkpoints(session_id,covered,data) VALUES(?1,?2,?3) ON CONFLICT(session_id) DO UPDATE SET covered=excluded.covered,data=excluded.data", params![session, covered as i64, data])?;
        Ok(())
    }

    pub fn checkpoint(&self, session: i64) -> Result<Option<(usize, String)>> {
        let conn = self.conn();
        conn.query_row("SELECT covered,data FROM context_checkpoints WHERE session_id=?1 AND covered>0 AND covered<=(SELECT COUNT(*) FROM messages WHERE session_id=?1)", [session], |r| Ok((r.get::<_, i64>(0)? as usize, r.get(1)?))).optional()
    }

    /// 读取设置；不存在返回 `None`。
    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        let conn = self.conn();
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
        let conn = self.conn();
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
CREATE INDEX IF NOT EXISTS idx_sessions_updated ON sessions(updated_ms DESC);

CREATE TABLE IF NOT EXISTS context_checkpoints (
    session_id INTEGER PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
    covered INTEGER NOT NULL,
    data TEXT NOT NULL
);
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
#[path = "tests.rs"]
mod tests;
