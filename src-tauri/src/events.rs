//! 统一事件流：机器、会话、任务、部署的动态与审计都记在同一张表里，
//! 「活动」页与仪表盘从这里读。

use crate::AppState;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::sync::Arc;
use tauri::State;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    pub id: i64,
    pub ts: String,
    pub kind: String,
    pub level: String,
    pub source: String,
    pub message: String,
    pub link: Option<String>,
}

/// 记一条事件。`db` 由调用方提供已加锁的连接（本函数内部不再加锁）。
///
/// - `kind`：`node.online` / `session.started` / `task.status` / `deploy.state` …
/// - `level`：`info` / `warning` / `error`
/// - `source`：事件主体的名字（机器名、会话号、项目名），界面上按它筛选
/// - `link`：界面上可以点过去的位置（如 `#/sessions?s=12`）
pub fn record(
    db: &Connection,
    kind: &str,
    level: &str,
    source: &str,
    message: &str,
    link: Option<&str>,
) {
    let _ = db.execute(
        "INSERT INTO events (kind, level, source, message, link) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![kind, level, source, message, link],
    );
}

fn row_to_event(row: &rusqlite::Row) -> rusqlite::Result<Event> {
    Ok(Event {
        id: row.get(0)?,
        ts: row.get(1)?,
        kind: row.get(2)?,
        level: row.get(3)?,
        source: row.get(4)?,
        message: row.get(5)?,
        link: row.get(6)?,
    })
}

const COLS: &str = "id, ts, kind, level, source, message, link";

#[tauri::command]
pub fn events_list(
    state: State<'_, Arc<AppState>>,
    limit: Option<i64>,
    kind: Option<String>,
) -> Result<Vec<Event>, String> {
    let limit = limit.unwrap_or(100).clamp(1, 1000);
    let db = state.db.lock().unwrap();
    let mut stmt = db
        .prepare_cached(&format!(
            "SELECT {COLS} FROM events
             WHERE (?1 IS NULL OR kind = ?1 OR kind LIKE ?1 || '.%')
             ORDER BY id DESC LIMIT ?2"
        ))
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![kind, limit], row_to_event)
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(rows)
}

/// 事件的条数：总计、需要留意的（警告与错误）、以及各个模块（kind 的第一段）各多少
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventCounts {
    pub total: i64,
    pub warning: i64,
    pub error: i64,
    pub by_kind: std::collections::HashMap<String, i64>,
}

#[tauri::command]
pub fn events_counts(state: State<'_, Arc<AppState>>) -> Result<EventCounts, String> {
    let db = state.db.lock().unwrap();
    let mut stmt = db
        .prepare_cached("SELECT level, substr(kind, 1, instr(kind || '.', '.') - 1), count(*) FROM events GROUP BY level, 2")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    let mut counts = EventCounts::default();
    for (level, kind, count) in rows.flatten() {
        counts.total += count;
        if level == "warning" {
            counts.warning += count;
        } else if level == "error" {
            counts.error += count;
        }
        *counts.by_kind.entry(kind).or_default() += count;
    }
    Ok(counts)
}

/// 清空事件（保留最近 `keep` 条）
#[tauri::command]
pub fn events_clear(state: State<'_, Arc<AppState>>, keep: Option<i64>) -> Result<usize, String> {
    let db = state.db.lock().unwrap();
    let keep = keep.unwrap_or(0).max(0);
    let removed = if keep == 0 {
        db.execute("DELETE FROM events", [])
            .map_err(|e| e.to_string())?
    } else {
        let threshold: Option<i64> = db
            .query_row(
                "SELECT id FROM events ORDER BY id DESC LIMIT 1 OFFSET ?1",
                params![keep],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        match threshold {
            Some(id) => db
                .execute("DELETE FROM events WHERE id <= ?1", params![id])
                .map_err(|e| e.to_string())?,
            None => 0,
        }
    };
    Ok(removed)
}
