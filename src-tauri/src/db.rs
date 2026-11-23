//! 本地状态库：SQLite（app_data_dir/autoagents.db）。

use rusqlite::Connection;
use std::path::Path;

pub fn init(path: &Path) -> Result<Connection, String> {
    let conn = Connection::open(path).map_err(|e| format!("打开数据库失败: {e}"))?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS tasks (
            id           INTEGER PRIMARY KEY AUTOINCREMENT,
            title        TEXT NOT NULL,
            source       TEXT NOT NULL DEFAULT 'manual',
            repo         TEXT,
            issue_number INTEGER,
            issue_url    TEXT,
            agent_id     TEXT,
            status       TEXT NOT NULL DEFAULT 'pending',
            branch       TEXT,
            created_at   TEXT DEFAULT (datetime('now')),
            updated_at   TEXT DEFAULT (datetime('now'))
        );
        CREATE TABLE IF NOT EXISTS sessions (
            id           INTEGER PRIMARY KEY AUTOINCREMENT,
            adapter      TEXT NOT NULL,
            title        TEXT NOT NULL,
            model        TEXT,
            -- Agent 的工作目录；grok / pi 的续聊依赖它保持一致
            workdir      TEXT,
            -- Agent 侧的会话标识，用于续聊
            external_ref TEXT,
            created_at   TEXT DEFAULT (datetime('now'))
        );
        CREATE TABLE IF NOT EXISTS messages (
            id            INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id    INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            role          TEXT NOT NULL,
            content       TEXT NOT NULL,
            cost_usd      REAL,
            input_tokens  INTEGER,
            output_tokens INTEGER,
            created_at    TEXT DEFAULT (datetime('now'))
        );
        CREATE TABLE IF NOT EXISTS custom_models (
            adapter    TEXT NOT NULL,
            model      TEXT NOT NULL,
            created_at TEXT DEFAULT (datetime('now')),
            PRIMARY KEY (adapter, model)
        );",
    )
    .map_err(|e| format!("初始化表结构失败: {e}"))?;
    // 轻量迁移：给旧库补列（列已存在时 ALTER 会报错，直接忽略）
    for column in [
        "ALTER TABLE sessions ADD COLUMN model TEXT",
        "ALTER TABLE sessions ADD COLUMN workdir TEXT",
        "ALTER TABLE messages ADD COLUMN input_tokens INTEGER",
        "ALTER TABLE messages ADD COLUMN output_tokens INTEGER",
    ] {
        let _ = conn.execute(column, []);
    }
    Ok(conn)
}
