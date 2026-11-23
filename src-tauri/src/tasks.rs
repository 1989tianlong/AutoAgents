//! 任务：Issue 转任务的本地持久化与状态流转（执行引擎在 M2/M3 接入）。

use crate::AppState;
use rusqlite::params;
use serde::Serialize;
use std::sync::Arc;
use tauri::State;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub id: i64,
    pub title: String,
    pub source: String,
    pub repo: Option<String>,
    pub issue_number: Option<i64>,
    pub issue_url: Option<String>,
    pub agent_id: Option<String>,
    pub status: String,
    pub branch: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

const STATUSES: &[&str] = &["pending", "running", "reviewing", "done", "failed"];

fn row_to_task(row: &rusqlite::Row) -> rusqlite::Result<Task> {
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
    })
}

const COLS: &str = "id, title, source, repo, issue_number, issue_url, agent_id, status, branch, created_at, updated_at";

#[tauri::command]
pub fn tasks_list(state: State<'_, Arc<AppState>>) -> Result<Vec<Task>, String> {
    let db = state.db.lock().unwrap();
    let mut stmt = db
        .prepare(&format!("SELECT {COLS} FROM tasks ORDER BY updated_at DESC, id DESC LIMIT 200"))
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], row_to_task)
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(rows)
}

/// 从 Issue 创建任务：自动生成 agent/issue-N 分支名。
#[tauri::command]
pub fn task_create_issue(
    state: State<'_, Arc<AppState>>,
    title: String,
    repo: String,
    issue_number: i64,
    issue_url: String,
) -> Result<i64, String> {
    let branch = format!("agent/issue-{issue_number}");
    let db = state.db.lock().unwrap();
    db.execute(
        "INSERT INTO tasks (title, source, repo, issue_number, issue_url, branch)
         VALUES (?1, 'issue', ?2, ?3, ?4, ?5)",
        params![title, repo, issue_number, issue_url, branch],
    )
    .map_err(|e| e.to_string())?;
    Ok(db.last_insert_rowid())
}

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
    db.execute(
        "UPDATE tasks SET status = ?1, updated_at = datetime('now') WHERE id = ?2",
        params![status, id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn task_delete(state: State<'_, Arc<AppState>>, id: i64) -> Result<(), String> {
    let db = state.db.lock().unwrap();
    db.execute("DELETE FROM tasks WHERE id = ?1", params![id])
        .map_err(|e| e.to_string())?;
    Ok(())
}
