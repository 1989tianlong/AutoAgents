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
        );
        -- 部署目标：登记了的 Vercel 项目 / Cloudflare Pages 项目 / Worker。平台令牌不在这里，存 Keychain
        CREATE TABLE IF NOT EXISTS deploy_targets (
            id                     INTEGER PRIMARY KEY AUTOINCREMENT,
            provider               TEXT NOT NULL,
            name                   TEXT NOT NULL,
            -- 平台上的标识：Vercel 的项目 id、Worker 的标签；Pages 项目就是名字
            platform_id            TEXT NOT NULL,
            -- 属于哪个团队 / 账户（Vercel teamId、Cloudflare account id）；空表示 Vercel 个人账户
            scope                  TEXT NOT NULL DEFAULT '',
            -- 生产地址
            url                    TEXT,
            repo                   TEXT,
            -- GitHub 仓库的数字 id（Vercel 按它指定仓库）
            repo_id                TEXT,
            -- 平台那边连的 Git 服务：github / gitlab …；空表示没连
            git_type               TEXT,
            -- 生产分支
            branch                 TEXT,
            framework              TEXT,
            -- 应用里发起生产部署要先确认
            prod_requires_approval INTEGER NOT NULL DEFAULT 1,
            -- 生产部署失败时自动建修复任务
            auto_fix               INTEGER NOT NULL DEFAULT 1,
            health_path            TEXT NOT NULL DEFAULT '/',
            health_contains        TEXT,
            -- Workers：存了 Deploy Hook 的地址（地址本身在 Keychain）
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
            -- queued / building / ready / error / canceled / other
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
            -- 能回滚到这一次
            rollback_ok    INTEGER NOT NULL DEFAULT 0,
            -- 现在线上跑的就是这一次
            current        INTEGER NOT NULL DEFAULT 0,
            -- 到终态后守护已经处理过（查过健康、该建的修复任务建过了）
            guarded        INTEGER NOT NULL DEFAULT 0,
            -- 健康检查结果：ok 或没通过的原因
            health         TEXT,
            -- 守护为这次失败建的修复任务
            task_id        INTEGER,
            updated_at     TEXT DEFAULT (datetime('now'))
        );
        CREATE INDEX IF NOT EXISTS deployments_target ON deployments(target_id, created_ms);
        CREATE INDEX IF NOT EXISTS deployments_branch ON deployments(branch, commit_sha);
        -- 工作流：多个步骤串成的一张有向无环图（竞争模式、交叉审查这类多 Agent 协同）
        CREATE TABLE IF NOT EXISTS workflows (
            id           INTEGER PRIMARY KEY AUTOINCREMENT,
            name         TEXT NOT NULL,
            -- 从哪个模板建的（issue-fix / pr-review / race）；自定义时为空
            template     TEXT,
            repo         TEXT NOT NULL,
            issue_number INTEGER,
            issue_url    TEXT,
            issue_title  TEXT,
            pr_number    INTEGER,
            -- 没有 Issue 时的需求说明，建任务时原样交给 Agent
            instructions TEXT,
            -- pending / running / waiting / done / failed / cancelled
            status       TEXT NOT NULL DEFAULT 'pending',
            detail       TEXT,
            created_at   TEXT DEFAULT (datetime('now')),
            updated_at   TEXT DEFAULT (datetime('now'))
        );
        -- 工作流的步骤。depends_on 是步骤 seq 的数组（JSON），带 ? 前缀表示软依赖：
        -- 上游失败也算解锁（竞争模式的评审门不因为一路失败而打不开）
        CREATE TABLE IF NOT EXISTS workflow_steps (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            workflow_id INTEGER NOT NULL REFERENCES workflows(id) ON DELETE CASCADE,
            seq         INTEGER NOT NULL,
            name        TEXT NOT NULL,
            -- task: 走任务流水线；session: 一段纯会话（审查、写说明）；gate: 等人决策
            kind        TEXT NOT NULL,
            depends_on  TEXT NOT NULL DEFAULT '[]',
            -- JSON：adapter、node_id、workdir、model、auto_pr、prompt
            config      TEXT NOT NULL DEFAULT '{}',
            -- pending / running / waiting / done / failed / skipped
            status      TEXT NOT NULL DEFAULT 'pending',
            task_id     INTEGER,
            session_id  INTEGER,
            outcome     TEXT,
            detail      TEXT,
            failure     TEXT,
            started_at  TEXT,
            finished_at TEXT
        );
        CREATE INDEX IF NOT EXISTS workflow_steps_wf ON workflow_steps(workflow_id, seq);
        -- 自动化规则：盯着 GitHub，满足条件就自动建工作流（Issue 打标签、新 PR）。
        -- 机器 / Agent / 仓库位置在规则里配一份，之后不再要人介入
        CREATE TABLE IF NOT EXISTS rules (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            name       TEXT NOT NULL,
            -- issue-label（Issue 打上某个标签） / new-pr（仓库来了新 PR）
            kind       TEXT NOT NULL,
            repo       TEXT NOT NULL,
            -- JSON：label、template、adapters、model、auto_pr、node_id、workdir、comment
            config     TEXT NOT NULL DEFAULT '{}',
            enabled    INTEGER NOT NULL DEFAULT 1,
            created_at TEXT DEFAULT (datetime('now')),
            updated_at TEXT DEFAULT (datetime('now'))
        );
        -- 规则跑过的记录：一条 Issue / 一个 PR 只自动处理一次
        CREATE TABLE IF NOT EXISTS rule_runs (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            rule_id     INTEGER NOT NULL REFERENCES rules(id) ON DELETE CASCADE,
            -- 去重键：issue:7 / pr:5
            key         TEXT NOT NULL,
            workflow_id INTEGER,
            -- 审查结论已经贴回 PR 评论
            posted      INTEGER NOT NULL DEFAULT 0,
            created_at  TEXT DEFAULT (datetime('now')),
            UNIQUE(rule_id, key)
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
        "ALTER TABLE tasks ADD COLUMN worktree TEXT",
        "ALTER TABLE tasks ADD COLUMN base_sha TEXT",
        "ALTER TABLE tasks ADD COLUMN head_sha TEXT",
        "ALTER TABLE tasks ADD COLUMN failure TEXT",
        "ALTER TABLE tasks ADD COLUMN auto_pr INTEGER NOT NULL DEFAULT 0",
        "ALTER TABLE tasks ADD COLUMN checks TEXT",
        "ALTER TABLE tasks ADD COLUMN instructions TEXT",
    ] {
        let _ = conn.execute(column, []);
    }
    Ok(conn)
}
