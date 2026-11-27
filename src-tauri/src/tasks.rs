//! 任务：Issue（或一段需求说明）转成的执行单元，以及它的状态流转。
//!
//! 任务本身只是记录；真正干活的是 [`crate::pipeline`]：在独立的工作树里让 Agent 实现，
//! 人工确认后提交、推送、开 PR，再跟踪 PR 与 CI。
//!
//! 状态：
//! - `pending`   待启动
//! - `running`   Agent 正在处理（这一任务的会话里有新一轮在跑）
//! - `changes`   改动已就绪，等人工确认后再提交、开 PR
//! - `reviewing` PR 已创建，等审查、合并
//! - `done`      PR 已合并（或人工标记完成）
//! - `failed`    中途失败，或 PR 被关闭；可以重试

use crate::AppState;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::sync::Arc;
use tauri::State;

pub const STATUSES: &[&str] = &[
    "pending",
    "running",
    "changes",
    "reviewing",
    "done",
    "failed",
];

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub id: i64,
    pub title: String,
    /// `issue` 或 `manual`
    pub source: String,
    /// `owner/repo`
    pub repo: Option<String>,
    pub issue_number: Option<i64>,
    pub issue_url: Option<String>,
    pub agent_id: Option<String>,
    pub status: String,
    pub branch: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    // ---- 流水线 ----
    /// 在哪台机器上做
    pub node_id: Option<String>,
    pub adapter: Option<String>,
    pub model: Option<String>,
    /// 仓库在那台机器上的位置
    pub workdir: Option<String>,
    /// 这一任务专用的工作树
    pub worktree: Option<String>,
    pub base_branch: Option<String>,
    /// 起步时的提交
    pub base_sha: Option<String>,
    /// 最近一次推送的提交
    pub head_sha: Option<String>,
    /// 这一任务的会话：Agent 的全部往来都在里面
    pub session_id: Option<i64>,
    pub pr_number: Option<i64>,
    pub pr_url: Option<String>,
    /// 最近一步的说明（改了多少文件、几项检查通过……）
    pub detail: Option<String>,
    /// 失败原因
    pub failure: Option<String>,
    /// 手动任务的需求说明（启动时原样交给 Agent）
    pub instructions: Option<String>,
    /// Agent 做完后不等确认，直接提交并开 PR
    pub auto_pr: bool,
    /// 最近一次取到的 PR 检查（JSON）
    pub checks: Option<serde_json::Value>,
}

const COLS: &str = "id, title, source, repo, issue_number, issue_url, agent_id, status, branch, created_at, updated_at, \
     node_id, adapter, model, workdir, session_id, detail, base_branch, pr_url, pr_number, \
     worktree, base_sha, head_sha, failure, auto_pr, checks, instructions";

fn row_to_task(row: &rusqlite::Row) -> rusqlite::Result<Task> {
    let checks: Option<String> = row.get(25)?;
    Ok(Task {
        id: row.get(0)?,
        title: row.get(1)?,
        source: row.get(2)?,
        repo: row.get(3)?,
        issue_number: row.get(4)?,
        issue_url: row.get(5)?,
        agent_id: row.get(6)?,
        status: row.get(7)?,
        branch: row.get(8)?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
        node_id: row.get(11)?,
        adapter: row.get(12)?,
        model: row.get(13)?,
        workdir: row.get(14)?,
        session_id: row.get(15)?,
        detail: row.get(16)?,
        base_branch: row.get(17)?,
        pr_url: row.get(18)?,
        pr_number: row.get(19)?,
        worktree: row.get(20)?,
        base_sha: row.get(21)?,
        head_sha: row.get(22)?,
        failure: row.get(23)?,
        auto_pr: row.get::<_, Option<i64>>(24)?.unwrap_or(0) != 0,
        checks: checks.and_then(|text| serde_json::from_str(&text).ok()),
        instructions: row.get(26)?,
    })
}

/// 按 id 取任务
pub fn load(db: &Connection, id: i64) -> Result<Task, String> {
    db.query_row(
        &format!("SELECT {COLS} FROM tasks WHERE id = ?1"),
        params![id],
        row_to_task,
    )
    .optional()
    .map_err(|e| e.to_string())?
    .ok_or_else(|| format!("任务不存在：#{id}"))
}

/// 这一任务的会话对应的任务
pub fn by_session(db: &Connection, session_id: i64) -> Option<Task> {
    db.query_row(
        &format!("SELECT {COLS} FROM tasks WHERE session_id = ?1 ORDER BY id DESC LIMIT 1"),
        params![session_id],
        row_to_task,
    )
    .optional()
    .ok()
    .flatten()
}

/// 按状态取任务
pub fn in_status(db: &Connection, status: &str) -> Result<Vec<Task>, String> {
    let mut stmt = db
        .prepare_cached(&format!(
            "SELECT {COLS} FROM tasks WHERE status = ?1 ORDER BY id"
        ))
        .map_err(|e| e.to_string())?;
    stmt.query_map(params![status], row_to_task)
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

/// 改状态并留一句说明。调用方持有数据库锁
pub fn set_status(
    db: &Connection,
    id: i64,
    status: &str,
    detail: Option<&str>,
    failure: Option<&str>,
) -> Result<(), String> {
    if !STATUSES.contains(&status) {
        return Err(format!("非法状态: {status}"));
    }
    db.execute(
        "UPDATE tasks SET status = ?1, detail = ?2, failure = ?3, updated_at = datetime('now') WHERE id = ?4",
        params![status, detail, failure, id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// 任务的分支名：Issue 任务沿用 `agent/issue-N`，其它用 `agent/task-N`
pub fn branch_name(task_id: i64, source: &str, issue_number: Option<i64>) -> String {
    match (source, issue_number) {
        ("issue", Some(number)) => format!("agent/issue-{number}"),
        _ => format!("agent/task-{task_id}"),
    }
}

/// 任务在活动流里的来源：仓库名（带 Issue 号）
pub fn event_source(task: &Task) -> String {
    match (&task.repo, task.issue_number) {
        (Some(repo), Some(number)) => {
            format!("{} #{number}", repo.rsplit('/').next().unwrap_or(repo))
        }
        (Some(repo), None) => repo.rsplit('/').next().unwrap_or(repo).to_string(),
        _ => "任务".to_string(),
    }
}

/// 记一条任务事件（活动流里按任务归，链回任务详情）
pub fn record(db: &Connection, task: &Task, kind: &str, level: &str, message: &str) {
    let title: String = task.title.chars().take(60).collect();
    let ellipsis = if task.title.chars().count() > 60 {
        "…"
    } else {
        ""
    };
    crate::events::record(
        db,
        kind,
        level,
        &event_source(task),
        &format!("{title}{ellipsis}：{message}"),
        Some(&format!("#/tasks?t={}", task.id)),
    );
}

#[tauri::command]
pub fn tasks_list(state: State<'_, Arc<AppState>>) -> Result<Vec<Task>, String> {
    let db = state.db.lock().unwrap();
    let mut stmt = db
        .prepare_cached(&format!(
            "SELECT {COLS} FROM tasks ORDER BY updated_at DESC, id DESC LIMIT 200"
        ))
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], row_to_task)
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(rows)
}

#[tauri::command]
pub fn task_get(state: State<'_, Arc<AppState>>, id: i64) -> Result<Task, String> {
    load(&state.db.lock().unwrap(), id)
}

/// 从 Issue 创建任务：分支名按 `agent/issue-N` 生成。
#[tauri::command]
pub fn task_create_issue(
    state: State<'_, Arc<AppState>>,
    title: String,
    repo: String,
    issue_number: i64,
    issue_url: String,
) -> Result<Task, String> {
    if title.trim().is_empty() || repo.trim().is_empty() {
        return Err("标题和仓库不能为空".into());
    }
    create_issue(
        &state.db.lock().unwrap(),
        &title,
        &repo,
        issue_number,
        &issue_url,
    )
}

/// 建任务的两个入口（不经过 Tauri 的命令层，测试与内部调用用）
pub fn create_issue(
    db: &Connection,
    title: &str,
    repo: &str,
    number: i64,
    url: &str,
) -> Result<Task, String> {
    let branch = branch_name(0, "issue", Some(number));
    db.execute(
        "INSERT INTO tasks (title, source, repo, issue_number, issue_url, branch)
         VALUES (?1, 'issue', ?2, ?3, ?4, ?5)",
        params![title.trim(), repo.trim(), number, url, branch],
    )
    .map_err(|e| e.to_string())?;
    let task = load(db, db.last_insert_rowid())?;
    record(db, &task, "task.created", "info", "已从 Issue 创建任务");
    Ok(task)
}

pub fn create_manual(
    db: &Connection,
    title: &str,
    repo: &str,
    instructions: &str,
) -> Result<Task, String> {
    db.execute(
        "INSERT INTO tasks (title, source, repo, instructions) VALUES (?1, 'manual', ?2, ?3)",
        params![title.trim(), repo.trim(), instructions.trim()],
    )
    .map_err(|e| e.to_string())?;
    let id = db.last_insert_rowid();
    db.execute(
        "UPDATE tasks SET branch = ?1 WHERE id = ?2",
        params![branch_name(id, "manual", None), id],
    )
    .map_err(|e| e.to_string())?;
    let task = load(db, id)?;
    record(db, &task, "task.created", "info", "已创建任务");
    Ok(task)
}

/// 手动建一个任务：一段需求说明（不经过 Issue）。说明会原样交给 Agent。
#[tauri::command]
pub fn task_create_manual(
    state: State<'_, Arc<AppState>>,
    title: String,
    repo: String,
    instructions: String,
) -> Result<Task, String> {
    if title.trim().is_empty() || repo.trim().is_empty() {
        return Err("标题和仓库不能为空".into());
    }
    if instructions.trim().is_empty() {
        return Err("请写清楚要做什么".into());
    }
    create_manual(&state.db.lock().unwrap(), &title, &repo, &instructions)
}

/// 人工改状态（看板上直接改，或标记完成）
#[tauri::command]
pub fn task_set_status(
    state: State<'_, Arc<AppState>>,
    id: i64,
    status: String,
) -> Result<(), String> {
    if !STATUSES.contains(&status.as_str()) {
        return Err(format!("非法状态: {status}"));
    }
    let db = state.db.lock().unwrap();
    set_status(&db, id, &status, None, None)?;
    let task = load(&db, id)?;
    record(
        &db,
        &task,
        "task.status",
        "info",
        &format!("状态改为 {status}"),
    );
    Ok(())
}

/// 删除任务（若正在运行则先中止会话，再清掉工作树；PR 不动）
#[tauri::command]
pub async fn task_delete(state: State<'_, Arc<AppState>>, id: i64) -> Result<(), String> {
    let task = load(&state.db.lock().unwrap(), id)?;
    let running = task.status == "running"
        || task
            .session_id
            .is_some_and(|sid| state.live.lock().unwrap().contains_key(&sid));
    if running {
        let _ = crate::pipeline::abort(&state, id).await;
    }
    if let Err(e) = crate::pipeline::clean_worktree(&state, &task).await {
        eprintln!("[pipeline] 清理工作树失败：{e}");
    }
    let db = state.db.lock().unwrap();
    db.execute("DELETE FROM tasks WHERE id = ?1", params![id])
        .map_err(|e| e.to_string())?;
    record(&db, &task, "task.deleted", "info", "已删除任务");
    Ok(())
}
