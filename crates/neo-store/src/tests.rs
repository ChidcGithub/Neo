use super::*;

fn temp_store(tag: &str) -> (Store, PathBuf) {
    let dir =
        std::env::temp_dir().join(format!("neo-store-test-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = Store::open(&dir.join("neo.db")).expect("打开测试库");
    (store, dir)
}

#[test]
fn checkpoint_boundary_reopen_and_delete() {
    let (store, dir) = temp_store("checkpoint");
    let id = store.create_session("摘要").unwrap();
    assert!(store.save_checkpoint(id, 1, "summary").is_err());
    store.append_message(id, "user", "原文", "", "").unwrap();
    assert!(store.save_checkpoint(id, 0, "summary").is_err());
    store.save_checkpoint(id, 1, "summary").unwrap();
    drop(store);
    let store = Store::open(&dir.join("neo.db")).unwrap();
    assert_eq!(store.checkpoint(id).unwrap(), Some((1, "summary".into())));
    assert_eq!(store.messages(id).unwrap()[0].content, "原文");
    store.delete_session(id).unwrap();
    assert!(store.checkpoint(id).unwrap().is_none());
}

#[test]
fn checkpoint_boundary_uses_session_position_and_failed_update_keeps_previous() {
    let (store, dir) = temp_store("checkpoint-position");
    let id = store.create_session("摘要会话").unwrap();
    let other = store.create_session("其他会话").unwrap();
    store.append_message(other, "user", "其他记录", "", "").unwrap();
    store.append_message(id, "user", "首条原意", "", "").unwrap();
    store.append_message(other, "assistant", "交错记录", "", "").unwrap();
    store.append_message(id, "assistant", "历史回答", "", "").unwrap();
    store.save_checkpoint(id, 2, "previous").unwrap();
    assert!(store.save_checkpoint(id, 3, "invalid").is_err());
    assert_eq!(store.checkpoint(id).unwrap(), Some((2, "previous".into())));
    store.append_message(id, "user", "最近意图", "", "").unwrap();
    store.conn().execute_batch("CREATE TRIGGER reject_checkpoint BEFORE UPDATE ON context_checkpoints BEGIN SELECT RAISE(ABORT, 'test'); END;").unwrap();
    assert!(store.save_checkpoint(id, 3, "failed").is_err());
    drop(store);
    let store = Store::open(&dir.join("neo.db")).unwrap();
    assert_eq!(store.checkpoint(id).unwrap(), Some((2, "previous".into())));
    let rows = store.messages(id).unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].content, "首条原意");
    assert_eq!(rows[2].content, "最近意图");
    assert!(store.checkpoint(other).unwrap().is_none());
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
fn tool_protocol_survives_reopen() {
    let (store, dir) = temp_store("tool-protocol");
    let id = store.create_session("工具协议").unwrap();
    let calls = r#"[{"id":"call-1","name":"read_file","arguments":"{ \"path\": \"a.txt\" }"}]"#;
    store.append_message_with_protocol(id, "assistant", "读取", "思考", "", "[]", calls, "").unwrap();
    store.append_message_with_protocol(id, "tool", r#"{"ok":true,"data":{"content":"原文"}}"#, "", "read_file", "[]", "[]", "call-1").unwrap();
    drop(store);
    let store = Store::open(&dir.join("neo.db")).unwrap();
    let rows = store.messages(id).unwrap();
    assert_eq!(rows[0].tool_calls, calls);
    assert_eq!(rows[1].tool_call_id, "call-1");
    assert!(rows[1].content.contains("原文"));
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
fn failed_delete_and_readonly_write_preserve_existing_data() {
    let (store, dir) = temp_store("delete-rollback");
    let id = store.create_session("保留").unwrap();
    store.append_message(id, "user", "原消息", "", "").unwrap();
    store.conn().execute_batch(
        "CREATE TRIGGER reject_delete BEFORE DELETE ON sessions
             BEGIN SELECT RAISE(FAIL, 'synthetic failure'); END;"
    ).unwrap();
    assert!(store.delete_session(id).is_err());
    assert_eq!(store.messages(id).unwrap()[0].content, "原消息");
    assert_eq!(store.sessions().unwrap().len(), 1);
    store.conn().execute_batch("DROP TRIGGER reject_delete; PRAGMA query_only=ON;").unwrap();
    assert!(store.append_message(id, "user", "失败", "", "").is_err());
    assert_eq!(store.messages(id).unwrap().len(), 1);
    store.conn().execute_batch("PRAGMA query_only=OFF;").unwrap();
    store.append_message(id, "user", "重试", "", "").unwrap();
    assert_eq!(store.messages(id).unwrap().len(), 2);
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn concurrent_connections_keep_all_messages_and_busy_failure_is_retryable() {
    let (store, dir) = temp_store("concurrent");
    let id = store.create_session("并发").unwrap();
    let other = Store::open(&dir.join("neo.db")).unwrap();
    std::thread::scope(|s| {
        for store in [&store, &other] {
            s.spawn(move || {
                for i in 0..20 {
                    store.append_message(id, "user", &format!("记录{i}"), "", "").unwrap();
                }
            });
        }
    });
    assert_eq!(store.messages(id).unwrap().len(), 40);
    other.conn().busy_timeout(std::time::Duration::ZERO).unwrap();
    store.conn().execute_batch("BEGIN IMMEDIATE;").unwrap();
    assert!(other.append_message(id, "user", "被锁定", "", "").is_err());
    store.conn().execute_batch("ROLLBACK;").unwrap();
    assert_eq!(store.messages(id).unwrap().len(), 40);
    other.append_message(id, "user", "重试", "", "").unwrap();
    assert_eq!(store.messages(id).unwrap().len(), 41);
    drop(other);
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn foreign_keys_are_enforced() {
    let (store, dir) = temp_store("fk");
    // 外键真的开着：给不存在的会话写消息必须报错，而不是产生孤儿消息。
    assert!(store.append_message(999, "user", "孤儿", "", "").is_err());
    let on: i64 = {
        let conn = store.conn.lock().unwrap();
        conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(on, 1);
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
