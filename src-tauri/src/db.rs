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
            -- Agent 的工作目录（机器上的路径）；grok / pi 的续聊依赖它保持一致
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
        );
        -- 配对过的远程机器。与机器配对的密钥不在这里：存系统 Keychain
        CREATE TABLE IF NOT EXISTS nodes (
            id          TEXT PRIMARY KEY,
            name        TEXT NOT NULL,
            -- 非空表示经这个中继接入；否则用 addr 直连
            relay_url   TEXT NOT NULL DEFAULT '',
            addr        TEXT NOT NULL DEFAULT '',
            fingerprint TEXT NOT NULL DEFAULT '',
            -- 最近一次拿到的机器信息（JSON）
            info        TEXT,
            created_at  TEXT DEFAULT (datetime('now'))
        );
        -- 杂项设置（中继地址、安装脚本地址……；令牌类不在这里，存 Keychain）
        CREATE TABLE IF NOT EXISTS settings (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        -- 统一事件流：机器、会话、任务、部署的动态与审计都在这里
        CREATE TABLE IF NOT EXISTS events (
            id      INTEGER PRIMARY KEY AUTOINCREMENT,
            ts      TEXT DEFAULT (datetime('now')),
            kind    TEXT NOT NULL,
            level   TEXT NOT NULL DEFAULT 'info',
            source  TEXT,
            message TEXT NOT NULL,
            link    TEXT
        );",
    )
    .map_err(|e| format!("初始化表结构失败: {e}"))?;
    // 轻量迁移：给旧库补列（列已存在时 ALTER 会报错，直接忽略）
    for column in [
        "ALTER TABLE sessions ADD COLUMN model TEXT",
        "ALTER TABLE sessions ADD COLUMN workdir TEXT",
        "ALTER TABLE messages ADD COLUMN input_tokens INTEGER",
        "ALTER TABLE messages ADD COLUMN output_tokens INTEGER",
        "ALTER TABLE sessions ADD COLUMN node_id TEXT",
        "ALTER TABLE messages ADD COLUMN duration_ms INTEGER",
        "ALTER TABLE tasks ADD COLUMN node_id TEXT",
        "ALTER TABLE tasks ADD COLUMN adapter TEXT",
        "ALTER TABLE tasks ADD COLUMN model TEXT",
        "ALTER TABLE tasks ADD COLUMN workdir TEXT",
        "ALTER TABLE tasks ADD COLUMN session_id INTEGER",
        "ALTER TABLE tasks ADD COLUMN detail TEXT",
        "ALTER TABLE tasks ADD COLUMN base_branch TEXT",
        "ALTER TABLE tasks ADD COLUMN pr_url TEXT",
        "ALTER TABLE tasks ADD COLUMN pr_number INTEGER",
    ] {
        let _ = conn.execute(column, []);
    }
    Ok(conn)
}
