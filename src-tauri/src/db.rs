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
            external_ref TEXT,
            created_at   TEXT DEFAULT (datetime('now'))
        );
        CREATE TABLE IF NOT EXISTS messages (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            role       TEXT NOT NULL,
            content    TEXT NOT NULL,
            cost_usd   REAL,
            created_at TEXT DEFAULT (datetime('now'))
        );",
    )
    .map_err(|e| format!("初始化表结构失败: {e}"))?;
    Ok(conn)
}
