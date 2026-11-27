//! 本地状态库：SQLite（app_data_dir/autoagents.db）。

use rusqlite::Connection;
use std::path::Path;

pub fn init(path: &Path) -> Result<Connection, String> {
    let conn = Connection::open(path).map_err(|e| format!("打开数据库失败: {e}"))?;
    // 启用外键级联、WAL 并发读写、正常同步与锁等待超时，消除默认设置下的锁竞争与孤儿行
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA busy_timeout = 5000;",
    )
    .map_err(|e| format!("配置数据库参数失败: {e}"))?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS tasks (
            id           INTEGER PRIMARY KEY AUTOINCREMENT,
            title        TEXT NOT NULL,
            source       TEXT NOT NULL DEFAULT 'manual',
            repo         TEXT,
            issue_number INTEGER,
            issue_url    TEXT,
            agent_id     TEXT,
            workflow_id  INTEGER,
            status       TEXT NOT NULL DEFAULT 'pending',
            branch       TEXT,
            node_id      TEXT,
            adapter      TEXT,
            model        TEXT,
            workdir      TEXT,
            session_id   INTEGER,
            detail       TEXT,
            base_branch  TEXT,
            pr_url       TEXT,
            pr_number    INTEGER,
            worktree     TEXT,
            base_sha     TEXT,
            head_sha     TEXT,
            failure      TEXT,
            auto_pr      INTEGER NOT NULL DEFAULT 0,
            checks       TEXT,
            instructions TEXT,
            created_at   TEXT DEFAULT (datetime('now')),
            updated_at   TEXT DEFAULT (datetime('now'))
        );
        CREATE TABLE IF NOT EXISTS sessions (
            id           INTEGER PRIMARY KEY AUTOINCREMENT,
            adapter      TEXT NOT NULL,
            title        TEXT NOT NULL,
            model        TEXT,
            workdir      TEXT,
            node_id      TEXT,
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
            duration_ms   INTEGER,
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
        );
        -- 部署目标：登记了的 Vercel 项目 / Cloudflare Pages 项目 / Worker。平台令牌不在这里，存 Keychain
        CREATE TABLE IF NOT EXISTS deploy_targets (
            id                     INTEGER PRIMARY KEY AUTOINCREMENT,
            provider               TEXT NOT NULL,
            name                   TEXT NOT NULL,
            platform_id            TEXT NOT NULL,
            scope                  TEXT NOT NULL DEFAULT '',
            url                    TEXT,
            repo                   TEXT,
            repo_id                TEXT,
            git_type               TEXT,
            branch                 TEXT,
            framework              TEXT,
            prod_requires_approval INTEGER NOT NULL DEFAULT 1,
            auto_fix               INTEGER NOT NULL DEFAULT 1,
            auto_pr                INTEGER NOT NULL DEFAULT 0,
            auto_deploy            INTEGER NOT NULL DEFAULT 0,
            auto_rollback          INTEGER NOT NULL DEFAULT 0,
            health_path            TEXT NOT NULL DEFAULT '/',
            health_contains        TEXT,
            has_hook               INTEGER NOT NULL DEFAULT 0,
            created_at             TEXT DEFAULT (datetime('now')),
            updated_at             TEXT DEFAULT (datetime('now'))
        );
        -- 部署记录：从平台拉回来后归成同一种样子。项目名等抄一份，目标删了也还能看
        CREATE TABLE IF NOT EXISTS deployments (
            id             TEXT PRIMARY KEY,
            target_id      INTEGER NOT NULL,
            provider       TEXT NOT NULL,
            project        TEXT NOT NULL,
            env            TEXT NOT NULL,
            state          TEXT NOT NULL,
            raw_state      TEXT,
            url            TEXT,
            inspector_url  TEXT,
            branch         TEXT,
            commit_sha     TEXT,
            commit_message TEXT,
            author         TEXT,
            created_ms     INTEGER,
            building_ms    INTEGER,
            ready_ms       INTEGER,
            error_code     TEXT,
            error_message  TEXT,
            rollback_ok    INTEGER NOT NULL DEFAULT 0,
            current        INTEGER NOT NULL DEFAULT 0,
            guarded        INTEGER NOT NULL DEFAULT 0,
            health         TEXT,
            task_id        INTEGER,
            updated_at     TEXT DEFAULT (datetime('now'))
        );
        -- 工作流：多个步骤串成的一张有向无环图（竞争模式、交叉审查这类多 Agent 协同）
        CREATE TABLE IF NOT EXISTS workflows (
            id           INTEGER PRIMARY KEY AUTOINCREMENT,
            name         TEXT NOT NULL,
            template     TEXT,
            repo         TEXT NOT NULL,
            issue_number INTEGER,
            issue_url    TEXT,
            issue_title  TEXT,
            pr_number    INTEGER,
            instructions TEXT,
            status       TEXT NOT NULL DEFAULT 'pending',
            detail       TEXT,
            created_at   TEXT DEFAULT (datetime('now')),
            updated_at   TEXT DEFAULT (datetime('now'))
        );
        CREATE TABLE IF NOT EXISTS workflow_steps (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            workflow_id INTEGER NOT NULL REFERENCES workflows(id) ON DELETE CASCADE,
            seq         INTEGER NOT NULL,
            name        TEXT NOT NULL,
            kind        TEXT NOT NULL,
            depends_on  TEXT NOT NULL DEFAULT '[]',
            config      TEXT NOT NULL DEFAULT '{}',
            status      TEXT NOT NULL DEFAULT 'pending',
            task_id     INTEGER,
            session_id  INTEGER,
            outcome     TEXT,
            detail      TEXT,
            failure     TEXT,
            started_at  TEXT,
            finished_at TEXT
        );
        CREATE TABLE IF NOT EXISTS rules (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            name       TEXT NOT NULL,
            kind       TEXT NOT NULL,
            repo       TEXT NOT NULL,
            config     TEXT NOT NULL DEFAULT '{}',
            enabled    INTEGER NOT NULL DEFAULT 1,
            created_at TEXT DEFAULT (datetime('now')),
            updated_at TEXT DEFAULT (datetime('now'))
        );
        CREATE TABLE IF NOT EXISTS rule_runs (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            rule_id     INTEGER NOT NULL REFERENCES rules(id) ON DELETE CASCADE,
            key         TEXT NOT NULL,
            workflow_id INTEGER,
            posted      INTEGER NOT NULL DEFAULT 0,
            created_at  TEXT DEFAULT (datetime('now')),
            UNIQUE(rule_id, key)
        );",
    )
    .map_err(|e| format!("初始化表结构失败: {e}"))?;

    // 版本化迁移：旧库升级（user_version < 1 时补齐历史缺失列）
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap_or(0);
    if version < 1 {
        for column in [
            "ALTER TABLE sessions ADD COLUMN model TEXT",
            "ALTER TABLE sessions ADD COLUMN workdir TEXT",
            "ALTER TABLE sessions ADD COLUMN node_id TEXT",
            "ALTER TABLE messages ADD COLUMN input_tokens INTEGER",
            "ALTER TABLE messages ADD COLUMN output_tokens INTEGER",
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
            "ALTER TABLE tasks ADD COLUMN worktree TEXT",
            "ALTER TABLE tasks ADD COLUMN base_sha TEXT",
            "ALTER TABLE tasks ADD COLUMN head_sha TEXT",
            "ALTER TABLE tasks ADD COLUMN failure TEXT",
            "ALTER TABLE tasks ADD COLUMN auto_pr INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE tasks ADD COLUMN workflow_id INTEGER",
            "ALTER TABLE tasks ADD COLUMN checks TEXT",
            "ALTER TABLE tasks ADD COLUMN instructions TEXT",
            "ALTER TABLE deploy_targets ADD COLUMN auto_pr INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE deploy_targets ADD COLUMN auto_deploy INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE deploy_targets ADD COLUMN auto_rollback INTEGER NOT NULL DEFAULT 0",
        ] {
            let _ = conn.execute(column, []);
        }
        conn.execute_batch("PRAGMA user_version = 1;")
            .map_err(|e| format!("更新数据库版本失败: {e}"))?;
    }

    // 高频查询索引（幂等创建）
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS deployments_target ON deployments(target_id, created_ms);
         CREATE INDEX IF NOT EXISTS deployments_branch ON deployments(branch, commit_sha);
         CREATE INDEX IF NOT EXISTS workflow_steps_wf ON workflow_steps(workflow_id, seq);
         CREATE INDEX IF NOT EXISTS workflow_steps_session ON workflow_steps(session_id);
         CREATE INDEX IF NOT EXISTS messages_session ON messages(session_id, id);
         CREATE INDEX IF NOT EXISTS messages_role_created ON messages(role, created_at);
         CREATE INDEX IF NOT EXISTS tasks_status ON tasks(status, updated_at);
         CREATE INDEX IF NOT EXISTS tasks_session ON tasks(session_id);
         CREATE INDEX IF NOT EXISTS tasks_workflow ON tasks(workflow_id);
         CREATE INDEX IF NOT EXISTS events_kind_ts ON events(kind, ts);
         CREATE INDEX IF NOT EXISTS workflows_status ON workflows(status);
         CREATE INDEX IF NOT EXISTS rule_runs_workflow ON rule_runs(workflow_id);",
    )
    .map_err(|e| format!("创建索引失败: {e}"))?;

    Ok(conn)
}
